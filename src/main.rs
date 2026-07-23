mod commands;
mod hook;
mod mcp;
mod store;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "murmur",
    version,
    about = "Local message passing for AI agents. A directory of files, not a daemon.",
    after_help = "\
Everything lives in .murmur/ (found like .git, walking up from cwd; override with MURMUR_DIR).
Messages wait in the recipient's inbox until read — nobody needs to be listening.
Identity comes from --as <name> or the MURMUR_AGENT env var.

QUICK START:
    murmur join backend                    # announce yourself
    murmur send frontend \"API is ready\"    # message a peer (delivered even if they're busy)
    murmur send '*' \"rebasing main\"        # broadcast to everyone
    murmur inbox --wait                    # read your mail, block until some arrives
    murmur watch                           # (human) watch all agent chatter live

Inspect everything with plain tools: cat .murmur/log.jsonl, ls .murmur/inbox/"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Send a message to an agent ('*' broadcasts to everyone). Reads stdin if no message given.
    Send {
        /// Recipient agent name, or '*' for broadcast
        to: String,
        /// Message text (omit to read from stdin)
        message: Option<String>,
        /// Your agent name (defaults to $MURMUR_AGENT)
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Read your pending messages (consumes them unless --peek)
    Inbox {
        /// Block until at least one message arrives
        #[arg(long)]
        wait: bool,
        /// Give up waiting after this many seconds (0 = forever)
        #[arg(long, default_value_t = 60)]
        timeout: u64,
        /// Read without consuming
        #[arg(long)]
        peek: bool,
        /// Output newline-delimited JSON
        #[arg(long)]
        json: bool,
        /// Your agent name (defaults to $MURMUR_AGENT)
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// List agents and whether they're alive
    Who {
        #[arg(long)]
        json: bool,
    },
    /// Register your presence and see who else is here
    Join {
        /// Your agent name (defaults to $MURMUR_AGENT)
        name: Option<String>,
    },
    /// Remove your presence and release your claims
    Leave {
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Take an advisory claim on a file so other agents avoid it
    Claim {
        path: String,
        /// Claim lifetime in seconds
        #[arg(long, default_value_t = store::DEFAULT_CLAIM_TTL_SECS)]
        ttl: u64,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Release your claim on a file
    Release {
        path: String,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// List active claims
    Claims {
        #[arg(long)]
        json: bool,
    },
    /// Print recent message history
    Log {
        /// Number of messages to show
        #[arg(short = 'n', long, default_value_t = 20)]
        count: usize,
        #[arg(long)]
        json: bool,
    },
    /// Follow all message traffic live (for humans)
    Watch {
        /// Include history from the beginning
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove dead agents and expired claims (--all removes the whole .murmur dir)
    Clean {
        #[arg(long)]
        all: bool,
    },
    /// MCP server over stdio (tools: send_message, broadcast, check_inbox, list_agents, claim_file, release_file)
    Mcp,
    /// Claude Code hook adapter (reads hook JSON on stdin)
    Hook,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("murmur: {}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    match Cli::parse().command {
        Command::Send { to, message, r#as } => commands::send(&to, message, r#as),
        Command::Inbox { wait, timeout, peek, json, r#as } => {
            commands::inbox(r#as, wait, timeout, peek, json)
        }
        Command::Who { json } => commands::who(json),
        Command::Join { name } => commands::join(name),
        Command::Leave { r#as } => commands::leave(r#as),
        Command::Claim { path, ttl, r#as } => commands::claim(&path, r#as, ttl),
        Command::Release { path, r#as } => commands::release(&path, r#as),
        Command::Claims { json } => commands::claims(json),
        Command::Log { count, json } => commands::log(count, json),
        Command::Watch { all, json } => commands::watch(all, json),
        Command::Clean { all } => commands::clean(all),
        Command::Mcp => mcp::run(),
        Command::Hook => hook::run(),
    }
}
