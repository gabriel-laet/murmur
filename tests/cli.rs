//! Integration tests that drive the real binary, std-only.
//! Each test gets its own store via MURMUR_DIR.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_murmur")
}

fn fresh_dir(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "murmur-test-{}-{}-{}",
        std::process::id(),
        tag,
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(".murmur")
}

fn murmur(store: &PathBuf, args: &[&str]) -> Output {
    Command::new(bin())
        .args(args)
        .env("MURMUR_DIR", store)
        .env_remove("MURMUR_AGENT")
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_PANE_ID")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

#[test]
fn secret_exec_injects_env_without_printing() {
    let store = fresh_dir("secret-exec");
    let out = Command::new(bin())
        .args([
            "secret",
            "exec",
            "INJECTED=secret://env/MURMUR_TEST_SRC",
            "--",
            "sh",
            "-c",
            "printf %s \"$INJECTED\"",
        ])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_TEST_SRC", "hunter2")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "hunter2");
}

#[test]
fn secret_exec_propagates_exit_codes() {
    let store = fresh_dir("secret-exit");
    let out = Command::new(bin())
        .args([
            "secret",
            "exec",
            "X=secret://env/MURMUR_TEST_SRC",
            "--",
            "sh",
            "-c",
            "exit 3",
        ])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_TEST_SRC", "v")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn secret_exec_fails_on_unknown_backend_and_missing_var() {
    let store = fresh_dir("secret-errors");
    let out = murmur(
        &store,
        &["secret", "exec", "K=secret://vault/x/KEY", "--", "true"],
    );
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown secret backend"));
    let out = Command::new(bin())
        .args([
            "secret",
            "exec",
            "K=secret://env/MURMUR_DEFINITELY_UNSET",
            "--",
            "true",
        ])
        .env("MURMUR_DIR", &store)
        .env_remove("MURMUR_DEFINITELY_UNSET")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

fn fake_bd(base: &std::path::Path, log: &std::path::Path) -> PathBuf {
    bd_stub(
        base,
        "bd-stub.sh",
        log,
        r#"  ready) echo '[{"id":"bd-a1b2","title":"Fix login flow","description":"Users bounce on refresh."}]' ;;
  show) echo '{"id":"bd-a1b2","title":"Fix login flow","description":"Users bounce on refresh.","status":"open"}' ;;
  create) echo '{"id":"bd-9f3c","title":"'"$2"'","status":"open"}' ;;
  *) echo '{}' ;;"#,
    )
}

#[test]
fn setup_is_idempotent_and_merges() {
    let dir = fresh_dir("setup");
    let workdir = dir.parent().unwrap().to_path_buf();
    // pre-existing settings survive the merge
    std::fs::create_dir_all(workdir.join(".claude")).unwrap();
    std::fs::write(
        workdir.join(".claude/settings.json"),
        r#"{"permissions":{"allow":["Bash(ls:*)"]},"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"my-other-hook"}]}]}}"#,
    )
    .unwrap();
    let home = workdir.join("fake-home");
    std::fs::create_dir_all(&home).unwrap();
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .current_dir(&workdir)
            .env("MURMUR_DIR", &dir)
            .env("HOME", &home)
            .env("PATH", "") // no harnesses detectable
            .output()
            .unwrap()
    };
    let out = run(&["setup"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(&["setup"]); // second run changes nothing
    assert!(stdout(&out).contains("already present"));

    // No hooks, no MCP, no per-harness config: pre-existing Claude Code
    // settings are untouched and the CLI is the protocol.
    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workdir.join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1, "murmur writes no hooks");
    assert!(!workdir.join(".mcp.json").exists());
    assert!(!workdir.join(".gemini/settings.json").exists());

    // AGENTS.md carries the universal contract, idempotently.
    let agents = std::fs::read_to_string(workdir.join("AGENTS.md")).unwrap();
    assert_eq!(agents.matches("murmur:begin").count(), 1);
    assert!(agents.contains("murmur tell"));

    // FLEET.md is seeded once and never clobbered.
    let fleet = std::fs::read_to_string(workdir.join("FLEET.md")).unwrap();
    assert!(fleet.contains("Fleet roster"), "{fleet}");
    std::fs::write(workdir.join("FLEET.md"), "# mine\n").unwrap();
    let out = run(&["setup"]);
    assert!(out.status.success());
    assert_eq!(
        std::fs::read_to_string(workdir.join("FLEET.md")).unwrap(),
        "# mine\n"
    );
}

