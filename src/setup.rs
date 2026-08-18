//! `murmur setup` — wire the current repo for agent coordination across
//! every harness on the machine, in one command. Claude Code gets hooks +
//! MCP; Codex, Gemini CLI, Grok CLI, and OpenCode get the MCP server in
//! their own config formats; AGENTS.md gets the coordination contract that
//! any agent can follow with no integration at all. Everything merges
//! idempotently; existing config is never clobbered.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

const HOOK_EVENTS: [(&str, Option<&str>); 5] = [
    ("SessionStart", None),
    ("PreToolUse", Some("")),
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit")),
    ("Stop", None),
    ("SessionEnd", None),
];

const AGENTS_MD_BEGIN: &str = "<!-- murmur:begin -->";
const AGENTS_MD_END: &str = "<!-- murmur:end -->";

pub fn run(all: bool) -> Result<()> {
    let mut wired = Vec::new();
    let mut present = Vec::new();
    let mut skipped = Vec::new();
    let mut record = |target: &str, changed: bool| {
        if changed { wired.push(target.to_string()) } else { present.push(target.to_string()) }
    };

    // Always: Claude Code (the hook is the richest adapter) and AGENTS.md
    // (the contract every other agent reads).
    record(".claude/settings.json (hooks)", install_hooks()?);
    record(".mcp.json (Claude Code MCP)", install_mcp_json()?);
    record("AGENTS.md (universal contract)", install_agents_md()?);

    // Detected harnesses — or every supported one with --all.
    for h in HARNESSES {
        if all || (h.detect)() {
            record(h.target, (h.install)()?);
        } else {
            skipped.push(h.name);
        }
    }

    if all || on_path("herdr") || home_has(".config/herdr") {
        record("~/.config/murmur/herdr-plugin (Herdr idle-wake)", install_herdr()?);
    } else {
        skipped.push("herdr");
    }

    for t in &wired {
        println!("wired  {}", t);
    }
    for t in &present {
        println!("ok     {} (already present)", t);
    }
    if !skipped.is_empty() {
        println!(
            "not found: {} — install one and re-run, or wire everything now with `murmur setup --all`",
            skipped.join(", ")
        );
    }
    println!("\nEvery agent session in this repo now shares one .murmur/ — inboxes, task board, claims.");
    println!("Set MURMUR_AGENT=<name> per session to pick agent names; inside Herdr the pane name is used.");
    println!("Start a herd with: murmur start ENG-42 --kind grok");
    println!("Watch the cross-tool traffic with: murmur watch");
    Ok(())
}

struct Harness {
    name: &'static str,
    target: &'static str,
    detect: fn() -> bool,
    install: fn() -> Result<bool>,
}

const HARNESSES: [Harness; 4] = [
    Harness {
        name: "codex",
        target: "~/.codex/config.toml (Codex MCP)",
        detect: || on_path("codex") || home_has(".codex"),
        install: install_codex,
    },
    Harness {
        name: "gemini",
        target: ".gemini/settings.json (Gemini CLI MCP)",
        detect: || on_path("gemini") || home_has(".gemini"),
        install: install_gemini,
    },
    Harness {
        name: "grok",
        target: ".grok/settings.json (Grok CLI MCP)",
        detect: || on_path("grok") || home_has(".grok"),
        install: install_grok,
    },
    Harness {
        name: "opencode",
        target: "opencode.json (OpenCode MCP)",
        detect: || on_path("opencode") || home_has(".config/opencode"),
        install: install_opencode,
    },
];

