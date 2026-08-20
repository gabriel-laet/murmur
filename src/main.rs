mod beads;
mod cloud;
mod commands;
mod doctor;
mod fleet;
mod herdr;
mod hook;
mod restack;
mod secrets;
mod setup;
mod skills;
mod start;
mod store;
mod sync;
mod tasks;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "murmur",
    version,
    about = "The communication kernel for AI agents — any agent, any harness. A directory of files, not a daemon.",
    after_help = "\
Everything lives in .murmur/ (found like .git, walking up from cwd; override with MURMUR_DIR).
Messages wait in the recipient's inbox until read — nobody needs to be listening.
Identity comes from --as <name>, $MURMUR_AGENT, or the Herdr pane name.

QUICK START:
    murmur setup                           # hooks + AGENTS.md contract + Herdr plugin
    murmur plan bd-a1b2 --kind claude      # one lead plans, then summons its herd
    murmur start bd-a1b2 --kind grok       # bead → board → Herdr herd
    murmur stop                            # close that herd's workspace + worktrees
    murmur send frontend \"API is ready\"    # message a peer (delivered even if they're busy)
    murmur send '*' \"rebasing main\"        # broadcast to everyone
    murmur send db \"schema ok?\" --reply    # ask and block for the answer
    murmur inbox --wait                    # read your mail, block until some arrives
    murmur task add \"write auth tests\"     # put work on the shared board
    murmur task take <id>                  # atomically take the task you were assigned
    murmur watch                           # (human) watch all agent chatter live

