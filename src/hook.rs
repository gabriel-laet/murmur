//! Claude Code hook adapter. Wire `murmur hook` into settings.json and the
//! agent coordinates with zero config:
//!
//! - SessionStart: join, learn who else is around
//! - PreToolUse: inbox drained into additionalContext; Edit/Write on a file
//!   claimed by another agent is denied with an explanation
//! - PostToolUse (Edit/Write): release the claim
//! - Stop: blocked if teammates messaged you mid-turn, so you see it before
//!   ending the turn
//! - SessionEnd: leave, releasing presence and claims
//!
//! Everything degrades gracefully: malformed input or a missing store exits 0
//! so the hook never breaks the editor.

use serde::Deserialize;
use serde_json::{json, Value};
use std::io::Read;
use std::process::exit;

use crate::store::{ClaimResult, Msg, Store};

const HOOK_CLAIM_TTL_SECS: u64 = 600;

#[derive(Deserialize, Default)]
struct HookInput {
    #[serde(alias = "hookEventName")]
    hook_event_name: Option<String>,
    #[serde(alias = "toolName")]
    tool_name: Option<String>,
    #[serde(alias = "toolInput")]
    tool_input: Option<Value>,
    #[serde(alias = "sessionId")]
    session_id: Option<String>,
}

pub fn run() -> anyhow::Result<()> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let hook: HookInput = serde_json::from_str(&input).unwrap_or_default();

    let Some(event) = hook.hook_event_name.clone() else { exit(0) };
    let Some(name) = agent_name(&hook) else { exit(0) };
    let Ok(store) = Store::locate() else { exit(0) };

    match event.as_str() {
        "SessionStart" => session_start(&store, &name),
        "PreToolUse" => pre_tool_use(&store, &name, &hook),
        "PostToolUse" => post_tool_use(&store, &name, &hook),
        "Stop" => stop(&store, &name),
        "SessionEnd" => {
            let _ = store.leave(&name);
        }
        _ => {}
    }
    Ok(())
}

/// `MURMUR_AGENT` if set, else a stable name derived from the session id.
fn agent_name(hook: &HookInput) -> Option<String> {
    if let Ok(name) = std::env::var("MURMUR_AGENT") {
        return Some(name);
    }
    let session = hook.session_id.as_deref()?;
    let short: String = session.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
    if short.is_empty() {
        None
    } else {
        Some(format!("agent-{}", short))
    }
}

fn session_start(store: &Store, name: &str) {
    if store.touch(name).is_err() {
        return;
    }
    let peers: Vec<String> = store
        .agents()
        .unwrap_or_default()
        .into_iter()
        .map(|a| a.name)
        .filter(|n| n != name)
        .collect();
    let peers_line = if peers.is_empty() {
        "No other agents yet.".to_string()
    } else {
        format!("Other agents here: {}.", peers.join(", "))
    };
    let open_tasks = store.open_task_count();
    let tasks_line = if open_tasks > 0 {
        format!(
            " {} open task(s) on the shared board — grab one with `murmur task take --as {}`.",
            open_tasks, name
        )
    } else {
        String::new()
    };
    let context = format!(
        "murmur: you are agent '{}'. {} Message a peer with `murmur send <peer> \"...\" --as {}` \
         or broadcast with `murmur send '*' \"...\" --as {}`. Incoming messages appear in your \
         context automatically.{}",
        name, peers_line, name, name, tasks_line
    );
    emit(json!({
        "hookSpecificOutput": {
            "hookEventName": "SessionStart",
            "additionalContext": context
        }
    }));
}

fn pre_tool_use(store: &Store, name: &str, hook: &HookInput) {
    let messages = store.drain(name, false).unwrap_or_default();
    let context = format_messages(&messages);

    // Advisory claim on Edit/Write targets: deny if someone else holds it.
    let tool = hook.tool_name.as_deref().unwrap_or("");
    if matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        if let Some(path) = extract_file_path(&hook.tool_input) {
            if let Ok(ClaimResult::Held(claim)) = store.claim(&path, name, HOOK_CLAIM_TTL_SECS) {
                let reason = format!(
                    "murmur: {} is currently claimed by agent '{}'. Work on something else, \
                     or coordinate with them: murmur send {} \"...\" --as {}",
                    claim.path, claim.holder, claim.holder, name
                );
                let mut output = json!({
                    "hookSpecificOutput": {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": reason
                    }
                });
                if let Some(ctx) = &context {
                    output["hookSpecificOutput"]["additionalContext"] = json!(ctx);
                }
                emit(output);
                return;
            }
        }
    }

    if let Some(ctx) = context {
        emit(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": ctx
            }
        }));
    }
}

fn post_tool_use(store: &Store, name: &str, hook: &HookInput) {
    let tool = hook.tool_name.as_deref().unwrap_or("");
    if matches!(tool, "Edit" | "Write" | "MultiEdit" | "NotebookEdit") {
        if let Some(path) = extract_file_path(&hook.tool_input) {
            let _ = store.release(&path, name);
        }
    }
}

/// Don't let the agent end its turn with unread mail — deliver it as the
/// block reason so the agent can act on it.
fn stop(store: &Store, name: &str) {
    let messages = store.drain(name, false).unwrap_or_default();
    if let Some(text) = format_messages(&messages) {
        let open_tasks = store.open_task_count();
        let tasks_note = if open_tasks > 0 {
            format!("\n(Also: {} open task(s) on the board — `murmur task list`.)", open_tasks)
        } else {
            String::new()
        };
        emit(json!({
            "decision": "block",
            "reason": format!(
                "You received messages from other agents while working:\n{}\nAddress them (reply with `murmur send`) or continue if no action is needed.{}",
                text, tasks_note
            )
        }));
    }
}

fn format_messages(messages: &[Msg]) -> Option<String> {
    if messages.is_empty() {
        return None;
    }
    Some(
        messages
            .iter()
            .map(|m| {
                if m.wants_reply {
                    format!(
                        "[murmur from {}] {} (they are blocked waiting — reply with: murmur send {} \"...\" --reply-to {})",
                        m.from, m.body, m.from, m.id
                    )
                } else {
                    format!("[murmur from {}] {}", m.from, m.body)
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn extract_file_path(tool_input: &Option<Value>) -> Option<String> {
    let obj = tool_input.as_ref()?.as_object()?;
    for key in ["file_path", "filePath", "notebook_path", "path", "file"] {
        if let Some(Value::String(s)) = obj.get(key) {
            return Some(s.clone());
        }
    }
    None
}

fn emit(value: Value) {
    println!("{}", value);
}