#[test]
fn setup_all_appends_the_contract_and_wires_the_plugin() {
    let dir = fresh_dir("setup-all");
    let workdir = dir.parent().unwrap().to_path_buf();
    let home = workdir.join("fake-home");
    std::fs::create_dir_all(&home).unwrap();
    // Pre-existing AGENTS.md survives and gets appended to.
    std::fs::write(
        workdir.join("AGENTS.md"),
        "# My project\n\nBuild with make.\n",
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .current_dir(&workdir)
            .env("MURMUR_DIR", &dir)
            .env("HOME", &home)
            .env("PATH", "")
            .output()
            .unwrap()
    };
    let out = run(&["setup", "--all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out2 = run(&["setup", "--all"]); // idempotent
    assert!(out2.status.success());

    // The CLI is the protocol: no per-harness MCP configs are written.
    assert!(!workdir.join(".mcp.json").exists());
    assert!(!workdir.join(".gemini/settings.json").exists());
    assert!(!workdir.join(".grok/settings.json").exists());
    assert!(!workdir.join("opencode.json").exists());
    assert!(!home.join(".codex/config.toml").exists());

    let agents = std::fs::read_to_string(workdir.join("AGENTS.md")).unwrap();
    assert!(
        agents.starts_with("# My project"),
        "existing AGENTS.md kept"
    );
    assert_eq!(agents.matches("murmur:begin").count(), 1);

    let plugin =
        std::fs::read_to_string(home.join(".config/murmur/herdr-plugin/herdr-plugin.toml"))
            .unwrap();
    assert!(plugin.contains("id = \"murmur.herdr\""), "{}", plugin);
    assert!(plugin.contains("pane.agent_status_changed"), "{}", plugin);
    assert!(
        plugin.contains("\"herdr\""),
        "plugin invokes murmur herdr: {}",
        plugin
    );
}

fn fake_herdr(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-herdr.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn start_goal_becomes_a_bead_and_needs_herdr() {
    let store = fresh_dir("start-goal-bead");
    let base = store.parent().unwrap();
    let log = base.join("goal-bd.log");
    let stub = fake_bd(base, &log);
    // no herdr and no cloud kinds: start refuses — murmur is tied to herdr
    let out = Command::new(bin())
        .args(["start", "rewrite claim TTLs", "--kind", "grok"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("needs a running herdr"),
        "{}",
        stderr(&out)
    );
    // but the goal still got its durable home in beads first
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("create rewrite claim TTLs"), "{calls}");
}

#[test]
fn start_mixes_kinds_and_hands_the_lead_the_fleet_roster() {
    let store = fresh_dir("start-fleet");
    let base = store.parent().unwrap();
    let log = base.join("fleet-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("fleet-bd.log"));
    std::fs::write(
        base.join("FLEET.md"),
        "# Fleet roster\ncodex: tests and sweeps. claude: review.\n",
    )
    .unwrap();

    let out = Command::new(bin())
        .args(["start", "bd-a1b2", "--kind", "claude,codex=2"])
        .current_dir(base)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    let calls = std::fs::read_to_string(&log).unwrap();
    // the mix: one claude lead, two codex workers
    assert!(calls.contains("agent start lead --kind claude"), "{calls}");
    assert!(calls.contains("agent start w1 --kind codex"), "{calls}");
    assert!(calls.contains("agent start w2 --kind codex"), "{calls}");
    // the lead's brief carries the roster verbatim — and only the lead's
    assert_eq!(
        calls.matches("codex: tests and sweeps").count(),
        1,
        "roster goes to the lead only: {calls}"
    );
    // everyone knows what they are and what their peers are
    assert!(calls.contains("you are agent 'lead' (claude)"), "{calls}");
    assert!(calls.contains("you are agent 'w1' (codex)"), "{calls}");
    assert!(calls.contains("lead (claude)"), "{calls}");
    assert!(
        stdout(&out).contains("lead (claude), w1 (codex), w2 (codex)"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn start_orchestrates_a_herdr_herd() {
    let store = fresh_dir("start-herd");
    let base = store.parent().unwrap();
    let log = base.join("herdr-calls.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  "agent start") echo '{{"result":{{"agent":{{"name":"lead","pane_id":"w1:p1"}}}}}}' ;;
  "agent prompt") echo '{{"result":{{"agent":{{}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("herd-bd.log"));

    let out = Command::new(bin())
        .args(["start", "bd-a1b2", "--kind", "grok", "--workers", "2"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("workspace create"), "{calls}");
    assert!(calls.contains("pane split"), "{calls}");
    assert!(calls.contains("agent start"), "{calls}");
    assert!(calls.contains("agent prompt"), "{calls}");
    assert!(
        calls.contains("MURMUR_AGENT=lead") || calls.contains("lead"),
        "{calls}"
    );
    assert!(stdout(&out).contains("herd"), "{}", stdout(&out));
}

#[test]
fn start_worktree_isolates_agents_and_briefs_the_merge_queue() {
    let store = fresh_dir("start-wt");
    let base = store.parent().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, stderr(&out));
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "--allow-empty",
        "-m",
        "init",
        "-q",
    ]);

    let log = base.join("wt-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("wt-bd.log"));

    let out = Command::new(bin())
        .args([
            "start",
            "bd-a1b2",
            "--kind",
            "grok",
            "--workers",
            "2",
            "--worktree",
        ])
        .current_dir(&repo)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));

    // real worktrees on real branches, as siblings of the repo
    for name in ["lead", "w1"] {
        let dir = base.join(format!("repo--bd-a1b2-{name}"));
        assert!(dir.join(".git").exists(), "worktree missing for {name}");
    }
    let branches = Command::new("git")
        .args(["branch", "--list"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let branches = stdout(&branches);
    assert!(branches.contains("herd/bd-a1b2/lead"), "{branches}");
    assert!(branches.contains("herd/bd-a1b2/w1"), "{branches}");

    // panes get the worktree as cwd and the shared store pinned
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("repo--bd-a1b2-w1"), "{calls}");
    assert!(calls.contains("MURMUR_DIR="), "{calls}");
    // the briefs carry the discipline: workers isolate, lead owns merges
    assert!(
        calls.contains("your own git worktree on branch herd/bd-a1b2/w1"),
        "{calls}"
    );
    assert!(calls.contains("merge queue"), "{calls}");
    assert!(calls.contains("integration branch"), "{calls}");

    // re-running reuses the worktrees instead of erroring
    let out = Command::new(bin())
        .args([
            "start",
            "bd-a1b2",
            "--kind",
            "grok",
            "--workers",
            "2",
            "--worktree",
        ])
        .current_dir(&repo)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "rerun: {}", stderr(&out));
}

fn bd_stub(base: &std::path::Path, name: &str, log: &std::path::Path, cases: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = base.join(name);
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"{log}\"\ncase \"$1\" in\n{cases}\nesac\n",
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn tell_revives_a_done_agent_before_prompting() {
    let store = fresh_dir("poke-revive");
    let base = store.parent().unwrap();
    let log = base.join("poke-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"w1","agent":"claude","pane_id":"w1:p3","agent_status":"done"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args(["tell", "w1", "pick up the follow-ups"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("delivered to w1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    let start_at = calls.find("agent start w1 --kind claude --pane w1:p3");
    let prompt_at = calls.find("pick up the follow-ups");
    assert!(
        start_at.is_some(),
        "must restart the done pane first: {calls}"
    );
    assert!(prompt_at.is_some(), "{calls}");
    assert!(start_at < prompt_at, "revive before prompt: {calls}");
}

#[test]
fn start_joins_agents_and_writes_herd_snap() {
    let store = fresh_dir("start-snap");
    let base = store.parent().unwrap();
    let log = base.join("snap-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "workspace create") echo '{{"result":{{"workspace":{{"workspace_id":"w9"}},"root_pane":{{"pane_id":"w9:p0"}}}}}}' ;;
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"w9:p1"}}}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("snap-bd.log"));
    let out = Command::new(bin())
        .args(["start", "bd-a1b2", "--kind", "grok", "--workers", "2"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("stop   murmur stop"),
        "{}",
        stdout(&out)
    );
    let snap: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(store.join("herd.json")).unwrap()).unwrap();
    assert_eq!(snap["workspace_id"], "w9");
    assert_eq!(snap["agents"][0], "lead");
    assert_eq!(snap["agents"][1], "w1");
    // who is a view over herdr's live agents
    std::fs::create_dir_all(base.join("who-stub")).unwrap();
    let live = fake_herdr(
        &base.join("who-stub"),
        r#"#!/bin/sh
case "$1 $2" in
  "agent list") echo '{"result":{"agents":[{"name":"lead","agent":"grok","agent_status":"idle","pane_id":"w9:p1"},{"name":"w1","agent":"grok","agent_status":"working","pane_id":"w9:p2"}]}}' ;;
  *) echo '{"result":{}}' ;;
esac
"#,
    );
    let who = stdout(
        &Command::new(bin())
            .args(["who"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_HERDR", &live)
            .output()
            .unwrap(),
    );
    assert!(who.contains("lead"), "{who}");
    assert!(who.contains("w1"), "{who}");
}

#[test]
fn stop_closes_workspace_and_clears_the_snap() {
    let store = fresh_dir("stop-herd");
    let base = store.parent().unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("herd.json"),
        r#"{"workspace_id":"w9","label":"bd-a1b2","agents":["lead","w1"]}"#,
    )
    .unwrap();
    let log = base.join("stop-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "workspace close") echo '{{"result":{{}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args(["stop"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env_remove("HERDR_ENV")
        .env_remove("HERDR_WORKSPACE_ID")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("closed workspace w9"),
        "{}",
        stdout(&out)
    );
    assert!(!store.join("herd.json").exists());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("workspace close w9"), "{calls}");
}

#[test]
fn stop_refuses_to_close_the_current_workspace() {
    let store = fresh_dir("stop-self");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("herd.json"),
        r#"{"workspace_id":"w9","label":"bd-a1b2","agents":["lead"]}"#,
    )
    .unwrap();
    let out = Command::new(bin())
        .args(["stop"])
        .env("MURMUR_DIR", &store)
        .env("HERDR_WORKSPACE_ID", "w9")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("from inside"), "{}", stderr(&out));
    assert!(
        store.join("herd.json").exists(),
        "snap kept when stop aborts"
    );
}

