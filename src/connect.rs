use crate::{message, socket};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::signal;
use tokio::sync::broadcast;

pub async fn run(channel: &str) -> anyhow::Result<()> {
    let path = socket::socket_path(channel)?;

    // Clean up stale socket if needed
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(&path).is_ok() {
            // Active listener exists - connect as client
            return run_as_client(channel).await;
        }
        // Stale socket - remove it
        std::fs::remove_file(&path)?;
    }

    // No existing listener - become the server
    run_as_server(channel).await
}

async fn run_as_server(channel: &str) -> anyhow::Result<()> {
    let listener = socket::bind(channel)?;
    let channel_name = channel.to_string();

    print_server_instructions(channel);

    // Channel for broadcasting stdin to all connected peers
    let (tx, _) = broadcast::channel::<String>(100);
    let tx_clone = tx.clone();

    // Stdin reader task - reads stdin and broadcasts to all peers
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let _ = tx_clone.send(line.clone());
                }
            }
        }
    });

    // Accept connections loop
    let tx_for_accept = tx.clone();
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    eprintln!("peer connected");
                    let mut rx = tx_for_accept.subscribe();

                    tokio::spawn(async move {
                        let (reader, mut writer) = stream.into_split();

                        // Task to read from peer and print to stdout
                        let read_handle = tokio::spawn(async move {
                            let _ = message::read_messages(reader, |msg| {
                                println!("{}", msg);
                            })
                            .await;
                        });

                        // Task to send broadcast messages to peer
                        let write_handle = tokio::spawn(async move {
                            while let Ok(msg) = rx.recv().await {
                                if writer.write_all(msg.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = writer.flush().await;
                            }
                        });

                        // Wait for read to finish (peer disconnected)
                        let _ = read_handle.await;
                        write_handle.abort();
                        eprintln!("peer disconnected");
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
    socket::cleanup(&channel_name);
    Ok(())
}

async fn run_as_client(channel: &str) -> anyhow::Result<()> {
    let path = socket::socket_path(channel)?;
    let stream = UnixStream::connect(&path).await?;

    print_client_instructions(channel);

    let (reader, mut writer) = stream.into_split();

    // socket → stdout
    let read_task = tokio::spawn(async move {
        let _ = message::read_messages(reader, |msg| {
            println!("{}", msg);
        })
        .await;
    });

    // stdin → socket
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut buf_reader = BufReader::new(stdin);
        let mut line = String::new();
        loop {
            line.clear();
            match buf_reader.read_line(&mut line).await {
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

    // Exit when socket closes
    read_task.await?;
    Ok(())
}

fn print_server_instructions(channel: &str) {
    let socket_path = socket::socket_path(channel)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("/tmp/murmur-{}.sock", channel));

    eprintln!(
        r#"murmur channel "{}" ready (server mode)

Other agents can connect with:
  murmur {}                               # bidirectional
  murmur send {} "message"                # send one message
  murmur send --reply {} "question"       # send and wait for response
  echo "msg" | nc -U {}     # raw socket

Protocol: newline-delimited, 1MB max. Messages appear on stdout.
---"#,
        channel, channel, channel, channel, socket_path
    );
}

fn print_client_instructions(channel: &str) {
    eprintln!(
        r#"murmur channel "{}" connected (client mode)

Bidirectional: stdin -> server, server -> stdout
Type messages and press Enter to send. Ctrl+C to disconnect.
---"#,
        channel
    );
}
