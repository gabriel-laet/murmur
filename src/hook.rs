use serde::Deserialize;
use serde_json::Value;
use std::process;

use crate::protocol::{self, OrchestratorRequest};

#[derive(Deserialize)]
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

pub async fn run(channel: &str) -> anyhow::Result<()> {
    // Read stdin (hook JSON)
    let mut input = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;

    let hook: HookInput = match serde_json::from_str(&input) {
        Ok(h) => h,
        Err(_) => process::exit(0), // graceful degradation
    };

    let agent_id = std::env::var("MURMUR_AGENT_ID")
        .ok()
        .or_else(|| hook.session_id.clone())
        .unwrap_or_else(|| "unknown".into());

    let event = match &hook.hook_event_name {
        Some(e) => e.as_str(),
        None => process::exit(0),
    };

    match event {
        "PreToolUse" => handle_pre_tool_use(channel, &agent_id, &hook).await,
        "PostToolUse" => handle_post_tool_use(channel, &agent_id, &hook).await,
        "Stop" => handle_stop(channel, &agent_id).await,
        _ => process::exit(0),
    }
}

async fn handle_pre_tool_use(
    channel: &str,
    agent_id: &str,
    hook: &HookInput,
) -> anyhow::Result<()> {
    let mut additional_context = Vec::new();

    // 1. Check for pending messages
    let check = OrchestratorRequest::Check {
        agent_id: agent_id.into(),
    };
    if let Ok(Some(resp)) = protocol::orchestrator_request(channel, &check).await {
        if let Some(messages) = resp.messages {
            for msg in &messages {
                additional_context.push(format!("[murmur from {}] {}", msg.from, msg.message));
            }
        }
    }

    // 2. If tool is Edit/Write, try to lock the file
    let tool = hook.tool_name.as_deref().unwrap_or("");
    let file_path = extract_file_path(&hook.tool_input);

    if matches!(tool, "Edit" | "Write") {
        if let Some(path) = &file_path {
            let lock = OrchestratorRequest::Lock {
                agent_id: agent_id.into(),
                path: path.clone(),
            };
            if let Ok(Some(resp)) = protocol::orchestrator_request(channel, &lock).await {
                if resp.status == "denied" {
                    let holder = resp.holder.as_deref().unwrap_or("another agent");
                    eprintln!(
                        "murmur: file {} is locked by {}",
                        path, holder
                    );
                    process::exit(2);
                }
            }
        }
    }

    // 3. Output hook JSON if we have context to inject
    if !additional_context.is_empty() {
        let output = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "additionalContext": additional_context.join("\n")
            }
        });
        println!("{}", serde_json::to_string(&output)?);
    }

    Ok(())
}

async fn handle_post_tool_use(
    channel: &str,
    agent_id: &str,
    hook: &HookInput,
) -> anyhow::Result<()> {
    let tool = hook.tool_name.as_deref().unwrap_or("");
    let file_path = extract_file_path(&hook.tool_input);

    // Notify about the event
    let notify = OrchestratorRequest::Notify {
        agent_id: agent_id.into(),
        event: format!("PostToolUse:{}", tool),
        path: file_path.clone(),
    };
    let _ = protocol::orchestrator_request(channel, &notify).await;

    // Unlock if file was involved
    if matches!(tool, "Edit" | "Write") {
        if let Some(path) = file_path {
            let unlock = OrchestratorRequest::Unlock {
                agent_id: agent_id.into(),
                path,
            };
            let _ = protocol::orchestrator_request(channel, &unlock).await;
        }
    }

    Ok(())
}

async fn handle_stop(channel: &str, agent_id: &str) -> anyhow::Result<()> {
    let idle = OrchestratorRequest::Idle {
        agent_id: agent_id.into(),
    };
    let _ = protocol::orchestrator_request(channel, &idle).await;
    Ok(())
}

fn extract_file_path(tool_input: &Option<Value>) -> Option<String> {
    let obj = tool_input.as_ref()?.as_object()?;
    for key in &["file_path", "filePath", "path", "file"] {
        if let Some(Value::String(s)) = obj.get(*key) {
            return Some(s.clone());
        }
    }
    None
}
