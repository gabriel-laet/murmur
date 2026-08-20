//! Beads as the board's upstream. Beads (`bd`) is the agent-native tracker:
//! git-distributed, local-first, with a dependency graph and ready-work
//! detection. The delegation split:
//!
//! - Beads owns planning, dependencies, priorities, and long-term memory.
//! - The board owns the agent mechanics: atomic take, holder-checked done.
//! - `murmur task sync beads` reconciles the two, both directions, and is
//!   idempotent — run it from a hook, a cron, or by hand.
//!
//! Pulled beads keep their own ids on the board (`bd-a1b2`), so
//! `murmur task done bd-a1b2` reads naturally and re-syncs never duplicate.
//! Only *ready* beads are pulled — `bd ready` is beads' dependency-aware
//! answer to "what can start now", and the board should never offer an
//! agent blocked work. Local transitions push back: take → in_progress
//! (with the agent as assignee), done → closed, drop → open. `synced_state`
//! in the task file tracks what beads has already been told, which is what
//! makes push idempotent.
//!
//! We shell out to the `bd` CLI the same way we shell out to `herdr` — no
//! SDK, no socket, and murmur never touches `.beads/` internals.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::store::Store;
use crate::tasks::{Task, STATES};

pub struct Issue {
    pub id: String,
    pub title: String,
    pub body: String,
    pub parent: Option<String>,
    pub status: String,
    pub assignee: Option<String>,
}

impl Issue {
    pub fn closed(&self) -> bool {
        matches!(self.status.as_str(), "closed" | "done")
    }
}

pub fn bin() -> PathBuf {
    if let Ok(p) = std::env::var("MURMUR_BEADS") {
        return PathBuf::from(p);
    }
    PathBuf::from("bd")
}

/// True when beads can serve this directory: a test stub is configured, or
/// `bd` is installed and some ancestor of `cwd` has a `.beads/`.
pub fn available_in(cwd: &Path) -> bool {
    if std::env::var_os("MURMUR_BEADS").is_some() {
        return true;
    }
    crate::store::on_path("bd") && beads_root(cwd).is_some()
}

pub fn available() -> bool {
    std::env::current_dir()
        .map(|c| available_in(&c))
        .unwrap_or(false)
}

