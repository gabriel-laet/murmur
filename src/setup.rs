//! `murmur setup` — wire the current repo for agent coordination in one
//! command. Two tiers, one directory: Claude Code gets hooks (the passive,
//! enforced adapter), and AGENTS.md gets the coordination contract every
//! other harness — Codex, Gemini, Grok, OpenCode, anything with a shell —
//! follows through the murmur CLI, no per-harness config at all. The CLI
//! is the protocol. Everything merges idempotently; existing config is
//! never clobbered.

use crate::store::on_path;
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
        if changed {
            wired.push(target.to_string())
        } else {
            present.push(target.to_string())
        }
    };

    // Claude Code gets hooks (the enforced tier); every other harness
    // coordinates through the murmur CLI as written in AGENTS.md.
    record(".claude/settings.json (hooks)", install_hooks()?);
    record("AGENTS.md (universal contract)", install_agents_md()?);
    record("FLEET.md (fleet roster)", crate::fleet::seed()?);

    if all || on_path("herdr") || home_has(".config/herdr") {
        record(
            "~/.config/murmur/herdr-plugin (Herdr idle-wake)",
            install_herdr()?,
        );
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
    println!(
        "\nEvery agent session in this repo now shares one .murmur/ — inboxes, task board, claims."
    );
    println!("Set MURMUR_AGENT=<name> per session to pick agent names; inside Herdr the pane name is used.");
    println!("Start a herd with: murmur start bd-a1b2 --kind grok  (mixed: --kind claude,codex=2)");
    println!("Route by strength: edit FLEET.md — the lead's brief carries it verbatim.");
    println!("Watch the cross-tool traffic with: murmur watch");
    Ok(())
}

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
        let already = entries.iter().any(|e| {
            serde_json::to_string(e)
                .unwrap_or_default()
                .contains("murmur hook")
        });
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
    let existing = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };
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
        - **Announce yourself** once per session: `murmur join <name>`.\n\
        - **Check your mail first** and between tasks: `murmur inbox --as <name>`.\n\
        Your lead assigns work by mail — that assignment, not the board, is your queue.\n\
        - **Message peers** instead of guessing their state:\n\
        `murmur send <peer> \"...\" --as <name>` (`'*'` broadcasts;\n\
        `--reply` blocks for an answer). `murmur who` lists everyone.\n\
        - **Take what you were assigned, by id:** `murmur task take <id> --as <name>`;\n\
        finish with `murmur task done <id> --as <name>` or put it back with\n\
        `murmur task drop <id> --as <name>`. Bare `murmur task take` (oldest open leaf)\n\
        only when the lead says the board is yours. `murmur task list` shows the board.\n\
        - **If you were briefed as a worker**, your slice is the job: when it is done and\n\
        your inbox is empty, report to lead and stop. Merging, CI, and the wider board\n\
        belong to the lead.\n\
        - **Never sync the tracker wholesale.** `murmur task sync beads` is the lead's\n\
        (or the human's) call, scoped: `--parent <epic>` or `--label <l>`. On a real\n\
        tracker an unscoped sync floods the board for everyone. Durable planning lives\n\
        in `bd` (`bd create`, `bd dep add`); decisions go in beads, chatter in murmur.\n\
        Which agent kind fits which work is in `FLEET.md`.\n\
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

fn home() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn home_has(rel: &str) -> bool {
    home().map(|h| h.join(rel).is_dir()).unwrap_or(false)
}

fn read_json(path: &Path) -> Result<Value> {
    if path.exists() {
        let content = fs::read_to_string(path)?;
        serde_json::from_str(&content).with_context(|| {
            format!(
                "{} contains invalid JSON — fix it and re-run",
                path.display()
            )
        })
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