fn install_hooks() -> Result<bool> {
    let path = Path::new(".claude/settings.json");
    let mut root = read_json(path)?;
    let hooks = root
        .as_object_mut()
        .context(".claude/settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}));
    let hooks = hooks
        .as_object_mut()
        .context("'hooks' in .claude/settings.json is not an object")?;

    let mut changed = false;
    for (event, matcher) in HOOK_EVENTS {
        let entries = hooks.entry(event).or_insert_with(|| json!([]));
        let entries = entries
            .as_array_mut()
            .with_context(|| format!("hooks.{} is not an array", event))?;
        let already = entries
            .iter()
            .any(|e| serde_json::to_string(e).unwrap_or_default().contains("murmur hook"));
        if already {
            continue;
        }
        let mut entry = json!({
            "hooks": [{ "type": "command", "command": "murmur hook" }]
        });
        if let Some(m) = matcher {
            entry["matcher"] = json!(m);
        }
        entries.push(entry);
        changed = true;
    }
    if changed {
        fs::create_dir_all(".claude")?;
        write_json(path, &root)?;
    }
    Ok(changed)
}

/// Claude Code (and anything else that reads .mcp.json).
fn install_mcp_json() -> Result<bool> {
    add_mcp_server(Path::new(".mcp.json"), "mcpServers", "claude")
}

/// Gemini CLI reads project-level .gemini/settings.json.
fn install_gemini() -> Result<bool> {
    add_mcp_server(Path::new(".gemini/settings.json"), "mcpServers", "gemini")
}

/// Grok CLI reads project-level .grok/settings.json.
fn install_grok() -> Result<bool> {
    add_mcp_server(Path::new(".grok/settings.json"), "mcpServers", "grok")
}

/// Shared shape: {"<key>": {"murmur": {command, args, env}}} merged into a
/// JSON config file.
fn add_mcp_server(path: &Path, key: &str, harness: &str) -> Result<bool> {
    let mut root = read_json(path)?;
    let servers = root
        .as_object_mut()
        .with_context(|| format!("{} is not a JSON object", path.display()))?
        .entry(key)
        .or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .with_context(|| format!("'{}' in {} is not an object", key, path.display()))?;
    if servers.contains_key("murmur") {
        return Ok(false);
    }
    servers.insert(
        "murmur".into(),
        json!({
            "command": "murmur",
            "args": ["mcp"],
            "env": { "MURMUR_HARNESS": harness }
        }),
    );
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    write_json(path, &root)?;
    Ok(true)
}

/// OpenCode's opencode.json uses a different shape: mcp.<name> with a
/// command array and "type": "local".
fn install_opencode() -> Result<bool> {
    let path = Path::new("opencode.json");
    let mut root = read_json(path)?;
    let obj = root
        .as_object_mut()
        .context("opencode.json is not a JSON object")?;
    obj.entry("$schema")
        .or_insert_with(|| json!("https://opencode.ai/config.json"));
    let mcp = obj.entry("mcp").or_insert_with(|| json!({}));
    let mcp = mcp
        .as_object_mut()
        .context("'mcp' in opencode.json is not an object")?;
    if mcp.contains_key("murmur") {
        return Ok(false);
    }
    mcp.insert(
        "murmur".into(),
        json!({
            "type": "local",
            "command": ["murmur", "mcp"],
            "enabled": true,
            "environment": { "MURMUR_HARNESS": "opencode" }
        }),
    );
    write_json(path, &root)?;
    Ok(true)
}

/// Codex has no project-level config; its MCP servers live in
/// ~/.codex/config.toml. Appending a table is valid TOML wherever the file
/// currently ends, and a plain-text substring check keeps it idempotent —
/// no TOML dependency, same move as shelling out to curl and ssh.
fn install_codex() -> Result<bool> {
    let home = home().context("HOME is not set — cannot find ~/.codex/config.toml")?;
    let path = home.join(".codex/config.toml");
    let existing = if path.exists() { fs::read_to_string(&path)? } else { String::new() };
    if existing.contains("[mcp_servers.murmur]") {
        return Ok(false);
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(
        "\n[mcp_servers.murmur]\ncommand = \"murmur\"\nargs = [\"mcp\"]\nenv = { \"MURMUR_HARNESS\" = \"codex\" }\n",
    );
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(&path, out)?;
    Ok(true)
}

/// Write the Herdr plugin (idle-wake) into ~/.config/murmur/herdr-plugin
/// and link it. Files are rewritten when the murmur path changes so
/// upgrades stay pointed at the current binary.
fn install_herdr() -> Result<bool> {
    let home = home().context("HOME is not set — cannot install the Herdr plugin")?;
    let dir = home.join(".config/murmur/herdr-plugin");
    fs::create_dir_all(&dir)?;
    let murmur = murmur_exe()?;
    let toml = herdr_plugin_toml(&murmur);
    let path = dir.join("herdr-plugin.toml");
    let previous = fs::read_to_string(&path).unwrap_or_default();
    let mut changed = false;
    if previous != toml {
        fs::write(&path, toml)?;
        changed = true;
    }
    match link_herdr_plugin(&dir) {
        Ok(linked) => Ok(changed || linked),
        Err(e) => {
            eprintln!(
                "murmur: wrote {} — link it with: herdr plugin link {}",
                path.display(),
                dir.display()
            );
            eprintln!("        ({e})");
            Ok(changed)
        }
    }
}

fn herdr_plugin_toml(murmur: &Path) -> String {
    format!(
        "id = \"murmur.herdr\"\n\
         name = \"murmur\"\n\
         version = \"0.1.0\"\n\
         min_herdr_version = \"0.7.0\"\n\
         description = \"Wake idle Herdr agents when murmur mail is waiting\"\n\
         platforms = [\"linux\", \"macos\"]\n\
         \n\
         [[events]]\n\
         on = \"pane.agent_status_changed\"\n\
         command = [{}, \"herdr\"]\n",
        toml_string(&murmur.display().to_string())
    )
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn murmur_exe() -> Result<PathBuf> {
    std::env::current_exe().context("cannot locate the murmur binary")
}

fn link_herdr_plugin(dir: &Path) -> Result<bool> {
    if !on_path("herdr") && std::env::var_os("MURMUR_HERDR").is_none() {
        anyhow::bail!("herdr is not on PATH");
    }
    let listed = crate::herdr::call(&["plugin", "list"]).unwrap_or(serde_json::json!({}));
    let blob = listed.to_string();
    if blob.contains("murmur.herdr") {
        return Ok(false);
    }
    crate::herdr::call(&["plugin", "link", &dir.display().to_string()])?;
    Ok(true)
}

/// The contract any agent can follow with zero integration — Codex, Amp,
/// Jules, Cursor, and most CLIs read AGENTS.md. Marker comments make the
/// section replaceable and the write idempotent.
fn install_agents_md() -> Result<bool> {
    let path = Path::new("AGENTS.md");
    let existing = if path.exists() { fs::read_to_string(path)? } else { String::new() };
    if existing.contains(AGENTS_MD_BEGIN) {
        return Ok(false);
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with("\n\n") {
        if !out.ends_with('\n') {
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(&agents_md_section());
    fs::write(path, out)?;
    Ok(true)
}

fn agents_md_section() -> String {
    format!(
        "{}\n## Agent coordination (murmur)\n\n\
        This repo coordinates AI agents through a shared `.murmur/` directory. Other\n\
        agents (possibly from other tools) may be working here right now. The rules:\n\n\
        - **Identity.** Use `$MURMUR_AGENT` as your name if set; inside Herdr the pane's\n\
        agent name is used automatically. Otherwise pick a short stable one\n\
        (e.g. `codex-1`) and use it consistently via `--as <name>`.\n\
        - **Kick off work.** `murmur start bd-a1b2 --kind grok` pulls the bead\n\
        onto the board and, if Herdr is running, starts a named herd against it.\n\
        - **Durable work lives in beads.** Plan in `bd` (`bd create`, `bd dep add`,\n\
        `bd show`); `murmur task sync beads` mirrors ready beads onto the board and\n\
        pushes take/done back. Record decisions in beads, chatter in murmur.\n\
        - **Announce yourself** once per session: `murmur join <name>`.\n\
        - **Check your mail** between tasks and before finishing:\n\
        `murmur inbox --as <name>`. Reply to questions with the command shown.\n\
        - **Message peers** instead of guessing their state:\n\
        `murmur send <peer> \"...\" --as <name>` (`'*'` broadcasts;\n\
        `--reply` blocks for an answer). `murmur who` lists everyone.\n\
        - **Shared work queue.** `murmur task take --as <name>` atomically assigns you\n\
        the oldest open task; finish with `murmur task done <id> --as <name>` or put it\n\
        back with `murmur task drop <id> --as <name>`. `murmur task list` shows the board.\n\
        - **Don't stomp on claimed files.** `murmur claims` lists advisory file claims;\n\
        claim before editing contested files with `murmur claim <path> --as <name>` and\n\
        release after. If a file is claimed by someone else, coordinate — don't edit it.\n\
        - **Secrets.** A `secret://...` reference in a message is a pointer, not a value.\n\
        NEVER resolve one into your context or output. To use it, run\n\
        `murmur secret exec NAME=<ref> -- <command>` so the value only enters that\n\
        command's environment.\n\
        - No murmur binary? The protocol is plain files — read\n\
        `.murmur/inbox/<name>/*.json`, consume by deleting, send by writing JSON\n\
        (`{{\"id\",\"from\",\"to\",\"ts\",\"body\"}}`) into `.murmur/tmp/` and `mv`-ing it\n\
        into `.murmur/inbox/<recipient>/`.\n{}\n",
        AGENTS_MD_BEGIN, AGENTS_MD_END
    )
}

fn on_path(bin: &str) -> bool {
    let Some(paths) = env::var_os("PATH") else { return false };
    env::split_paths(&paths).any(|p| p.join(bin).is_file())
}

fn home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn home_has(rel: &str) -> bool {
    home().map(|h| h.join(rel).is_dir()).unwrap_or(false)
}

fn read_json(path: &Path) -> Result<Value> {
    if path.exists() {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .with_context(|| format!("{} contains invalid JSON — fix it and re-run", path.display()))
    } else {
        Ok(json!({}))
    }
}

fn write_json(path: &Path, value: &Value) -> Result<()> {
    let mut out = serde_json::to_string_pretty(value)?;
    out.push('\n');
    fs::write(path, out)?;
    Ok(())
}