#[test]
fn herdr_idle_wake_revives_a_done_pane_with_mail() {
    // "Done" is not "idle": the model finished its turn and stopped
    // listening. New mail for a done pane must restart the agent before
    // the prompt, or the nudge lands on a corpse.
    let store = fresh_dir("wake-revive");
    let base = store.parent().unwrap();
    // spool a tell while nobody is listening (prompt fails on this stub)
    std::fs::create_dir_all(base.join("dead-stub")).unwrap();
    let dead = fake_herdr(&base.join("dead-stub"), "#!/bin/sh\nexit 1\n");
    let out = Command::new(bin())
        .args(["tell", "w1", "please review", "--as", "lead"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &dead)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("spooled"), "{}", stdout(&out));
    let log = base.join("wake-revive-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"w1","agent":"claude","pane_id":"w1:p2","cwd":".","agent_status":"done"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let event = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"done"}}"#;
    let out = Command::new(bin())
        .args(["herdr"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p2")
        .env("HERDR_PLUGIN_EVENT_JSON", event)
        .env("HERDR_PLUGIN_STATE_DIR", base.join("wake-revive-state"))
        .env_remove("MURMUR_BEADS")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    let start_at = calls.find("agent start w1 --kind claude --pane w1:p2");
    let prompt_at = calls.find("agent prompt w1");
    assert!(start_at.is_some(), "done pane must be revived: {calls}");
    assert!(prompt_at.is_some(), "{calls}");
    assert!(start_at < prompt_at, "revive before prompt: {calls}");
}

#[test]
fn herdr_idle_wake_points_an_empty_spool_at_ready_beads_once() {
    let store = fresh_dir("wake-beads");
    std::fs::create_dir_all(&store).unwrap(); // the plugin is a no-op without a notebook
    let base = store.parent().unwrap();
    let log = base.join("wake-beads-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"lead","pane_id":"w1:p1","cwd":"."}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("wake-bd.log"));
    let event = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"idle"}}"#;
    let run = || {
        Command::new(bin())
            .args(["herdr"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_HERDR", &stub)
            .env("MURMUR_BEADS", &bd)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p1")
            .env("HERDR_PLUGIN_EVENT_JSON", event)
            .env("HERDR_PLUGIN_STATE_DIR", base.join("wake-beads-state"))
            .output()
            .unwrap()
    };
    // no mail at all — the nudge should offer the ready bead instead
    let out = run();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("agent prompt"), "{calls}");
    assert!(calls.contains("bd-a1b2"), "names the ready bead: {calls}");
    assert!(
        calls.contains("murmur assign"),
        "points at the assignment verb: {calls}"
    );
    // same ready set → no re-prompt
    let before = calls.matches("agent prompt").count();
    let out = run();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("agent prompt").count(),
        before,
        "must not re-nudge the same ready beads: {calls}"
    );
}

// ---- cloud kinds (temporary executor over curl) ----

fn fake_curl(dir: &std::path::Path, log: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-curl.sh");
    // Create responses nest the agent (like the live API); everything else
    // is exercised through the flat-id fallback via the GET endpoints.
    let script = format!(
        r#"#!/bin/sh
cat > /dev/null
printf '%s\n' "$*" >> "{log}"
case "$*" in
  *"-X POST"*"/v1/agents "*|*"-X POST"*"/v1/agents") echo '{{"agent":{{"id":"bc-test-1","status":"CREATING"}},"run":{{"id":"run-test-1"}}}}' ;;
  *) echo '{{"id":"bc-test-1"}}' ;;
esac
"#,
        log = log.display()
    );
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn git_repo_with_origin(dir: &std::path::Path) {
    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init", "-q"]);
    run(&["remote", "add", "origin", "git@github.com:acme/demo.git"]);
}

#[test]
fn start_all_cloud_launches_workers_via_curl() {
    let store = fresh_dir("cloud-only");
    let base = store.parent().unwrap().to_path_buf();
    git_repo_with_origin(&base);
    let log = base.join("cloud-curl.log");
    let curl = fake_curl(&base, &log);
    let out = Command::new(bin())
        .args(["start", "ship dark mode", "--kind", "cloud:cursor=2"])
        .current_dir(&base)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_CURL", &curl)
        .env("CURSOR_API_KEY", "test-key")
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .env_remove("MURMUR_CURSOR_MODEL")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("cloud  w1  bc-test-1"), "{s}");
    assert!(s.contains("cloud  w2  bc-test-1"), "{s}");
    assert!(s.contains("integration point"), "{s}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(calls.matches("-X POST").count(), 2, "{calls}");
    assert!(
        calls.contains("https://api.cursor.com/v1/agents"),
        "{calls}"
    );
    assert!(
        calls.contains("https://github.com/acme/demo"),
        "ssh remote normalized: {calls}"
    );
    assert!(
        calls.contains("ship dark mode"),
        "brief rides the prompt: {calls}"
    );
    assert!(
        !calls.contains("test-key"),
        "the API key must never be an argv: {calls}"
    );
}

#[test]
fn cloud_kind_cannot_lead_a_mixed_herd() {
    let store = fresh_dir("cloud-lead");
    let out = Command::new(bin())
        .args(["start", "fix auth", "--kind", "cloud:cursor,claude"])
        .env("MURMUR_DIR", &store)
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("can't lead"), "{}", stderr(&out));
}

