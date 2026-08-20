//! The fleet roster — heterogeneity as data.
//!
//! Which model is good at what is judgment, and judgment belongs to the
//! coordinator model and the human who curates it — never to kernel code.
//! So the roster is a plain markdown file, `FLEET.md`, living next to
//! `AGENTS.md` where it rides git (durable knowledge belongs in the repo,
//! like beads — `.murmur/` is gitignored state). Murmur never parses it
//! for routing: `murmur start` pastes it verbatim into the lead's brief,
//! and that's the entire mechanism. Routing quality comes from the file's
//! content plus the ledger (`bd` records who closed what), not from
//! murmur. The one exception is `murmur doctor`, which reads the kind
//! column as a human-facing lint — never to route.

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

pub const FILE: &str = "FLEET.md";

/// Long rosters shouldn't eat the brief; this is a prompt, not a wiki.
/// 8KiB still leaves room for a kind table plus a short routing note.
const BRIEF_CAP: usize = 8192;

const TEMPLATE: &str = "\
# Fleet roster

Which agent kind fits which work. Curated by humans, read by the lead agent:
`murmur start` pastes this file into the lead's brief verbatim and never
interprets it. Keep it short and opinionated, and update it when the ledger
proves you wrong (beads records which kind closed — and reopened — what).

| kind         | strong at                                  | avoid for             | cost    |
| ------------ | ------------------------------------------ | --------------------- | ------- |
| claude       | architecture, review, gnarly debugging     | bulk mechanical edits | high    |
| codex        | tests, mechanical refactors, wide sweeps   | ambiguous design      | mid     |
| grok         | quick fixes, exploration, one-off scripts  | large refactors       | low     |
| cloud:cursor | parallel bulk slices, long unattended runs | tight pairing         | metered |

Start a mixed herd: `murmur start bd-a1b2 --kind claude,codex=2`
Cloud kinds run on the provider's VM and come back as PRs (needs CURSOR_API_KEY).
";

/// Nearest FLEET.md walking up from cwd, like AGENTS.md and .murmur.
pub fn find() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join(FILE);
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// The roster as brief text: trimmed, capped, None when absent or empty.
/// The observed-usage tally rides along so a planning lead sizes slices
/// against what this machine has already burned today.
pub fn for_brief() -> Option<String> {
    let text = fs::read_to_string(find()?).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut out = truncate(text, BRIEF_CAP);
    if let Some(usage) = usage_line() {
        out.push_str(&format!(
            "\n\nAgent starts on this machine (last 24h): {usage} — weigh remaining \
             quota/subscription when sizing the herd."
        ));
    }
    Some(out)
}

// ---- observed usage ----
//
// Murmur cannot see a provider's quota, but it can count what it launched.
// The tally is machine-level (subscriptions are per account, not per repo)
// and append-only; `murmur fleet` and the lead's brief read it. Estimating
// remaining capacity stays judgment — the file only supplies the facts.

fn usage_file() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("MURMUR_USAGE_FILE") {
        return Some(PathBuf::from(p));
    }
    let base = std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".local/state"))
        })?;
    Some(base.join("murmur").join("usage.jsonl"))
}

