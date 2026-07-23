//! MCP stdio server — a thin adapter over the store for agents that want to
//! murmur through tool calls instead of the shell. JSON-RPC 2.0, one message
//! per line, no async needed.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

use crate::store::{self, ClaimResult, Store};

pub fn run() -> Result<()> {
    let name = std::env::var("MURMUR_AGENT")
        .unwrap_or_else(|_| format!("agent-{}", std::process::id()));
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                write_response(&mut stdout, &error_response(Value::Null, -32700, &format!("parse error: {}", e)))?;
                continue;
            }
        };
        let id = req.get("id").cloned();
        // Notifications (no id) get no response.
        let Some(id) = id else { continue };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let response = handle(id, method, &params, &name);
        write_response(&mut stdout, &response)?;
    }
    Ok(())
}

fn handle(id: Value, method: &str, params: &Value, name: &str) -> Value {
    match method {
        "initialize" => {
            let version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2024-11-05");
            result_response(id, json!({
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "ping" => result_response(id, json!({})),
        "tools/list" => result_response(id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let tool = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(tool, &args, name) {
                Ok(text) => result_response(id, json!({
                    "content": [{ "type": "text", "text": text }]
                })),
                Err(e) => result_response(id, json!({
                    "content": [{ "type": "text", "text": e.to_string() }],
                    "isError": true
                })),
            }
        }
        _ => error_response(id, -32601, "method not found"),
    }
}

fn call_tool(tool: &str, args: &Value, name: &str) -> Result<String> {
    let store = Store::locate()?;
    let str_arg = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();

    match tool {
        "send_message" => {
            let recipients = store.send(name, &str_arg("to"), &str_arg("message"))?;
            Ok(format!("delivered to {}", recipients.join(", ")))
        }
        "broadcast" => {
            let recipients = store.send(name, "*", &str_arg("message"))?;
            Ok(format!("delivered to {}", recipients.join(", ")))
        }
        "check_inbox" => {
            let msgs = store.drain(name, false)?;
            if msgs.is_empty() {
                Ok("no new messages".into())
            } else {
                Ok(msgs
                    .iter()
                    .map(|m| format!("[{}] {}: {}", store::clock(m.ts), m.from, m.body))
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        "list_agents" => {
            let agents = store.agents()?;
            if agents.is_empty() {
                Ok("no agents have joined".into())
            } else {
                Ok(agents
                    .iter()
                    .map(|a| {
                        let status = if store::pid_alive(a.pid) { "up" } else { "gone" };
                        format!("{} ({}, seen {} ago, cwd {})", a.name, status, store::ago(a.last_seen), a.cwd)
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        "claim_file" => {
            let path = str_arg("path");
            match store.claim(&path, name, store::DEFAULT_CLAIM_TTL_SECS)? {
                ClaimResult::Granted => Ok(format!("claimed {}", path)),
                ClaimResult::Held(c) => Ok(format!(
                    "denied: {} is claimed by {} ({} ago)",
                    c.path, c.holder, store::ago(c.ts)
                )),
            }
        }
        "release_file" => {
            let path = str_arg("path");
            if store.release(&path, name)? {
                Ok(format!("released {}", path))
            } else {
                Ok(format!("denied: {} is claimed by someone else", path))
            }
        }
        _ => anyhow::bail!("unknown tool: {}", tool),
    }
}

fn tool_definitions() -> Value {
    json!([
        {
            "name": "send_message",
            "description": "Send a message to another agent. It waits in their inbox until they read it — the recipient does not need to be running.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Recipient agent name (see list_agents)" },
                    "message": { "type": "string", "description": "Message text" }
                },
                "required": ["to", "message"]
            }
        },
        {
            "name": "broadcast",
            "description": "Send a message to every other agent that has joined.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "Message text" }
                },
                "required": ["message"]
            }
        },
        {
            "name": "check_inbox",
            "description": "Read and consume pending messages sent to you by other agents.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_agents",
            "description": "List agents in this workspace and whether they are alive.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "claim_file",
            "description": "Take an advisory claim on a file so other agents avoid editing it. Expires automatically.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to claim" }
                },
                "required": ["path"]
            }
        },
        {
            "name": "release_file",
            "description": "Release your claim on a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to release" }
                },
                "required": ["path"]
            }
        }
    ])
}

fn result_response(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_response(stdout: &mut std::io::Stdout, response: &Value) -> Result<()> {
    let mut out = serde_json::to_vec(response)?;
    out.push(b'\n');
    stdout.write_all(&out)?;
    stdout.flush()?;
    Ok(())
}
