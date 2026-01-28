mod channels;
mod cli;
mod error;
mod listen;
mod message;
mod pair;
mod pubsub;
mod send;
mod socket;

use clap::Parser;
use cli::{Cli, Command};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Listen { channel } => listen::run(&channel).await,
        Command::Send {
            channel,
            message,
            no_wait,
            timeout,
            reply,
        } => send::run(&channel, message, !no_wait, timeout, reply).await,
        Command::Pair { channel } => pair::run(&channel).await,
        Command::Pub { channel } => pubsub::run_pub(&channel).await,
        Command::Sub { channel } => pubsub::run_sub(&channel).await,
        Command::Ls => channels::ls(),
        Command::Rm { channel } => channels::rm(&channel),
    }
}