#[test]
fn mixed_herd_mails_the_lead_each_cloud_launch() {
    let store = fresh_dir("cloud-mixed");
    let base = store.parent().unwrap().to_path_buf();
    git_repo_with_origin(&base);
    let herdr_log = base.join("mixed-herdr.log");
    let herdr = fake_herdr(
        &base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"w1:p1"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = herdr_log.display()
        ),
    );
    let curl_log = base.join("mixed-curl.log");
    let curl = fake_curl(&base, &curl_log);
    let out = Command::new(bin())
        .args(["start", "fix auth", "--kind", "claude,cloud:cursor"])
        .current_dir(&base)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &herdr)
        .env("MURMUR_CURL", &curl)
        .env("CURSOR_API_KEY", "test-key")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("pane   lead"), "{s}");
    assert!(s.contains("cloud  w1  bc-test-1"), "{s}");
    // the lead's brief explains how to reach cloud peers
    let herdr_calls = std::fs::read_to_string(&herdr_log).unwrap();
    assert!(herdr_calls.contains("Cloud peers"), "{herdr_calls}");
    // the launch note reaches the lead: prompted live, or spooled
    let prompted = herdr_calls.contains("bc-test-1");
    let spooled = store.join("spool").join("lead").is_dir()
        && std::fs::read_dir(store.join("spool").join("lead"))
            .unwrap()
            .count()
            > 0;
    assert!(prompted || spooled, "launch note lost: {herdr_calls}");
}

