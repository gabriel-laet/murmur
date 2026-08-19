//! `murmur doctor` — does the roster match the machine?
//!
//! FLEET.md stays routing-opaque to murmur (the lead routes, never code);
//! the doctor reads only its kind column, best-effort, to tell a human
//! which kinds `murmur start` could actually launch right now: is herdr
//! up for the local kinds, are their binaries on PATH, do cloud kinds
//! have their key, curl, and a git origin to clone from.

use anyhow::Result;

use crate::{beads, cloud, fleet, herdr};

pub fn run() -> Result<()> {
    let herdr_up = herdr::available();
    if herdr_up {
        println!("ok    herdr server running");
    } else {
        println!("warn  herdr not running — local kinds have no panes to spawn into");
    }
    if beads::available() {
        println!("ok    beads (bd) on PATH");
    } else {
        println!("warn  beads (bd) not on PATH — durable tracker (optional)");
    }

    let Some(path) = fleet::find() else {
        println!("miss  FLEET.md not found — `murmur setup` seeds one");
        return Ok(());
    };
    println!("\nroster {}", path.display());
    let kinds = fleet::kinds();
    if kinds.is_empty() {
        println!("warn  no kinds found in the roster table");
        return Ok(());
    }
    for kind in kinds {
        if cloud::is_cloud(&kind) {
            check_cloud(&kind);
        } else {
            check_local(&kind, herdr_up);
        }
    }
    Ok(())
}

fn check_local(kind: &str, herdr_up: bool) {
    // Best-effort: herdr's kinds name their canonical executable, so a
    // missing binary is a strong hint, not proof — herdr may still know
    // how to launch it.
    let bin = on_path(kind);
    match (bin, herdr_up) {
        (true, true) => println!("ok    {kind}"),
        (true, false) => println!("warn  {kind}: binary present, but herdr is down — no panes to spawn into"),
        (false, true) => println!("warn  {kind}: '{kind}' not on PATH (herdr may still know how to launch it)"),
        (false, false) => println!("miss  {kind}: no '{kind}' on PATH and herdr is down"),
    }
}

fn check_cloud(kind: &str) {
    let backend = cloud::backend(kind);
    if !cloud::known_backend(backend) {
        println!("miss  {kind}: unknown cloud backend (supported: cursor)");
        return;
    }
    let mut missing = Vec::new();
    if std::env::var(cloud::CURSOR_KEY_ENV).is_err() {
        missing.push(format!("{} not set", cloud::CURSOR_KEY_ENV));
    }
    if !on_path("curl") && std::env::var("MURMUR_CURL").is_err() {
        missing.push("curl not on PATH".to_string());
    }
    if cloud::repo_ref(&std::env::current_dir().unwrap_or_default()).is_err() {
        missing.push("no git 'origin' remote to clone from".to_string());
    }
    if missing.is_empty() {
        println!("ok    {kind}");
    } else {
        println!("miss  {kind}: {}", missing.join(", "));
    }
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|d| d.join(bin).is_file())
}
