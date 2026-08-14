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
//! Transport prefers the Linear MCP already configured on the machine
//! (Grok's `~/.grok` OAuth session at https://mcp.linear.app/mcp). That is
//! the same "use the ubiquitous tool" move as the filesystem and ssh: murmur
//! does not own Linear identity. `LINEAR_API_KEY` + GraphQL remains a
//! fallback. No SDK; both paths shell out to `curl`.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::Write;

use crate::store::Store;
use crate::tasks::{Task, STATES};

const API_URL: &str = "https://api.linear.app/graphql";
const DEFAULT_MCP_URL: &str = "https://mcp.linear.app/mcp";

pub struct Issue {
    pub id: String,
    pub identifier: String,
    pub title: String,
    pub description: String,
    pub url: String,
}

/// Fetch one issue by identifier (`ENG-42`). Prefers Linear MCP.
pub fn fetch(identifier: &str) -> Result<Issue> {
    match discover()? {
        Backend::Mcp(mcp) => mcp.get_issue(identifier),
        Backend::Graphql(api) => api.issue(identifier),
    }
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

pub fn sync(team: &str, label: Option<&str>) -> Result<()> {
    let store = Store::locate()?;
    let backend = discover()?;
    let via = match &backend {
        Backend::Mcp(_) => "mcp",
        Backend::Graphql(_) => "api",
    };

    let mut pulled = 0;
    for issue in backend.open_issues(team, label)? {
        let parsed = match parse_issue(&issue) {
            Ok(i) => i,
            Err(_) => continue,
        };
        if store.task_import(task_from_issue(&parsed))? {
            pulled += 1;
        }
    }

    let states = match &backend {
        Backend::Graphql(api) => Some(api.team_states(team)?),
        Backend::Mcp(_) => None,
    };

    let mut pushed = 0;
    for (state, mut task) in store.task_list(&STATES)? {
        let Some(external_id) = task.external_id.clone() else { continue };
        let Some(target_type) = transition(&state, task.synced_state.as_deref()) else {
            continue;
        };
        match &backend {
            Backend::Mcp(mcp) => {
                mcp.set_state(&external_id, target_type)?;
                mcp.comment(&external_id, &attribution(&state, &task))?;
            }
            Backend::Graphql(api) => {
                let state_id = states.as_ref().unwrap().get(target_type).with_context(|| {
                    format!("team '{}' has no workflow state of type '{}'", team, target_type)
                })?;
                api.set_state(&external_id, state_id)?;
                api.comment(&external_id, &attribution(&state, &task))?;
            }
        }
        task.synced_state = Some(state.clone());
        store.task_rewrite(&state, &task)?;
        pushed += 1;
    }

    println!(
        "linear ({via}): pulled {} new issue(s), pushed {} transition(s)",
        pulled, pushed
    );
    Ok(())
}

fn task_from_issue(issue: &Issue) -> Task {
    Task {
        id: format!("linear-{}", issue.identifier),
        title: issue.title.clone(),
        body: issue.description.clone(),
        by: "linear".into(),
        ts: crate::store::now_millis(),
        taken_by: None,
        done_by: None,
        external_id: Some(issue.id.clone()),
        external_url: Some(issue.url.clone()),
        synced_state: None,
    }
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

enum Backend {
    Mcp(Mcp),
    Graphql(Api),
}

impl Backend {
    fn open_issues(&self, team: &str, label: Option<&str>) -> Result<Vec<Value>> {
        match self {
            Backend::Mcp(mcp) => mcp.open_issues(team, label),
            Backend::Graphql(api) => api.open_issues(team, label),
        }
    }
}

/// Prefer Linear MCP (already-authenticated Grok session), then GraphQL.
/// Test stubs win so existing curl fixtures keep working.
fn discover() -> Result<Backend> {
    if std::env::var_os("MURMUR_LINEAR_MCP").is_some() {
        return Ok(Backend::Mcp(Mcp {
            url: std::env::var("MURMUR_LINEAR_MCP_URL")
                .unwrap_or_else(|_| DEFAULT_MCP_URL.into()),
            token: std::env::var("MURMUR_LINEAR_MCP_TOKEN").unwrap_or_default(),
        }));
    }
    if std::env::var_os("MURMUR_LINEAR_CURL").is_some() {
        return Ok(Backend::Graphql(Api::from_env()?));
    }
    if let Some(mcp) = grok_linear_mcp() {
        return Ok(Backend::Mcp(mcp));
    }
    if let Ok(api) = Api::from_env() {
        return Ok(Backend::Graphql(api));
    }
    bail!(
        "no Linear credentials — authenticate the Linear MCP in Grok (`/mcps`) \
         or set LINEAR_API_KEY"
    )
}

/// Read the Linear MCP URL + OAuth token Grok already stored. Never logs
/// the token. Missing or unreadable files mean "not configured".
fn grok_linear_mcp() -> Option<Mcp> {
    let home = std::env::var_os("GROK_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".grok")))?;
    let creds_path = home.join("mcp_credentials.json");
    let raw = std::fs::read_to_string(creds_path).ok()?;
    let creds: Value = serde_json::from_str(&raw).ok()?;
    let obj = creds.as_object()?;
    for (key, entry) in obj {
        if !key.starts_with("linear:") {
            continue;
        }
        let url = key
            .strip_prefix("linear:")
            .filter(|u| !u.is_empty())
            .unwrap_or(DEFAULT_MCP_URL)
            .to_string();
        let token = entry
            .pointer("/token_response/access_token")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())?;
        return Some(Mcp {
            url,
            token: token.to_string(),
        });
    }
    None
}

fn parse_issue(v: &Value) -> Result<Issue> {
    let id = v["id"].as_str().unwrap_or("").to_string();
    let identifier = v["identifier"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(id.as_str())
        .to_string();
    if identifier.is_empty() {
        bail!("linear issue is missing an id");
    }
    Ok(Issue {
        id: if id.is_empty() {
            identifier.clone()
        } else {
            id
        },
        identifier,
        title: v["title"].as_str().unwrap_or_default().to_string(),
        description: v["description"].as_str().unwrap_or_default().to_string(),
        url: v["url"].as_str().unwrap_or_default().to_string(),
    })
}

struct Mcp {
    url: String,
    token: String,
}

impl Mcp {
    fn get_issue(&self, identifier: &str) -> Result<Issue> {
        let data = self.call("get_issue", json!({ "id": identifier }))?;
        let node = if data.get("id").is_some() {
            data
        } else {
            data.get("issue").cloned().unwrap_or(data)
        };
        parse_issue(&node)
    }

    fn open_issues(&self, team: &str, label: Option<&str>) -> Result<Vec<Value>> {
        let mut args = json!({
            "team": team,
            "state": "unstarted",
            "includeArchived": false,
            "limit": 50,
            "fields": ["id", "title", "description", "url", "status", "statusType"]
        });
        if let Some(label) = label {
            args["label"] = json!(label);
        }
        let data = self.call("list_issues", args)?;
        let nodes = data
            .get("issues")
            .or_else(|| data.get("nodes"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(nodes)
    }

    fn set_state(&self, issue_id: &str, state_type: &str) -> Result<()> {
        self.call(
            "save_issue",
            json!({ "id": issue_id, "state": state_type }),
        )?;
        Ok(())
    }

    fn comment(&self, issue_id: &str, body: &str) -> Result<()> {
        self.call(
            "save_comment",
            json!({ "issueId": issue_id, "body": body }),
        )?;
        Ok(())
    }

    fn call(&self, name: &str, arguments: Value) -> Result<Value> {
        let body = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))?;
        let raw = http_post(
            std::env::var_os("MURMUR_LINEAR_MCP"),
            &self.url,
            &[
                ("Authorization", &format!("Bearer {}", self.token)),
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
            ],
            &body,
        )?;
        let resp = parse_mcp_response(&raw)?;
        if let Some(err) = resp.get("error") {
            bail!(
                "linear MCP {name}: {}",
                err["message"].as_str().unwrap_or("error")
            );
        }
        mcp_tool_payload(&resp["result"])
    }
}

fn parse_mcp_response(raw: &str) -> Result<Value> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed)
            .context("Linear MCP returned something that isn't JSON");
    }
    let mut last = None;
    for line in trimmed.lines() {
        let Some(data) = line.strip_prefix("data:") else { continue };
        if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
            last = Some(v);
        }
    }
    last.context("Linear MCP returned no JSON payload")
}

