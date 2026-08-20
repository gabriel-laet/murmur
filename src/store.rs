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
    /// node the message originated on
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
    /// id of the message this replies to
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub re: Option<String>,
    /// sender is blocked waiting on a reply to this id
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub wants_reply: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub pid: u32,
    pub cwd: String,
    pub joined_at: u64,
    pub last_seen: u64,
    /// node the agent lives on
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub node: String,
}

/// One line of a per-node log. The line number IS the sequence number, so a
/// sync vector is just {node: line_count} and "what am I missing" is
/// "lines after N" — no counters to maintain.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Entry {
    /// a message delivered to one recipient's inbox
    #[serde(rename = "msg")]
    Msg { msg: Msg },
    /// recipient consumed message `id` — suppresses re-materialization
    /// everywhere and deletes stray copies
    #[serde(rename = "tomb")]
    Tomb { id: String, to: String, ts: u64 },
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

/// Snapshot of the last `murmur start` herd, so `murmur stop` can tear it
/// down without the human remembering pane ids.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HerdSnap {
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub agents: Vec<String>,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub worktrees: Vec<String>,
    #[serde(default)]
    pub slug: String,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root }
    }

    /// `MURMUR_DIR`, else the nearest `.murmur` walking up from cwd,
    /// else `.murmur` in cwd (created lazily on first write).
    pub fn locate() -> Result<Store> {
        if let Ok(dir) = std::env::var("MURMUR_DIR") {
            return Ok(Store {
                root: PathBuf::from(dir),
            });
        }
        let cwd = std::env::current_dir().context("cannot determine cwd")?;
        Ok(Store::locate_in(&cwd))
    }

    /// Nearest `.murmur` walking up from `start`, else `start/.murmur`.
    pub fn locate_in(start: &Path) -> Store {
        let mut dir = start;
        loop {
            let candidate = dir.join(".murmur");
            if candidate.is_dir() {
                return Store { root: candidate };
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => {
                    return Store {
                        root: start.join(".murmur"),
                    }
                }
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
    /// Returns the recipients and the message id.
    pub fn send(
        &self,
        from: &str,
        to: &str,
        body: &str,
        re: Option<&str>,
        wants_reply: bool,
    ) -> Result<(Vec<String>, String)> {
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
                bail!(
                    "broadcast has no recipients: no other agents have joined (see `murmur who`)"
                );
            }
            peers
        } else {
            valid_name(to)?;
            vec![to.to_string()]
        };

        let node = self.node_name()?;
        let ts = now_millis();
        let id = format!("{}-{}", next_id(ts), node);
        for recipient in &recipients {
            let msg = Msg {
                id: id.clone(),
                from: from.to_string(),
                to: recipient.clone(),
                ts,
                body: body.to_string(),
                node: node.clone(),
                re: re.map(|s| s.to_string()),
                wants_reply,
            };
            let inbox = self.root.join("inbox").join(recipient);
            fs::create_dir_all(&inbox)?;
            let tmp = self.root.join("tmp").join(format!("{}-{}", recipient, id));
            fs::write(&tmp, serde_json::to_vec(&msg)?)?;
            fs::rename(&tmp, inbox.join(format!("{}.json", id)))?;
            self.append_entry(&node, &Entry::Msg { msg })?;
        }
        Ok((recipients, id))
    }

    /// Block until a reply to `re_id` lands in `name`'s inbox, consuming only
    /// that message — everything else stays queued for a normal drain.
    pub fn await_reply(
        &self,
        name: &str,
        re_id: &str,
        timeout: std::time::Duration,
    ) -> Result<Option<Msg>> {
        valid_name(name)?;
        self.init()?;
        let inbox = self.root.join("inbox").join(name);
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if inbox.is_dir() {
                let mut paths: Vec<PathBuf> = fs::read_dir(&inbox)?
                    .filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .collect();
                paths.sort();
                for path in paths {
                    let Ok(bytes) = fs::read(&path) else { continue };
                    if let Ok(msg) = serde_json::from_slice::<Msg>(&bytes) {
                        if msg.re.as_deref() == Some(re_id) {
                            let _ = fs::remove_file(&path);
                            return Ok(Some(msg));
                        }
                    }
                }
            }
            if std::time::Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
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
        if !peek && !msgs.is_empty() {
            let node = self.node_name()?;
            for msg in &msgs {
                self.append_entry(
                    &node,
                    &Entry::Tomb {
                        id: msg.id.clone(),
                        to: name.to_string(),
                        ts: now_millis(),
                    },
                )?;
            }
        }
        Ok(msgs)
    }

    // ---- node identity & per-node logs ----

    pub fn node_name(&self) -> Result<String> {
        self.init()?;
        let path = self.root.join("node.json");
        if let Ok(bytes) = fs::read(&path) {
            if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(name) = v["name"].as_str() {
                    return Ok(name.to_string());
                }
            }
        }
        let host = std::env::var("HOSTNAME")
            .ok()
            .filter(|h| !h.is_empty())
            .or_else(|| {
                std::process::Command::new("hostname")
                    .output()
                    .ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            })
            .unwrap_or_default();
        let host: String = host
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(24)
            .collect();
        let host = if host.is_empty() {
            "node".to_string()
        } else {
            host
        };
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos();
        let name = format!(
            "{}-{}{:04x}",
            host,
            std::process::id() % 10000,
            nanos & 0xffff
        );
        let tmp = self.root.join("tmp").join(format!("node-{}", name));
        fs::write(
            &tmp,
            serde_json::to_vec(&serde_json::json!({ "name": name, "created": now_secs() }))?,
        )?;
        fs::rename(&tmp, &path)?;
        Ok(name)
    }

    pub fn log_dir(&self) -> PathBuf {
        self.root.join("log")
    }

    fn append_entry(&self, node: &str, entry: &Entry) -> Result<()> {
        fs::create_dir_all(self.log_dir())?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_dir().join(format!("{}.jsonl", node)))?;
        let mut line = serde_json::to_vec(entry)?;
        line.push(b'\n');
        file.write_all(&line)?;
        Ok(())
    }

    /// {node → entry count}: the sync vector.
    pub fn log_vector(&self) -> Result<std::collections::HashMap<String, u64>> {
        let mut vector = std::collections::HashMap::new();
        if !self.log_dir().is_dir() {
            return Ok(vector);
        }
        for entry in fs::read_dir(self.log_dir())?.filter_map(|e| e.ok()) {
            let path = entry.path();
            let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let count = fs::read_to_string(&path)
                .map(|c| c.lines().count())
                .unwrap_or(0);
            vector.insert(stem.to_string(), count as u64);
        }
        Ok(vector)
    }

    pub fn entries_after(&self, node: &str, after: u64) -> Result<Vec<Entry>> {
        let path = self.log_dir().join(format!("{}.jsonl", node));
        if !path.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_to_string(&path)?
            .lines()
            .skip(after as usize)
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect())
    }

    /// Append entries to a foreign node's log. `after` is the count the
    /// sender based the batch on; anything we already have is skipped, so
    /// re-applying a batch is harmless.
    pub fn append_foreign(&self, node: &str, after: u64, entries: &[Entry]) -> Result<()> {
        let have = *self.log_vector()?.get(node).unwrap_or(&0);
        anyhow::ensure!(
            have >= after,
            "gap in log for node {} (have {}, batch starts at {}) — sync with a peer that has the missing entries",
            node, have, after
        );
        for entry in entries.iter().skip((have - after) as usize) {
            self.append_entry(node, entry)?;
        }
        Ok(())
    }

    /// All message entries across all node logs, oldest first.
    pub fn log_msgs(&self) -> Result<Vec<Msg>> {
        let mut msgs = Vec::new();
        for node in self.log_vector()?.keys() {
            for entry in self.entries_after(node, 0)? {
                if let Entry::Msg { msg } = entry {
                    msgs.push(msg);
                }
            }
        }
        msgs.sort_by_key(|m| (m.ts, m.id.clone()));
        Ok(msgs)
    }

    /// Rebuild inbox files from the merged logs: materialize messages that
    /// have no tombstone anywhere, delete copies of consumed ones. Run after
    /// applying a sync batch. Idempotent.
    pub fn reconcile_inboxes(&self) -> Result<()> {
        let mut tombs = std::collections::HashSet::new();
        let mut msgs = Vec::new();
        for node in self.log_vector()?.keys() {
            for entry in self.entries_after(node, 0)? {
                match entry {
                    Entry::Msg { msg } => msgs.push(msg),
                    Entry::Tomb { id, .. } => {
                        tombs.insert(id);
                    }
                }
            }
        }
        for msg in msgs {
            if valid_name(&msg.to).is_err() {
                continue;
            }
            let inbox = self.root.join("inbox").join(&msg.to);
            let path = inbox.join(format!("{}.json", msg.id));
            if tombs.contains(&msg.id) {
                let _ = fs::remove_file(&path);
            } else if !path.exists() {
                fs::create_dir_all(&inbox)?;
                let tmp = self
                    .root
                    .join("tmp")
                    .join(format!("mat-{}-{}", msg.to, msg.id));
                fs::write(&tmp, serde_json::to_vec(&msg)?)?;
                fs::rename(&tmp, &path)?;
            }
        }
        Ok(())
    }

    // ---- state merge (used by sync) ----

    /// Last-writer-wins by `last_seen`.
    pub fn merge_agent(&self, incoming: Agent) -> Result<()> {
        self.init()?;
        let path = self
            .root
            .join("agents")
            .join(format!("{}.json", incoming.name));
        if valid_name(&incoming.name).is_err() {
            return Ok(());
        }
        if let Some(existing) = fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Agent>(&b).ok())
        {
            if existing.last_seen >= incoming.last_seen {
                return Ok(());
            }
        }
        let tmp = self
            .root
            .join("tmp")
            .join(format!("agent-{}-{}", incoming.name, now_millis()));
        fs::write(&tmp, serde_json::to_vec(&incoming)?)?;
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// First claim wins; expired claims never displace anything.
    pub fn merge_claim(&self, incoming: Claim) -> Result<()> {
        self.init()?;
        if incoming.expired() {
            return Ok(());
        }
        let file = self.claim_file(&incoming.path);
        if let Some(existing) = read_claim(&file) {
            if !existing.expired() && existing.ts <= incoming.ts {
                return Ok(());
            }
        }
        let tmp = self
            .root
            .join("tmp")
            .join(format!("claim-m-{}", now_millis()));
        fs::write(&tmp, serde_json::to_vec(&incoming)?)?;
        fs::rename(&tmp, &file)?;
        Ok(())
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
            node: self.node_name()?,
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
        let tmp = self
            .root
            .join("tmp")
            .join(format!("claim-{}-{}", holder, now_millis()));
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
        self.root
            .join("claims")
            .join(format!("{}.json", encode(canonical)))
    }

    // ---- housekeeping ----

    /// Remove local agents whose pid is gone and claims that have expired.
    /// Remote agents are never pruned here — their own node knows best.
    /// Returns (agents_removed, claims_removed).
    pub fn clean(&self) -> Result<(usize, usize)> {
        let me = self.node_name()?;
        let mut agents_removed = 0;
        for agent in self.agents()? {
            let local = agent.node.is_empty() || agent.node == me;
            if local && !pid_alive(agent.pid) {
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

    // ---- herd snapshot (userland start/stop) ----

    fn herd_path(&self) -> PathBuf {
        self.root.join("herd.json")
    }

    pub fn herd_save(&self, snap: &HerdSnap) -> Result<()> {
        self.init()?;
        let tmp = self.root.join("tmp").join(format!("herd-{}", now_millis()));
        fs::write(&tmp, serde_json::to_vec(snap)?)?;
        fs::rename(&tmp, self.herd_path())?;
        Ok(())
    }

    pub fn herd_load(&self) -> Result<Option<HerdSnap>> {
        let path = self.herd_path();
        if !path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&path)?;
        Ok(serde_json::from_slice(&bytes).ok())
    }

    pub fn herd_clear(&self) -> Result<()> {
        let _ = fs::remove_file(self.herd_path());
        Ok(())
    }
}

fn read_claim(path: &Path) -> Option<Claim> {
    fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

/// Agent names become directory names, so keep them boring.
pub fn valid_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if !ok {
        bail!(
            "invalid agent name '{}': use letters, digits, '-', '_', '.'",
            name
        );
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

pub fn next_id(ts: u64) -> String {
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
