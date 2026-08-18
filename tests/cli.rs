//! Integration tests that drive the real binary, std-only.
//! Each test gets its own store via MURMUR_DIR.

use std::path::PathBuf;
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
fn send_and_inbox_roundtrip() {
    let store = fresh_dir("roundtrip");
    let out = murmur(&store, &["send", "bob", "hello there", "--as", "alice"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("delivered to bob"));

    let out = murmur(&store, &["inbox", "--as", "bob"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("alice: hello there"));

    // consumed: second read is empty
    let out = murmur(&store, &["inbox", "--as", "bob"]);
    assert!(stdout(&out).is_empty());
}

#[test]
fn messages_wait_for_agents_that_never_joined() {
    let store = fresh_dir("offline");
    murmur(&store, &["send", "sleeper", "wake up", "--as", "alice"]);
    let out = murmur(&store, &["inbox", "--as", "sleeper"]);
    assert!(stdout(&out).contains("wake up"));
}

#[test]
fn broadcast_reaches_all_peers_but_not_sender() {
    let store = fresh_dir("broadcast");
    murmur(&store, &["join", "a"]);
    murmur(&store, &["join", "b"]);
    murmur(&store, &["join", "c"]);
    let out = murmur(&store, &["send", "*", "heads up", "--as", "a"]);
    assert!(out.status.success());
    assert!(stdout(&murmur(&store, &["inbox", "--as", "b"])).contains("heads up"));
    assert!(stdout(&murmur(&store, &["inbox", "--as", "c"])).contains("heads up"));
    assert!(stdout(&murmur(&store, &["inbox", "--as", "a"])).is_empty());
}

#[test]
fn broadcast_with_no_peers_fails() {
    let store = fresh_dir("broadcast-empty");
    let out = murmur(&store, &["send", "*", "anyone?", "--as", "a"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no other agents"));
}

#[test]
fn bad_agent_names_are_rejected() {
    let store = fresh_dir("names");
    let out = murmur(&store, &["send", "../escape", "x", "--as", "a"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("invalid agent name"));
    let out = murmur(&store, &["inbox", "--as", ".hidden"]);
    assert!(!out.status.success());
}

#[test]
fn claim_conflicts_are_denied_and_release_works() {
    let store = fresh_dir("claims");
    let out = murmur(&store, &["claim", "/repo/src/auth.rs", "--as", "alice"]);
    assert!(out.status.success());
    let out = murmur(&store, &["claim", "/repo/src/auth.rs", "--as", "bob"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("claimed by alice"));
    // re-claiming your own file is fine
    let out = murmur(&store, &["claim", "/repo/src/auth.rs", "--as", "alice"]);
    assert!(out.status.success());
    // bob cannot release alice's claim
    let out = murmur(&store, &["release", "/repo/src/auth.rs", "--as", "bob"]);
    assert!(!out.status.success());
    let out = murmur(&store, &["release", "/repo/src/auth.rs", "--as", "alice"]);
    assert!(out.status.success());
    let out = murmur(&store, &["claim", "/repo/src/auth.rs", "--as", "bob"]);
    assert!(out.status.success());
}

#[test]
fn task_lifecycle() {
    let store = fresh_dir("tasks");
    let out = murmur(&store, &["task", "add", "write tests", "--body", "unit + integration", "--as", "lead"]);
    assert!(out.status.success(), "{}", stderr(&out));
    murmur(&store, &["task", "add", "update docs", "--as", "lead"]);

    let out = murmur(&store, &["task", "list"]);
    assert!(stdout(&out).contains("write tests"));
    assert!(stdout(&out).contains("update docs"));

    // oldest first
    let out = murmur(&store, &["task", "take", "--as", "worker", "--json"]);
    assert!(out.status.success());
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(task["title"], "write tests");
    assert_eq!(task["taken_by"], "worker");
    let id = task["id"].as_str().unwrap().to_string();

    // only the holder can finish it
    let out = murmur(&store, &["task", "done", &id, "--as", "impostor"]);
    assert!(!out.status.success());
    let out = murmur(&store, &["task", "done", &id, "--as", "worker"]);
    assert!(out.status.success(), "{}", stderr(&out));

    // one task left, then the board is empty
    let out = murmur(&store, &["task", "take", "--as", "worker", "--json"]);
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(task["title"], "update docs");
    let out = murmur(&store, &["task", "take", "--as", "worker"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("no open tasks"));
}

#[test]
fn dropped_tasks_return_to_the_board() {
    let store = fresh_dir("task-drop");
    murmur(&store, &["task", "add", "flaky job", "--as", "lead"]);
    let out = murmur(&store, &["task", "take", "--as", "w1", "--json"]);
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    let id = task["id"].as_str().unwrap().to_string();
    let out = murmur(&store, &["task", "drop", &id, "--as", "w1"]);
    assert!(out.status.success());
    let out = murmur(&store, &["task", "take", "--as", "w2", "--json"]);
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(task["taken_by"], "w2");
}

#[test]
fn contested_take_has_exactly_one_winner() {
    let store = fresh_dir("task-race");
    murmur(&store, &["task", "add", "the one task", "--as", "lead"]);
    let handles: Vec<_> = (0..8)
        .map(|i| {
            let store = store.clone();
            std::thread::spawn(move || {
                let name = format!("racer{}", i);
                murmur(&store, &["task", "take", "--as", &name]).status.success()
            })
        })
        .collect();
    let wins = handles.into_iter().map(|h| h.join().unwrap()).filter(|w| *w).count();
    assert_eq!(wins, 1, "exactly one racer should win the task");
}

#[test]
fn ask_and_reply_flow() {
    let store = fresh_dir("reply");
    // asker blocks waiting for a reply
    let asker = {
        let store = store.clone();
        std::thread::spawn(move || {
            murmur(
                &store,
                &["send", "oracle", "is the schema final?", "--as", "asker", "--reply", "--timeout", "10"],
            )
        })
    };
    // oracle polls its inbox, finds the question, replies to its id
    let mut question: Option<serde_json::Value> = None;
    for _ in 0..100 {
        let out = murmur(&store, &["inbox", "--as", "oracle", "--json"]);
        let text = stdout(&out);
        if !text.trim().is_empty() {
            question = Some(serde_json::from_str(text.lines().next().unwrap()).unwrap());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let question = question.expect("oracle never received the question");
    assert_eq!(question["wants_reply"], true);
    let id = question["id"].as_str().unwrap();
    let out = murmur(&store, &["send", "asker", "yes, ship it", "--as", "oracle", "--reply-to", id]);
    assert!(out.status.success());

    let out = asker.join().unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out).trim(), "yes, ship it");
}

#[test]
fn inbox_wait_blocks_until_delivery() {
    let store = fresh_dir("wait");
    let receiver = {
        let store = store.clone();
        std::thread::spawn(move || {
            murmur(&store, &["inbox", "--as", "r", "--wait", "--timeout", "10"])
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    murmur(&store, &["send", "r", "finally", "--as", "s"]);
    let out = receiver.join().unwrap();
    assert!(stdout(&out).contains("finally"));
}

#[test]
fn secret_exec_injects_env_without_printing() {
    let store = fresh_dir("secret-exec");
    let out = Command::new(bin())
        .args([
            "secret", "exec", "INJECTED=secret://env/MURMUR_TEST_SRC",
            "--", "sh", "-c", "printf %s \"$INJECTED\"",
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
        .args(["secret", "exec", "X=secret://env/MURMUR_TEST_SRC", "--", "sh", "-c", "exit 3"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_TEST_SRC", "v")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn secret_resolve_fails_on_unknown_backend_and_missing_var() {
    let store = fresh_dir("secret-errors");
    let out = murmur(&store, &["secret", "resolve", "secret://vault/x/KEY"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("unknown secret backend"));
    let out = Command::new(bin())
        .args(["secret", "resolve", "secret://env/MURMUR_DEFINITELY_UNSET"])
        .env("MURMUR_DIR", &store)
        .env_remove("MURMUR_DEFINITELY_UNSET")
        .output()
        .unwrap();
    assert!(!out.status.success());
}

#[test]
fn inbox_flags_secret_references_without_resolving() {
    let store = fresh_dir("secret-inbox");
    murmur(
        &store,
        &["send", "bob", "db creds: secret://infisical/proj/dev/DATABASE_URL", "--as", "alice"],
    );
    let out = Command::new(bin())
        .args(["inbox", "--as", "bob"])
        .env("MURMUR_DIR", &store)
        .env("DATABASE_URL", "must-not-appear")
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(text.contains("secret://infisical/proj/dev/DATABASE_URL"), "ref itself is delivered");
    assert!(text.contains("secret reference"), "annotation present");
    assert!(text.contains("murmur secret exec"), "points at the safe path");
    assert!(!text.contains("must-not-appear"), "value was never resolved");
}

fn fake_bd(base: &std::path::Path, log: &std::path::Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let stub = base.join("bd-stub.sh");
    std::fs::write(
        &stub,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  ready) echo '[{{"id":"bd-a1b2","title":"Fix login flow","description":"Users bounce on refresh."}}]' ;;
  show) echo '{{"id":"bd-a1b2","title":"Fix login flow","description":"Users bounce on refresh.","status":"open"}}' ;;
  create) echo '{{"id":"bd-9f3c","title":"'"$2"'","status":"open"}}' ;;
  *) echo '{{}}' ;;
esac
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();
    stub
}

#[test]
fn beads_sync_pulls_pushes_and_stays_idempotent() {
    let store = fresh_dir("beads");
    let base = store.parent().unwrap();
    let log = base.join("bd-calls.log");
    let stub = fake_bd(base, &log);

    let sync = || {
        Command::new(bin())
            .args(["task", "sync", "beads"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_BEADS", &stub)
            .output()
            .unwrap()
    };

    // pull: the ready bead lands on the board once, keeping its own id
    let out = sync();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("pulled 1"));
    let out = sync();
    assert!(stdout(&out).contains("pulled 0"), "re-sync must not duplicate");
    let out = murmur(&store, &["task", "list"]);
    assert!(stdout(&out).contains("bd-a1b2"));
    assert!(stdout(&out).contains("Fix login flow"));

    // take, then sync pushes in_progress with the agent as assignee, once
    murmur(&store, &["task", "take", "--as", "worker-1"]);
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("update bd-a1b2 --status in_progress"), "{calls}");
    assert!(calls.contains("--assignee worker-1"), "assignee attributes the agent: {calls}");
    let out = sync();
    assert!(stdout(&out).contains("pushed 0"), "push is idempotent");

    // done flows through as a close, with attribution
    murmur(&store, &["task", "done", "bd-a1b2", "--as", "worker-1"]);
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("close bd-a1b2"), "closed in beads: {calls}");
    assert!(calls.contains("worker-1 via murmur"), "{calls}");
}

fn sync(from: &PathBuf, to: &std::path::Path) -> Output {
    murmur(from, &["sync", &to.display().to_string()])
}

#[test]
fn sync_delivers_messages_across_stores() {
    let a = fresh_dir("sync-a");
    let b = fresh_dir("sync-b");
    murmur(&a, &["send", "bob", "hello across nodes", "--as", "alice"]);
    let out = sync(&a, &b);
    assert!(out.status.success(), "{}", stderr(&out));
    let out = murmur(&b, &["inbox", "--as", "bob"]);
    assert!(stdout(&out).contains("alice: hello across nodes"));
    // the merged log is visible on b too
    let out = murmur(&b, &["log", "-n", "10"]);
    assert!(stdout(&out).contains("hello across nodes"));
}

#[test]
fn consumed_messages_never_resurrect() {
    let a = fresh_dir("tomb-a");
    let b = fresh_dir("tomb-b");
    murmur(&a, &["send", "bob", "read me once", "--as", "alice"]);
    sync(&a, &b);
    // bob reads on b — this writes a tombstone into b's log
    let out = murmur(&b, &["inbox", "--as", "bob"]);
    assert!(stdout(&out).contains("read me once"));
    // tombstone flows back to a and kills a's materialized copy
    sync(&a, &b);
    assert!(stdout(&murmur(&a, &["inbox", "--as", "bob"])).is_empty(), "consumed on b, gone on a");
    // and a third sync doesn't bring it back anywhere
    sync(&a, &b);
    assert!(stdout(&murmur(&a, &["inbox", "--as", "bob"])).is_empty());
    assert!(stdout(&murmur(&b, &["inbox", "--as", "bob"])).is_empty());
}

#[test]
fn sync_relays_through_intermediate_nodes() {
    let a = fresh_dir("relay-a");
    let b = fresh_dir("relay-b");
    let c = fresh_dir("relay-c");
    murmur(&a, &["send", "carol", "via b", "--as", "alice"]);
    sync(&a, &b);
    sync(&b, &c); // c never talks to a
    let out = murmur(&c, &["inbox", "--as", "carol"]);
    assert!(stdout(&out).contains("alice: via b"), "entry relayed a→b→c: {}", stdout(&out));
}

#[test]
fn sync_merges_presence_and_claims() {
    let a = fresh_dir("state-a");
    let b = fresh_dir("state-b");
    murmur(&a, &["join", "alice"]);
    murmur(&a, &["claim", "/repo/src/core.rs", "--as", "alice"]);
    sync(&a, &b);
    let out = murmur(&b, &["who"]);
    assert!(stdout(&out).contains("alice"));
    assert!(stdout(&out).contains("remote"), "alice shows as remote on b: {}", stdout(&out));
    // claim crossed too: bob is denied on b
    let out = murmur(&b, &["claim", "/repo/src/core.rs", "--as", "bob"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("claimed by alice"));
}

#[test]
fn contested_task_across_nodes_resolves_deterministically() {
    let a = fresh_dir("conflict-a");
    let b = fresh_dir("conflict-b");
    murmur(&a, &["task", "add", "the contested one", "--as", "lead"]);
    sync(&a, &b);
    // partition: both sides take it
    murmur(&a, &["task", "take", "--as", "zed"]);
    murmur(&b, &["task", "take", "--as", "alpha"]);
    sync(&a, &b);
    // smaller holder name wins on BOTH sides
    for store in [&a, &b] {
        let out = murmur(store, &["task", "list", "--json"]);
        let tasks: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
        assert_eq!(tasks[0]["taken_by"], "alpha", "on {}: {}", store.display(), stdout(&out));
        assert_eq!(tasks[0]["state"], "doing");
    }
    // the loser's done fails visibly; the winner's flows through
    let out = murmur(&a, &["task", "list", "--json"]);
    let id = serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap()[0]["id"]
        .as_str().unwrap().to_string();
    let out = murmur(&a, &["task", "done", &id, "--as", "zed"]);
    assert!(!out.status.success());
    let out = murmur(&b, &["task", "done", &id, "--as", "alpha"]);
    assert!(out.status.success(), "{}", stderr(&out));
    sync(&a, &b);
    let out = murmur(&a, &["task", "list", "--all", "--json"]);
    let tasks: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(tasks[0]["state"], "done", "completion syncs back: {}", stdout(&out));
}

#[test]
fn sync_over_stdio_pipes_like_ssh() {
    use std::os::unix::fs::PermissionsExt;
    let a = fresh_dir("ssh-a");
    let b = fresh_dir("ssh-b");
    // stand-in for ssh: drop the host arg, run the murmur binary directly
    let stub = a.parent().unwrap().join("fake-ssh.sh");
    std::fs::write(
        &stub,
        format!("#!/bin/sh\nshift\nshift\nexec \"{}\" \"$@\"\n", bin()),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    murmur(&a, &["send", "bob", "over the wire", "--as", "alice"]);
    let out = Command::new(bin())
        .args(["sync", &format!("fakehost:{}", b.display())])
        .env("MURMUR_DIR", &a)
        .env("MURMUR_SYNC_SSH", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("sent 1 entries"), "{}", stdout(&out));
    let out = murmur(&b, &["inbox", "--as", "bob"]);
    assert!(stdout(&out).contains("alice: over the wire"));
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

    let settings: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(workdir.join(".claude/settings.json")).unwrap()).unwrap();
    assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "existing hook kept, murmur hook appended");
    assert!(settings["hooks"]["Stop"].is_array());
    let mcp: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(workdir.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(mcp["mcpServers"]["murmur"]["command"], "murmur");

    // Undetected harnesses are reported, not wired.
    assert!(stdout(&out).contains("not found:"), "{}", stdout(&out));
    assert!(!workdir.join(".gemini/settings.json").exists());

    // AGENTS.md carries the universal contract, idempotently.
    let agents = std::fs::read_to_string(workdir.join("AGENTS.md")).unwrap();
    assert_eq!(agents.matches("murmur:begin").count(), 1);
    assert!(agents.contains("murmur inbox"));

    // FLEET.md is seeded once and never clobbered.
    let fleet = std::fs::read_to_string(workdir.join("FLEET.md")).unwrap();
    assert!(fleet.contains("Fleet roster"), "{fleet}");
    std::fs::write(workdir.join("FLEET.md"), "# mine\n").unwrap();
    let out = run(&["setup"]);
    assert!(out.status.success());
    assert_eq!(std::fs::read_to_string(workdir.join("FLEET.md")).unwrap(), "# mine\n");
}

#[test]
fn setup_all_wires_every_harness() {
    let dir = fresh_dir("setup-all");
    let workdir = dir.parent().unwrap().to_path_buf();
    let home = workdir.join("fake-home");
    std::fs::create_dir_all(&home).unwrap();
    // Pre-existing AGENTS.md and codex config survive and get appended to.
    std::fs::write(workdir.join("AGENTS.md"), "# My project\n\nBuild with make.\n").unwrap();
    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/config.toml"), "model = \"o4\"\n").unwrap();
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

    let codex = std::fs::read_to_string(home.join(".codex/config.toml")).unwrap();
    assert!(codex.starts_with("model = \"o4\""), "existing config kept: {}", codex);
    assert_eq!(codex.matches("[mcp_servers.murmur]").count(), 1);
    assert!(codex.contains("MURMUR_HARNESS"));

    for (path, key) in [
        (".gemini/settings.json", "mcpServers"),
        (".grok/settings.json", "mcpServers"),
        ("opencode.json", "mcp"),
    ] {
        let v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(workdir.join(path)).unwrap()).unwrap();
        assert!(v[key]["murmur"].is_object(), "{} missing murmur entry", path);
    }

    let agents = std::fs::read_to_string(workdir.join("AGENTS.md")).unwrap();
    assert!(agents.starts_with("# My project"), "existing AGENTS.md kept");
    assert_eq!(agents.matches("murmur:begin").count(), 1);

    let plugin = std::fs::read_to_string(home.join(".config/murmur/herdr-plugin/herdr-plugin.toml")).unwrap();
    assert!(plugin.contains("id = \"murmur.herdr\""), "{}", plugin);
    assert!(plugin.contains("pane.agent_status_changed"), "{}", plugin);
    assert!(plugin.contains("\"herdr\""), "plugin invokes murmur herdr: {}", plugin);
}

fn fake_herdr(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.join("fake-herdr.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn identity_uses_herdr_agent_name_inside_herdr() {
    let store = fresh_dir("herdr-id");
    let base = store.parent().unwrap();
    let stub = fake_herdr(
        base,
        r#"#!/bin/sh
case "$1 $2" in
  "agent get")
    echo '{"result":{"agent":{"name":"backend","pane_id":"w1:p2","cwd":"."}}}' ;;
  *) echo '{"result":{}}' ;;
esac
"#,
    );
    let out = Command::new(bin())
        .args(["join"])
        .env("MURMUR_DIR", &store)
        .env_remove("MURMUR_AGENT")
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p2")
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("joined as 'backend'"), "{}", stdout(&out));
}

#[test]
fn identity_prefers_murmur_agent_over_herdr() {
    let store = fresh_dir("herdr-id-override");
    let base = store.parent().unwrap();
    let stub = fake_herdr(
        base,
        r#"#!/bin/sh
echo '{"result":{"agent":{"name":"backend","pane_id":"w1:p2"}}}'
"#,
    );
    let out = Command::new(bin())
        .args(["join"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_AGENT", "frontend")
        .env("HERDR_ENV", "1")
        .env("HERDR_PANE_ID", "w1:p2")
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("joined as 'frontend'"), "{}", stdout(&out));
}

#[test]
fn start_no_herdr_puts_goal_on_the_board() {
    let store = fresh_dir("start-board");
    let out = Command::new(bin())
        .args(["start", "rewrite claim TTLs", "--no-herdr"])
        .env("MURMUR_DIR", &store)
        .env_remove("MURMUR_AGENT")
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("rewrite claim TTLs"), "{}", stdout(&out));
    let list = murmur(&store, &["task", "list"]);
    assert!(stdout(&list).contains("rewrite claim TTLs"));
}

#[test]
fn start_pulls_a_bead_without_herdr() {
    let store = fresh_dir("start-bead");
    let base = store.parent().unwrap();
    let log = base.join("start-bd.log");
    let stub = fake_bd(base, &log);
    let out = Command::new(bin())
        .args(["start", "bd-a1b2", "--no-herdr"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("bd-a1b2"), "{}", stdout(&out));
    assert!(stdout(&out).contains("Fix login flow"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("show bd-a1b2"), "{calls}");
    let list = murmur(&store, &["task", "list"]);
    assert!(stdout(&list).contains("bd-a1b2"));
}

#[test]
fn start_goal_becomes_a_bead_when_beads_is_around() {
    let store = fresh_dir("start-goal-bead");
    let base = store.parent().unwrap();
    let log = base.join("goal-bd.log");
    let stub = fake_bd(base, &log);
    let out = Command::new(bin())
        .args(["start", "rewrite claim TTLs", "--no-herdr"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .env_remove("HERDR_ENV")
        .env_remove("MURMUR_HERDR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("bd-9f3c"), "goal got a durable bead id: {}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("create rewrite claim TTLs"), "{calls}");
    let list = murmur(&store, &["task", "list"]);
    assert!(stdout(&list).contains("bd-9f3c"));
    assert!(stdout(&list).contains("rewrite claim TTLs"));
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
    assert!(stdout(&out).contains("lead (claude), w1 (codex), w2 (codex)"), "{}", stdout(&out));
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
    assert!(calls.contains("MURMUR_AGENT=lead") || calls.contains("lead"), "{calls}");
    assert!(stdout(&out).contains("herd"), "{}", stdout(&out));
}

#[test]
fn herdr_idle_wake_prompts_once_per_message() {
    let store = fresh_dir("wake");
    murmur(&store, &["send", "lead", "please review", "--as", "alice"]);
    let base = store.parent().unwrap();
    let log = base.join("wake-herdr.log");
    let stub = fake_herdr(
        base,
        &format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1 $2" in
  "agent get") echo '{{"result":{{"agent":{{"name":"lead","pane_id":"w1:p1","cwd":"."}}}}}}' ;;
  "agent prompt") echo '{{"result":{{}}}}' ;;
  "notification show") echo '{{"result":{{}}}}' ;;
  "pane report-metadata") echo '{{"result":{{}}}}' ;;
  *) echo '{{"result":{{}}}}' ;;
esac
"#,
            log = log.display()
        ),
    );
    let event = r#"{"event":"pane.agent_status_changed","data":{"type":"pane_agent_status_changed","pane_id":"w1:p1","workspace_id":"w1","agent_status":"idle"}}"#;
    let run = || {
        Command::new(bin())
            .args(["herdr"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_HERDR", &stub)
            .env("HERDR_ENV", "1")
            .env("HERDR_PANE_ID", "w1:p1")
            .env("HERDR_PLUGIN_EVENT_JSON", event)
            .env("HERDR_PLUGIN_STATE_DIR", base.join("wake-state"))
            .env_remove("MURMUR_BEADS")
            .output()
            .unwrap()
    };
    let out = run();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("agent prompt"), "{calls}");
    assert!(calls.contains("please review") || calls.contains("unread"), "{calls}");
    let before = calls.matches("agent prompt").count();
    let out = run();
    assert!(out.status.success(), "{}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.matches("agent prompt").count(),
        before,
        "must not re-prompt the same mail: {calls}"
    );
}

#[test]
fn herdr_idle_wake_points_an_empty_inbox_at_ready_beads_once() {
    let store = fresh_dir("wake-beads");
    murmur(&store, &["join", "lead"]); // the plugin is a no-op without a store
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
    assert!(calls.contains("task sync beads"), "points at the pull path: {calls}");
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
