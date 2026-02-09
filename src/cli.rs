use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "murmur",
    about = "Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages.",
    args_conflicts_with_subcommands = true,
    after_long_help = r#"CHEAT SHEET:
  murmur ch                        # connect to channel (bidirectional I/O, auto host/peer)
  murmur listen ch                 # bind socket, print incoming messages to stdout
  murmur send ch "msg"             # send a message (retries for 5s if listener isn't up)
  murmur send --reply ch "msg"     # send and wait for one line back
  murmur orchestrate               # start the coordination server (file locks, messages, agents)
  murmur hook                      # editor hook (reads JSON from stdin, talks to orchestrator)
  murmur mcp-server                # MCP server over stdio (JSON-RPC 2.0)
  murmur spawn agent-2 -- claude   # spawn an agent in a new tmux pane
  murmur ls                        # list active channel names
  murmur rm ch                     # delete a channel socket file

PROTOCOL:
  Socket path: /tmp/murmur-<channel>.sock  (canonicalized on macOS to /private/tmp)
  Framing:     newline-delimited (\n)
  Max message: 1 MB"#
)]
pub struct Cli {
    /// Channel to connect to (shorthand for bidirectional pair mode with instructions)
    pub channel: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Bind a Unix socket for <channel> and print every incoming message to stdout, one per line.
    /// Blocks until Ctrl-C. Multiple senders can connect concurrently.
    /// Example: murmur listen work
    Listen { channel: String },

    /// Connect to <channel> and send a message. Reads stdin if <message> is omitted.
    /// By default, retries connecting for up to 5s so you never need sleep hacks.
    /// Exits after sending unless --reply is set.
    /// Example: murmur send work "hello"
    Send {
        channel: String,
        /// Message to send (reads stdin if omitted)
        message: Option<String>,

        /// Fail immediately if the channel socket is not available, instead of retrying.
        #[arg(long)]
        no_wait: bool,

        /// Max seconds to wait for the channel to become available. Default: 5
        #[arg(short, long, default_value = "5")]
        timeout: u64,

        /// After sending, read one \n-delimited line back from the socket and print it to stdout.
        /// The listener process can write a response on the same connection.
        #[arg(short, long)]
        reply: bool,
    },

    /// List active channel names (one per line) by scanning for murmur-*.sock files.
    /// Does not block.
    Ls,

    /// Remove a channel's socket file. Does not block.
    /// Example: murmur rm mychannel
    Rm { channel: String },

    /// Run as an editor hook (Claude Code / Cursor). Reads hook JSON from stdin,
    /// coordinates with the orchestrator for file locking and message delivery.
    /// Exit 0 = allow, exit 2 = block (file locked by another agent).
    /// Example: murmur hook
    Hook {
        /// Orchestrator channel to connect to
        #[arg(short, long, default_value = "orchestrator")]
        channel: String,
    },

    /// Start an MCP server over stdio. Exposes send_message, check_messages,
    /// list_agents, and broadcast tools via JSON-RPC 2.0.
    /// Example: murmur mcp-server
    McpServer {
        /// Orchestrator channel to connect to
        #[arg(short, long, default_value = "orchestrator")]
        channel: String,
    },

    /// Start the orchestrator: agent registry, file locks, message queues.
    /// All hook and MCP server instances connect to this.
    /// Example: murmur orchestrate
    Orchestrate {
        /// Channel name for the orchestrator socket
        #[arg(default_value = "orchestrator")]
        channel: String,
    },

    /// Spawn an agent in a new tmux pane with MURMUR_AGENT_ID set.
    /// Requires tmux. The command runs in a split pane with the agent ID in the environment.
    /// Example: murmur spawn agent-2 -- claude
    Spawn {
        /// Agent ID (set as MURMUR_AGENT_ID in the spawned pane)
        id: String,

        /// Command to run in the new pane (everything after --)
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}
