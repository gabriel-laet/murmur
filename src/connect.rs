use crate::{message, socket};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::signal;
use tokio::sync::broadcast;

pub async fn run(channel: &str) -> anyhow::Result<()> {
    loop {
        let path = socket::socket_path(channel)?;

        // Try to connect to existing host
        if path.exists() {
            if let Ok(stream) = UnixStream::connect(&path).await {
                eprintln!("connected to channel \"{}\"", channel);
                print_peer_instructions(channel);

                match run_as_peer(stream).await {
                    Ok(_) => {
                        eprintln!("host disconnected, attempting to become new host...");
                        // Small delay to avoid race with other peers
                        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                        continue; // Try to become host or reconnect
                    }
                    Err(e) => {
                        eprintln!("connection error: {}", e);
                        continue;
                    }
                }
            }
            // Socket exists but can't connect - stale, remove it
            let _ = std::fs::remove_file(&path);
        }

        // No host exists - become the host
        match socket::bind(channel) {
            Ok(listener) => {
                eprintln!("hosting channel \"{}\"", channel);
                print_host_instructions(channel);
                run_as_host(channel, listener).await?;
                return Ok(()); // Host exited cleanly (Ctrl+C)
            }
            Err(_) => {
                // Another peer became host, retry as client
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                continue;
            }
        }
    }
}

async fn run_as_host(channel: &str, listener: tokio::net::UnixListener) -> anyhow::Result<()> {
    let channel_name = channel.to_string();

    // Channel for broadcasting to all peers
    let (tx, _) = broadcast::channel::<String>(100);
    let tx_clone = tx.clone();

    // Stdin reader task - broadcasts to all peers
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
                    let tx_for_peer = tx_for_accept.clone();

                    tokio::spawn(async move {
                        let (reader, mut writer) = stream.into_split();

                        // Read from peer, broadcast to all
                        let read_handle = tokio::spawn(async move {
                            let _ = message::read_messages(reader, |msg| {
                                println!("{}", msg);
                                let _ = tx_for_peer.send(format!("{}\n", msg));
                            })
                            .await;
                        });

                        // Send broadcasts to this peer
                        let write_handle = tokio::spawn(async move {
                            while let Ok(msg) = rx.recv().await {
                                if writer.write_all(msg.as_bytes()).await.is_err() {
                                    break;
                                }
                                let _ = writer.flush().await;
                            }
                        });

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

async fn run_as_peer(stream: UnixStream) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();

    // socket → stdout
    let read_task = tokio::spawn(async move {
        message::read_messages(reader, |msg| {
            println!("{}", msg);
        })
        .await
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

    // Wait for host to disconnect
    read_task.await??;
    Ok(())
}

fn print_host_instructions(channel: &str) {
    let socket_path = socket::socket_path(channel)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| format!("/tmp/murmur-{}.sock", channel));

    eprintln!(
        r#"
Other agents connect with:
  murmur {}                               # join (auto-failover if host dies)
  murmur send {} "message"                # send one message
  echo "msg" | nc -U {}     # raw socket

All messages broadcast to all peers. Ctrl+C to exit.
---"#,
        channel, channel, socket_path
    );
}

fn print_peer_instructions(_channel: &str) {
    eprintln!(
        r#"
All messages broadcast to all peers. If host dies, a peer auto-promotes.
---"#,
    );
}