#[test]
fn cloud_status_and_prompt_hit_the_provider_endpoints() {
    let store = fresh_dir("cloud-cmds");
    let base = store.parent().unwrap().to_path_buf();
    let log = base.join("cmds-curl.log");
    let curl = fake_curl(&base, &log);
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .env("MURMUR_DIR", &store)
            .env("MURMUR_CURL", &curl)
            .env("CURSOR_API_KEY", "test-key")
            .output()
            .unwrap()
    };
    let out = run(&["cloud", "status", "bc-42"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = run(&["cloud", "prompt", "bc-42", "keep going"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("-X GET https://api.cursor.com/v1/agents/bc-42"),
        "{calls}"
    );
    assert!(
        calls.contains("-X POST https://api.cursor.com/v1/agents/bc-42/runs"),
        "{calls}"
    );
    assert!(calls.contains("keep going"), "{calls}");
}

#[test]
fn doctor_lints_the_roster_against_the_machine() {
    let store = fresh_dir("doctor");
    let base = store.parent().unwrap().to_path_buf();
    git_repo_with_origin(&base);
    std::fs::write(
        base.join("FLEET.md"),
        "# Fleet roster\n\n| kind | strong at |\n| --- | --- |\n| zz-noagent | nothing |\n| cloud:cursor | bulk |\n| cloud:nope | ? |\n",
    )
    .unwrap();
    let run = |envs: &[(&str, &str)]| {
        let mut cmd = Command::new(bin());
        cmd.args(["doctor"])
            .current_dir(&base)
            .env("MURMUR_DIR", &store)
            .env("HOME", &base)
            .env_remove("HERDR_ENV")
            .env_remove("MURMUR_HERDR")
            .env_remove("CURSOR_API_KEY")
            .env_remove("CUSROR_API_KEY")
            .env_remove("MURMUR_CURL");
        for (k, v) in envs {
            cmd.env(k, v);
        }
        cmd.output().unwrap()
    };
    // nothing configured: local kind missing, cursor lacks its key, nope unknown
    let out = run(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("zz-noagent"), "{s}");
    assert!(s.contains("CURSOR_API_KEY not set"), "{s}");
    assert!(s.contains("unknown cloud backend"), "{s}");
    // key + curl override + origin remote: cursor comes back ok
    let out = run(&[("CURSOR_API_KEY", "k"), ("MURMUR_CURL", "/bin/true")]);
    let s = stdout(&out);
    assert!(s.contains("ok    cloud:cursor"), "{s}");
    // config fine but the provider says no — the probe surfaces its message
    let errcurl = {
        use std::os::unix::fs::PermissionsExt;
        let p = base.join("curl-err.sh");
        std::fs::write(
            &p,
            "#!/bin/sh\necho '{\"error\":{\"message\":\"rate limit exceeded for this hour\"}}'\n",
        )
        .unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    };
    let out = run(&[
        ("CURSOR_API_KEY", "k"),
        ("MURMUR_CURL", errcurl.to_str().unwrap()),
    ]);
    let s = stdout(&out);
    assert!(s.contains("warn  cloud:cursor"), "{s}");
    assert!(s.contains("rate limit exceeded"), "{s}");
}

#[test]
fn doctor_reads_cursor_key_from_dot_secrets() {
    let store = fresh_dir("doctor-secrets");
    let base = store.parent().unwrap().to_path_buf();
    git_repo_with_origin(&base);
    std::fs::write(
        base.join("FLEET.md"),
        "# Fleet roster\n\n| kind | strong at |\n| --- | --- |\n| cloud:cursor | bulk |\n",
    )
    .unwrap();
    std::fs::write(base.join(".secrets"), "CURSOR_API_KEY=from-file\n").unwrap();
    let out = Command::new(bin())
        .args(["doctor"])
        .current_dir(&base)
        .env("MURMUR_DIR", &store)
        .env("HOME", &base)
        .env("MURMUR_CURL", "/bin/true")
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .env_remove("CURSOR_API_KEY")
        .env_remove("CUSROR_API_KEY")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("ok    cloud:cursor"), "{s}");
    assert!(
        !s.contains("from-file"),
        "the API key must never be printed: {s}"
    );
}

// ---- 0.7: targeted takes, scoped sync, boards, restack, plan ----

#[test]
fn start_board_gives_the_herd_its_own_bus() {
    let dir = fresh_dir("board-herd");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(bin())
        .args([
            "start",
            "fix the shelves",
            "--kind",
            "grok",
            "--board",
            "Oficina",
        ])
        .current_dir(&dir)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("MURMUR_DIR")
        .env_remove("MURMUR_BEADS")
        .env_remove("MURMUR_HERDR")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    // no herdr → start refuses, but the named notebook was already scoped
    assert!(!out.status.success());
    assert!(stdout(&out).contains(".murmur-oficina"), "{}", stdout(&out));
    let board = dir.join(".murmur-oficina");
    assert!(board.is_dir(), "board notebook must exist");
    assert!(
        !dir.join(".murmur").exists(),
        "the default notebook must stay untouched"
    );
}

#[test]
fn worktree_cmd_replaces_git_worktree_add() {
    let store = fresh_dir("wt-cmd");
    let base = store.parent().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, stderr(&out));
    };
    git(&["init", "-q"]);
    git(&[
        "-c",
        "user.email=t@t",
        "-c",
        "user.name=t",
        "commit",
        "--allow-empty",
        "-m",
        "init",
        "-q",
    ]);

    let log = base.join("wtcmd-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let cmd_log = base.join("wtcmd.log");
    let cmd = format!(
        "echo \"$MURMUR_WORKTREE_NAME $MURMUR_WORKTREE_BRANCH\" >> {} && \
         git worktree add \"$MURMUR_WORKTREE_DIR\" -b \"$MURMUR_WORKTREE_BRANCH\" -q",
        cmd_log.display()
    );
    let out = Command::new(bin())
        .args([
            "start",
            "shelve",
            "--kind",
            "grok",
            "--workers",
            "1",
            "--worktree",
            "--worktree-cmd",
            &cmd,
        ])
        .current_dir(&repo)
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env_remove("MURMUR_BEADS")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let ran = std::fs::read_to_string(&cmd_log).unwrap();
    assert!(
        ran.contains("lead herd/shelve/lead"),
        "helper got env: {ran}"
    );
    assert!(
        base.join("repo--shelve-lead").join(".git").exists(),
        "helper-built checkout exists where murmur expects it"
    );
}

