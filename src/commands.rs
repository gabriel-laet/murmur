use anyhow::{Context, Result};
use std::io::Read;
use std::thread;
use std::time::{Duration, Instant};

use crate::store::{self, ClaimResult, Msg, Store};

/// Identity: `--as`, then `MURMUR_AGENT`, then the Herdr pane name when
/// `HERDR_ENV=1`. Explicit beats ambient.
pub fn identity(explicit: Option<String>) -> Result<String> {
    let name =
        ambient(explicit).context("who are you? pass --as <name> or export MURMUR_AGENT=<name>")?;
    store::valid_name(&name)?;
    Ok(name)
}

/// Best-effort name with no error — used by the hook and MCP adapters.
pub fn ambient(explicit: Option<String>) -> Option<String> {
    if let Some(name) = explicit.filter(|s| !s.is_empty()) {
        return Some(name);
    }
    if let Ok(name) = std::env::var("MURMUR_AGENT") {
        if !name.is_empty() {
            return Some(name);
        }
    }
    crate::herdr::agent_name()
}

pub fn send(
    to: &str,
    body: Option<String>,
    from: Option<String>,
    reply_to: Option<String>,
    await_reply: bool,
    timeout: u64,
) -> Result<()> {
    let from = identity(from)?;
    let body = match body {
        Some(b) => b,
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf.trim_end().to_string()
        }
    };
    let store = Store::locate()?;
    let (recipients, id) = store.send(&from, to, &body, reply_to.as_deref(), await_reply)?;
    crate::sync::auto(&store, true); // push to peers now, not next interval
    if !await_reply {
        println!("delivered to {}", recipients.join(", "));
        return Ok(());
    }
    eprintln!(
        "delivered to {}, waiting for a reply...",
        recipients.join(", ")
    );
    // Wait in slices so a reply travelling through a peer sync still arrives.
    let deadline = Instant::now() + Duration::from_secs(timeout);
    loop {
        let slice = Duration::from_secs(2).min(deadline.saturating_duration_since(Instant::now()));
        if let Some(reply) = store.await_reply(&from, &id, slice)? {
            println!("{}", reply.body);
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!("no reply within {}s (message stays delivered)", timeout);
        }
        crate::sync::auto(&store, false);
    }
}

