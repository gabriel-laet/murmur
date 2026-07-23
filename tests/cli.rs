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