fn restack_repo(base: &std::path::Path) -> std::path::PathBuf {
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(
                ["-c", "user.email=t@t", "-c", "user.name=t"]
                    .iter()
                    .chain(args.iter()),
            )
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, stderr(&out));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("base.txt"), "base\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "init"]);
    git(&["checkout", "-q", "-b", "herd/s/lead"]);
    // w1: its own file
    git(&["checkout", "-q", "-b", "herd/s/w1"]);
    std::fs::write(repo.join("w1.txt"), "one\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "w1 slice"]);
    // w2: its own file
    git(&["checkout", "-q", "herd/s/lead"]);
    git(&["checkout", "-q", "-b", "herd/s/w2"]);
    std::fs::write(repo.join("w2.txt"), "two\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "w2 slice"]);
    git(&["checkout", "-q", "herd/s/lead"]);
    repo
}

fn write_herd_snap(store: &Path, repo: &Path, agents: &[&str]) {
    let agents_json: Vec<String> = agents.iter().map(|a| format!("\"{a}\"")).collect();
    std::fs::create_dir_all(store).unwrap();
    std::fs::write(
        store.join("herd.json"),
        format!(
            r#"{{"workspace_id":"","label":"s","agents":[{}],"repo":"{}","worktrees":[],"slug":"s","hubs":[]}}"#,
            agents_json.join(","),
            repo.display()
        ),
    )
    .unwrap();
}

#[test]
fn restack_merges_worker_branches_into_the_integration_branch() {
    let store = fresh_dir("restack");
    let base = store.parent().unwrap();
    let repo = restack_repo(base);
    write_herd_snap(&store, &repo, &["lead", "w1", "w2"]);

    let out = Command::new(bin())
        .args(["restack"])
        .current_dir(&repo)
        .env("MURMUR_DIR", &store)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let s = stdout(&out);
    assert!(s.contains("merged herd/s/w1"), "{s}");
    assert!(s.contains("merged herd/s/w2"), "{s}");
    assert!(repo.join("w1.txt").exists() && repo.join("w2.txt").exists());
}

#[test]
fn restack_stops_on_conflict_with_the_facts_and_a_clean_tree() {
    let store = fresh_dir("restack-conflict");
    let base = store.parent().unwrap();
    let repo = restack_repo(base);
    // make w2 conflict with w1 on the same file
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(
                ["-c", "user.email=t@t", "-c", "user.name=t"]
                    .iter()
                    .chain(args.iter()),
            )
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, stderr(&out));
    };
    git(&["checkout", "-q", "herd/s/w1"]);
    std::fs::write(repo.join("hub.txt"), "from w1\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "w1 hub"]);
    git(&["checkout", "-q", "herd/s/w2"]);
    std::fs::write(repo.join("hub.txt"), "from w2\n").unwrap();
    git(&["add", "."]);
    git(&["commit", "-q", "-m", "w2 hub"]);
    git(&["checkout", "-q", "herd/s/lead"]);
    write_herd_snap(&store, &repo, &["lead", "w1", "w2"]);

    let out = Command::new(bin())
        .args(["restack"])
        .current_dir(&repo)
        .env("MURMUR_DIR", &store)
        .output()
        .unwrap();
    assert!(!out.status.success(), "conflict must stop the queue");
    assert!(stderr(&out).contains("hub.txt"), "{}", stderr(&out));
    // merge was aborted: tree is clean and w1's merge survived
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        stdout(&status).trim().is_empty(),
        "aborted merge must leave a clean tree"
    );
    assert!(repo.join("w1.txt").exists(), "first merge stands");
}

#[test]
fn plan_starts_a_single_planning_lead() {
    let store = fresh_dir("plan");
    let base = store.parent().unwrap();
    let log = base.join("plan-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"w1:p1"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let bd = fake_bd(base, &base.join("plan-bd.log"));
    let out = Command::new(bin())
        .args(["plan", "bd-a1b2", "--kind", "claude"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_BEADS", &bd)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_AGENT")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("agent start").count(),
        1,
        "plan is a herd of one: {calls}"
    );
    assert!(calls.contains("planning lead"), "{calls}");
    assert!(
        calls.contains("bd dep add"),
        "plan brief points at beads: {calls}"
    );
    assert!(
        calls.contains("murmur start --bead bd-a1b2"),
        "the lead summons its own workers: {calls}"
    );
}

#[test]
fn caller_led_start_spawns_workers_under_the_calling_agent() {
    let store = fresh_dir("caller-led");
    let base = store.parent().unwrap();
    let log = base.join("caller-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[{{"name":"oficina"}}]}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args(["start", "shelve wave", "--kind", "grok=2"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p9")
        .env("MURMUR_AGENT", "oficina")
        .env_remove("MURMUR_BEADS")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("workspace create"),
        "caller-led start splits the caller's workspace: {calls}"
    );
    assert_eq!(
        calls.matches("agent start").count(),
        2,
        "--kind grok=2 spawns exactly two workers: {calls}"
    );
    let s = stdout(&out);
    assert!(s.contains("lead   oficina"), "{s}");
    assert!(s.contains("You lead from this pane"), "{s}");
}