pub fn inbox(name: Option<String>, wait: bool, timeout: u64, peek: bool, json: bool) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    let deadline = (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout));
    loop {
        crate::sync::auto(&store, false); // debounced; pulls mail from peers
        let msgs = store.drain(&name, peek)?;
        if !msgs.is_empty() {
            print_msgs(&msgs, json);
            return Ok(());
        }
        let expired = deadline.is_some_and(|d| Instant::now() >= d);
        if !wait || expired {
            if !json {
                eprintln!("no messages");
            }
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn print_msgs(msgs: &[Msg], json: bool) {
    for msg in msgs {
        if json {
            println!("{}", serde_json::to_string(msg).unwrap_or_default());
        } else {
            let marker = if msg.re.is_some() { "↩ " } else { "" };
            println!(
                "[{}] {}: {}{}",
                store::clock(msg.ts),
                msg.from,
                marker,
                msg.body
            );
            if msg.wants_reply {
                println!(
                    "  ↳ sender is waiting: murmur send {} \"...\" --reply-to {}",
                    msg.from, msg.id
                );
            }
            if crate::secrets::contains_ref(&msg.body) {
                println!(
                    "  ⚿ contains secret reference(s) — resolve explicitly with `murmur secret exec NAME=<ref> -- <cmd>` (refs grant nothing; your own backend credentials are required)"
                );
            }
        }
    }
}

pub fn who(json: bool) -> Result<()> {
    let store = Store::locate()?;
    let agents = store.agents()?;
    if json {
        println!("{}", serde_json::to_string(&agents)?);
        return Ok(());
    }
    if agents.is_empty() {
        eprintln!("no agents (join with: murmur join <name>)");
        return Ok(());
    }
    let me = store.node_name()?;
    let herdr_live = crate::herdr::live_names();
    for a in agents {
        // Idle is not dead: agents touch presence per *command*, and the
        // recorded pid is usually a finished CLI invocation. Recent
        // last_seen means "between commands", not "gone".
        let status = if a.node.is_empty() || a.node == me {
            if store::pid_alive(a.pid) || herdr_live.contains(&a.name) {
                "up"
            } else if store::now_secs().saturating_sub(a.last_seen) < 900 {
                "idle"
            } else {
                "gone"
            }
        } else {
            "remote"
        };
        let node = if a.node.is_empty() || a.node == me {
            "here".to_string()
        } else {
            a.node.clone()
        };
        println!(
            "{:<20} {:<7} {:<20} seen {:<6} {}",
            a.name,
            status,
            node,
            store::ago(a.last_seen),
            a.cwd
        );
    }
    Ok(())
}

pub fn join(name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    store.touch(&name)?;
    let peers: Vec<String> = store
        .agents()?
        .into_iter()
        .map(|a| a.name)
        .filter(|n| n != &name)
        .collect();
    println!("joined as '{}' at {}", name, store.root().display());
    if peers.is_empty() {
        println!("no peers yet");
    } else {
        println!("peers: {}", peers.join(", "));
    }
    Ok(())
}

pub fn leave(name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    Store::locate()?.leave(&name)?;
    println!("left");
    Ok(())
}

pub fn claim(path: &str, name: Option<String>, ttl: u64) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    match store.claim(path, &name, ttl)? {
        ClaimResult::Granted => {
            println!("claimed {} for {}s", path, ttl);
            Ok(())
        }
        ClaimResult::Held(c) => {
            anyhow::bail!(
                "{} is claimed by {} ({} ago, expires in {}s)",
                c.path,
                c.holder,
                store::ago(c.ts),
                (c.ts + c.ttl_secs).saturating_sub(store::now_secs())
            );
        }
    }
}

pub fn release(path: &str, name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    if Store::locate()?.release(path, &name)? {
        println!("released {}", path);
        Ok(())
    } else {
        anyhow::bail!("{} is claimed by someone else", path);
    }
}

pub fn claims(json: bool) -> Result<()> {
    let store = Store::locate()?;
    let claims = store.claims()?;
    if json {
        println!("{}", serde_json::to_string(&claims)?);
        return Ok(());
    }
    if claims.is_empty() {
        eprintln!("no active claims");
        return Ok(());
    }
    for c in claims {
        println!(
            "{:<50} {} ({} ago, ttl {}s)",
            c.path,
            c.holder,
            store::ago(c.ts),
            c.ttl_secs
        );
    }
    Ok(())
}

pub fn log(n: usize, json: bool) -> Result<()> {
    let store = Store::locate()?;
    let msgs = store.log_msgs()?;
    let skip = msgs.len().saturating_sub(n);
    let msgs = &msgs[skip..];
    if msgs.is_empty() && !json {
        eprintln!("no messages yet");
        return Ok(());
    }
    for msg in msgs {
        if json {
            println!("{}", serde_json::to_string(&msg)?);
        } else {
            println!(
                "[{}] {} → {}: {}",
                store::clock(msg.ts),
                msg.from,
                msg.to,
                msg.body
            );
        }
    }
    Ok(())
}

/// Follow all per-node logs — the human's window into agent chatter,
/// cluster-wide once syncs are flowing.
pub fn watch(all: bool, json: bool) -> Result<()> {
    let store = Store::locate()?;
    store.init()?;
    let dir = store.log_dir();
    eprintln!("watching {} (ctrl-c to stop)", dir.display());
    let mut offsets: std::collections::HashMap<std::path::PathBuf, usize> =
        std::collections::HashMap::new();
    loop {
        if dir.is_dir() {
            let mut paths: Vec<_> = std::fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            paths.sort();
            for path in paths {
                let Ok(content) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let seen = *offsets.entry(path.clone()).or_insert_with(|| {
                    if all {
                        0
                    } else {
                        content.lines().count()
                    }
                });
                let mut count = 0;
                for (i, line) in content.lines().enumerate() {
                    count = i + 1;
                    if i < seen {
                        continue;
                    }
                    if let Ok(store::Entry::Msg { msg }) =
                        serde_json::from_str::<store::Entry>(line)
                    {
                        if json {
                            println!("{}", serde_json::to_string(&msg).unwrap_or_default());
                        } else {
                            println!(
                                "[{}] {} → {}: {}",
                                store::clock(msg.ts),
                                msg.from,
                                msg.to,
                                msg.body
                            );
                        }
                    }
                }
                offsets.insert(path, count);
            }
        }
        thread::sleep(Duration::from_millis(200));
    }
}

pub fn task_add(title: &str, body: Option<String>, name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    let task = store.task_add(&name, title, body.as_deref().unwrap_or(""))?;
    println!("added task {}", task.id);
    Ok(())
}

pub fn task_list(all: bool, json: bool) -> Result<()> {
    let store = Store::locate()?;
    let states: &[&str] = if all {
        &crate::tasks::STATES
    } else {
        &["todo", "doing"]
    };
    let tasks = store.task_list(states)?;
    if json {
        let items: Vec<serde_json::Value> = tasks
            .iter()
            .map(|(state, t)| {
                let mut v = serde_json::to_value(t).unwrap_or_default();
                v["state"] = serde_json::Value::String(state.clone());
                v
            })
            .collect();
        println!("{}", serde_json::to_string(&items)?);
        return Ok(());
    }
    if tasks.is_empty() {
        eprintln!("no tasks (add one with: murmur task add \"...\")");
        return Ok(());
    }
    for (state, t) in tasks {
        let who = match (&t.taken_by, &t.done_by) {
            (_, Some(d)) => format!(" (done by {})", d),
            (Some(w), _) => format!(" (taken by {})", w),
            _ => String::new(),
        };
        println!("{:<6} {}  {}{}", state, t.id, t.title, who);
    }
    Ok(())
}

pub fn task_take(id: Option<String>, name: Option<String>, json: bool) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    let veto = crate::beads::take_veto(&store);
    // A specific id is the primary path — the lead assigns, the worker
    // takes by name. Oldest-open is the fallback for boards that are
    // genuinely a free-for-all.
    if let Some(id) = id {
        let task = store.task_take_id(&name, &id, &veto)?;
        if json {
            println!("{}", serde_json::to_string(&task)?);
        } else {
            println!("took task {}", task.id);
            println!("  {}", task.title);
            println!(
                "  when finished: murmur task done {} --as {}",
                task.id, name
            );
        }
        return Ok(());
    }
    match store.task_take(&name, &veto)? {
        Some(task) => {
            if json {
                println!("{}", serde_json::to_string(&task)?);
            } else {
                println!("took task {}", task.id);
                println!("  {}", task.title);
                if !task.body.is_empty() {
                    println!("  {}", task.body);
                }
                if let Some(url) = &task.external_url {
                    println!("  {}", url);
                }
                println!(
                    "  when finished: murmur task done {} --as {}",
                    task.id, name
                );
            }
            Ok(())
        }
        None => {
            if store.open_task_count() > 0 {
                anyhow::bail!(
                    "no open leaf tasks — what's left is an epic waiting on its \
                     children or work beads no longer offers"
                );
            }
            anyhow::bail!("no open tasks")
        }
    }
}

