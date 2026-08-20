//! The lead's merge queue — `murmur restack` and `murmur pr status`.
//!
//! Murmur already tells the lead "your branch is the integration branch";
//! this module is the doing. Still userland policy, still shelling out to
//! tools everyone has (`git`, optionally `gh`): merge each worker branch
//! into the current checkout one at a time, gate each merge on an optional
//! command, stop with the facts on the first conflict. No history rewrites
//! — worker branches are other people's checkouts, so integration is
//! merges, never rebases.

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::store::Store;

/// Merge every herd worker branch into the current branch, oldest herd
/// order, gated by `cmd` when given. Run from the integration checkout
/// (the lead's worktree).
pub fn run(cmd: Option<String>) -> Result<()> {
    let store = Store::locate()?;
    let snap = store.herd_load()?.context(
        "no herd snapshot (herd.json) — restack serves a `murmur start --worktree` herd",
    )?;
    anyhow::ensure!(
        !snap.slug.is_empty(),
        "herd snapshot has no slug — was this herd started with --worktree?"
    );
    let cwd = std::env::current_dir().context("cannot determine cwd")?;
    let current = git_out(&cwd, &["rev-parse", "--abbrev-ref", "HEAD"])
        .context("restack must run inside the integration checkout")?;
    let gh = crate::store::on_path("gh");

    let mut merged = 0;
    for name in &snap.agents {
        let branch = format!("herd/{}/{}", snap.slug, name);
        if branch == current {
            continue;
        }
        if git(&cwd, &["rev-parse", "--verify", "--quiet", &branch]).is_err() {
            continue; // cloud worker or a pane that never committed
        }
        let changed = git_out(
            &cwd,
            &["diff", "--name-only", &format!("{current}...{branch}")],
        )
        .unwrap_or_default();
        if changed.is_empty() {
            println!("skip   {branch} (no changes)");
            continue;
        }
        let hub_hits: Vec<&str> = snap
            .hubs
            .iter()
            .map(|h| h.as_str())
            .filter(|h| {
                changed
                    .lines()
                    .any(|f| f == *h || f.starts_with(&format!("{h}/")))
            })
            .collect();
        if !hub_hits.is_empty() {
            println!("hub    {branch} touches {}", hub_hits.join(", "));
        }
        if gh {
            if let Some(note) = pr_note(&cwd, &branch) {
                println!("pr     {branch}: {note}");
                if note.contains("failing") {
                    println!("hold   {branch} — its PR checks are failing; merge it after they're green, or run restack again to retry");
                    continue;
                }
            }
        }
        let msg = format!("restack: merge {branch}");
        if git(&cwd, &["merge", "--no-ff", "-m", &msg, &branch]).is_err() {
            let conflicts =
                git_out(&cwd, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(&cwd, &["merge", "--abort"]);
            bail!(
                "conflict merging {branch}: {}\nresolve by hand: git merge {branch}, fix, \
                 commit, then run `murmur restack` again for the rest",
                conflicts.split_whitespace().collect::<Vec<_>>().join(", ")
            );
        }
        if let Some(cmd) = &cmd {
            let ok = Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&cwd)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if !ok {
                bail!(
                    "'{cmd}' failed after merging {branch} — the merge is committed; fix \
                     forward or `git reset --hard HEAD~1` to unwind it"
                );
            }
        }
        merged += 1;
        println!("merged {branch}");
    }
    println!(
        "restack done: {merged} branch(es) merged into {current}{}",
        if cmd.is_some() { " (gated)" } else { "" }
    );
    Ok(())
}

/// One snapshot of every herd branch's PR: number, state, checks. The
/// lead polls this between turns instead of babysitting the git host.
pub fn pr_status() -> Result<()> {
    anyhow::ensure!(
        crate::store::on_path("gh"),
        "pr status needs the `gh` CLI on PATH"
    );
    let store = Store::locate()?;
    let snap = store
        .herd_load()?
        .context("no herd snapshot (herd.json) — start a herd first")?;
    let cwd = std::env::current_dir().context("cannot determine cwd")?;
    let mut found = 0;
    for name in &snap.agents {
        let branch = format!("herd/{}/{}", snap.slug, name);
        match pr_note(&cwd, &branch) {
            Some(note) => {
                println!("{:<40} {note}", branch);
                found += 1;
            }
            None => println!("{:<40} no PR", branch),
        }
    }
    if found == 0 {
        eprintln!("no PRs found for herd branches — workers may not have pushed yet");
    }
    Ok(())
}

/// "#123 open, 2 checks failing" — or None when the branch has no PR.
fn pr_note(cwd: &Path, branch: &str) -> Option<String> {
    let out = Command::new("gh")
        .args([
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "all",
            "--json",
            "number,state,statusCheckRollup",
            "--limit",
            "1",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let pr = v.as_array()?.first()?;
    let number = pr.get("number").and_then(|x| x.as_u64())?;
    let state = pr
        .get("state")
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_lowercase();
    let checks = pr
        .get("statusCheckRollup")
        .and_then(|x| x.as_array())
        .map(|arr| {
            let mut failing = 0;
            let mut pending = 0;
            for c in arr {
                let conclusion = c
                    .get("conclusion")
                    .and_then(|x| x.as_str())
                    .unwrap_or_default();
                let status = c.get("status").and_then(|x| x.as_str()).unwrap_or_default();
                match conclusion {
                    "FAILURE" | "TIMED_OUT" | "CANCELLED" => failing += 1,
                    "" if status != "COMPLETED" => pending += 1,
                    _ => {}
                }
            }
            (failing, pending)
        })
        .unwrap_or((0, 0));
    let checks_note = match checks {
        (0, 0) => "checks green".to_string(),
        (f, 0) => format!("{f} check(s) failing"),
        (0, p) => format!("{p} check(s) pending"),
        (f, p) => format!("{f} failing, {p} pending"),
    };
    Some(format!("#{number} {state}, {checks_note}"))
}

fn git(cwd: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn git_out(cwd: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
