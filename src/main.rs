mod channels;
mod cli;
mod error;
mod listen;
mod message;
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
        Command::Send { channel, message } => send::run(&channel, message).await,
        Command::Pub { channel } => pubsub::run_pub(&channel).await,
        Command::Sub { channel } => pubsub::run_sub(&channel).await,
        Command::Ls => channels::ls(),
        Command::Rm { channel } => channels::rm(&channel),
    }
}
