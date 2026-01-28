use crate::{message, socket};
use tokio::signal;

pub async fn run(channel: &str) -> anyhow::Result<()> {
    let listener = socket::bind(channel)?;
    let channel = channel.to_string();

    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    tokio::spawn(async move {
                        let _ = message::read_messages(stream, |msg| {
                            println!("{}", msg);
                        })
                        .await;
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
