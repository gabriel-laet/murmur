use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "murmur", about = "Dead-simple local IPC for AI agents")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Listen on a channel, print messages to stdout
    Listen { channel: String },
    /// Send a message to a channel
    Send {
        channel: String,
        /// Message to send (reads stdin if omitted)
        message: Option<String>,
    },
    /// Broadcast stdin to all subscribers
    Pub { channel: String },
    /// Subscribe to broadcasts, print to stdout
    Sub { channel: String },
    /// List active channels
    Ls,
    /// Remove a channel socket
    Rm { channel: String },
}