Inspect everything with plain tools: cat .murmur/log/*.jsonl, ls .murmur/inbox/"
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
        /// Block until the recipient replies, then print the reply
        #[arg(long)]
        reply: bool,
        /// Mark this message as a reply to a message id
        #[arg(long, value_name = "MSG_ID")]
        reply_to: Option<String>,
        /// Give up waiting for a reply after this many seconds
        #[arg(long, default_value_t = 60)]
        timeout: u64,
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
    /// Who is up plus the open board, one screen
    Status,
    /// Prompt a live Herdr agent by murmur name (revives finished panes)
    Poke {
        /// Agent name (lead, w1, …)
        target: String,
        /// Prompt text (or use --brief)
        message: Option<String>,
        /// Re-deliver the stored start brief — for when a login or trust
        /// dialog ate the first delivery
        #[arg(long)]
        brief: bool,
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
        /// Also prune what a finished herd left behind — long-gone agents and
        /// old done tasks — without touching inboxes or logs
        #[arg(long)]
        stale: bool,
        /// How old "stale" is, in hours
        #[arg(long, default_value_t = 24)]
        age_hours: u64,
    },
    /// Shared work queue: a task board where taking a task is atomic
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Sync with a peer .murmur — a path, or user@host[:path] over ssh. No daemon; git-pull-style trust.
    Sync {
        /// Peer: a path to another .murmur (or its parent), or user@host[:path]
        target: Option<String>,
        /// Serve one sync session over stdin/stdout (run by the remote end)
        #[arg(long)]
        stdio: bool,
    },
    /// Secret references: pass secrets between agents without the values ever touching the bus
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
    /// Wire this repo for cross-tool agent coordination: Claude Code hooks + the AGENTS.md contract + FLEET.md + the Herdr plugin
    Setup {
        /// Wire every supported harness, even ones not detected on this machine
        #[arg(long)]
        all: bool,
    },
    /// Stand up a herd for a piece of work: bead → board → Herdr panes
    Start {
        /// What to work on. A bead id like bd-a1b2 is enough on its own.
        goal: Option<String>,
        /// Bead id (bd-a1b2). Implied when GOAL looks like one.
        #[arg(long)]
        bead: Option<String>,
        /// How many agents to start (lead + workers). Default 2.
        #[arg(long, default_value_t = 2)]
        workers: usize,
        /// Agent kind (grok), a mixed herd: claude,codex=2 (first entry leads),
        /// or provider-hosted workers: cloud:cursor=2 (needs CURSOR_API_KEY)
        #[arg(long)]
        kind: Option<String>,
        /// Set up the board only; don't spawn Herdr panes
        #[arg(long)]
        no_herdr: bool,
        /// One git worktree per agent (branch herd/<slug>/<name>); the lead's
        /// branch is the integration branch and only the lead merges
        #[arg(long)]
        worktree: bool,
        /// Give this herd its own bus (.murmur-<name>/) so waves never mix
        #[arg(long)]
        board: Option<String>,
        /// Repo helper that builds each agent checkout instead of bare
        /// `git worktree add` (runs with MURMUR_WORKTREE_{DIR,BRANCH,NAME})
        #[arg(long, value_name = "CMD")]
        worktree_cmd: Option<String>,
        /// A path the whole herd converges on (repeatable); named in every
        /// brief and checked by `murmur restack`
        #[arg(long, value_name = "PATH")]
        hub: Vec<String>,
        /// Run this command in a service pane beside each worker (dev server
        /// etc.) — explicit only; MURMUR_WORKTREE_SLOT distinguishes
        /// instances, port/URL allocation stays the command's business
        #[arg(long, value_name = "CMD")]
        with: Option<String>,
    },
    /// Plan first: start only a lead, briefed to slice the goal into beads
    /// and summon its own workers when the plan is ready
    Plan {
        /// What to work on. A bead id like bd-a1b2 is enough on its own.
        goal: Option<String>,
        /// Bead id (bd-a1b2). Implied when GOAL looks like one.
        #[arg(long)]
        bead: Option<String>,
        /// Kind of the planning lead (claude, grok, ...)
        #[arg(long)]
        kind: Option<String>,
        /// Give this herd its own bus (.murmur-<name>/)
        #[arg(long)]
        board: Option<String>,
        /// A path the whole herd converges on (repeatable)
        #[arg(long, value_name = "PATH")]
        hub: Vec<String>,
    },
    /// Lead's merge queue: merge each worker branch into the current branch,
    /// one at a time, gated by --cmd; stops with facts on the first conflict
    Restack {
        /// Command to run after each merge (its failure stops the queue)
        #[arg(long, value_name = "CMD")]
        cmd: Option<String>,
    },
    /// One snapshot of every herd branch's PR: number, state, checks (needs gh)
    Pr {
        #[command(subcommand)]
        cmd: PrCmd,
    },
    /// The fleet roster plus murmur-observed agent starts (24h / 7d)
    Fleet,
    /// Tear down the last herd: close its Herdr workspace, drop presence,
    /// remove worktrees `start --worktree` created
    Stop {
        /// The named board whose herd to stop (see start --board)
        #[arg(long)]
        board: Option<String>,
    },
    /// Check the fleet roster against what this machine can launch right now:
    /// herdr up, kind binaries on PATH, cloud keys present
    Doctor,
    /// Follow up on provider-hosted agents launched by `start --kind cloud:<backend>`
    /// (a temporary adapter until herdr owns cloud agents)
    Cloud {
        #[command(subcommand)]
        cmd: CloudCmd,
    },
    /// Claude Code hook adapter (reads hook JSON on stdin)
    Hook,
    /// Herdr plugin adapter (idle-wake). Called by the plugin, not by hand.
    Herdr,
}

#[derive(Subcommand)]
enum SecretCmd {
    /// Resolve refs into a command's environment and run it — values never touch stdout or context
    Exec {
        /// NAME=secret://... pairs to inject as environment variables
        #[arg(required = true)]
        pairs: Vec<String>,
        /// The command to run (after --)
        #[arg(last = true, required = true)]
        command: Vec<String>,
    },
}

#[derive(Subcommand)]
enum PrCmd {
    /// PR number, state, and check rollup for each herd branch
    Status,
}

#[derive(Subcommand)]
enum CloudCmd {
    /// Show a cloud agent's state (the provider's agent record, as JSON)
    Status { id: String },
    /// Send a follow-up instruction to a running cloud agent
    Prompt { id: String, text: String },
    /// List cloud agents on the provider
    List,
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Put a task on the board
    Add {
        title: String,
        /// Longer description
        #[arg(long)]
        body: Option<String>,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// List open and in-progress tasks (--all includes done)
    List {
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Atomically take a task: a specific one by id, or the oldest open leaf
    Take {
        /// Task id (prefix is enough). Omit to take the oldest open leaf.
        id: Option<String>,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Mark a task you took as done (id prefix is enough)
    Done {
        id: String,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Put a task you took back on the board
    Drop {
        id: String,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Reconcile the board with the tracker (currently: beads). Pulls are
    /// scoped: on a big tracker pass --parent/--label, or --all to force
    Sync {
        /// Adapter name (beads)
        backend: String,
        /// Pull only this bead and its ready descendants
        #[arg(long)]
        parent: Option<String>,
        /// Pull only beads carrying this label
        #[arg(long)]
        label: Option<String>,
        /// Pull everything even when the ready set is large
        #[arg(long)]
        all: bool,
    },
}

fn main() {
    if let Err(e) = run() {
        eprintln!("murmur: {}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    cloud::load_cursor_key();
    match Cli::parse().command {
        Command::Send {
            to,
            message,
            r#as,
            reply,
            reply_to,
            timeout,
        } => commands::send(&to, message, r#as, reply_to, reply, timeout),
        Command::Inbox {
            wait,
            timeout,
            peek,
            json,
            r#as,
        } => commands::inbox(r#as, wait, timeout, peek, json),
        Command::Who { json } => commands::who(json),
        Command::Status => commands::status(),
        Command::Poke {
            target,
            message,
            brief,
        } => commands::poke(&target, message, brief),
        Command::Join { name } => commands::join(name),
        Command::Leave { r#as } => commands::leave(r#as),
        Command::Claim { path, ttl, r#as } => commands::claim(&path, r#as, ttl),
        Command::Release { path, r#as } => commands::release(&path, r#as),
        Command::Claims { json } => commands::claims(json),
        Command::Log { count, json } => commands::log(count, json),
        Command::Watch { all, json } => commands::watch(all, json),
        Command::Clean {
            all,
            stale,
            age_hours,
        } => commands::clean(all, stale, age_hours),
        Command::Task { cmd } => match cmd {
            TaskCmd::Add { title, body, r#as } => commands::task_add(&title, body, r#as),
            TaskCmd::List { all, json } => commands::task_list(all, json),
            TaskCmd::Take { id, r#as, json } => commands::task_take(id, r#as, json),
            TaskCmd::Done { id, r#as } => commands::task_done(&id, r#as),
            TaskCmd::Drop { id, r#as } => commands::task_drop(&id, r#as),
            TaskCmd::Sync {
                backend,
                parent,
                label,
                all,
            } => commands::task_sync(&backend, parent, label, all),
        },
        Command::Sync { target, stdio } => {
            if stdio {
                sync::run_stdio(target)
            } else if let Some(target) = target {
                sync::run(&target)
            } else {
                sync::run_peers()
            }
        }
        Command::Secret { cmd } => match cmd {
            SecretCmd::Exec { pairs, command } => commands::secret_exec(pairs, command),
        },
        Command::Setup { all } => setup::run(all),
        Command::Start {
            goal,
            bead,
            workers,
            kind,
            no_herdr,
            worktree,
            board,
            worktree_cmd,
            hub,
            with,
        } => start::run(start::Opts {
            goal,
            bead,
            workers,
            kind,
            no_herdr,
            worktree,
            board,
            worktree_cmd,
            hubs: hub,
            with,
            plan: false,
        }),
        Command::Plan {
            goal,
            bead,
            kind,
            board,
            hub,
        } => start::run(start::Opts {
            goal,
            bead,
            workers: 1,
            kind,
            no_herdr: false,
            worktree: false,
            board,
            worktree_cmd: None,
            hubs: hub,
            with: None,
            plan: true,
        }),
        Command::Restack { cmd } => restack::run(cmd),
        Command::Pr { cmd } => match cmd {
            PrCmd::Status => restack::pr_status(),
        },
        Command::Fleet => fleet::show(),
        Command::Stop { board } => start::stop(board),
        Command::Doctor => doctor::run(),
        Command::Cloud { cmd } => match cmd {
            CloudCmd::Status { id } => cloud::status(&id),
            CloudCmd::Prompt { id, text } => cloud::followup(&id, &text),
            CloudCmd::List => cloud::list(),
        },
        Command::Hook => hook::run(),
        Command::Herdr => herdr::run(),
    }
}
