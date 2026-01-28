use crate::{message, socket};
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;

pub async fn run(channel: &str, msg: Option<String>) -> anyhow::Result<()> {
    let path = socket::socket_path(channel)?;
    let mut stream = UnixStream::connect(&path).await?;

    match msg {
        Some(m) => {
            message::write_message(&mut stream, &m).await?;
        }
        None => {
            let stdin = tokio::io::stdin();
            let mut reader = tokio::io::BufReader::new(stdin);
            let mut line = String::new();
            while reader.read_line(&mut line).await? > 0 {
                let trimmed = line.trim_end_matches('\n');
                if !trimmed.is_empty() {
                    message::write_message(&mut stream, trimmed).await?;
                }
                line.clear();
            }
        }
    }
    Ok(())
}
