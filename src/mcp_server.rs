use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::protocol::{self, OrchestratorRequest};

#[derive(Deserialize)]
#[allow(dead_code)]
struct JsonRpcRequest {
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

pub async fn run(channel: &str) -> anyhow::Result<()> {
    let channel = channel.to_string();
    let agent_id = std::env::var("MURMUR_AGENT_ID").unwrap_or_else(|_| "mcp-agent".into());

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut reader = BufReader::new(stdin);
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0".into(),
                    id: None,
                    result: None,
                    error: Some(JsonRpcError {
                        code: -32700,
                        message: format!("Parse error: {}", e),
                    }),
                };
                let out = serde_json::to_string(&resp)?;
                stdout.write_all(out.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
                continue;
            }
        };

        // Notifications have no id — don't send a response
        if request.id.is_none() {
            continue;
        }

        let response = handle_request(&request, &channel, &agent_id).await;
        let out = serde_json::to_string(&response)?;
        stdout.write_all(out.as_bytes()).await?;
        stdout.write_all(b"\n").await?;
        stdout.flush().await?;
    }

    Ok(())
}

async fn handle_request(
    req: &JsonRpcRequest,
    channel: &str,
    agent_id: &str,
) -> JsonRpcResponse {
    let id = req.id.clone();

    match req.method.as_str() {
        "initialize" => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": {
                    "name": "murmur",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
            error: None,
        },

        "tools/list" => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(json!({
                "tools": [
                    {
                        "name": "send_message",
                        "description": "Send a message to another agent via murmur orchestrator",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "to": { "type": "string", "description": "Target agent ID" },
                                "message": { "type": "string", "description": "Message content" }
                            },
                            "required": ["to", "message"]
                        }
                    },
                    {
                        "name": "check_messages",
                        "description": "Check for pending messages from other agents",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    },
                    {
                        "name": "list_agents",
                        "description": "List all agents known to the orchestrator",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    },
                    {
                        "name": "broadcast",
                        "description": "Broadcast a message to all other agents",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "message": { "type": "string", "description": "Message content" }
                            },
                            "required": ["message"]
                        }
                    }
                ]
            })),
            error: None,
        },

        "tools/call" => handle_tool_call(id, &req.params, channel, agent_id).await,

        _ => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32601,
                message: "Method not found".into(),
            }),
        },
    }
}

async fn handle_tool_call(
    id: Option<Value>,
    params: &Value,
    channel: &str,
    agent_id: &str,
) -> JsonRpcResponse {
    let tool_name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let arguments = params.get("arguments").cloned().unwrap_or(json!({}));

    let result = match tool_name {
        "send_message" => {
            let to = arguments.get("to").and_then(|v| v.as_str()).unwrap_or("");
            let message = arguments.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let req = OrchestratorRequest::Send {
                from: agent_id.into(),
                to: to.into(),
                message: message.into(),
            };
            call_orchestrator(channel, &req).await
        }

        "check_messages" => {
            let req = OrchestratorRequest::Check {
                agent_id: agent_id.into(),
            };
            call_orchestrator(channel, &req).await
        }

        "list_agents" => {
            let req = OrchestratorRequest::Agents;
            call_orchestrator(channel, &req).await
        }

        "broadcast" => {
            let message = arguments.get("message").and_then(|v| v.as_str()).unwrap_or("");
            let req = OrchestratorRequest::Broadcast {
                from: agent_id.into(),
                message: message.into(),
            };
            call_orchestrator(channel, &req).await
        }

        _ => Err("Unknown tool".into()),
    };

    match result {
        Ok(text) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(json!({
                "content": [{ "type": "text", "text": text }]
            })),
            error: None,
        },
        Err(e) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: Some(json!({
                "content": [{ "type": "text", "text": e }],
                "isError": true
            })),
            error: None,
        },
    }
}

async fn call_orchestrator(
    channel: &str,
    request: &OrchestratorRequest,
) -> Result<String, String> {
    match protocol::orchestrator_request(channel, request).await {
        Ok(Some(resp)) => Ok(serde_json::to_string_pretty(&resp).unwrap_or_default()),
        Ok(None) => Err("Orchestrator is not running. Start it with: murmur orchestrate".into()),
        Err(e) => Err(format!("Orchestrator error: {}", e)),
    }
}