pub fn task_done(id: &str, name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    let task = Store::locate()?.task_done(&name, id)?;
    println!("done: {}  {}", task.id, task.title);
    Ok(())
}

pub fn task_drop(id: &str, name: Option<String>) -> Result<()> {
    let name = identity(name)?;
    let task = Store::locate()?.task_drop(&name, id)?;
    println!("back on the board: {}  {}", task.id, task.title);
    Ok(())
}

pub fn task_sync(
    backend: &str,
    parent: Option<String>,
    label: Option<String>,
    all: bool,
) -> Result<()> {
    anyhow::ensure!(
        backend == "beads",
        "unknown task backend '{}' (supported: beads)",
        backend
    );
    crate::beads::sync(&crate::beads::SyncOpts { parent, label, all })
}

/// One-screen snapshot: who is up, what is on the board.
pub fn status() -> Result<()> {
    println!("who");
    who(false)?;
    println!("board");
    task_list(false, false)
}

/// Deliver a prompt into a live Herdr agent by name. A pane whose model
/// already finished (`done`/`exited`) is revived first — a poke must reach
/// a listening prompt or fail loudly, never report success on a corpse.
pub fn poke(target: &str, text: &str) -> Result<()> {
    if let Some(kind) = crate::herdr::revive_if_finished(target)
        .with_context(|| format!("{target} has finished and would not restart"))?
    {
        println!("revived {target} ({kind})");
    }
    crate::herdr::prompt(target, text)?;
    println!("prompted {target}");
    Ok(())
}

/// Prints the value to stdout. `exec` is the safer path — values in a
/// terminal end up in scrollback, logs, and agent context.
pub fn secret_resolve(reference: &str) -> Result<()> {
    let value = crate::secrets::resolve(reference)?;
    eprintln!("note: value printed to stdout — prefer `murmur secret exec NAME=<ref> -- <cmd>`");
    println!("{}", value);
    Ok(())
}

/// Resolve refs into the child's environment and run it. The values never
/// touch stdout, the log, or an agent's context.
pub fn secret_exec(pairs: Vec<String>, command: Vec<String>) -> Result<()> {
    anyhow::ensure!(
        !command.is_empty(),
        "no command given (use: murmur secret exec NAME=<ref> -- <cmd>)"
    );
    let mut cmd = std::process::Command::new(&command[0]);
    cmd.args(&command[1..]);
    for pair in &pairs {
        let (name, reference) = pair
            .split_once('=')
            .with_context(|| format!("expected NAME=secret://..., got '{}'", pair))?;
        cmd.env(name, crate::secrets::resolve(reference)?);
    }
    let status = cmd
        .status()
        .with_context(|| format!("failed to run '{}'", command[0]))?;
    std::process::exit(status.code().unwrap_or(1));
}

pub fn clean(all: bool, stale: bool, age_hours: u64) -> Result<()> {
    let store = Store::locate()?;
    if all {
        if store.root().is_dir() {
            std::fs::remove_dir_all(store.root())?;
        }
        println!("removed {}", store.root().display());
        return Ok(());
    }
    if stale {
        let (agents, tasks, claims) = store.clean_stale(age_hours.max(1) * 3600)?;
        println!(
            "removed {} stale agents, {} old done tasks, {} expired claims (bus untouched)",
            agents, tasks, claims
        );
        return Ok(());
    }
    let (agents, claims) = store.clean()?;
    println!("removed {} dead agents, {} expired claims", agents, claims);
    Ok(())
}
