use crate::{message, socket};
use std::sync::Arc;
use tokio::io::AsyncBufReadExt;
use tokio::net::UnixStream;
use tokio::signal;
use tokio::sync::broadcast;

pub async fn run_pub(channel: &str) -> anyhow::Result<()> {
    let listener = socket::bind(channel)?;
    let channel = channel.to_string();
    let (tx, _) = broadcast::channel::<String>(256);
    let tx = Arc::new(tx);

    // Accept subscribers
    let tx_clone = tx.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((mut stream, _)) => {
                    let mut rx = tx_clone.subscribe();
                    tokio::spawn(async move {
                        while let Ok(msg) = rx.recv().await {
                            if message::write_message(&mut stream, &msg).await.is_err() {
                                break;
                            }
                        }
                    });
                }
                Err(_) => break,
            }
        }
    });

    // Read stdin and broadcast
    let stdin = tokio::io::stdin();
    let mut reader = tokio::io::BufReader::new(stdin);
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        let msg = line.trim_end_matches('\n').to_string();
        if !msg.is_empty() {
            let _ = tx.send(msg);
        }
    }

    socket::cleanup(&channel);
    Ok(())
}

pub async fn run_sub(channel: &str) -> anyhow::Result<()> {
    let path = socket::socket_path(channel)?;
    let stream = UnixStream::connect(&path).await?;
    let channel = channel.to_string();

    let handle = tokio::spawn(async move {
        let _ = message::read_messages(stream, |msg| {
            println!("{}", msg);
        })
        .await;
    });

    tokio::select! {
        _ = handle => {}
        _ = signal::ctrl_c() => {
            let _ = &channel; // keep channel alive for potential cleanup
        }
    }
    Ok(())
}
