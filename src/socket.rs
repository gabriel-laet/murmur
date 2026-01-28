use crate::error::MurmurError;
use std::path::PathBuf;
use tokio::net::UnixListener;

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