fn beads_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = cwd.to_path_buf();
    loop {
        if dir.join(".beads").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Fetch one bead by id (`bd-a1b2`).
pub fn fetch(id: &str) -> Result<Issue> {
    let v = call(&["show", id, "--json"])?;
    first_issue(&v).with_context(|| format!("no bead '{id}'"))
}

/// Create a bead from a goal string, so the work has a durable home.
pub fn create(title: &str, body: &str) -> Result<Issue> {
    let mut args = vec!["create", title, "--json"];
    if !body.is_empty() {
        args.push("-d");
        args.push(body);
    }
    let v = call(&args)?;
    first_issue(&v).context("bd create returned no issue id")
}

/// Beads whose blockers are all closed — the only ones worth offering.
pub fn ready() -> Result<Vec<Issue>> {
    ready_in(None)
}

pub fn ready_in(cwd: Option<&Path>) -> Result<Vec<Issue>> {
    let v = call_in(cwd, &["ready", "--json"])?;
    Ok(issue_list(&v))
}

pub fn ensure_task(store: &Store, issue: &Issue) -> Result<(Task, bool)> {
    let task = task_from_issue(issue);
    let new = store.task_import(task.clone())?;
    if new {
        return Ok((task, true));
    }
    let existing = store
        .task_list(&STATES)?
        .into_iter()
        .find(|(_, t)| t.id == task.id)
        .map(|(_, t)| t)
        .unwrap_or(task);
    Ok((existing, false))
}

pub fn sync() -> Result<()> {
    let store = Store::locate()?;

    let issues = ready()?;
    let mut pulled = 0;
    for issue in leaves(&issues) {
        if store.task_import(task_from_issue(issue))? {
            pulled += 1;
        }
    }

    let reconciled = reconcile_authority(&store, &issues)?;

    let mut pushed = 0;
    for (state, mut task) in store.task_list(&STATES)? {
        let Some(bead_id) = task.external_id.clone() else {
            continue;
        };
        let Some(target) = transition(&state, task.synced_state.as_deref()) else {
            continue;
        };
        match target {
            "in_progress" => {
                let holder = task.taken_by.clone().unwrap_or_default();
                let mut args = vec!["update", &bead_id, "--status", "in_progress", "--json"];
                if !holder.is_empty() {
                    args.push("--assignee");
                    args.push(&holder);
                }
                call_mut(&args)?;
            }
            "closed" => {
                close(&bead_id, &attribution(&state, &task))?;
            }
            _ => {
                call_mut(&["update", &bead_id, "--status", "open", "--json"])?;
            }
        }
        task.synced_state = Some(state.clone());
        store.task_rewrite(&state, &task)?;
        pushed += 1;
    }

    println!(
        "beads: pulled {} ready bead(s), reconciled {} from beads, pushed {} transition(s)",
        pulled, reconciled, pushed
    );
    Ok(())
}

/// Beads wins identity. For every open board task that mirrors a bead:
/// a closed bead ends the task no matter who holds the murmur lock, and a
/// beads assignee overrides a conflicting local take (the loser is
/// force-dropped and mailed). One `bd show` per task that needs checking —
/// boards are small; tasks whose bead is still in the ready set are open
/// by definition and skipped.
fn reconcile_authority(store: &Store, ready: &[Issue]) -> Result<usize> {
    let ready_ids: std::collections::HashSet<&str> = ready.iter().map(|i| i.id.as_str()).collect();
    let mut reconciled = 0;
    for (state, task) in store.task_list(&["todo", "doing"])? {
        let Some(bead_id) = task.external_id.clone() else {
            continue;
        };
        if state == "todo" && ready_ids.contains(bead_id.as_str()) {
            continue;
        }
        let Ok(issue) = fetch(&bead_id) else {
            continue; // can't reach beads for this one — leave the board alone
        };
        if issue.closed() {
            if let Some(was) = store.task_force_done(&task.id, "beads")? {
                reconciled += 1;
                if let Some(holder) = was.taken_by.filter(|_| state == "doing") {
                    let _ = store.send(
                        "beads",
                        &holder,
                        &format!(
                            "{bead_id} was closed in beads — your take of task {} is void \
                             and the task is done. Take something else.",
                            task.id
                        ),
                        None,
                        false,
                    );
                }
            }
            continue;
        }
        if state == "doing" {
            let assignee = issue.assignee.as_deref().unwrap_or_default();
            let holder = task.taken_by.as_deref().unwrap_or_default();
            if !assignee.is_empty() && !holder.is_empty() && assignee != holder {
                if let Some((_, ex)) = store.task_force_drop(&task.id)? {
                    reconciled += 1;
                    let _ = store.send(
                        "beads",
                        &ex,
                        &format!(
                            "{bead_id} is assigned to {assignee} in beads — your take of \
                             task {} was dropped. Take something else.",
                            task.id
                        ),
                        None,
                        false,
                    );
                }
            }
        }
    }
    Ok(reconciled)
}

/// Board ids `task take` must skip because beads says so: an external task
/// is takeable only while its bead is still ready and is not the parent of
/// another ready bead. Best-effort — no beads (or a failing `bd`) vetoes
/// nothing, and the dotted-id heuristic in the kernel still applies.
pub fn take_veto(store: &Store) -> std::collections::HashSet<String> {
    let mut veto = std::collections::HashSet::new();
    if !available() {
        return veto;
    }
    let Ok(todo) = store.task_list(&["todo"]) else {
        return veto;
    };
    let external: Vec<String> = todo
        .into_iter()
        .filter_map(|(_, t)| t.external_id)
        .collect();
    if external.is_empty() {
        return veto;
    }
    let Ok(issues) = ready() else {
        return veto;
    };
    let ready_ids: std::collections::HashSet<&str> = issues.iter().map(|i| i.id.as_str()).collect();
    let parents: std::collections::HashSet<&str> =
        issues.iter().filter_map(|i| i.parent.as_deref()).collect();
    for id in external {
        if !ready_ids.contains(id.as_str()) || parents.contains(id.as_str()) {
            veto.insert(id);
        }
    }
    veto
}

/// Ready beads that are not the parent of another ready bead. Workers
/// should take leaves; the epic stays in beads until the children close.
pub fn leaves(issues: &[Issue]) -> Vec<&Issue> {
    let parents: std::collections::HashSet<&str> =
        issues.iter().filter_map(|i| i.parent.as_deref()).collect();
    issues
        .iter()
        .filter(|i| !parents.contains(i.id.as_str()))
        .collect()
}

pub fn task_from_issue(issue: &Issue) -> Task {
    Task {
        id: issue.id.clone(),
        title: issue.title.clone(),
        body: issue.body.clone(),
        by: "beads".into(),
        ts: crate::store::now_millis(),
        taken_by: None,
        done_by: None,
        external_id: Some(issue.id.clone()),
        external_url: None,
        synced_state: None,
    }
}

/// Which beads status (if any) a local state must be pushed as, given what
/// beads was last told. `None` synced means "as pulled" (todo/open).
fn transition(local: &str, synced: Option<&str>) -> Option<&'static str> {
    match (local, synced.unwrap_or("todo")) {
        ("doing", "todo") => Some("in_progress"),
        ("done", synced) if synced != "done" => Some("closed"),
        ("todo", "doing") => Some("open"),
        _ => None,
    }
}

