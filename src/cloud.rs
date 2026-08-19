//! Cloud agents — a thin, deliberately temporary executor.
//!
//! Herdr owns execution (panes, lifecycle) and should eventually own cloud
//! agents too; provider-hosted agents have no terminal today, so herdr
//! can't. Until it can, this adapter fills the gap the way every murmur
//! integration does: userland policy in `murmur start`, shelling out to a
//! CLI everyone already has (`curl`), one backend per match arm like
//! secrets.rs. The kernel still never spawns — launching stays `start`'s
//! policy — and nothing here supervises: `cloud status` and `cloud prompt`
//! are explicit follow-ups a human or the lead runs. Delete this module the
//! day herdr grows cloud panes.
//!
//! A cloud kind is `cloud:<backend>` (today: `cloud:cursor`). A cloud agent
//! lives outside the filesystem bus — no inbox, no board, no claims. It
//! gets the brief as its launch prompt, works on the provider's VM, and its
//! output comes back through the git host as a branch/PR. The lead learns
//! each launch id by durable mail at start time.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const PREFIX: &str = "cloud:";
pub const CURSOR_KEY_ENV: &str = "CURSOR_API_KEY";
/// Historical typo seen in ~/.secrets; accepted as a fallback only.
const CURSOR_KEY_ALIAS: &str = "CUSROR_API_KEY";
const CURSOR_API: &str = "https://api.cursor.com/v1";

pub fn known_backend(backend: &str) -> bool {
    backend == "cursor"
}

pub fn is_cloud(kind: &str) -> bool {
    kind.starts_with(PREFIX)
}

/// `cloud:cursor` → `cursor`.
pub fn backend(kind: &str) -> &str {
    kind.strip_prefix(PREFIX).unwrap_or(kind)
}

pub struct Launch {
    pub id: String,
}

pub struct RepoRef {
    pub url: String,
    pub starting_ref: Option<String>,
}

/// The coordinates a provider VM clones from: origin's URL (ssh forms
/// normalized to https) and the current branch when there is one.
pub fn repo_ref(cwd: &Path) -> Result<RepoRef> {
    let out = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        .context("failed to run git")?;
    if !out.status.success() {
        bail!(
            "no 'origin' remote — a cloud agent clones from the git host: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let url = normalize_repo_url(String::from_utf8_lossy(&out.stdout).trim());
    let starting_ref = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty() && b != "HEAD");
    Ok(RepoRef { url, starting_ref })
}

/// ssh remotes become https so the provider can clone through its own git
/// connection: `git@github.com:o/r.git` → `https://github.com/o/r`.
fn normalize_repo_url(raw: &str) -> String {
    let s = raw.trim();
    let s = s.strip_suffix(".git").unwrap_or(s);
    if let Some(rest) = s.strip_prefix("git@") {
        if let Some((host, path)) = rest.split_once(':') {
            return format!("https://{host}/{path}");
        }
    }
    if let Some(rest) = s.strip_prefix("ssh://git@") {
        return format!("https://{rest}");
    }
    s.to_string()
}

pub fn launch(kind: &str, brief: &str, repo: &RepoRef) -> Result<Launch> {
    match backend(kind) {
        "cursor" => cursor_launch(brief, repo),
        other => bail!("unknown cloud backend '{other}' (supported: cursor)"),
    }
}

pub fn status(id: &str) -> Result<()> {
    print_json(curl("GET", &format!("{CURSOR_API}/agents/{id}"), None)?)
}

pub fn followup(id: &str, text: &str) -> Result<()> {
    let body = json!({ "prompt": { "text": text } });
    print_json(curl(
        "POST",
        &format!("{CURSOR_API}/agents/{id}/runs"),
        Some(&body),
    )?)
}

pub fn list() -> Result<()> {
    print_json(curl("GET", &format!("{CURSOR_API}/agents"), None)?)
}

fn print_json(v: Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn cursor_launch(brief: &str, repo: &RepoRef) -> Result<Launch> {
    let mut repo_obj = json!({ "url": repo.url });
    if let Some(r) = &repo.starting_ref {
        repo_obj["startingRef"] = json!(r);
    }
    let mut body = json!({
        "prompt": { "text": brief },
        "repos": [repo_obj],
        "autoCreatePR": true,
    });
    if let Ok(model) = std::env::var("MURMUR_CURSOR_MODEL") {
        if !model.is_empty() {
            body["model"] = json!({ "id": model });
        }
    }
    let v = curl("POST", &format!("{CURSOR_API}/agents"), Some(&body))?;
    // The live create response nests the agent: {"agent":{"id":...},"run":{...}}
    // (verified 2026-08-19); a flat top-level id is kept as fallback.
    let id = v
        .pointer("/agent/id")
        .or_else(|| v.get("id"))
        .and_then(|x| x.as_str())
        .with_context(|| format!("cursor returned no agent id: {v}"))?
        .to_string();
    Ok(Launch { id })
}

fn curl_bin() -> PathBuf {
    std::env::var("MURMUR_CURL")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("curl"))
}

/// Fill `CURSOR_API_KEY` from the environment, `$HERDR_PLUGIN_CONFIG_DIR/.env`,
/// or `~/.secrets`. Herdr panes inherit the server's env, which often never
/// sourced a login-shell secrets file. Never prints the value.
pub fn load_cursor_key() {
    if env_nonempty(CURSOR_KEY_ENV) {
        return;
    }
    if let Some(k) = env_value(CURSOR_KEY_ALIAS) {
        std::env::set_var(CURSOR_KEY_ENV, k);
        return;
    }
    for path in secret_files() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Some(k) = parse_env_value(&text, CURSOR_KEY_ENV)
            .or_else(|| parse_env_value(&text, CURSOR_KEY_ALIAS))
            .filter(|s| !s.is_empty())
        {
            std::env::set_var(CURSOR_KEY_ENV, k);
            return;
        }
    }
}

