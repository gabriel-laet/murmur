mod beads;
mod cloud;
mod commands;
mod doctor;
mod fleet;
mod herdr;
mod restack;
mod secrets;
mod setup;
mod skills;
mod start;
mod store;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "murmur",
    version,
    about = "The foreman for AI agent waves: beads plans, herdr executes, murmur conducts.",
    after_help = "\
Murmur turns a beads plan into a working herd and a merged branch. It requires
a running herdr (panes, presence, delivery) and uses beads (bd) as the plan —
assignment lives on the bead, completion is the bead closing. Murmur's own
state is one small notebook: .murmur/ holds the herd snapshot, each agent's
brief, and a spool of undelivered tells. Nothing else.

QUICK START:
    murmur setup                           # AGENTS.md contract + playbooks + Herdr plugin
    murmur plan bd-a1b2 --kind claude      # one lead plans, then summons its herd
    murmur start bd-a1b2 --kind grok=3 --worktree
    murmur assign bd-a1b2.1 w1             # bead assignee + the worker hears it
    murmur tell w2 \"status?\"               # deliver now, or spool for their next idle
    murmur done bd-a1b2.1 --note \"...\"     # close the bead, lead hears it
    murmur restack --cmd 'npm test'        # lead: merge worker branches, gated
    murmur status                          # the wave on one screen
    murmur stop                            # tear the wave down"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Say something to an agent, reliably: delivered into their pane now,
    /// or spooled for their next idle — never silently lost
    Tell {
        /// Agent name (lead, w1, …)
        target: String,
        /// The message (or use --brief)
        message: Option<String>,
        /// Re-deliver the stored start brief — for when a login or trust
        /// dialog ate the first delivery
        #[arg(long)]
        brief: bool,
        /// Who is speaking (defaults to $MURMUR_AGENT, then "human")
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Assign a bead to a worker: sets the bead in_progress with the agent
    /// as assignee, then hands the worker its slice as a prompt
    Assign {
        /// Bead id (bd-a1b2.1)
        bead: String,
        /// Worker agent name
        agent: String,
        /// Extra context delivered with the slice
        #[arg(long)]
        note: Option<String>,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Close a bead with attribution and tell the lead
    Done {
        /// Bead id
        bead: String,
        /// What changed (rides the close reason and the lead's tell)
        #[arg(long)]
        note: Option<String>,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Hand a bead back: open again, lead told to reassign
    Drop {
        /// Bead id
        bead: String,
        #[arg(long = "as", value_name = "NAME")]
        r#as: Option<String>,
    },
    /// Live agents as herdr sees them, plus anything waiting in the spool
    Who {
        #[arg(long)]
        json: bool,
    },
    /// The wave on one screen: herd, live agents, spool, ready frontier
    Status,
    /// Prune old spool files and briefs (--all removes the whole .murmur dir)
    Clean {
        #[arg(long)]
        all: bool,
        /// How old "stale" is, in hours
        #[arg(long, default_value_t = 24)]
        age_hours: u64,
    },
    /// Secret references: pass secrets between agents without the values ever landing in context
    Secret {
        #[command(subcommand)]
        cmd: SecretCmd,
    },
    /// Wire this repo: the AGENTS.md contract + FLEET.md + role playbooks + the Herdr plugin
    Setup {
        /// Wire everything even when herdr isn't detected on this machine
        #[arg(long)]
        all: bool,
    },
    /// Stand up a wave: goal (bead) → workspace → named agents in worktrees → briefs
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
        /// One git worktree per agent (branch herd/<slug>/<name>); the lead's
        /// branch is the integration branch and only the lead merges
        #[arg(long)]
        worktree: bool,
        /// Give this wave its own notebook (.murmur-<name>/) so waves never mix
        #[arg(long)]
        board: Option<String>,
        /// Repo helper that builds each agent checkout instead of bare
        /// `git worktree add` (runs with MURMUR_WORKTREE_{DIR,BRANCH,NAME,SLOT})
        #[arg(long, value_name = "CMD")]
        worktree_cmd: Option<String>,
        /// A path the whole herd converges on (repeatable); named in every
        /// brief and checked by `murmur restack`
        #[arg(long, value_name = "PATH")]
        hub: Vec<String>,
        /// Run this command in a service pane beside each worker (dev server
        /// etc.); MURMUR_WORKTREE_SLOT distinguishes instances
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
        /// Give this wave its own notebook (.murmur-<name>/)
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
    /// Tear down the last wave: close its Herdr workspace, remove worktrees
    Stop {
        /// The named board whose wave to stop (see start --board)
        #[arg(long)]
        board: Option<String>,
    },
    /// Can this machine run the roster right now? herdr up, kind binaries,
    /// cloud keys, one live provider probe
    Doctor,
    /// Follow up on provider-hosted agents launched by `start --kind cloud:<backend>`
    /// (a temporary adapter until herdr owns cloud agents)
    Cloud {
        #[command(subcommand)]
        cmd: CloudCmd,
    },
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

fn main() {
    if let Err(e) = run() {
        eprintln!("murmur: {}", e);
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    cloud::load_cursor_key();
    match Cli::parse().command {
        Command::Tell {
            target,
            message,
            brief,
            r#as,
        } => commands::tell(&target, message, brief, r#as),
        Command::Assign {
            bead,
            agent,
            note,
            r#as,
        } => commands::assign(&bead, &agent, note, r#as),
        Command::Done { bead, note, r#as } => commands::done(&bead, note, r#as),
        Command::Drop { bead, r#as } => commands::drop_bead(&bead, r#as),
        Command::Who { json } => commands::who(json),
        Command::Status => commands::status(),
        Command::Clean { all, age_hours } => commands::clean(all, age_hours),
        Command::Secret { cmd } => match cmd {
            SecretCmd::Exec { pairs, command } => commands::secret_exec(pairs, command),
        },
        Command::Setup { all } => setup::run(all),
        Command::Start {
            goal,
            bead,
            workers,
            kind,
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
        Command::Herdr => herdr::run(),
    }
}
