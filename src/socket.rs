use crate::error::MurmurError;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::{UnixListener, UnixStream};

const SOCKET_DIR: &str = "/tmp";

pub fn socket_path(channel: &str) -> Result<PathBuf, MurmurError> {
    validate_channel(channel)?;
    Ok(PathBuf::from(format!("{}/murmur-{}.sock", SOCKET_DIR, channel)))
}

fn validate_channel(channel: &str) -> Result<(), MurmurError> {
    if channel.is_empty()
        || channel.len() > 64
        || !channel
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return Err(MurmurError::InvalidChannel(channel.to_string()));
    }
    Ok(())
}

pub fn bind(channel: &str) -> anyhow::Result<UnixListener> {
    let path = socket_path(channel)?;
    if path.exists() {
        // Check if a listener is already active by trying to connect
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            anyhow::bail!(
                "channel '{}' already has an active listener. To send messages, use: murmur send {} \"your message\". To remove the existing listener first: murmur rm {}",
                channel, channel, channel
            );
        }
        // Stale socket file — safe to remove
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    Ok(listener)
}

pub fn cleanup(channel: &str) {
    if let Ok(path) = socket_path(channel) {
        let _ = std::fs::remove_file(path);
    }
}

/// Retry connecting to a channel socket every 50ms until success or timeout.
pub async fn connect_with_retry(channel: &str, timeout_secs: u64) -> anyhow::Result<UnixStream> {
    let path = socket_path(channel)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        match UnixStream::connect(&path).await {
            Ok(stream) => return Ok(stream),
            Err(_e) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(anyhow::anyhow!(
                        "timeout after {}s waiting for channel '{}'. Start a listener with: murmur listen {}",
                        timeout_secs,
                        channel,
                        channel
                    ));
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}
