//! The foreman's notebook — the only state murmur keeps.
//!
//! ```text
//! .murmur/
//!   .gitignore            self-ignoring, like target/
//!   herd.json             the running wave: workspace, agents, worktrees, hubs
//!   briefs/<name>.txt     each agent's brief, kept for re-delivery
//!   spool/<name>/*.json   undelivered tells, drained into prompts on idle-wake
//!   tmp/                  staging for atomic renames
//! ```
//!
//! Everything live belongs to herdr (panes, presence, delivery); everything
//! durable about the *work* belongs to beads (plan, assignment, notes).
//! This directory holds only what neither owns: the wave snapshot, the
//! briefs, and messages waiting for an agent that wasn't listening.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// A tell that could not be delivered into a live prompt; the idle-wake
/// plugin drains these the moment the agent settles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Spooled {
    pub from: String,
    pub to: String,
    /// unix millis
    pub ts: u64,
    pub body: String,
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
    /// Paths the whole herd converges on (shared registries, barrel files):
    /// named in every brief, checked by `murmur restack`.
    #[serde(default)]
    pub hubs: Vec<String>,
}

pub struct Store {
    root: PathBuf,
}

impl Store {
    pub fn at(root: PathBuf) -> Store {
        Store { root }
    }

    /// `MURMUR_DIR`, else the nearest `.murmur` walking up from cwd,
    /// else `.murmur` at the repo root (created lazily on first write).
    pub fn locate() -> Result<Store> {
        if let Ok(dir) = std::env::var("MURMUR_DIR") {
            return Ok(Store {
                root: PathBuf::from(dir),
            });
        }
        let cwd = std::env::current_dir().context("cannot determine cwd")?;
        Ok(Store::locate_in(&cwd))
    }

    /// Nearest `.murmur` walking up from `start`; else anchored to the
    /// repo, not the checkout: a git worktree resolves through its `.git`
    /// file to the main checkout, so every worktree of one repo shares one
    /// notebook with no MURMUR_DIR plumbing. Outside git: `start/.murmur`.
    pub fn locate_in(start: &Path) -> Store {
        let mut dir = start;
        loop {
            let candidate = dir.join(".murmur");
            if candidate.is_dir() {
                return Store { root: candidate };
            }
            match dir.parent() {
                Some(parent) => dir = parent,
                None => break,
            }
        }
        let root = main_repo_root(start).unwrap_or_else(|| start.to_path_buf());
        Store {
            root: root.join(".murmur"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn init(&self) -> Result<()> {
        for sub in ["briefs", "spool", "tmp"] {
            fs::create_dir_all(self.root.join(sub))?;
        }
        let gitignore = self.root.join(".gitignore");
        if !gitignore.exists() {
            fs::write(&gitignore, "*\n")?;
        }
        Ok(())
    }

    // ---- spool (deferred delivery) ----

    /// Queue a tell for an agent that isn't listening right now. The
    /// idle-wake plugin delivers it as a prompt when the pane settles.
    pub fn spool_push(&self, from: &str, to: &str, body: &str) -> Result<()> {
        valid_name(to)?;
        self.init()?;
        let dir = self.root.join("spool").join(to);
        fs::create_dir_all(&dir)?;
        let ts = now_millis();
        let msg = Spooled {
            from: from.to_string(),
            to: to.to_string(),
            ts,
            body: body.to_string(),
        };
        let id = next_id(ts);
        let tmp = self.root.join("tmp").join(format!("spool-{to}-{id}"));
        fs::write(&tmp, serde_json::to_vec(&msg)?)?;
        fs::rename(&tmp, dir.join(format!("{id}.json")))?;
        Ok(())
    }

    /// Take everything waiting for `name`, oldest first, removing it.
    pub fn spool_drain(&self, name: &str) -> Result<Vec<Spooled>> {
        valid_name(name)?;
        let dir = self.root.join("spool").join(name);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut paths: Vec<PathBuf> = fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        paths.sort();
        let mut out = Vec::new();
        for path in paths {
            let Ok(bytes) = fs::read(&path) else { continue };
            if let Ok(msg) = serde_json::from_slice::<Spooled>(&bytes) {
                out.push(msg);
            }
            let _ = fs::remove_file(&path);
        }
        Ok(out)
    }

    /// {agent → waiting tells}, for `who`/`status`.
    pub fn spool_counts(&self) -> Vec<(String, usize)> {
        let dir = self.root.join("spool");
        let Ok(entries) = fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut out: Vec<(String, usize)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| {
                let n = fs::read_dir(e.path())
                    .map(|d| d.filter_map(|f| f.ok()).count())
                    .unwrap_or(0);
                (e.file_name().to_string_lossy().to_string(), n)
            })
            .filter(|(_, n)| *n > 0)
            .collect();
        out.sort();
        out
    }

    // ---- briefs ----

    /// Briefs are durable: a dialog (login picker, trust prompt) can eat
    /// the first delivery even when the pane looks interactive-ready, so
    /// the text is kept for re-delivery with `murmur tell <name> --brief`.
    pub fn brief_save(&self, name: &str, text: &str) -> Result<()> {
        valid_name(name)?;
        self.init()?;
        fs::write(self.root.join("briefs").join(format!("{name}.txt")), text)?;
        Ok(())
    }

    pub fn brief_load(&self, name: &str) -> Result<String> {
        valid_name(name)?;
        let path = self.root.join("briefs").join(format!("{name}.txt"));
        fs::read_to_string(&path)
            .with_context(|| format!("no stored brief for {name} (started by murmur start?)"))
    }

    // ---- housekeeping ----

    /// Drop spool files and briefs older than `age_secs`. Returns
    /// (spooled_removed, briefs_removed).
    pub fn clean(&self, age_secs: u64) -> Result<(usize, usize)> {
        let mut spooled = 0;
        let spool = self.root.join("spool");
        if spool.is_dir() {
            for agent_dir in fs::read_dir(&spool)?.filter_map(|e| e.ok()) {
                if !agent_dir.path().is_dir() {
                    continue;
                }
                for f in fs::read_dir(agent_dir.path())?.filter_map(|e| e.ok()) {
                    if file_older_than(&f.path(), age_secs) {
                        let _ = fs::remove_file(f.path());
                        spooled += 1;
                    }
                }
            }
        }
        let mut briefs = 0;
        let dir = self.root.join("briefs");
        if dir.is_dir() {
            for f in fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
                if file_older_than(&f.path(), age_secs) {
                    let _ = fs::remove_file(f.path());
                    briefs += 1;
                }
            }
        }
        Ok((spooled, briefs))
    }

    // ---- herd snapshot (start/stop) ----

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

fn file_older_than(path: &Path, age_secs: u64) -> bool {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() >= age_secs)
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

/// The main checkout's root for wherever `start` sits: walk up to the
/// nearest `.git`; a worktree's `.git` *file* points back at
/// `<main>/.git/worktrees/<x>`, whose parent-of-parent is the main root.
/// No subprocess. None outside a git checkout.
fn main_repo_root(start: &Path) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return Some(dir.to_path_buf());
        }
        if dot.is_file() {
            let text = fs::read_to_string(&dot).ok()?;
            let gitdir = text.strip_prefix("gitdir:")?.trim();
            let main = match gitdir.rfind("/worktrees/") {
                Some(i) => &gitdir[..i],
                None => gitdir,
            };
            return Path::new(main).parent().map(|p| p.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

/// Is `bin` an executable file on PATH? Adapters probe before shelling out.
pub fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|p| p.join(bin).is_file())
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