pub fn cursor_key_available() -> bool {
    load_cursor_key();
    env_nonempty(CURSOR_KEY_ENV)
}

fn env_nonempty(key: &str) -> bool {
    env_value(key).is_some()
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty())
}

fn secret_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Some(dir) = env_value("HERDR_PLUGIN_CONFIG_DIR") {
        files.push(PathBuf::from(dir).join(".env"));
    }
    if let Some(home) = env_value("HOME") {
        files.push(PathBuf::from(home).join(".secrets"));
    }
    files
}

fn parse_env_value(contents: &str, key: &str) -> Option<String> {
    for raw in contents.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim();
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let mut v = v.trim().to_string();
        let bytes = v.as_bytes();
        if bytes.len() >= 2
            && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
                || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
        {
            v = v[1..v.len() - 1].to_string();
        }
        return Some(v);
    }
    None
}

/// One HTTPS call via the curl CLI — no HTTP dependency in the kernel, the
/// same move as shelling out to ssh. The API key goes in over stdin as a
/// curl config line, never as a process argument `ps` could see.
fn curl(method: &str, url: &str, body: Option<&Value>) -> Result<Value> {
    load_cursor_key();
    let key = std::env::var(CURSOR_KEY_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .context(
            "cloud:cursor needs CURSOR_API_KEY (Cursor dashboard → API keys, or ~/.secrets)",
        )?;
    let mut cmd = Command::new(curl_bin());
    cmd.args([
        "-sS",
        "-X",
        method,
        url,
        "-H",
        "Content-Type: application/json",
        "-K",
        "-",
    ]);
    if let Some(b) = body {
        cmd.arg("--data");
        cmd.arg(serde_json::to_string(b)?);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to run '{}' — is curl installed?",
                curl_bin().display()
            )
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "header = \"Authorization: Bearer {key}\"");
    }
    let out = child.wait_with_output().context("curl did not finish")?;
    if !out.status.success() {
        bail!(
            "curl {} failed: {}",
            url,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(trimmed).with_context(|| format!("{url} returned non-JSON: {trimmed}"))
}

#[cfg(test)]
mod tests {
    use super::{backend, is_cloud, normalize_repo_url, parse_env_value};

    #[test]
    fn cloud_kinds_are_prefixed() {
        assert!(is_cloud("cloud:cursor"));
        assert!(!is_cloud("claude"));
        assert!(!is_cloud("cursor"));
        assert_eq!(backend("cloud:cursor"), "cursor");
    }

    #[test]
    fn ssh_remotes_normalize_to_https() {
        assert_eq!(
            normalize_repo_url("git@github.com:acme/demo.git"),
            "https://github.com/acme/demo"
        );
        assert_eq!(
            normalize_repo_url("ssh://git@github.com/acme/demo.git"),
            "https://github.com/acme/demo"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/acme/demo.git"),
            "https://github.com/acme/demo"
        );
        assert_eq!(
            normalize_repo_url("https://github.com/acme/demo"),
            "https://github.com/acme/demo"
        );
    }

    #[test]
    fn parse_env_value_reads_export_quotes_and_typo() {
        let text = "# c\nexport CURSOR_API_KEY='plain'\nCUSROR_API_KEY=\"typo\"\n";
        assert_eq!(
            parse_env_value(text, "CURSOR_API_KEY").as_deref(),
            Some("plain")
        );
        assert_eq!(
            parse_env_value(text, "CUSROR_API_KEY").as_deref(),
            Some("typo")
        );
        assert_eq!(parse_env_value(text, "MISSING"), None);
    }
}
