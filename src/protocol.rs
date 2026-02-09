use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::socket;

// ── Orchestrator request (client → server) ──────────────────────────────────

#[derive(Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum OrchestratorRequest {
    Check {
        agent_id: String,
    },
    Lock {
        agent_id: String,
        path: String,
    },
    Unlock {
        agent_id: String,
        path: String,
    },
    Notify {
        agent_id: String,
        event: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    Idle {
        agent_id: String,
    },
    Send {
        from: String,
        to: String,
        message: String,
    },
    Broadcast {
        from: String,
        message: String,
    },
    Agents,
}

// ── Orchestrator response (server → client) ──────────────────────────────────

#[derive(Serialize, Deserialize, Default)]
pub struct OrchestratorResponse {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<PendingMessage>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agents: Option<Vec<AgentInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PendingMessage {
    pub from: String,
    pub message: String,
    pub timestamp: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AgentInfo {
    pub id: String,
    pub last_seen: u64,
}

// ── Client helper ────────────────────────────────────────────────────────────

/// Send a request to the orchestrator and read one JSON-line response.
/// Returns `None` if the orchestrator isn't running (connect fails).
pub async fn orchestrator_request(
    channel: &str,
    request: &OrchestratorRequest,
) -> anyhow::Result<Option<OrchestratorResponse>> {
    let path = socket::socket_path(channel)?;
    let mut stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(_) => return Ok(None), // orchestrator not running
    };

    let payload = serde_json::to_string(request)?;
    stream.write_all(payload.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    stream.flush().await?;
    stream.shutdown().await?;

    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(None);
    }
    let resp: OrchestratorResponse = serde_json::from_str(line.trim())?;
    Ok(Some(resp))
}