/// Record one agent start (local pane or cloud launch). Best-effort.
pub fn record_start(kind: &str) {
    let Some(path) = usage_file() else { return };
    if let Some(dir) = path.parent() {
        let _ = fs::create_dir_all(dir);
    }
    let line = format!(
        "{{\"ts\":{},\"kind\":{}}}\n",
        crate::store::now_secs(),
        serde_json::to_string(kind).unwrap_or_else(|_| "\"?\"".into())
    );
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(path) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// Starts per kind within the last `secs`, most-used first.
pub fn usage_since(secs: u64) -> Vec<(String, usize)> {
    let Some(path) = usage_file() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let cutoff = crate::store::now_secs().saturating_sub(secs);
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ts = v.get("ts").and_then(|x| x.as_u64()).unwrap_or(0);
        let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("");
        if ts >= cutoff && !kind.is_empty() {
            *counts.entry(kind.to_string()).or_default() += 1;
        }
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    out
}

/// "claude 3, codex 12" for the last 24h, None when nothing was started.
pub fn usage_line() -> Option<String> {
    let u = usage_since(86_400);
    if u.is_empty() {
        return None;
    }
    Some(
        u.iter()
            .map(|(k, n)| format!("{k} {n}"))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

/// `murmur fleet` — the roster plus what this machine actually launched.
pub fn show() -> Result<()> {
    match find() {
        Some(path) => println!("roster {}", path.display()),
        None => println!("roster FLEET.md not found — `murmur setup` seeds one"),
    }
    for (label, secs) in [("24h", 86_400u64), ("7d", 604_800)] {
        let u = usage_since(secs);
        if u.is_empty() {
            println!("usage  {label}: no agent starts recorded");
        } else {
            let s: Vec<String> = u.iter().map(|(k, n)| format!("{k} {n}")).collect();
            println!("usage  {label}: {}", s.join(", "));
        }
    }
    println!("note   counts are murmur-observed starts, not provider quota — judgment stays yours");
    Ok(())
}

/// The kind column of the roster table, best-effort — `murmur doctor`'s
/// lint input, never a routing input.
pub fn kinds() -> Vec<String> {
    let Some(path) = find() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    kinds_in(&text)
}

fn first_cell(line: &str) -> String {
    line.trim()
        .trim_matches('|')
        .split('|')
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn is_separator_cell(cell: &str) -> bool {
    !cell.is_empty() && cell.chars().all(|c| c == '-' || c == ' ' || c == ':')
}

/// Harness / cloud kind tokens: `claude`, `cursor-agent`, `cloud:cursor`.
fn looks_like_kind(cell: &str) -> bool {
    let mut parts = cell.split(':');
    let Some(head) = parts.next() else {
        return false;
    };
    let tail = parts.next();
    if parts.next().is_some() {
        return false;
    }
    fn token(s: &str) -> bool {
        let mut chars = s.chars();
        match chars.next() {
            Some(c) if c.is_ascii_alphabetic() => {
                chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            }
            _ => false,
        }
    }
    token(head) && tail.map(token).unwrap_or(true)
}

fn kinds_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut in_kind_table = false;
    for line in text.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            in_kind_table = false;
            continue;
        }
        let cell = first_cell(line);
        if is_separator_cell(&cell) {
            continue;
        }
        if !in_kind_table {
            // Only the roster table (header cell `kind`) is kinds. A later
            // "| bead | owner |" table is routing prose, not a harness list.
            in_kind_table = cell.eq_ignore_ascii_case("kind");
            continue;
        }
        if looks_like_kind(&cell) && !out.contains(&cell) {
            out.push(cell);
        }
    }
    out
}

/// Seed a starter roster in cwd unless one exists up the tree.
pub fn seed() -> Result<bool> {
    if find().is_some() {
        return Ok(false);
    }
    fs::write(FILE, TEMPLATE)?;
    Ok(true)
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

#[cfg(test)]
mod tests {
    use super::kinds_in;

    #[test]
    fn kind_column_parses_from_the_template() {
        let kinds = kinds_in(super::TEMPLATE);
        assert_eq!(kinds, vec!["claude", "codex", "grok", "cloud:cursor"]);
    }

    #[test]
    fn header_separator_and_duplicates_are_skipped() {
        let text = "| kind | x |\n| --- | --- |\n| claude | a |\n| claude | b |\nprose\n";
        assert_eq!(kinds_in(text), vec!["claude"]);
    }

    #[test]
    fn later_markdown_tables_are_not_kinds() {
        let text = "\
| kind | strong at |\n\
| --- | --- |\n\
| claude | architecture |\n\
| cursor-agent | worktrees |\n\
\n\
| bead | kind |\n\
| --- | --- |\n\
| pal-nq9.2 | codex |\n\
| pal-blm.4 | grok |\n";
        assert_eq!(kinds_in(text), vec!["claude", "cursor-agent"]);
    }

    #[test]
    fn cloud_kinds_parse_and_bead_ids_do_not() {
        let text = "| kind |\n| --- |\n| cloud:cursor |\n| not a kind |\n| pal-nq9.2 |\n";
        assert_eq!(kinds_in(text), vec!["cloud:cursor"]);
    }
}
