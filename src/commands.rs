use anyhow::{Context, Result};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::thread;
use std::time::{Duration, Instant};

use crate::store::{self, ClaimResult, Msg, Store};

/// Identity comes from `--as`, then `MURMUR_AGENT`. Explicit beats ambient.
pub fn identity(explicit: Option<String>) -> Result<String> {
    let name = explicit
        .or_else(|| std::env::var("MURMUR_AGENT").ok())
        .context("who are you? pass --as <name> or export MURMUR_AGENT=<name>")?;
    store::valid_name(&name)?;
    Ok(name)
}

pub fn send(to: &str, body: Option<String>, from: Option<String>) -> Result<()> {
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
    let recipients = store.send(&from, to, &body)?;
    println!("delivered to {}", recipients.join(", "));
    Ok(())
}

pub fn inbox(name: Option<String>, wait: bool, timeout: u64, peek: bool, json: bool) -> Result<()> {
    let name = identity(name)?;
    let store = Store::locate()?;
    let deadline = (timeout > 0).then(|| Instant::now() + Duration::from_secs(timeout));
    loop {
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
            println!("[{}] {}: {}", store::clock(msg.ts), msg.from, msg.body);
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
    for a in agents {
        let status = if store::pid_alive(a.pid) { "up" } else { "gone" };
        println!(
            "{:<20} {:<5} pid {:<8} seen {:<6} {}",
            a.name,
            status,
            a.pid,
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
    let msgs = store.log_tail(n)?;
    if msgs.is_empty() && !json {
        eprintln!("no messages yet");
        return Ok(());
    }
    for msg in msgs {
        if json {
            println!("{}", serde_json::to_string(&msg)?);
        } else {
            println!("[{}] {} → {}: {}", store::clock(msg.ts), msg.from, msg.to, msg.body);
        }
    }
    Ok(())
}

/// Follow the message log — the human's window into agent chatter.
pub fn watch(all: bool, json: bool) -> Result<()> {
    let store = Store::locate()?;
    store.init()?;
    let path = store.log_path();
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    let mut reader = std::io::BufReader::new(file);
    if !all {
        reader.seek(SeekFrom::End(0))?;
    }
    eprintln!("watching {} (ctrl-c to stop)", path.display());
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            thread::sleep(Duration::from_millis(200));
            continue;
        }
        if let Ok(msg) = serde_json::from_str::<Msg>(line.trim()) {
            if json {
                print!("{}", line);
            } else {
                println!("[{}] {} → {}: {}", store::clock(msg.ts), msg.from, msg.to, msg.body);
            }
        }
    }
}

pub fn clean(all: bool) -> Result<()> {
    let store = Store::locate()?;
    if all {
        if store.root().is_dir() {
            std::fs::remove_dir_all(store.root())?;
        }
        println!("removed {}", store.root().display());
        return Ok(());
    }
    let (agents, claims) = store.clean()?;
    println!("removed {} dead agents, {} expired claims", agents, claims);
    Ok(())
}