fn attribution(state: &str, task: &Task) -> String {
    match state {
        "done" => format!(
            "Completed by agent {} via murmur.",
            task.done_by.as_deref().unwrap_or("unknown")
        ),
        _ => "Updated via murmur.".into(),
    }
}

/// `bd` sometimes returns one object, sometimes an array, sometimes a
/// wrapper — take whatever holds issues and read them defensively.
fn issue_list(v: &Value) -> Vec<Issue> {
    let arr = v
        .as_array()
        .cloned()
        .or_else(|| v.get("issues").and_then(|x| x.as_array()).cloned())
        .unwrap_or_else(|| vec![v.clone()]);
    arr.iter().filter_map(parse_issue).collect()
}

fn first_issue(v: &Value) -> Option<Issue> {
    issue_list(v).into_iter().next()
}

fn parse_issue(v: &Value) -> Option<Issue> {
    let id = v
        .get("id")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())?;
    let title = v.get("title").and_then(|x| x.as_str()).unwrap_or_default();
    let body = v
        .get("description")
        .or_else(|| v.get("body"))
        .or_else(|| v.get("design"))
        .and_then(|x| x.as_str())
        .unwrap_or_default();
    let status = v.get("status").and_then(|x| x.as_str()).unwrap_or_default();
    let assignee = v
        .get("assignee")
        .and_then(|x| {
            x.as_str()
                .or_else(|| x.get("name").and_then(|n| n.as_str()))
        })
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    Some(Issue {
        id: id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        parent: parse_parent(v),
        status: status.to_string(),
        assignee,
    })
}

fn parse_parent(v: &Value) -> Option<String> {
    if let Some(s) = v
        .get("parent")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_string());
    }
    if let Some(s) = v
        .get("parent")
        .and_then(|x| x.get("id"))
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
    {
        return Some(s.to_string());
    }
    v.get("dependencies")
        .and_then(|d| d.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|dep| {
                (dep.get("type").and_then(|t| t.as_str()) == Some("parent-child"))
                    .then(|| {
                        dep.get("depends_on_id")
                            .and_then(|x| x.as_str())
                            .filter(|s| !s.is_empty())
                            .map(|s| s.to_string())
                    })
                    .flatten()
            })
        })
}

pub fn call(args: &[&str]) -> Result<Value> {
    call_in(None, args)
}

