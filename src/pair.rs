use crate::socket;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

pub async fn run(channel: &str) -> anyhow::Result<()> {
    let path = socket::socket_path(channel)?;

    let stream = if path.exists() {
        // Second caller: connect
        UnixStream::connect(&path).await?
    } else {
        // First caller: bind and wait for peer
        let listener = socket::bind(channel)?;
        let (stream, _) = listener.accept().await?;
        stream
    };

    let channel = channel.to_string();
    let result = duplex(stream).await;
    socket::cleanup(&channel);
    result
}

async fn duplex(stream: UnixStream) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();

    // socket → stdout
    let read_task = tokio::spawn(async move {
        let mut buf_reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match buf_reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    print!("{}", line);
                }
            }
        }
    });

    // stdin → socket
    let write_task = tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = writer.flush().await;
                }
            }
        }
    });

    // Exit when either side closes
    tokio::select! {
        _ = read_task => {}
        _ = write_task => {}
    }

    Ok(())
}
