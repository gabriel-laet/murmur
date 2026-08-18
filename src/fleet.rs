//! The fleet roster — heterogeneity as data.
//!
//! Which model is good at what is judgment, and judgment belongs to the
//! coordinator model and the human who curates it — never to kernel code.
//! So the roster is a plain markdown file, `FLEET.md`, living next to
//! `AGENTS.md` where it rides git (durable knowledge belongs in the repo,
//! like beads — `.murmur/` is gitignored state). Murmur never parses it:
//! `murmur start` pastes it verbatim into the lead's brief, and that's the
//! entire mechanism. Routing quality comes from the file's content plus the
//! ledger (`bd` records who closed what), not from murmur.

use anyhow::Result;
use std::fs;
use std::path::PathBuf;

pub const FILE: &str = "FLEET.md";

/// Long rosters shouldn't eat the brief; this is a prompt, not a wiki.
const BRIEF_CAP: usize = 2000;

const TEMPLATE: &str = "\
# Fleet roster

Which agent kind fits which work. Curated by humans, read by the lead agent:
`murmur start` pastes this file into the lead's brief verbatim and never
interprets it. Keep it short and opinionated, and update it when the ledger
proves you wrong (beads records which kind closed — and reopened — what).

| kind   | strong at                                 | avoid for             | cost |
| ------ | ----------------------------------------- | --------------------- | ---- |
| claude | architecture, review, gnarly debugging    | bulk mechanical edits | high |
| codex  | tests, mechanical refactors, wide sweeps  | ambiguous design      | mid  |
| grok   | quick fixes, exploration, one-off scripts | large refactors       | low  |

Start a mixed herd: `murmur start bd-a1b2 --kind claude,codex=2`
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
pub fn for_brief() -> Option<String> {
    let text = fs::read_to_string(find()?).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(truncate(text, BRIEF_CAP))
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
