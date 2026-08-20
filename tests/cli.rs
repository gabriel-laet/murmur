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
    let out = murmur(
        &store,
        &[
            "task",
            "add",
            "write tests",
            "--body",
            "unit + integration",
            "--as",
            "lead",
        ],
    );
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
                murmur(&store, &["task", "take", "--as", &name])
                    .status
                    .success()
            })
        })
        .collect();
    let wins = handles
        .into_iter()
        .map(|h| h.join().unwrap())
        .filter(|w| *w)
        .count();
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
                &[
                    "send",
                    "oracle",
                    "is the schema final?",
                    "--as",
                    "asker",
                    "--reply",
                    "--timeout",
                    "10",
                ],
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
    let out = murmur(
        &store,
        &[
            "send",
            "asker",
            "yes, ship it",
            "--as",
            "oracle",
            "--reply-to",
            id,
        ],
    );
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

#[test]
fn inbox_flags_secret_references_without_resolving() {
    let store = fresh_dir("secret-inbox");
    murmur(
        &store,
        &[
            "send",
            "bob",
            "db creds: secret://infisical/proj/dev/DATABASE_URL",
            "--as",
            "alice",
        ],
    );
    let out = Command::new(bin())
        .args(["inbox", "--as", "bob"])
        .env("MURMUR_DIR", &store)
        .env("DATABASE_URL", "must-not-appear")
        .output()
        .unwrap();
    let text = stdout(&out);
    assert!(
        text.contains("secret://infisical/proj/dev/DATABASE_URL"),
        "ref itself is delivered"
    );
    assert!(text.contains("secret reference"), "annotation present");
    assert!(
        text.contains("murmur secret exec"),
        "points at the safe path"
    );
    assert!(
        !text.contains("must-not-appear"),
        "value was never resolved"
    );
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
    assert!(
        stdout(&out).contains("pulled 0"),
        "re-sync must not duplicate"
    );
    let out = murmur(&store, &["task", "list"]);
    assert!(stdout(&out).contains("bd-a1b2"));
    assert!(stdout(&out).contains("Fix login flow"));

    // take, then sync pushes in_progress with the agent as assignee, once
    murmur(&store, &["task", "take", "--as", "worker-1"]);
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("update bd-a1b2 --status in_progress"),
        "{calls}"
    );
    assert!(
        calls.contains("--assignee worker-1"),
        "assignee attributes the agent: {calls}"
    );
    let out = sync();
    assert!(stdout(&out).contains("pushed 0"), "push is idempotent");

    // done flows through as a close, with attribution
    murmur(&store, &["task", "done", "bd-a1b2", "--as", "worker-1"]);
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        calls.contains("close bd-a1b2 --reason"),
        "close reason is a flag, not a positional id: {calls}"
    );
    assert!(calls.contains("worker-1 via murmur"), "{calls}");
}

fn sync(from: &PathBuf, to: &std::path::Path) -> Output {
    murmur(from, &["sync", &to.display().to_string()])
}

#[test]
fn peers_auto_sync_converges_mailboxes_without_manual_sync() {
    let a = fresh_dir("peers-a");
    let b = fresh_dir("peers-b");
    murmur(&a, &["join", "alice"]); // create a's store so the peer list has a home
    std::fs::write(a.join("peers"), format!("# my fleet\n{}\n", b.display())).unwrap();

    // send from a pushes to the peer immediately — bob reads on b, no sync run
    murmur(
        &a,
        &["send", "bob", "ping across machines", "--as", "alice"],
    );
    let out = murmur(&b, &["inbox", "--as", "bob"]);
    assert!(
        stdout(&out).contains("alice: ping across machines"),
        "{}",
        stdout(&out)
    );

    // bob replies on b, which lists no peers (one-way reachability);
    // alice's next inbox pulls it, because every sync is a two-way exchange
    murmur(&b, &["send", "alice", "pong back", "--as", "bob"]);
    let out = Command::new(bin())
        .args(["inbox", "--as", "alice"])
        .env("MURMUR_DIR", &a)
        .env("MURMUR_SYNC_INTERVAL", "0")
        .env_remove("MURMUR_AGENT")
        .output()
        .unwrap();
    assert!(stdout(&out).contains("bob: pong back"), "{}", stdout(&out));
}

