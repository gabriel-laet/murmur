mod channels;
mod cli;
mod connect;
mod error;
mod hook;
mod listen;
mod mcp_server;
mod message;
mod orchestrate;
mod protocol;
mod send;
mod spawn;
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
        Some(Command::Ls) => channels::ls(),
        Some(Command::Rm { channel }) => channels::rm(&channel),
        Some(Command::Hook { channel }) => hook::run(&channel).await,
        Some(Command::McpServer { channel }) => mcp_server::run(&channel).await,
        Some(Command::Orchestrate { channel }) => orchestrate::run(&channel).await,
        Some(Command::Spawn { id, command }) => spawn::run(&id, &command),
        None => {
            // No channel and no command - print help
            use clap::CommandFactory;
            Cli::command().print_help()?;
            println!();
            Ok(())
        }
    }
}