#[test]
fn fleet_records_starts_and_reports_usage() {
    let store = fresh_dir("fleet-usage");
    let base = store.parent().unwrap();
    let usage = base.join("usage.jsonl");
    let log = base.join("fleet-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args(["start", "measure me", "--kind", "grok", "--workers", "2"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_USAGE_FILE", &usage)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env_remove("MURMUR_BEADS")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = Command::new(bin())
        .args(["fleet"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_USAGE_FILE", &usage)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("grok 2"), "{}", stdout(&out));
}

#[test]
fn agents_md_contract_teaches_the_wave_verbs() {
    let dir = fresh_dir("agents-md");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(bin())
        .args(["setup"])
        .current_dir(&dir)
        .env("MURMUR_DIR", dir.join(".murmur"))
        .env("HOME", &dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let text = std::fs::read_to_string(dir.join("AGENTS.md")).unwrap();
    assert!(text.contains("Assignments arrive as prompts"), "{text}");
    assert!(text.contains("murmur tell"), "{text}");
    assert!(text.contains("murmur done <bead>"), "{text}");
    assert!(
        !text.contains("task take") && !text.contains("murmur inbox"),
        "the board-era recipes are gone: {text}"
    );
}

#[test]
fn worktrees_resolve_to_the_main_checkouts_store() {
    // parent of the (unused) store path — a plain unique dir
    let base = fresh_dir("wt-locate").parent().unwrap().to_path_buf();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let out = Command::new("git")
            .args(
                ["-c", "user.email=t@t", "-c", "user.name=t"]
                    .iter()
                    .chain(args.iter()),
            )
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {:?}: {}", args, stderr(&out));
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["commit", "-q", "--allow-empty", "-m", "init"]);
    git(&["worktree", "add", "-q", "../repo--wt", "-b", "wt"]);
    let wt = base.join("repo--wt");

    // tell from the worktree with NO MURMUR_DIR and no herdr: the spool
    // must land in the main checkout, never as a stray copy in the worktree
    std::fs::create_dir_all(base.join("wt-dead")).unwrap();
    let dead = fake_herdr(&base.join("wt-dead"), "#!/bin/sh\nexit 1\n");
    let out = Command::new(bin())
        .args(["tell", "wtworker", "hello", "--as", "mainside"])
        .current_dir(&wt)
        .env("MURMUR_HERDR", &dead)
        .env_remove("MURMUR_DIR")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        repo.join(".murmur/spool/wtworker").is_dir(),
        "notebook anchored to the repo"
    );
    assert!(
        !wt.join(".murmur").exists(),
        "no stray notebook in the worktree"
    );

    // a tell spooled from the main checkout is the same file the worktree
    // side sees — one notebook for the whole repo
    let out = Command::new(bin())
        .args(["tell", "wtworker", "one bus", "--as", "mainside"])
        .current_dir(&repo)
        .env("MURMUR_HERDR", &dead)
        .env_remove("MURMUR_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let spooled = std::fs::read_dir(repo.join(".murmur/spool/wtworker"))
        .unwrap()
        .count();
    assert_eq!(spooled, 2, "both tells share one spool");
}

#[test]
fn setup_seeds_role_playbook_skills_idempotently() {
    let dir = fresh_dir("skills");
    std::fs::create_dir_all(&dir).unwrap();
    let run = || {
        Command::new(bin())
            .args(["setup"])
            .current_dir(&dir)
            .env("MURMUR_DIR", dir.join(".murmur"))
            .env("HOME", &dir)
            .env("PATH", "")
            .output()
            .unwrap()
    };
    let out = run();
    assert!(out.status.success(), "{}", stderr(&out));
    let lead = dir.join(".claude/skills/murmur-lead/SKILL.md");
    let worker = dir.join(".claude/skills/murmur-worker/SKILL.md");
    let text = std::fs::read_to_string(&lead).unwrap();
    assert!(text.starts_with("---\n"), "frontmatter first: {text}");
    assert!(text.contains("murmur restack"), "{text}");
    assert!(std::fs::read_to_string(&worker)
        .unwrap()
        .contains("Close it, report anything surprising, and STOP"));
    let out = run();
    assert!(stdout(&out).contains("already present"), "{}", stdout(&out));

    // a human-edited skill (marker removed) is never rewritten
    std::fs::write(&lead, "---\nname: murmur-lead\n---\nmine now\n").unwrap();
    run();
    assert_eq!(
        std::fs::read_to_string(&lead).unwrap(),
        "---\nname: murmur-lead\n---\nmine now\n"
    );
}

#[test]
fn start_with_runs_a_service_pane_per_worker_with_slot_facts() {
    let store = fresh_dir("with-svc");
    let base = store.parent().unwrap();
    let log = base.join("svc-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split")
    n=$(grep -c "pane split" "{log}" || true)
    echo "{{\"result\":{{\"pane\":{{\"pane_id\":\"w1:p$n\"}}}}}}" ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args([
            "start",
            "serve wave",
            "--kind",
            "grok",
            "--workers",
            "2",
            "--with",
            "pnpm dev",
        ])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env_remove("MURMUR_BEADS")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("pane run").count(),
        2,
        "one service per worker: {calls}"
    );
    assert!(calls.contains("pnpm dev"), "{calls}");
    assert!(
        calls.contains("MURMUR_WORKTREE_SLOT=1") && calls.contains("MURMUR_WORKTREE_SLOT=2"),
        "slots are facts panes carry: {calls}"
    );
    assert!(
        calls.contains("A service pane beside yours runs `pnpm dev`"),
        "briefs name the service: {calls}"
    );
}

#[test]
fn briefs_are_durable_and_tell_redelivers_them() {
    // A login picker or trust dialog can eat the first brief even when the
    // pane reports interactive-ready (observed against real herdr 0.8.2).
    // The brief is stored at start; poke --brief re-delivers it.
    let store = fresh_dir("brief-redeliver");
    let base = store.parent().unwrap();
    let log = base.join("brief-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "status --json") echo '{{"server":{{"running":true}}}}' ;;
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"root_pane":{{"pane_id":"w1:p0"}}}}}}' ;;
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"w1:p1"}}}}}}' ;;
  "agent get") echo '{{"result":{{"agent":{{"name":"lead","agent":"grok","pane_id":"w1:p1","agent_status":"idle"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let out = Command::new(bin())
        .args(["start", "durable brief", "--kind", "grok", "--workers", "1"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .env("MURMUR_READY_TIMEOUT_MS", "1")
        .env_remove("MURMUR_BEADS")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let saved = std::fs::read_to_string(store.join("briefs").join("lead.txt")).unwrap();
    assert!(saved.contains("you are agent 'lead'"), "{saved}");

    let out = Command::new(bin())
        .args(["tell", "lead", "--brief"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("stored brief"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.matches("you are agent 'lead'").count() >= 2,
        "the same brief went out twice: {calls}"
    );

    // no message and no --brief is an error, not a silent no-op
    let out = Command::new(bin())
        .args(["tell", "lead"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("--brief"), "{}", stderr(&out));
}

#[test]
fn tell_spools_when_nobody_listens_and_the_wake_delivers_once() {
    let store = fresh_dir("tell-spool");
    let base = store.parent().unwrap();
    std::fs::create_dir_all(base.join("dead")).unwrap();
    let dead = fake_herdr(&base.join("dead"), "#!/bin/sh\nexit 1\n");
    let out = Command::new(bin())
        .args(["tell", "w1", "rebase onto main", "--as", "lead"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &dead)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("spooled for w1"), "{}", stdout(&out));
    assert_eq!(
        std::fs::read_dir(store.join("spool/w1")).unwrap().count(),
        1,
        "one spooled tell"
    );

    // the idle-wake drains the spool into one prompt and empties it
    let log = base.join("tell-wake.log");
    let live = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"w1","agent":"grok","pane_id":"w1:p2","cwd":".","agent_status":"idle"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let event = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p2","workspace_id":"w1","agent_status":"idle"}}"#;
    let wake = || {
        Command::new(bin())
            .args(["herdr"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_HERDR", &live)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p2")
            .env("HERDR_PLUGIN_EVENT_JSON", event)
            .env("HERDR_PLUGIN_STATE_DIR", base.join("tell-wake-state"))
            .env_remove("MURMUR_BEADS")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    };
    let out = wake();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("from lead: rebase onto main"), "{calls}");
    assert_eq!(
        std::fs::read_dir(store.join("spool/w1")).unwrap().count(),
        0,
        "spool drained"
    );
    let before = calls.matches("agent prompt").count();
    let out = wake();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("agent prompt").count(),
        before,
        "an empty spool must not re-prompt: {calls}"
    );
}

#[test]
fn assign_sets_the_bead_and_hands_the_slice() {
    let store = fresh_dir("assign");
    let base = store.parent().unwrap();
    let bd_log = base.join("assign-bd.log");
    let bd = fake_bd(base, &bd_log);
    let herdr_log = base.join("assign-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"w1","agent":"grok","pane_id":"w1:p2","agent_status":"idle"}}}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = herdr_log.display()
        ),
    );
    let out = Command::new(bin())
        .args([
            "assign",
            "bd-a1b2",
            "w1",
            "--as",
            "lead",
            "--note",
            "watch the flaky test",
        ])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &bd)
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("assigned bd-a1b2 to w1"),
        "{}",
        stdout(&out)
    );
    let bd_calls = std::fs::read_to_string(&bd_log).unwrap();
    assert!(
        bd_calls.contains("update bd-a1b2 --status in_progress --assignee w1 --json"),
        "the bead carries the assignment: {bd_calls}"
    );
    let herdr_calls = std::fs::read_to_string(&herdr_log).unwrap();
    assert!(herdr_calls.contains("[assigned] bd-a1b2"), "{herdr_calls}");
    assert!(
        herdr_calls.contains("watch the flaky test"),
        "the note rides along: {herdr_calls}"
    );
    assert!(
        herdr_calls.contains("murmur done bd-a1b2"),
        "the worker learns how to finish: {herdr_calls}"
    );
}

