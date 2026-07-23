//! Linear as a task-board adapter. Humans plan in Linear; agents execute on
//! the murmur board; state flows back. The delegation split:
//!
//! - Linear owns planning, priorities, the human UI, and history.
//! - The board owns the agent mechanics: atomic take, holder-checked done.
//! - `murmur task sync linear` reconciles the two, both directions, and is
//!   idempotent — run it from a hook, a cron, or by hand.
//!
//! Pulled issues become tasks with id `linear-<identifier>` (e.g.
//! `linear-ENG-42`), so `murmur task done linear-ENG-42` reads naturally and
//! re-syncs never duplicate. Local transitions push back as workflow-state
//! changes plus an attributed comment: take → started, done → completed,
//! drop → unstarted. `synced_state` in the task file tracks what Linear has
//! already been told, which is what makes push idempotent.
//!
//! Transport is the same delegation move as secrets: shell out to `curl`
//! (auth via LINEAR_API_KEY, never stored). No SDK, no HTTP stack.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;

use crate::store::Store;
use crate::tasks::{Task, STATES};

const API_URL: &str = "https://api.linear.app/graphql";

pub fn sync(team: &str, label: Option<&str>) -> Result<()> {
    let store = Store::locate()?;
    let api = Api::new()?;

    let states = api.team_states(team)?;

    let mut pulled = 0;
    for issue in api.open_issues(team, label)? {
        let identifier = issue["identifier"].as_str().unwrap_or_default();
        if identifier.is_empty() {
            continue;
        }
        let task = Task {
            id: format!("linear-{}", identifier),
            title: issue["title"].as_str().unwrap_or_default().to_string(),
            body: issue["description"].as_str().unwrap_or_default().to_string(),
            by: "linear".into(),
            ts: crate::store::now_millis(),
            taken_by: None,
            done_by: None,
            external_id: issue["id"].as_str().map(|s| s.to_string()),
            external_url: issue["url"].as_str().map(|s| s.to_string()),
            synced_state: None,
        };
        if store.task_import(task)? {
            pulled += 1;
        }
    }

    let mut pushed = 0;
    for (state, mut task) in store.task_list(&STATES)? {
        let Some(external_id) = task.external_id.clone() else { continue };
        let Some(target_type) = transition(&state, task.synced_state.as_deref()) else {
            continue;
        };
        let state_id = states.get(target_type).with_context(|| {
            format!("team '{}' has no workflow state of type '{}'", team, target_type)
        })?;
        api.set_state(&external_id, state_id)?;
        api.comment(&external_id, &attribution(&state, &task))?;
        task.synced_state = Some(state.clone());
        store.task_rewrite(&state, &task)?;
        pushed += 1;
    }

    println!("linear: pulled {} new issue(s), pushed {} transition(s)", pulled, pushed);
    Ok(())
}

/// Which Linear state type (if any) a local state must be pushed as, given
/// what Linear was last told. `None` synced means "as pulled" (todo).
fn transition(local: &str, synced: Option<&str>) -> Option<&'static str> {
    match (local, synced.unwrap_or("todo")) {
        ("doing", "todo") => Some("started"),
        ("done", synced) if synced != "done" => Some("completed"),
        ("todo", "doing") => Some("unstarted"),
        _ => None,
    }
}

fn attribution(state: &str, task: &Task) -> String {
    match state {
        "doing" => format!(
            "Taken by agent `{}` via murmur.",
            task.taken_by.as_deref().unwrap_or("unknown")
        ),
        "done" => format!(
            "Completed by agent `{}` via murmur.",
            task.done_by.as_deref().unwrap_or("unknown")
        ),
        _ => "Released back to the board via murmur.".into(),
    }
}

struct Api {
    key: String,
}

impl Api {
    fn new() -> Result<Api> {
        let key = std::env::var("LINEAR_API_KEY")
            .context("LINEAR_API_KEY is not set — create a personal API key in Linear settings")?;
        Ok(Api { key })
    }

