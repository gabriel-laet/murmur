use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "murmur",
    about = "Dead-simple local IPC for AI agents. Unix sockets, newline-delimited messages.",
    after_long_help = r#"CHEAT SHEET:
  murmur listen ch                 # binds socket, prints incoming messages to stdout (blocks)
  murmur send ch "msg"             # sends (retries for 5s if listener isn't up yet)
  murmur send --no-wait ch "msg"   # fail immediately if listener isn't up
  murmur send --reply ch "msg"     # sends, waits for one line back, prints to stdout
  murmur pair ch                   # bidirectional duplex — first caller binds, second connects
  echo '{"j":1}' | murmur send ch # pipe stdin as message
  murmur pub ch                    # reads stdin lines, broadcasts to all subscribers
  murmur sub ch                    # subscribes and prints broadcast lines to stdout
  murmur ls                        # list active channel names
  murmur rm ch                     # delete a channel socket file

PROTOCOL:
  Socket path: /tmp/murmur-<channel>.sock
  Framing:     newline-delimited (\n)
  Max message: 1 MB"#
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
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

    /// Bidirectional duplex on a single channel.
    /// First process binds the socket; second process connects.
    /// Both sides: stdin → socket, socket → stdout. Exits when either side closes.
    /// Example: murmur pair chat
    Pair { channel: String },

    /// Read lines from stdin and broadcast each to all connected subscribers.
    /// Blocks until stdin closes. Subscribers connect via `murmur sub`.
    /// Example: tail -f log.json | murmur pub events
    Pub { channel: String },

    /// Connect to a pub channel and print every broadcast line to stdout.
    /// Blocks until the publisher disconnects or Ctrl-C.
    /// Example: murmur sub events
    Sub { channel: String },

    /// List active channel names (one per line) by scanning /tmp/murmur-*.sock.
    /// Does not block.
    Ls,

    /// Remove a channel's socket file. Does not block.
    /// Example: murmur rm mychannel
    Rm { channel: String },
}