#[test]
fn assign_refuses_a_closed_bead() {
    let store = fresh_dir("assign-closed");
    let base = store.parent().unwrap();
    let log = base.join("assign-closed-bd.log");
    let bd = bd_stub(
        base,
        "bd-closed.sh",
        &log,
        r#"  show) echo '{"id":"bd-a1b2","title":"t","status":"closed"}' ;;
  *) echo '{}' ;;"#,
    );
    let out = Command::new(bin())
        .args(["assign", "bd-a1b2", "w1"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &bd)
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("already closed"), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(!calls.contains("update"), "no assignment pushed: {calls}");
}

#[test]
fn done_closes_with_attribution_and_tells_the_lead() {
    let store = fresh_dir("done");
    let base = store.parent().unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("herd.json"),
        r#"{"workspace_id":"w9","label":"wave","agents":["lead","w1"]}"#,
    )
    .unwrap();
    let bd_log = base.join("done-bd.log");
    let bd = fake_bd(base, &bd_log);
    std::fs::create_dir_all(base.join("done-dead")).unwrap();
    let dead = fake_herdr(&base.join("done-dead"), "#!/bin/sh\nexit 1\n");
    let out = Command::new(bin())
        .args([
            "done",
            "bd-a1b2",
            "--as",
            "w1",
            "--note",
            "shelves render live",
        ])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &bd)
        .env("MURMUR_HERDR", &dead)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let bd_calls = std::fs::read_to_string(&bd_log).unwrap();
    assert!(bd_calls.contains("close bd-a1b2 --reason"), "{bd_calls}");
    assert!(
        bd_calls.contains("closed by w1 via murmur"),
        "attribution rides the close: {bd_calls}"
    );
    // the lead hears it — spooled, since nobody is listening
    let spooled = std::fs::read_dir(store.join("spool/lead")).unwrap().count();
    assert_eq!(spooled, 1, "lead told about the close");
}

#[test]
fn drop_reopens_and_tells_the_lead() {
    let store = fresh_dir("drop");
    let base = store.parent().unwrap();
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(
        store.join("herd.json"),
        r#"{"workspace_id":"w9","label":"wave","agents":["lead","w1"]}"#,
    )
    .unwrap();
    let bd_log = base.join("drop-bd.log");
    let bd = fake_bd(base, &bd_log);
    std::fs::create_dir_all(base.join("drop-dead")).unwrap();
    let dead = fake_herdr(&base.join("drop-dead"), "#!/bin/sh\nexit 1\n");
    let out = Command::new(bin())
        .args(["drop", "bd-a1b2", "--as", "w1"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &bd)
        .env("MURMUR_HERDR", &dead)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let bd_calls = std::fs::read_to_string(&bd_log).unwrap();
    assert!(
        bd_calls.contains("update bd-a1b2 --status open"),
        "{bd_calls}"
    );
    assert_eq!(
        std::fs::read_dir(store.join("spool/lead")).unwrap().count(),
        1,
        "lead told to reassign"
    );
}