    /// Map of workflow-state type ("unstarted"/"started"/"completed") → state id.
    fn team_states(&self, team: &str) -> Result<HashMap<String, String>> {
        let data = self.post(
            "query($team: String!) { teams(filter: { key: { eq: $team } }) { nodes { id states { nodes { id name type } } } } }",
            json!({ "team": team }),
        )?;
        let nodes = data["teams"]["nodes"]
            .as_array()
            .filter(|n| !n.is_empty())
            .with_context(|| format!("no Linear team with key '{}'", team))?;
        let mut map = HashMap::new();
        for state in nodes[0]["states"]["nodes"].as_array().into_iter().flatten() {
            if let (Some(ty), Some(id)) = (state["type"].as_str(), state["id"].as_str()) {
                map.entry(ty.to_string()).or_insert_with(|| id.to_string());
            }
        }
        Ok(map)
    }

    fn open_issues(&self, team: &str, label: Option<&str>) -> Result<Vec<Value>> {
        let mut filter = json!({
            "team": { "key": { "eq": team } },
            "state": { "type": { "eq": "unstarted" } }
        });
        if let Some(label) = label {
            filter["labels"] = json!({ "some": { "name": { "eq": label } } });
        }
        let data = self.post(
            "query($filter: IssueFilter!) { issues(filter: $filter, first: 50) { nodes { id identifier title description url } } }",
            json!({ "filter": filter }),
        )?;
        Ok(data["issues"]["nodes"].as_array().cloned().unwrap_or_default())
    }

    fn set_state(&self, issue_id: &str, state_id: &str) -> Result<()> {
        self.post(
            "mutation($id: String!, $state: String!) { issueUpdate(id: $id, input: { stateId: $state }) { success } }",
            json!({ "id": issue_id, "state": state_id }),
        )?;
        Ok(())
    }

    fn comment(&self, issue_id: &str, body: &str) -> Result<()> {
        self.post(
            "mutation($id: String!, $body: String!) { commentCreate(input: { issueId: $id, body: $body }) { success } }",
            json!({ "id": issue_id, "body": body }),
        )?;
        Ok(())
    }

    fn post(&self, query: &str, variables: Value) -> Result<Value> {
        let body = serde_json::to_vec(&json!({ "query": query, "variables": variables }))?;
        // MURMUR_LINEAR_CURL swaps the transport for a stub in tests.
        let mut cmd = match std::env::var("MURMUR_LINEAR_CURL") {
            Ok(stub) => std::process::Command::new(stub),
            Err(_) => {
                let mut c = std::process::Command::new("curl");
                c.args([
                    "-sS",
                    API_URL,
                    "-H",
                    &format!("Authorization: {}", self.key),
                    "-H",
                    "Content-Type: application/json",
                    "--data-binary",
                    "@-",
                ]);
                c
            }
        };
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("failed to run curl — is it installed?")?;
        child.stdin.as_mut().unwrap().write_all(&body)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            bail!("linear request failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        let resp: Value = serde_json::from_slice(&out.stdout)
            .context("Linear returned something that isn't JSON")?;
        if let Some(errors) = resp.get("errors").and_then(|e| e.as_array()) {
            if !errors.is_empty() {
                bail!(
                    "linear API error: {}",
                    errors[0]["message"].as_str().unwrap_or("unknown")
                );
            }
        }
        Ok(resp["data"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::transition;

    #[test]
    fn take_pushes_started_once() {
        assert_eq!(transition("doing", None), Some("started"));
        assert_eq!(transition("doing", Some("doing")), None);
    }

    #[test]
    fn done_pushes_completed_from_any_lagging_state() {
        assert_eq!(transition("done", None), Some("completed"));
        assert_eq!(transition("done", Some("doing")), Some("completed"));
        assert_eq!(transition("done", Some("done")), None);
    }

    #[test]
    fn drop_pushes_back_to_unstarted_only_if_linear_was_told_doing() {
        assert_eq!(transition("todo", Some("doing")), Some("unstarted"));
        assert_eq!(transition("todo", None), None);
    }
}