fn mcp_tool_payload(result: &Value) -> Result<Value> {
    if result.get("isError").and_then(|v| v.as_bool()) == Some(true) {
        let msg = result["content"]
            .as_array()
            .and_then(|c| c.first())
            .and_then(|c| c["text"].as_str())
            .unwrap_or("tool error");
        bail!("linear MCP: {msg}");
    }
    if let Some(text) = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|c| c.first())
        .and_then(|c| c["text"].as_str())
    {
        if let Ok(v) = serde_json::from_str::<Value>(text) {
            return Ok(v);
        }
    }
    if result.get("content").is_none() && result.is_object() {
        return Ok(result.clone());
    }
    Ok(result.clone())
}

fn http_post(
    stub: Option<std::ffi::OsString>,
    url: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<String> {
    let mut cmd = match stub {
        Some(stub) => std::process::Command::new(stub),
        None => {
            let mut c = std::process::Command::new("curl");
            c.args(["-sS", url, "--data-binary", "@-"]);
            for (k, v) in headers {
                if v.is_empty() {
                    continue;
                }
                c.args(["-H", &format!("{k}: {v}")]);
            }
            c
        }
    };
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("failed to run curl — is it installed?")?;
    child.stdin.as_mut().unwrap().write_all(body)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!(
            "linear request failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

struct Api {
    key: String,
}

impl Api {
    fn from_env() -> Result<Api> {
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

    fn issue(&self, identifier: &str) -> Result<Issue> {
        let data = self.post(
            "query($id: String!) { issue(id: $id) { id identifier title description url } }",
            json!({ "id": identifier }),
        )?;
        let issue = &data["issue"];
        if issue.is_null() {
            bail!("no Linear issue '{identifier}'");
        }
        parse_issue(issue)
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
        let raw = http_post(
            std::env::var_os("MURMUR_LINEAR_CURL"),
            API_URL,
            &[
                ("Authorization", self.key.as_str()),
                ("Content-Type", "application/json"),
            ],
            &body,
        )?;
        let resp: Value =
            serde_json::from_str(&raw).context("Linear returned something that isn't JSON")?;
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
    use super::{mcp_tool_payload, parse_issue, parse_mcp_response, transition};
    use serde_json::json;

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

    #[test]
    fn mcp_issue_uses_human_id_when_identifier_is_absent() {
        let issue = parse_issue(&json!({
            "id": "GAR-1052",
            "title": "Foundation",
            "description": "body",
            "url": "https://linear.app/garagem/issue/GAR-1052"
        }))
        .unwrap();
        assert_eq!(issue.identifier, "GAR-1052");
        assert_eq!(issue.id, "GAR-1052");
    }

    #[test]
    fn graphql_issue_keeps_uuid_and_identifier() {
        let issue = parse_issue(&json!({
            "id": "uuid-42",
            "identifier": "ENG-42",
            "title": "Fix login",
            "url": "https://linear.app/acme/issue/ENG-42"
        }))
        .unwrap();
        assert_eq!(issue.id, "uuid-42");
        assert_eq!(issue.identifier, "ENG-42");
    }

    #[test]
    fn mcp_sse_and_wrapped_tool_payloads_unwrap() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"{\\\"id\\\":\\\"ENG-1\\\"}\"}]}}\n";
        let resp = parse_mcp_response(sse).unwrap();
        let payload = mcp_tool_payload(&resp["result"]).unwrap();
        assert_eq!(payload["id"], "ENG-1");
    }
}
