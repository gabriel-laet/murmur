//! `murmur setup` — wire the current repo for agent coordination in one
//! command: hooks into `.claude/settings.json`, the MCP server into
//! `.mcp.json`. Merges idempotently; existing config is never clobbered.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;

const HOOK_EVENTS: [(&str, Option<&str>); 5] = [
    ("SessionStart", None),
    ("PreToolUse", Some("")),
    ("PostToolUse", Some("Edit|Write|MultiEdit|NotebookEdit")),
    ("Stop", None),
    ("SessionEnd", None),
];

pub fn run() -> Result<()> {
    let hooks_changed = install_hooks()?;
    let mcp_changed = install_mcp()?;
    if hooks_changed {
        println!("wired murmur hooks into .claude/settings.json");
    } else {
        println!(".claude/settings.json: murmur hooks already present");
    }
    if mcp_changed {
        println!("wired murmur MCP server into .mcp.json");
    } else {
        println!(".mcp.json: murmur server already present");
    }
    println!("\nEvery Claude Code session here now joins murmur automatically.");
    println!("Optionally set MURMUR_AGENT=<name> per session to pick agent names.");
    println!("Watch the traffic with: murmur watch");
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

fn install_mcp() -> Result<bool> {
    let path = Path::new(".mcp.json");
    let mut root = read_json(path)?;
    let servers = root
        .as_object_mut()
        .context(".mcp.json is not a JSON object")?
        .entry("mcpServers")
        .or_insert_with(|| json!({}));
    let servers = servers
        .as_object_mut()
        .context("'mcpServers' in .mcp.json is not an object")?;
    if servers.contains_key("murmur") {
        return Ok(false);
    }
    servers.insert(
        "murmur".into(),
        json!({ "command": "murmur", "args": ["mcp"] }),
    );
    write_json(path, &root)?;
    Ok(true)
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
