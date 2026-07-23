//! The entire system is a directory of JSON files.
//!
//! ```text
//! .murmur/
//!   .gitignore            self-ignoring, like target/
//!   agents/<name>.json    presence (pid, cwd, last_seen)
//!   inbox/<name>/*.json   one file per pending message
//!   claims/<path>.json    advisory file claims with TTL
//!   log.jsonl             append-only copy of every message
//!   tmp/                  staging for atomic renames
//! ```
//!
//! Delivery is `write to tmp/ + rename into inbox/` — atomic on POSIX, so a
//! reader never sees a half-written message. No daemon, no sockets, no locks
//! beyond what the filesystem gives us for free.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_CLAIM_TTL_SECS: u64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Msg {
    pub id: String,
    pub from: String,
    pub to: String,
    /// unix millis
    pub ts: u64,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub pid: u32,
    pub cwd: String,
    pub joined_at: u64,
    pub last_seen: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub path: String,
    pub holder: String,
    /// unix secs
    pub ts: u64,
    pub ttl_secs: u64,
}

impl Claim {
    pub fn expired(&self) -> bool {
        now_secs() >= self.ts + self.ttl_secs
    }
}

pub enum ClaimResult {
    Granted,
    Held(Claim),
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    /// `MURMUR_DIR`, else the nearest `.murmur` walking up from cwd,
    /// else `.murmur` in cwd (created lazily on first write).
    pub fn locate() -> Result<Store> {
        if let Ok(dir) = std::env::var("MURMUR_DIR") {
            return Ok(Store { root: PathBuf::from(dir) });
        }
        let cwd = std::env::current_dir().context("cannot determine cwd")?;
        let mut dir = cwd.as_path();
        loop {
            let candidate = dir.join(".murmur");
            if candidate.is_dir() {
                return Ok(Store { root: candidate });
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => return Ok(Store { root: cwd.join(".murmur") }),
            }
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn init(&self) -> Result<()> {
        for sub in ["agents", "inbox", "claims", "tmp"] {
            fs::create_dir_all(self.root.join(sub))?;
        }
        let gitignore = self.root.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, "*\n")?;
        }
        Ok(())
    }

    // ---- messages ----

    /// Deliver a message. `to` may be an agent name or `*` for broadcast.
    /// Returns the list of recipients.
    pub fn send(&self, from: &str, to: &str, body: &str) -> Result<Vec<String>> {
        valid_name(from)?;
        self.init()?;
        self.touch(from)?;

        let recipients: Vec<String> = if to == "*" || to == "all" {
            let peers: Vec<String> = self
                .agents()?
                .into_iter()
                .map(|a| a.name)
                .filter(|n| n != from)
                .collect();
            if peers.is_empty() {
                bail!("broadcast has no recipients: no other agents have joined (see `murmur who`)");
            }
            peers
        } else {
            valid_name(to)?;
            vec![to.to_string()]
        };

        let ts = now_millis();
        let id = next_id(ts);
        let logged = Msg {
            id: id.clone(),
            from: from.to_string(),
            to: if recipients.len() > 1 { "*".into() } else { to.to_string() },
            ts,
            body: body.to_string(),
        };
        for recipient in &recipients {
            let msg = Msg { to: recipient.clone(), ..logged.clone() };
            let inbox = self.root.join("inbox").join(recipient);
            fs::create_dir_all(&inbox)?;
            let tmp = self.root.join("tmp").join(format!("{}-{}", recipient, id));
            fs::write(&tmp, serde_json::to_vec(&msg)?)?;
            fs::rename(&tmp, inbox.join(format!("{}.json", id)))?;
        }
        self.append_log(&logged)?;
        Ok(recipients)
    }

    /// Read pending messages, oldest first. Consumes them unless `peek`.
    pub fn drain(&self, name: &str, peek: bool) -> Result<Vec<Msg>> {
        valid_name(name)?;
        self.init()?;
        self.touch(name)?;
        let inbox = self.root.join("inbox").join(name);
        if !inbox.is_dir() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(&inbox)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
            .collect();
        paths.sort();
        let mut msgs = Vec::new();
        for path in paths {
            let Ok(bytes) = fs::read(&path) else { continue };
            if let Ok(msg) = serde_json::from_slice::<Msg>(&bytes) {
                msgs.push(msg);
            }
            if !peek {
                let _ = fs::remove_file(&path);
            }
        }
        Ok(msgs)
    }