#[test]
fn sync_without_target_walks_the_peer_list() {
    let a = fresh_dir("peers-cmd-a");
    let b = fresh_dir("peers-cmd-b");
    murmur(&a, &["join", "alice"]);

    // no peers listed → a helpful error, not a crash
    let out = murmur(&a, &["sync"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("peers"), "{}", stderr(&out));

    std::fs::write(a.join("peers"), format!("{}\n", b.display())).unwrap();
    murmur(&a, &["send", "bob", "queued for the peer", "--as", "alice"]);
    let out = murmur(&a, &["sync"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("synced with"), "{}", stdout(&out));
    let out = murmur(&b, &["inbox", "--as", "bob"]);
    assert!(
        stdout(&out).contains("queued for the peer"),
        "{}",
        stdout(&out)
    );
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
    assert!(
        stdout(&murmur(&a, &["inbox", "--as", "bob"])).is_empty(),
        "consumed on b, gone on a"
    );
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
    assert!(
        stdout(&out).contains("alice: via b"),
        "entry relayed a→b→c: {}",
        stdout(&out)
    );
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
    assert!(
        stdout(&out).contains("remote"),
        "alice shows as remote on b: {}",
        stdout(&out)
    );
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
        assert_eq!(
            tasks[0]["taken_by"],
            "alpha",
            "on {}: {}",
            store.display(),
            stdout(&out)
        );
        assert_eq!(tasks[0]["state"], "doing");
    }
    // the loser's done fails visibly; the winner's flows through
    let out = murmur(&a, &["task", "list", "--json"]);
    let id = serde_json::from_str::<serde_json::Value>(&stdout(&out)).unwrap()[0]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let out = murmur(&a, &["task", "done", &id, "--as", "zed"]);
    assert!(!out.status.success());
    let out = murmur(&b, &["task", "done", &id, "--as", "alpha"]);
    assert!(out.status.success(), "{}", stderr(&out));
    sync(&a, &b);
    let out = murmur(&a, &["task", "list", "--all", "--json"]);
    let tasks: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        tasks[0]["state"],
        "done",
        "completion syncs back: {}",
        stdout(&out)
    );
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

    let settings: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(workdir.join(".claude/settings.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
    let pre = settings["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 2, "existing hook kept, murmur hook appended");
    assert!(settings["hooks"]["Stop"].is_array());
    // No MCP, no per-harness config: the CLI is the protocol.
    assert!(!workdir.join(".mcp.json").exists());
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
    assert!(
        stdout(&out).contains("joined as 'backend'"),
        "{}",
        stdout(&out)
    );
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
    assert!(
        stdout(&out).contains("joined as 'frontend'"),
        "{}",
        stdout(&out)
    );
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
    assert!(
        stdout(&out).contains("rewrite claim TTLs"),
        "{}",
        stdout(&out)
    );
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
    assert!(
        stdout(&out).contains("bd-9f3c"),
        "goal got a durable bead id: {}",
        stdout(&out)
    );
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

fn write_task(store: &Path, state: &str, id: &str, title: &str) {
    let dir = store.join("tasks").join(state);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","title":"{title}","body":"","by":"beads","ts":1,"external_id":"{id}"}}"#
        ),
    )
    .unwrap();
}

#[test]
fn task_take_skips_umbrella_when_children_are_open() {
    let store = fresh_dir("umbrella");
    murmur(&store, &["join", "lead"]);
    write_task(&store, "todo", "bd-1", "epic");
    write_task(&store, "todo", "bd-1.2", "leaf");
    let out = murmur(&store, &["task", "take", "--as", "w1", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(task["id"], "bd-1.2", "leaf, not the epic: {}", stdout(&out));

    // child now doing — parent still skipped, and the refusal says why
    let out = murmur(&store, &["task", "take", "--as", "w2"]);
    assert!(!out.status.success(), "parent must stay: {}", stdout(&out));
    assert!(
        stderr(&out).contains("no open leaf tasks"),
        "{}",
        stderr(&out)
    );
}

/// A board task with a holder and a synced state, as sync would leave it.
fn write_held_task(store: &Path, state: &str, id: &str, holder: &str, synced: &str) {
    let dir = store.join("tasks").join(state);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.json")),
        format!(
            r#"{{"id":"{id}","title":"t","body":"","by":"beads","ts":1,"taken_by":"{holder}","external_id":"{id}","synced_state":"{synced}"}}"#
        ),
    )
    .unwrap();
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
fn take_refuses_the_epic_when_beads_holds_the_children() {
    // The parent is the ONLY file on the board — the dotted-id heuristic
    // can't see the children, but beads can: take must refuse, not swallow
    // the epic.
    let store = fresh_dir("veto-epic");
    let base = store.parent().unwrap();
    let log = base.join("veto-bd.log");
    let stub = bd_stub(
        base,
        "bd-veto.sh",
        &log,
        r#"  ready) echo '[{"id":"bd-1","title":"Epic"},{"id":"bd-1.2","title":"Leaf","parent":"bd-1"}]' ;;
  *) echo '{}' ;;"#,
    );
    write_task(&store, "todo", "bd-1", "epic");
    let out = Command::new(bin())
        .args(["task", "take", "--as", "w1"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(
        stderr(&out).contains("no open leaf tasks"),
        "{}",
        stderr(&out)
    );
    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(list.contains("todo   bd-1"), "epic must stay open: {list}");
}

#[test]
fn beads_closed_beats_a_murmur_take() {
    // Another agent closed the bead; this node still holds the murmur take.
    // Sync must end the task locally, never reopen or re-close the bead,
    // and tell the ex-holder.
    let store = fresh_dir("beads-closed");
    let base = store.parent().unwrap();
    let log = base.join("closed-bd.log");
    let stub = bd_stub(
        base,
        "bd-closed.sh",
        &log,
        r#"  ready) echo '[]' ;;
  show) echo '{"id":"bd-a1b2","title":"t","status":"closed"}' ;;
  *) echo '{}' ;;"#,
    );
    write_held_task(&store, "doing", "bd-a1b2", "w1", "doing");
    let out = Command::new(bin())
        .args(["task", "sync", "beads"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("reconciled 1"), "{}", stdout(&out));

    let list = stdout(&murmur(&store, &["task", "list", "--all"]));
    assert!(list.contains("done   bd-a1b2"), "{list}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("close bd-a1b2"),
        "already closed — no push: {calls}"
    );
    assert!(
        !calls.contains("--status open"),
        "never reopen a closed bead: {calls}"
    );
    let mail = stdout(&murmur(&store, &["inbox", "--as", "w1"]));
    assert!(
        mail.contains("closed in beads"),
        "holder must be told: {mail}"
    );
}

#[test]
fn beads_assignee_wins_a_contested_take() {
    // Beads says w2 owns the bead; this node's take by w1 loses: forced
    // drop, mail to w1, and no "open" pushed back over the assignment.
    let store = fresh_dir("beads-assignee");
    let base = store.parent().unwrap();
    let log = base.join("assignee-bd.log");
    let stub = bd_stub(
        base,
        "bd-assignee.sh",
        &log,
        r#"  ready) echo '[]' ;;
  show) echo '{"id":"bd-a1b2","title":"t","status":"in_progress","assignee":"w2"}' ;;
  *) echo '{}' ;;"#,
    );
    write_held_task(&store, "doing", "bd-a1b2", "w1", "doing");
    let out = Command::new(bin())
        .args(["task", "sync", "beads"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("reconciled 1"), "{}", stdout(&out));

    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(list.contains("todo   bd-a1b2"), "{list}");
    assert!(!list.contains("taken by"), "the lock is gone: {list}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(
        !calls.contains("--status open"),
        "the losing node must not reopen over the assignee: {calls}"
    );
    let mail = stdout(&murmur(&store, &["inbox", "--as", "w1"]));
    assert!(mail.contains("assigned to w2"), "{mail}");
}

#[test]
fn bd_human_text_on_mutations_is_success() {
    // Some bd builds print a checkmark even under --json. A green exit on
    // update/close is success; sync must carry on and mark the push done.
    let store = fresh_dir("beads-checkmark");
    let base = store.parent().unwrap();
    let log = base.join("checkmark-bd.log");
    let stub = bd_stub(
        base,
        "bd-checkmark.sh",
        &log,
        r#"  ready) echo '[{"id":"bd-a1b2","title":"t"}]' ;;
  show) echo '{"id":"bd-a1b2","title":"t","status":"open"}' ;;
  update|close) echo '✓ ok' ;;
  *) echo '{}' ;;"#,
    );
    let sync = || {
        Command::new(bin())
            .args(["task", "sync", "beads"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_BEADS", &stub)
            .output()
            .unwrap()
    };
    let out = sync();
    assert!(out.status.success(), "{}", stderr(&out));
    murmur(&store, &["task", "take", "--as", "w1"]);
    let out = sync();
    assert!(
        out.status.success(),
        "checkmark must not abort: {}",
        stderr(&out)
    );
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
    let out = sync();
    assert!(
        stdout(&out).contains("pushed 0"),
        "push stays idempotent: {}",
        stdout(&out)
    );
    murmur(&store, &["task", "done", "bd-a1b2", "--as", "w1"]);
    let out = sync();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("pushed 1"), "{}", stdout(&out));
}

#[test]
fn poke_revives_a_done_agent_before_prompting() {
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
        .args(["poke", "w1", "pick up the follow-ups"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_HERDR", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("revived w1"), "{}", stdout(&out));
    let calls = std::fs::read_to_string(&log).unwrap();
    let start_at = calls.find("agent start w1 --kind claude --pane w1:p3");
    let prompt_at = calls.find("agent prompt w1 pick up the follow-ups");
    assert!(
        start_at.is_some(),
        "must restart the done pane first: {calls}"
    );
    assert!(prompt_at.is_some(), "{calls}");
    assert!(start_at < prompt_at, "revive before prompt: {calls}");
}

#[test]
fn start_at_an_epic_boards_the_leaves() {
    let store = fresh_dir("start-epic");
    let base = store.parent().unwrap();
    let log = base.join("epic-bd.log");
    let stub = bd_stub(
        base,
        "bd-epic.sh",
        &log,
        r#"  ready) echo '[{"id":"bd-1.2","title":"Leaf A","parent":"bd-1"},{"id":"bd-1.3","title":"Leaf B","parent":"bd-1"}]' ;;
  show) echo '{"id":"bd-1","title":"The epic","status":"open"}' ;;
  *) echo '{}' ;;"#,
    );
    let out = Command::new(bin())
        .args(["start", "--bead", "bd-1", "--no-herdr"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("epic   bd-1"), "{}", stdout(&out));
    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(list.contains("bd-1.2"), "{list}");
    assert!(list.contains("bd-1.3"), "{list}");
    assert!(
        !list
            .lines()
            .any(|l| l.split_whitespace().nth(1) == Some("bd-1")),
        "the epic must not be takeable: {list}"
    );
}

#[test]
fn beads_sync_skips_parents_of_ready_children() {
    let store = fresh_dir("beads-leaves");
    let base = store.parent().unwrap();
    let log = base.join("leaves-bd.log");
    let stub = bd_stub(
        base,
        "bd-leaves.sh",
        &log,
        r#"  ready) echo '[{"id":"bd-1","title":"Epic"},{"id":"bd-1.2","title":"Leaf","parent":"bd-1"}]' ;;
  *) echo '{}' ;;"#,
    );
    let out = Command::new(bin())
        .args(["task", "sync", "beads"])
        .env("MURMUR_DIR", &store)
        .env("MURMUR_BEADS", &stub)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let list = stdout(&murmur(&store, &["task", "list"]));
    let ids: Vec<&str> = list
        .lines()
        .filter_map(|l| l.split_whitespace().nth(1))
        .collect();
    assert!(ids.contains(&"bd-1.2"), "{list}");
    assert!(
        !ids.contains(&"bd-1"),
        "epic must not land when a child is ready: {list}"
    );
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
  "agent list") echo '{{"result":{{"agents":[]}}}}' ;;
  "workspace create") echo '{{"result":{{"workspace":{{"workspace_id":"w9"}},"root_pane":{{"pane_id":"w9:p0"}}}}}}' ;;
  "pane split") echo '{{"result":{{"pane":{{"pane_id":"w9:p1"}}}}}}' ;;
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
    let who = stdout(&murmur(&store, &["who"]));
    assert!(who.contains("lead"), "{who}");
    assert!(who.contains("w1"), "{who}");
}

