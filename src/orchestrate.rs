use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::signal;

use crate::protocol::{AgentInfo, OrchestratorRequest, OrchestratorResponse, PendingMessage};
use crate::socket;

const LOCK_EXPIRY_SECS: u64 = 300; // 5 minutes

struct FileLock {
    holder: String,
    acquired_at: u64,
}

struct State {
    agents: HashMap<String, AgentInfo>,
    file_locks: HashMap<String, FileLock>,
    mailboxes: HashMap<String, Vec<PendingMessage>>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub async fn run(channel: &str) -> anyhow::Result<()> {
    let listener = socket::bind(channel)?;
    let channel = channel.to_string();

    let state = Arc::new(Mutex::new(State {
        agents: HashMap::new(),
        file_locks: HashMap::new(),
        mailboxes: HashMap::new(),
    }));

    eprintln!("orchestrator listening on channel '{}'", channel);

    let state_clone = Arc::clone(&state);
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let st = Arc::clone(&state_clone);
                    tokio::spawn(async move {
                        let _ = handle_connection(stream, st).await;
                    });
                }
                Err(e) => {
                    eprintln!("accept error: {}", e);
                    break;
                }
            }
        }
    });

    signal::ctrl_c().await?;
    handle.abort();
    socket::cleanup(&channel);
    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    state: Arc<Mutex<State>>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    let n = buf_reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let request: OrchestratorRequest = serde_json::from_str(line.trim())?;
    let response = dispatch(request, &state);
    let payload = serde_json::to_string(&response)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn dispatch(request: OrchestratorRequest, state: &Mutex<State>) -> OrchestratorResponse {
    let mut st = state.lock().unwrap();
    let ts = now();

    match request {
        OrchestratorRequest::Check { agent_id } => {
            touch_agent(&mut st, &agent_id, ts);
            let messages = st.mailboxes.remove(&agent_id).unwrap_or_default();
            OrchestratorResponse {
                status: "ok".into(),
                messages: if messages.is_empty() {
                    None
                } else {
                    Some(messages)
                },
                ..Default::default()
            }
        }

        OrchestratorRequest::Lock { agent_id, path } => {
            touch_agent(&mut st, &agent_id, ts);
            // Check existing lock (with expiry)
            if let Some(lock) = st.file_locks.get(&path) {
                if lock.holder != agent_id && ts - lock.acquired_at < LOCK_EXPIRY_SECS {
                    return OrchestratorResponse {
                        status: "denied".into(),
                        holder: Some(lock.holder.clone()),
                        ..Default::default()
                    };
                }
            }
            st.file_locks.insert(
                path,
                FileLock {
                    holder: agent_id,
                    acquired_at: ts,
                },
            );
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Unlock { agent_id, path } => {
            touch_agent(&mut st, &agent_id, ts);
            if let Some(lock) = st.file_locks.get(&path) {
                if lock.holder == agent_id {
                    st.file_locks.remove(&path);
                }
            }
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Notify {
            agent_id, event, path,
        } => {
            touch_agent(&mut st, &agent_id, ts);
            // Broadcast a notification to all other agents about this event
            let msg = match &path {
                Some(p) => format!("[{}] {} {}", agent_id, event, p),
                None => format!("[{}] {}", agent_id, event),
            };
            let pending = PendingMessage {
                from: agent_id.clone(),
                message: msg,
                timestamp: ts,
            };
            let agent_ids: Vec<String> = st.agents.keys().cloned().collect();
            for id in agent_ids {
                if id != agent_id {
                    st.mailboxes.entry(id).or_default().push(pending.clone());
                }
            }
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Idle { agent_id } => {
            touch_agent(&mut st, &agent_id, ts);
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Send { from, to, message } => {
            touch_agent(&mut st, &from, ts);
            st.mailboxes.entry(to).or_default().push(PendingMessage {
                from,
                message,
                timestamp: ts,
            });
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Broadcast { from, message } => {
            touch_agent(&mut st, &from, ts);
            let pending = PendingMessage {
                from: from.clone(),
                message,
                timestamp: ts,
            };
            let agent_ids: Vec<String> = st.agents.keys().cloned().collect();
            for id in agent_ids {
                if id != from {
                    st.mailboxes.entry(id).or_default().push(pending.clone());
                }
            }
            OrchestratorResponse {
                status: "ok".into(),
                ..Default::default()
            }
        }

        OrchestratorRequest::Agents => {
            let agents: Vec<AgentInfo> = st.agents.values().cloned().collect();
            OrchestratorResponse {
                status: "ok".into(),
                agents: Some(agents),
                ..Default::default()
            }
        }
    }
}

fn touch_agent(state: &mut State, agent_id: &str, ts: u64) {
    state
        .agents
        .entry(agent_id.to_string())
        .and_modify(|a| a.last_seen = ts)
        .or_insert(AgentInfo {
            id: agent_id.to_string(),
            last_seen: ts,
        });
}
