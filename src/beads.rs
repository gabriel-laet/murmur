//! Beads adapter. Beads (`bd`) is the plan: issues, dependencies, ready
//! detection, assignment, and memory across sessions. Murmur never mirrors
//! it — there is no board. Assignment IS the bead's assignee; done IS the
//! bead closing. Murmur reads the ready frontier to brief and route, and
//! writes exactly three transitions: assign (in_progress + assignee),
//! done (closed with attribution), drop (open again).
//!
//! We shell out to the `bd` CLI the same way we shell out to `herdr` — no
//! SDK, no socket, and murmur never touches `.beads/` internals. Mutating
//! calls ask for `--json` but treat human-text output on a green exit as
//! success (some builds print a checkmark), and closing an already-closed
//! bead succeeds — the goal state holds.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Issue {
    pub id: String,
    pub title: String,
    pub body: String,
    pub parent: Option<String>,
    pub status: String,
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

/// Ready beads that are not the parent of another ready bead. Work is
/// assigned at leaves; the epic stays in beads until the children close.
pub fn leaves(issues: &[Issue]) -> Vec<&Issue> {
    let parents: std::collections::HashSet<&str> =
        issues.iter().filter_map(|i| i.parent.as_deref()).collect();
    issues
        .iter()
        .filter(|i| !parents.contains(i.id.as_str()))
        .collect()
}

/// One assignment, owned by beads: in_progress + assignee, one call.
pub fn assign(id: &str, agent: &str) -> Result<()> {
    call_mut(&[
        "update",
        id,
        "--status",
        "in_progress",
        "--assignee",
        agent,
        "--json",
    ])
}

/// Close a bead with attribution, treating "already closed" as success:
/// if beads got there first, the goal state holds — stop.
pub fn close(id: &str, reason: &str) -> Result<()> {
    match call_mut(&["close", id, "--reason", reason, "--json"]) {
        Ok(()) => Ok(()),
        Err(e) if e.to_string().to_lowercase().contains("closed") => Ok(()),
        Err(e) => Err(e),
    }
}

/// Hand a bead back: open again, assignment cleared as far as bd allows.
pub fn reopen(id: &str) -> Result<()> {
    call_mut(&["update", id, "--status", "open", "--json"])
}

pub fn call(args: &[&str]) -> Result<Value> {
    call_in(None, args)
}

/// Mutating calls (`update`, `close`): ask for JSON, but a zero exit with
/// human-text output (a checkmark, a summary line) is still success — some
/// `bd` builds print prose even under `--json`, and failing a wave over an
/// unparseable "✓" is worse than moving on.
fn call_mut(args: &[&str]) -> Result<()> {
    match call(args) {
        Ok(_) => Ok(()),
        Err(e) if e.to_string().contains("returned non-JSON") => Ok(()),
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
    Some(Issue {
        id: id.to_string(),
        title: title.to_string(),
        body: body.to_string(),
        parent: parse_parent(v),
        status: status.to_string(),
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

#[cfg(test)]
mod tests {
    use super::{issue_list, leaves};
    use serde_json::json;

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

    #[test]
    fn closed_status_parses() {
        let v = json!({"id": "bd-1", "title": "t", "status": "closed"});
        assert!(issue_list(&v)[0].closed());
    }
}