/// Mutating calls (`update`, `close`): ask for JSON, but a zero exit with
/// human-text output (a checkmark, a summary line) is still success — some
/// `bd` builds print prose even under `--json`, and aborting the rest of a
/// sync over an unparseable "✓" is worse than moving on.
fn call_mut(args: &[&str]) -> Result<()> {
    match call(args) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("returned non-JSON") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Close a bead, treating "already closed" as success: if beads got there
/// first (another agent, another node), the goal state holds — stop.
fn close(bead_id: &str, reason: &str) -> Result<()> {
    match call_mut(&["close", bead_id, "--reason", reason, "--json"]) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().to_lowercase().contains("closed") => Ok(()),
        Err(e) => Err(e),
    }
}

fn call_in(cwd: Option<&Path>, args: &[&str]) -> Result<Value> {
    let mut cmd = Command::new(bin());
    cmd.args(args);
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let out = cmd
        .output()
        .with_context(|| format!("failed to run '{}' — is beads installed?", bin().display()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let out_s = String::from_utf8_lossy(&out.stdout);
        let detail = if err.trim().is_empty() {
            out_s.trim()
        } else {
            err.trim()
        };
        bail!("bd {} failed: {}", args.join(" "), detail);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    match serde_json::from_str(trimmed) {
        Ok(v) => Ok(v),
        Err(e) if args.contains(&"--json") => {
            Err(e).with_context(|| format!("bd {} returned non-JSON", args.join(" ")))
        }
        // Human-text success (update/close without a parseable body) is fine.
        Err(_) => Ok(json!({})),
    }
}

#[cfg(test)]
mod tests {
    use super::{issue_list, leaves, transition};
    use serde_json::json;

    #[test]
    fn take_pushes_in_progress_once() {
        assert_eq!(transition("doing", None), Some("in_progress"));
        assert_eq!(transition("doing", Some("doing")), None);
    }

    #[test]
    fn done_pushes_closed_from_any_lagging_state() {
        assert_eq!(transition("done", None), Some("closed"));
        assert_eq!(transition("done", Some("doing")), Some("closed"));
        assert_eq!(transition("done", Some("done")), None);
    }

    #[test]
    fn drop_pushes_back_to_open_only_if_beads_was_told_doing() {
        assert_eq!(transition("todo", Some("doing")), Some("open"));
        assert_eq!(transition("todo", None), None);
    }

    #[test]
    fn issues_parse_from_object_array_and_wrapper() {
        let obj = json!({"id": "bd-a1b2", "title": "Fix login", "description": "body"});
        assert_eq!(issue_list(&obj)[0].id, "bd-a1b2");
        let arr = json!([{"id": "bd-1", "title": "t"}, {"id": "bd-2", "title": "u"}]);
        assert_eq!(issue_list(&arr).len(), 2);
        let wrapped = json!({"issues": [{"id": "bd-3", "title": "v", "body": "b"}]});
        let list = issue_list(&wrapped);
        assert_eq!(list[0].id, "bd-3");
        assert_eq!(list[0].body, "b");
        assert!(
            issue_list(&json!({"count": 0})).is_empty(),
            "no id, no issue"
        );
    }

    #[test]
    fn parent_parses_from_field_object_or_dep() {
        let field = json!({"id": "bd-1.2", "title": "leaf", "parent": "bd-1"});
        assert_eq!(issue_list(&field)[0].parent.as_deref(), Some("bd-1"));
        let obj = json!({"id": "bd-1.2", "title": "leaf", "parent": {"id": "bd-1"}});
        assert_eq!(issue_list(&obj)[0].parent.as_deref(), Some("bd-1"));
        let dep = json!({
            "id": "bd-1.2",
            "title": "leaf",
            "dependencies": [{"issue_id": "bd-1.2", "depends_on_id": "bd-1", "type": "parent-child"}]
        });
        assert_eq!(issue_list(&dep)[0].parent.as_deref(), Some("bd-1"));
    }

    #[test]
    fn leaves_drop_parents_of_ready_children() {
        let issues = issue_list(&json!([
            {"id": "bd-1", "title": "epic"},
            {"id": "bd-1.2", "title": "leaf", "parent": "bd-1"},
            {"id": "bd-9", "title": "solo"}
        ]));
        let ids: Vec<&str> = leaves(&issues).into_iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["bd-1.2", "bd-9"]);
    }
}
