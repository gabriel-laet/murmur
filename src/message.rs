use crate::error::MurmurError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const MAX_MESSAGE_SIZE: usize = 1_048_576; // 1MB

pub async fn read_messages<R: tokio::io::AsyncRead + Unpin>(
    reader: R,
    mut on_message: impl FnMut(String),
) -> anyhow::Result<()> {
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if line.len() > MAX_MESSAGE_SIZE {
            return Err(MurmurError::MessageTooLarge(line.len()).into());
        }
        let msg = line.trim_end_matches('\n').to_string();
        if !msg.is_empty() {
            on_message(msg);
        }
    }
    Ok(())
}

pub async fn write_message<W: tokio::io::AsyncWrite + Unpin>(
    writer: &mut W,
    msg: &str,
) -> anyhow::Result<()> {
    let payload = format!("{}\n", msg);
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(MurmurError::MessageTooLarge(payload.len()).into());
    }
    writer.write_all(payload.as_bytes()).await?;
    writer.flush().await?;
    Ok(())
}
