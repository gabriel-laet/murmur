use std::process::Command;

pub fn run(id: &str, command: &[String]) -> anyhow::Result<()> {
    // Check we're inside tmux
    if std::env::var("TMUX").is_err() {
        anyhow::bail!("murmur spawn requires tmux. Start a tmux session first.");
    }

    if command.is_empty() {
        anyhow::bail!("no command specified. Usage: murmur spawn <id> -- <command>");
    }

    // Build the shell command string with MURMUR_AGENT_ID set
    let cmd_str = shell_escape_command(command);
    let full_cmd = format!("MURMUR_AGENT_ID={} {}", shell_escape(id), cmd_str);

    // Split a new pane and run the command
    let status = Command::new("tmux")
        .args(["split-window", "-h", &full_cmd])
        .status()?;

    if !status.success() {
        anyhow::bail!("tmux split-window failed");
    }

    eprintln!("spawned agent '{}' in new tmux pane", id);
    Ok(())
}

fn shell_escape(s: &str) -> String {
    if s.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '.') {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

fn shell_escape_command(args: &[String]) -> String {
    args.iter().map(|a| shell_escape(a)).collect::<Vec<_>>().join(" ")
}
