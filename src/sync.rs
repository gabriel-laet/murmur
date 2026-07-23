//! Anti-entropy sync between `.murmur/` directories — replication, not
//! networking. Each node appends only to its own log (`log/<node>.jsonl`);
//! the line count per node IS the sync vector. A sync exchanges vectors,
//! streams the entries each side lacks, merges state (agents LWW, claims
//! first-wins, tasks by precedence), then rebuilds inboxes from the merged
//! logs with tombstone suppression. Idempotent by construction; relaying
//! through an intermediate node works because logs are per-origin.
//!
//! Transports, in the spirit of using what already exists:
//! - a path (shared volume, sshfs, second checkout): merged in-process
//! - `user@host[:path]`: pipes to `ssh host murmur sync --stdio` — auth,
//!   encryption, and revocation are your existing ssh key hygiene. Syncing
//!   with a host means trusting it, exactly like `git pull`.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use crate::store::{Agent, Claim, Entry, Store};
use crate::tasks::{Task, STATES};

#[derive(Serialize, Deserialize)]
struct Hello {
    node: String,
    vector: HashMap<String, u64>,
}

#[derive(Serialize, Deserialize)]
struct LogSlice {
    after: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Batch {
    logs: HashMap<String, LogSlice>,
    agents: Vec<Agent>,
    claims: Vec<Claim>,
    tasks: Vec<(String, Task)>,
}

enum Target {
    Local(PathBuf),
    Ssh { host: String, path: Option<String> },
}

fn parse_target(target: &str) -> Target {
    if let Some((host, path)) = target.split_once(':') {
        return Target::Ssh {
            host: host.to_string(),
            path: if path.is_empty() { None } else { Some(path.to_string()) },
        };
    }
    if target.contains('/') || Path::new(target).exists() {
        Target::Local(PathBuf::from(target))
    } else if target.contains('@') {
        Target::Ssh { host: target.to_string(), path: None }
    } else {
        // a bare word that isn't a path here: treat as an ssh host
        Target::Ssh { host: target.to_string(), path: None }
    }
}

/// A path may point at a `.murmur` dir or at its parent.
fn resolve_store(path: &Path) -> Store {
    if path.file_name().is_some_and(|n| n == ".murmur") {
        Store::at(path.to_path_buf())
    } else {
        Store::at(path.join(".murmur"))
    }
}

pub fn run(target: &str) -> Result<()> {
    let local = Store::locate()?;
    local.init()?;
    match parse_target(target) {
        Target::Local(path) => {
            let remote = resolve_store(&path);
            remote.init()?;
            let (sent, received) = sync_pair(&local, &remote)?;
            println!(
                "synced with {}: sent {} entries, received {}",
                remote.root().display(), sent, received
            );
            Ok(())
        }
        Target::Ssh { host, path } => {
            let ssh = std::env::var("MURMUR_SYNC_SSH").unwrap_or_else(|_| "ssh".into());
            let mut cmd = std::process::Command::new(ssh);
            cmd.arg(&host).args(["murmur", "sync", "--stdio"]);
            if let Some(p) = &path {
                cmd.arg(p);
            }
            let mut child = cmd
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .spawn()
                .with_context(|| format!("failed to start ssh to {}", host))?;
            let mut w = child.stdin.take().unwrap();
            let mut r = BufReader::new(child.stdout.take().unwrap());

            send_json(&mut w, &Hello { node: local.node_name()?, vector: local.log_vector()? })?;
            let their_hello: Hello = recv_json(&mut r).context(
                "no hello from the remote end — is murmur installed there and on PATH?",
            )?;
            let batch = make_batch(&local, &their_hello.vector)?;
            let sent: usize = batch.logs.values().map(|s| s.entries.len()).sum();
            send_json(&mut w, &batch)?;
            let their_batch: Batch = recv_json(&mut r).context("remote closed before sending its batch")?;
            let received: usize = their_batch.logs.values().map(|s| s.entries.len()).sum();
            apply_batch(&local, their_batch)?;
            drop(w);
            let status = child.wait()?;
            if !status.success() {
                bail!("remote sync process exited with {}", status);
            }
            println!("synced with {}: sent {} entries, received {}", host, sent, received);
            Ok(())
        }
    }
}

/// The remote end: serve exactly one sync session over stdin/stdout.
pub fn run_stdio(path: Option<String>) -> Result<()> {
    let store = match path {
        Some(p) => resolve_store(Path::new(&p)),
        None => Store::locate()?,
    };
    store.init()?;
    let stdin = std::io::stdin();
    let mut r = BufReader::new(stdin.lock());
    let mut w = std::io::stdout();

    let their_hello: Hello = recv_json(&mut r).context("no hello from client")?;
    send_json(&mut w, &Hello { node: store.node_name()?, vector: store.log_vector()? })?;
    let their_batch: Batch = recv_json(&mut r).context("no batch from client")?;
    apply_batch(&store, their_batch)?;
    // computed after apply so freshly merged state flows back in one round trip
    let batch = make_batch(&store, &their_hello.vector)?;
    send_json(&mut w, &batch)?;
    Ok(())
}

fn sync_pair(a: &Store, b: &Store) -> Result<(usize, usize)> {
    let to_b = make_batch(a, &b.log_vector()?)?;
    let sent = to_b.logs.values().map(|s| s.entries.len()).sum();
    apply_batch(b, to_b)?;
    let to_a = make_batch(b, &a.log_vector()?)?;
    let received = to_a.logs.values().map(|s| s.entries.len()).sum();
    apply_batch(a, to_a)?;
    Ok((sent, received))
}

fn make_batch(store: &Store, theirs: &HashMap<String, u64>) -> Result<Batch> {
    let mut logs = HashMap::new();
    for (node, count) in store.log_vector()? {
        let after = *theirs.get(&node).unwrap_or(&0);
        if count > after {
            logs.insert(node.clone(), LogSlice { after, entries: store.entries_after(&node, after)? });
        }
    }
    Ok(Batch {
        logs,
        agents: store.agents()?,
        claims: store.claims()?,
        tasks: store.task_list(&STATES)?,
    })
}

fn apply_batch(store: &Store, batch: Batch) -> Result<()> {
    let me = store.node_name()?;
    for (node, slice) in &batch.logs {
        if node == &me {
            continue; // nobody knows my log better than I do
        }
        store.append_foreign(node, slice.after, &slice.entries)?;
    }
    for agent in batch.agents {
        store.merge_agent(agent)?;
    }
    for claim in batch.claims {
        store.merge_claim(claim)?;
    }
    for (state, task) in batch.tasks {
        store.merge_task(&state, task)?;
    }
    store.reconcile_inboxes()?;
    Ok(())
}

fn send_json<W: Write, T: Serialize>(w: &mut W, value: &T) -> Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    w.write_all(&line)?;
    w.flush()?;
    Ok(())
}

fn recv_json<R: BufRead, T: for<'de> Deserialize<'de>>(r: &mut R) -> Result<T> {
    let mut line = String::new();
    let n = r.read_line(&mut line)?;
    if n == 0 {
        bail!("peer closed the connection");
    }
    serde_json::from_str(line.trim()).context("malformed sync frame")
}

#[cfg(test)]
mod tests {
    use super::{parse_target, Target};

    #[test]
    fn targets_parse_sensibly() {
        assert!(matches!(parse_target("../other/.murmur"), Target::Local(_)));
        assert!(matches!(parse_target("/abs/path"), Target::Local(_)));
        match parse_target("dev@build:work/repo") {
            Target::Ssh { host, path } => {
                assert_eq!(host, "dev@build");
                assert_eq!(path.as_deref(), Some("work/repo"));
            }
            _ => panic!("expected ssh"),
        }
        match parse_target("buildbox:") {
            Target::Ssh { host, path } => {
                assert_eq!(host, "buildbox");
                assert!(path.is_none());
            }
            _ => panic!("expected ssh"),
        }
        assert!(matches!(parse_target("dev@buildbox"), Target::Ssh { .. }));
    }
}
