mod channels;
mod cli;
mod connect;
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

    // If a channel is provided directly (murmur <channel>), use connect mode
    if let Some(channel) = cli.channel {
        return connect::run(&channel).await;
    }

    // Otherwise, dispatch to subcommand
    match cli.command {
        Some(Command::Listen { channel }) => listen::run(&channel).await,
        Some(Command::Send {
            channel,
            message,
            no_wait,
            timeout,
            reply,
        }) => send::run(&channel, message, !no_wait, timeout, reply).await,
        Some(Command::Pair { channel }) => pair::run(&channel).await,
        Some(Command::Pub { channel }) => pubsub::run_pub(&channel).await,
        Some(Command::Sub { channel }) => pubsub::run_sub(&channel).await,
        Some(Command::Ls) => channels::ls(),
        Some(Command::Rm { channel }) => channels::rm(&channel),
        None => {
            // No channel and no command - print help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
