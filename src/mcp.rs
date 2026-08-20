//! MCP stdio server — a thin adapter over the store for agents that want to
//! murmur through tool calls instead of the shell. JSON-RPC 2.0, one message
//! per line, no async needed.

use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};

use crate::store::{self, ClaimResult, Store};

pub fn run() -> Result<()> {
    // MURMUR_AGENT wins; then the Herdr pane name; otherwise the harness
    // that launched us (stamped into its config by `murmur setup`).
    let name = crate::commands::ambient(None).unwrap_or_else(|| {
        let harness = std::env::var("MURMUR_HARNESS")
            .ok()
            .map(|h| {
                h.chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect::<String>()
            })
            .filter(|h| !h.is_empty())
            .unwrap_or_else(|| "agent".into());
        format!("{}-{}", harness, std::process::id())
    });
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
                write_response(
                    &mut stdout,
                    &error_response(Value::Null, -32700, &format!("parse error: {}", e)),
                )?;
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
            result_response(
                id,
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
        }
        "ping" => result_response(id, json!({})),
        "tools/list" => result_response(id, json!({ "tools": tool_definitions() })),
        "tools/call" => {
            let tool = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(tool, &args, name) {
                Ok(text) => result_response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": text }]
                    }),
                ),
                Err(e) => result_response(
                    id,
                    json!({
                        "content": [{ "type": "text", "text": e.to_string() }],
                        "isError": true
                    }),
                ),
            }
        }
        _ => error_response(id, -32601, "method not found"),
    }
}

fn call_tool(tool: &str, args: &Value, name: &str) -> Result<String> {
    let store = Store::locate()?;
    let str_arg = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };

    match tool {
        "send_message" => {
            let reply_to = args.get("reply_to").and_then(|v| v.as_str());
            let (recipients, id) =
                store.send(name, &str_arg("to"), &str_arg("message"), reply_to, false)?;
            Ok(format!(
                "delivered to {} (id {})",
                recipients.join(", "),
                id
            ))
        }
        "broadcast" => {
            let (recipients, _) = store.send(name, "*", &str_arg("message"), None, false)?;
            Ok(format!("delivered to {}", recipients.join(", ")))
        }
        "ask" => {
            let timeout = args
                .get("timeout_secs")
                .and_then(|v| v.as_u64())
                .unwrap_or(60);
            let (_, id) = store.send(name, &str_arg("to"), &str_arg("message"), None, true)?;
            match store.await_reply(name, &id, std::time::Duration::from_secs(timeout))? {
                Some(reply) => Ok(reply.body),
                None => Ok(format!(
                    "no reply within {}s — the message stays in their inbox; check_inbox later",
                    timeout
                )),
            }
        }
        "check_inbox" => {
            let msgs = store.drain(name, false)?;
            if msgs.is_empty() {
                Ok("no new messages".into())
            } else {
                Ok(msgs
                    .iter()
                    .map(|m| {
                        let mut line = if m.wants_reply {
                            format!(
                                "[{}] {}: {} (reply expected — use send_message with reply_to={})",
                                store::clock(m.ts), m.from, m.body, m.id
                            )
                        } else {
                            format!("[{}] {}: {}", store::clock(m.ts), m.from, m.body)
                        };
                        if crate::secrets::contains_ref(&m.body) {
                            line.push_str(" [contains a secret:// reference — never resolve it into context; run `murmur secret exec NAME=<ref> -- <command>` instead]");
                        }
                        line
                    })
                    .collect::<Vec<_>>()
                    .join("\n"))
            }
        }
        "add_task" => {
            let task = store.task_add(name, &str_arg("title"), &str_arg("body"))?;
            Ok(format!("added task {}", task.id))
        }
        "take_task" => {
            let veto = crate::beads::take_veto(&store);
            let id = str_arg("id");
            if !id.is_empty() {
                let task = store.task_take_id(name, &id, &veto)?;
                return Ok(format!(
                    "took task {}: {}\nmark it finished with complete_task when done",
                    task.id, task.title
                ));
            }
            match store.task_take(name, &veto)? {
                Some(task) => Ok(format!(
                    "took task {}: {}{}\nmark it finished with complete_task when done",
                    task.id,
                    task.title,
                    if task.body.is_empty() {
                        String::new()
                    } else {
                        format!("\n{}", task.body)
                    }
                )),
                None if store.open_task_count() > 0 => Ok(
                    "no open leaf tasks — what's left is an epic or work beads no longer offers"
                        .into(),
                ),
                None => Ok("no open tasks".into()),
            }
        }
        "complete_task" => {
            let task = store.task_done(name, &str_arg("id"))?;
            Ok(format!("done: {} {}", task.id, task.title))
        }
        "list_tasks" => {
            let tasks = store.task_list(&["todo", "doing"])?;
            if tasks.is_empty() {
                Ok("no open or in-progress tasks".into())
            } else {
                Ok(tasks
                    .iter()
                    .map(|(state, t)| {
                        let who = t
                            .taken_by
                            .as_deref()
                            .map(|w| format!(" (taken by {})", w))
                            .unwrap_or_default();
                        format!("{} {} {}{}", state, t.id, t.title, who)
                    })
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
                        let status = if store::pid_alive(a.pid) {
                            "up"
                        } else {
                            "gone"
                        };
                        format!(
                            "{} ({}, seen {} ago, cwd {})",
                            a.name,
                            status,
                            store::ago(a.last_seen),
                            a.cwd
                        )
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
                    c.path,
                    c.holder,
                    store::ago(c.ts)
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
                    "message": { "type": "string", "description": "Message text" },
                    "reply_to": { "type": "string", "description": "Message id this replies to (when the sender asked and is waiting)" }
                },
                "required": ["to", "message"]
            }
        },
        {
            "name": "ask",
            "description": "Send a question to another agent and block until they reply (or timeout). Returns the reply text.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "to": { "type": "string", "description": "Recipient agent name" },
                    "message": { "type": "string", "description": "The question" },
                    "timeout_secs": { "type": "number", "description": "How long to wait (default 60)" }
                },
                "required": ["to", "message"]
            }
        },
        {
            "name": "add_task",
            "description": "Put a task on the shared work board for any agent to take.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Short task title" },
                    "body": { "type": "string", "description": "Details, acceptance criteria, file paths" }
                },
                "required": ["title"]
            }
        },
        {
            "name": "take_task",
            "description": "Atomically take a task from the shared board: pass id for the specific task your lead assigned (preferred), or omit it for the oldest open leaf. Exactly one agent wins a contested task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id (prefix is enough). Omit only when the board is yours to pick from." }
                }
            }
        },
        {
            "name": "complete_task",
            "description": "Mark a task you took as done.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "Task id (prefix is enough)" }
                },
                "required": ["id"]
            }
        },
        {
            "name": "list_tasks",
            "description": "List open and in-progress tasks on the shared board.",
            "inputSchema": { "type": "object", "properties": {} }
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
