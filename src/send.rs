use crate::{message, socket};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::UnixStream;

pub async fn run(
    channel: &str,
    msg: Option<String>,
    wait: bool,
    timeout: u64,
    reply: bool,
) -> anyhow::Result<()> {
    let mut stream = if wait {
        socket::connect_with_retry(channel, timeout).await?
    } else {
        let path = socket::socket_path(channel)?;
        UnixStream::connect(&path).await.map_err(|_| {
            anyhow::anyhow!(
                "no listener on channel '{}'. Start one with: murmur listen {}",
                channel,
                channel
            )
        })?
    };

    match msg {
        Some(m) => {
            message::write_message(&mut stream, &m).await?;
        }
        None => {
            let stdin = tokio::io::stdin();
            let mut reader = BufReader::new(stdin);
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

    if reply {
        let mut reader = BufReader::new(&mut stream);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n > 0 {
            print!("{}", line.trim_end_matches('\n'));
        }
    }

    Ok(())
}
