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

#[test]
fn linear_sync_pulls_pushes_and_stays_idempotent() {
    use std::os::unix::fs::PermissionsExt;
    let store = fresh_dir("linear");
    let base = store.parent().unwrap();
    let log = base.join("linear-calls.log");
    let stub = base.join("linear-stub.sh");
    std::fs::write(
        &stub,
        format!(
            r#"#!/bin/sh
body=$(cat)
printf '%s\n' "$body" >> "{log}"
case "$body" in
  *'teams(filter'*)
    echo '{{"data":{{"teams":{{"nodes":[{{"id":"t1","states":{{"nodes":[{{"id":"s-un","name":"Todo","type":"unstarted"}},{{"id":"s-st","name":"In Progress","type":"started"}},{{"id":"s-co","name":"Done","type":"completed"}}]}}}}]}}}}}}' ;;
  *'issues(filter'*)
    echo '{{"data":{{"issues":{{"nodes":[{{"id":"uuid-42","identifier":"ENG-42","title":"Fix login flow","description":"Users bounce on refresh.","url":"https://linear.app/acme/issue/ENG-42"}}]}}}}}}' ;;
  *)
    echo '{{"data":{{"issueUpdate":{{"success":true}},"commentCreate":{{"success":true}}}}}}' ;;
esac
"#,
            log = log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    let sync = || {
        Command::new(bin())
            .args(["task", "sync", "linear", "--team", "ENG"])
            .env("MURMUR_DIR", &store)
            .env("MURMUR_LINEAR_CURL", &stub)
            .env("LINEAR_API_KEY", "test-key")
            .output()
            .unwrap()
    };

    // pull: the issue lands on the board once, with a readable id
    let out = sync();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("pulled 1"));
    let out = sync();
    assert!(stdout(&out).contains("pulled 0"), "re-sync must not duplicate");
    let out = murmur(&store, &["task", "list"]);
    assert!(stdout(&out).contains("linear-ENG-42"));
    assert!(stdout(&out).contains("Fix login flow"));

    // take, then sync pushes started + an attributed comment, exactly once
    let out = murmur(&store, &["task", "take", "--as", "worker-1"]);
    assert!(stdout(&out).contains("linear.app/acme/issue/ENG-42"), "url surfaces on take");
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("s-st"), "moved to started in Linear");
    assert!(calls.contains("worker-1"), "comment attributes the agent");
    let out = sync();
    assert!(stdout(&out).contains("pushed 0"), "push is idempotent");

    // done flows through as completed
    murmur(&store, &["task", "done", "linear-ENG-42", "--as", "worker-1"]);
    let out = sync();
    assert!(stdout(&out).contains("pushed 1"));
    let calls = std::fs::read_to_string(&log).unwrap();
    assert!(calls.contains("s-co"), "moved to completed in Linear");
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
    let run = |args: &[&str]| {
        Command::new(bin())
            .args(args)
            .current_dir(&workdir)
            .env("MURMUR_DIR", &dir)
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
}