#[test]
fn stop_closes_workspace_and_clears_the_snap() {
    let store = fresh_dir("stop-herd");
    let base = store.parent().unwrap();
    murmur(&store, &["join", "lead"]);
    murmur(&store, &["join", "w1"]);
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
    let who = stdout(&murmur(&store, &["who"]));
    assert!(!who.contains("lead"), "presence dropped: {who}");
}

#[test]
fn stop_refuses_to_close_the_current_workspace() {
    let store = fresh_dir("stop-self");
    murmur(&store, &["join", "lead"]);
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
    assert!(
        calls.contains("please review") || calls.contains("unread"),
        "{calls}"
    );
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
fn herdr_idle_wake_revives_a_done_pane_with_mail() {
    // "Done" is not "idle": the model finished its turn and stopped
    // listening. New mail for a done pane must restart the agent before
    // the prompt, or the nudge lands on a corpse.
    let store = fresh_dir("wake-revive");
    murmur(&store, &["join", "lead"]);
    murmur(&store, &["send", "w1", "please review", "--as", "lead"]);
    let base = store.parent().unwrap();
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
    assert!(
        calls.contains("task sync beads"),
        "points at the pull path: {calls}"
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
    // the launch id waits durably in the lead's inbox
    let inbox = murmur(&store, &["inbox", "--as", "lead"]);
    let mail = stdout(&inbox);
    assert!(mail.contains("bc-test-1"), "{mail}");
    assert!(mail.contains("murmur cloud prompt"), "{mail}");
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
fn take_by_id_wins_once_and_refuses_epics() {
    let store = fresh_dir("take-id");
    write_task(&store, "todo", "bd-1", "epic");
    write_task(&store, "todo", "bd-1.2", "leaf");
    write_task(&store, "todo", "bd-9", "other leaf");

    let out = murmur(&store, &["task", "take", "bd-9", "--as", "w1", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let task: serde_json::Value = serde_json::from_str(&stdout(&out)).unwrap();
    assert_eq!(
        task["id"],
        "bd-9",
        "specific id, not oldest: {}",
        stdout(&out)
    );

    let out = murmur(&store, &["task", "take", "bd-9", "--as", "w2"]);
    assert!(
        !out.status.success(),
        "second taker must lose: {}",
        stdout(&out)
    );

    let out = murmur(&store, &["task", "take", "bd-1", "--as", "w2"]);
    assert!(!out.status.success(), "{}", stdout(&out));
    assert!(stderr(&out).contains("epic"), "{}", stderr(&out));
}

#[test]
fn sync_refuses_an_unscoped_flood_and_scopes_to_a_parent() {
    let store = fresh_dir("sync-flood");
    let base = store.parent().unwrap();
    let log = base.join("flood-bd.log");
    // 22 unrelated ready leaves plus one epic subtree
    let mut ready: Vec<String> = (1..=22)
        .map(|i| format!(r#"{{"id":"bd-x{i}","title":"leaf {i}"}}"#))
        .collect();
    ready.push(r#"{"id":"bd-1","title":"Epic"}"#.into());
    ready.push(r#"{"id":"bd-1.2","title":"Mid","parent":"bd-1"}"#.into());
    ready.push(r#"{"id":"bd-1.2.1","title":"Deep leaf","parent":"bd-1.2"}"#.into());
    let stub = bd_stub(
        base,
        "bd-flood.sh",
        &log,
        &format!(
            "  ready) echo '[{}]' ;;\n  *) echo '{{}}' ;;",
            ready.join(",")
        ),
    );
    let sync = |extra: &[&str]| {
        let mut args = vec!["task", "sync", "beads"];
        args.extend_from_slice(extra);
        Command::new(bin())
            .args(&args)
            .env("MURMUR_DIR", &store)
            .env("MURMUR_BEADS", &stub)
            .output()
            .unwrap()
    };
    // unscoped + big ready set: nothing lands, exit 0 (hook-safe)
    let out = sync(&[]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("NOT pulling"), "{}", stdout(&out));
    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(!list.contains("bd-x1"), "guard must hold the flood: {list}");

    // scoped to the epic: only its deep leaf lands
    let out = sync(&["--parent", "bd-1"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(list.contains("bd-1.2.1"), "{list}");
    assert!(
        !list.contains("bd-x1"),
        "out-of-scope leaves stay out: {list}"
    );
    assert!(
        !list.contains("todo   bd-1 "),
        "the epic stays in beads: {list}"
    );

    // --all overrides the guard
    let out = sync(&["--all"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let list = stdout(&murmur(&store, &["task", "list"]));
    assert!(list.contains("bd-x1"), "{list}");
}

#[test]
fn start_board_gives_the_herd_its_own_bus() {
    let dir = fresh_dir("board-herd");
    std::fs::create_dir_all(&dir).unwrap();
    let out = Command::new(bin())
        .args([
            "start",
            "fix the shelves",
            "--no-herdr",
            "--board",
            "Oficina",
        ])
        .current_dir(&dir)
        .env_remove("MURMUR_DIR")
        .env_remove("MURMUR_BEADS")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains(".murmur-oficina"), "{}", stdout(&out));
    let board = dir.join(".murmur-oficina");
    assert!(board.is_dir(), "board store must exist");
    let tasks = std::fs::read_dir(board.join("tasks").join("todo"))
        .unwrap()
        .count();
    assert_eq!(tasks, 1, "the goal lands on the board's own bus");
    assert!(
        !dir.join(".murmur").exists(),
        "the default bus must stay untouched"
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
fn clean_stale_prunes_dead_herd_leftovers_but_not_the_bus() {
    let store = fresh_dir("clean-stale");
    murmur(&store, &["join", "fresh"]);
    murmur(&store, &["send", "fresh", "keep me", "--as", "fresh2"]);
    // a long-dead agent and an old done task
    std::fs::write(
        store.join("agents").join("old.json"),
        r#"{"name":"old","pid":999999,"cwd":"","joined_at":1,"last_seen":1}"#,
    )
    .unwrap();
    write_task(&store, "done", "bd-old", "finished ages ago");
    let done = store.join("tasks").join("done").join("bd-old.json");
    Command::new("touch")
        .args(["-d", "2 days ago"])
        .arg(&done)
        .output()
        .unwrap();

    let out = murmur(&store, &["clean", "--stale"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("1 stale agents"), "{}", stdout(&out));
    assert!(
        stdout(&out).contains("1 old done tasks"),
        "{}",
        stdout(&out)
    );
    assert!(!done.exists());
    let who = stdout(&murmur(&store, &["who"]));
    assert!(who.contains("fresh"), "live presence survives: {who}");
    assert!(!who.contains("old"), "{who}");
    let mail = stdout(&murmur(&store, &["inbox", "--as", "fresh"]));
    assert!(mail.contains("keep me"), "the bus is untouched: {mail}");
}

#[test]
fn who_reports_idle_between_commands_before_gone() {
    let store = fresh_dir("who-idle");
    murmur(&store, &["join", "seed"]); // creates the store
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        store.join("agents").join("recente.json"),
        format!(
            r#"{{"name":"recente","pid":999999,"cwd":"","joined_at":1,"last_seen":{}}}"#,
            now - 30
        ),
    )
    .unwrap();
    std::fs::write(
        store.join("agents").join("velho.json"),
        r#"{"name":"velho","pid":999999,"cwd":"","joined_at":1,"last_seen":1}"#,
    )
    .unwrap();
    let who = stdout(&murmur(&store, &["who"]));
    let line = |n: &str| {
        who.lines()
            .find(|l| l.starts_with(n))
            .unwrap_or("")
            .to_string()
    };
    assert!(
        line("recente").contains("idle"),
        "recently seen != dead: {who}"
    );
    assert!(line("velho").contains("gone"), "{who}");
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
fn agents_md_contract_assigns_by_id_and_never_syncs_wholesale() {
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
    assert!(text.contains("murmur task take <id>"), "{text}");
    assert!(text.contains("Never sync the tracker wholesale"), "{text}");
    assert!(text.contains("FLEET.md"), "{text}");
    assert!(
        !text.contains("assigns you\nthe oldest"),
        "the oldest-open recipe is gone: {text}"
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

    // join from the worktree with NO MURMUR_DIR: the store must land in
    // the main checkout, never as a stray copy in the worktree
    let out = Command::new(bin())
        .args(["join", "wtworker"])
        .current_dir(&wt)
        .env_remove("MURMUR_DIR")
        .env_remove("HERDR_ENV")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        repo.join(".murmur/agents/wtworker.json").exists(),
        "store anchored to the repo"
    );
    assert!(
        !wt.join(".murmur").exists(),
        "no stray store in the worktree"
    );

    // mail sent from the main checkout is readable from the worktree
    let out = Command::new(bin())
        .args(["send", "wtworker", "one bus", "--as", "mainside"])
        .current_dir(&repo)
        .env_remove("MURMUR_DIR")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    let out = Command::new(bin())
        .args(["inbox", "--as", "wtworker"])
        .current_dir(&wt)
        .env_remove("MURMUR_DIR")
        .output()
        .unwrap();
    assert!(stdout(&out).contains("one bus"), "{}", stdout(&out));
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
        .contains("Report to lead and STOP"));
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
        calls.matches("send-keys").count(),
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