    fn append_log(&self, msg: &Msg) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        let mut line = serde_json::to_vec(msg)?;
        line.push(b'\n');
        file.write_all(&line)?;
        Ok(())
    }

    pub fn log_path(&self) -> PathBuf {
        self.root.join("log.jsonl")
    }

    pub fn log_tail(&self, n: usize) -> Result<Vec<Msg>> {
        let path = self.log_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let content = fs::read_to_string(&path)?;
        let msgs: Vec<Msg> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        let skip = msgs.len().saturating_sub(n);
        Ok(msgs.into_iter().skip(skip).collect())
    }

    // ---- presence ----

    pub fn touch(&self, name: &str) -> Result<Agent> {
        valid_name(name)?;
        self.init()?;
        let path = self.root.join("agents").join(format!("{}.json", name));
        let ts = now_secs();
        let joined_at = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Agent>(&b).ok())
            .map(|a| a.joined_at)
            .unwrap_or(ts);
        let agent = Agent {
            name: name.to_string(),
            pid: owner_pid(),
            cwd: std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            joined_at,
            last_seen: ts,
        };
        let tmp = self.root.join("tmp").join(format!("agent-{}-{}", name, ts));
        fs::write(&tmp, serde_json::to_vec(&agent)?)?;
        fs::rename(&tmp, &path)?;
        Ok(agent)
    }

    pub fn leave(&self, name: &str) -> Result<()> {
        valid_name(name)?;
        let _ = fs::remove_file(self.root.join("agents").join(format!("{}.json", name)));
        for claim in self.claims()? {
            if claim.holder == name {
                let _ = self.release(&claim.path, name);
            }
        }
        Ok(())
    }

    pub fn agents(&self) -> Result<Vec<Agent>> {
        let dir = self.root.join("agents");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut agents: Vec<Agent> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| fs::read(e.path()).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect();
        agents.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(agents)
    }

    // ---- claims (advisory file locks) ----

    pub fn claim(&self, path: &str, holder: &str, ttl_secs: u64) -> Result<ClaimResult> {
        valid_name(holder)?;
        self.init()?;
        let canonical = canonical_path(path);
        let file = self.claim_file(&canonical);
        if let Some(existing) = read_claim(&file) {
            if existing.holder != holder && !existing.expired() {
                return Ok(ClaimResult::Held(existing));
            }
        }
        let claim = Claim {
            path: canonical,
            holder: holder.to_string(),
            ts: now_secs(),
            ttl_secs,
        };
        let tmp = self.root.join("tmp").join(format!("claim-{}-{}", holder, now_millis()));
        fs::write(&tmp, serde_json::to_vec(&claim)?)?;
        fs::rename(&tmp, &file)?;
        Ok(ClaimResult::Granted)
    }

    pub fn release(&self, path: &str, holder: &str) -> Result<bool> {
        let canonical = canonical_path(path);
        let file = self.claim_file(&canonical);
        if let Some(existing) = read_claim(&file) {
            if existing.holder == holder || existing.expired() {
                let _ = fs::remove_file(&file);
                return Ok(true);
            }
            return Ok(false);
        }
        Ok(true)
    }

    pub fn claims(&self) -> Result<Vec<Claim>> {
        let dir = self.root.join("claims");
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut claims: Vec<Claim> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .filter_map(|e| read_claim(&e.path()))
            .filter(|c| !c.expired())
            .collect();
        claims.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(claims)
    }

    fn claim_file(&self, canonical: &str) -> PathBuf {
        self.root.join("claims").join(format!("{}.json", encode(canonical)))
    }

    // ---- housekeeping ----

    /// Remove agents whose pid is gone and claims that have expired.
    /// Returns (agents_removed, claims_removed).
    pub fn clean(&self) -> Result<(usize, usize)> {
        let mut agents_removed = 0;
        for agent in self.agents()? {
            if !pid_alive(agent.pid) {
                self.leave(&agent.name)?;
                agents_removed += 1;
            }
        }
        let mut claims_removed = 0;
        let dir = self.root.join("claims");
        if dir.is_dir() {
            for entry in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
                match read_claim(&entry.path()) {
                    Some(c) if !c.expired() => {}
                    _ => {
                        let _ = fs::remove_file(entry.path());
                        claims_removed += 1;
                    }
                }
            }
        }
        Ok((agents_removed, claims_removed))
    }
}

fn read_claim(path: &Path) -> Option<Claim> {
    fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// Agent names become directory names, so keep them boring.
pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if !ok {
        bail!("invalid agent name '{}': use letters, digits, '-', '_', '.'", name);
    }
    Ok(())
}

/// Absolute-ize without requiring the file to exist (it may not, yet).
fn canonical_path(path: &str) -> String {
    let p = Path::new(path);
    if p.is_absolute() {
        path.to_string()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(p).display().to_string())
            .unwrap_or_else(|_| path.to_string())
    }
}

/// Filesystem-safe encoding of an arbitrary path.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'.' | b'_' | b'-' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// The process whose lifetime tracks the agent is our *parent* (the harness
/// or shell that invoked us) — every murmur command itself exits in ms.
fn owner_pid() -> u32 {
    #[cfg(unix)]
    {
        std::os::unix::process::parent_id()
    }
    #[cfg(not(unix))]
    {
        std::process::id()
    }
}

pub fn pid_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{}", pid)).exists()
    } else {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

pub fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn now_secs() -> u64 {
    now_millis() / 1000
}

fn next_id(ts: u64) -> String {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!("{:013}-{}-{:03}", ts, std::process::id(), seq)
}

pub fn ago(secs_since_epoch: u64) -> String {
    let delta = now_secs().saturating_sub(secs_since_epoch);
    match delta {
        0..=59 => format!("{}s", delta),
        60..=3599 => format!("{}m", delta / 60),
        3600..=86399 => format!("{}h", delta / 3600),
        _ => format!("{}d", delta / 86400),
    }
}

pub fn clock(ts_millis: u64) -> String {
    let secs = ts_millis / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}
