//! A shared work queue with no queue server. A task is a file; its state is
//! which directory it's in (`tasks/todo`, `tasks/doing`, `tasks/done`).
//! Taking a task is `rename(todo/X, doing/X)` — atomic, so when N agents race
//! for the same task exactly one rename succeeds and the losers move on to
//! the next file. Work-stealing semantics for free.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::store::{self, Store};

pub const STATES: [&str; 3] = ["todo", "doing", "done"];

fn precedence(state: &str) -> u8 {
    match state {
        "done" => 3,
        "doing" => 2,
        _ => 1,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    pub by: String,
    /// unix millis
    pub ts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub taken_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_by: Option<String>,
    /// backend-side id when this task mirrors an external tracker (Linear issue uuid)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_url: Option<String>,
    /// last local state pushed to the external tracker
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synced_state: Option<String>,
}

impl Store {
    fn task_dir(&self, state: &str) -> PathBuf {
        self.root().join("tasks").join(state)
    }

    fn task_init(&self) -> Result<()> {
        self.init()?;
        for state in STATES {
            fs::create_dir_all(self.task_dir(state))?;
        }
        Ok(())
    }

    pub fn task_add(&self, by: &str, title: &str, body: &str) -> Result<Task> {
        store::valid_name(by)?;
        self.task_init()?;
        self.touch(by)?;
        let ts = store::now_millis();
        let task = Task {
            id: store::next_id(ts),
            title: title.to_string(),
            body: body.to_string(),
            by: by.to_string(),
            ts,
            taken_by: None,
            done_by: None,
            external_id: None,
            external_url: None,
            synced_state: None,
        };
        let tmp = self.root().join("tmp").join(format!("task-{}", task.id));
        fs::write(&tmp, serde_json::to_vec(&task)?)?;
        fs::rename(&tmp, self.task_dir("todo").join(format!("{}.json", task.id)))?;
        Ok(task)
    }

    /// All tasks in the given states, oldest first within each state.
    pub fn task_list(&self, states: &[&str]) -> Result<Vec<(String, Task)>> {
        let mut out = Vec::new();
        for state in states {
            let dir = self.task_dir(state);
            if !dir.is_dir() {
                continue;
            }
            let mut paths: Vec<PathBuf> = fs::read_dir(&dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .collect();
            paths.sort();
            for path in paths {
                let Ok(bytes) = fs::read(&path) else { continue };
                if let Ok(task) = serde_json::from_slice::<Task>(&bytes) {
                    out.push((state.to_string(), task));
                }
            }
        }
        Ok(out)
    }

    pub fn open_task_count(&self) -> usize {
        self.task_list(&["todo"]).map(|t| t.len()).unwrap_or(0)
    }

    /// Grab the oldest todo task. Atomic under contention: the rename either
    /// succeeds (it's yours) or fails (someone else won; try the next one).
    pub fn task_take(&self, name: &str) -> Result<Option<Task>> {
        store::valid_name(name)?;
        self.task_init()?;
        self.touch(name)?;
        let todo = self.task_dir("todo");
        let mut paths: Vec<PathBuf> = fs::read_dir(&todo)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .collect();
        paths.sort();
        for path in paths {
            let Some(file_name) = path.file_name() else { continue };
            let dest = self.task_dir("doing").join(file_name);
            if fs::rename(&path, &dest).is_err() {
                continue; // lost the race for this one
            }
            let Ok(bytes) = fs::read(&dest) else { continue };
            let Ok(mut task) = serde_json::from_slice::<Task>(&bytes) else { continue };
            task.taken_by = Some(name.to_string());
            fs::write(&dest, serde_json::to_vec(&task)?)?;
            return Ok(Some(task));
        }
        Ok(None)
    }

    pub fn task_done(&self, name: &str, id_prefix: &str) -> Result<Task> {
        let (path, mut task) = self.find_task("doing", id_prefix)?;
        if task.taken_by.as_deref() != Some(name) {
            bail!(
                "task {} is taken by {}, not you",
                task.id,
                task.taken_by.as_deref().unwrap_or("nobody")
            );
        }
        task.done_by = Some(name.to_string());
        let dest = self.task_dir("done").join(format!("{}.json", task.id));
        fs::write(&dest, serde_json::to_vec(&task)?)?;
        fs::remove_file(&path)?;
        Ok(task)
    }

    /// Put a taken task back on the board.
    pub fn task_drop(&self, name: &str, id_prefix: &str) -> Result<Task> {
        let (path, mut task) = self.find_task("doing", id_prefix)?;
        if task.taken_by.as_deref() != Some(name) {
            bail!(
                "task {} is taken by {}, not you",
                task.id,
                task.taken_by.as_deref().unwrap_or("nobody")
            );
        }
        task.taken_by = None;
        let dest = self.task_dir("todo").join(format!("{}.json", task.id));
        fs::write(&dest, serde_json::to_vec(&task)?)?;
        fs::remove_file(&path)?;
        Ok(task)
    }

    /// Import an externally-sourced task onto the board unless a task with
    /// that id already exists in any state. Returns whether it was new.
    pub fn task_import(&self, task: Task) -> Result<bool> {
        self.task_init()?;
        let file = format!("{}.json", task.id);
        if STATES.iter().any(|s| self.task_dir(s).join(&file).exists()) {
            return Ok(false);
        }
        let tmp = self.root().join("tmp").join(&file);
        fs::write(&tmp, serde_json::to_vec(&task)?)?;
        fs::rename(&tmp, self.task_dir("todo").join(&file))?;
        Ok(true)
    }

    /// Rewrite a task's file in place (bookkeeping fields only — state
    /// changes go through take/done/drop).
    pub fn task_rewrite(&self, state: &str, task: &Task) -> Result<()> {
        let path = self.task_dir(state).join(format!("{}.json", task.id));
        anyhow::ensure!(path.exists(), "task {} is no longer in {}", task.id, state);
        fs::write(&path, serde_json::to_vec(task)?)?;
        Ok(())
    }

    /// Merge a task copy from another node. Precedence: done > doing > todo
    /// (work only moves forward). A doing-vs-doing conflict — two nodes took
    /// the same task during a partition — resolves deterministically: the
    /// lexicographically smaller holder keeps it, on both sides, so the
    /// loser's `task done` fails visibly and it moves on.
    pub fn merge_task(&self, incoming_state: &str, incoming: Task) -> Result<()> {
        self.task_init()?;
        let local = STATES.iter().find_map(|s| {
            let path = self.task_dir(s).join(format!("{}.json", incoming.id));
            fs::read(&path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Task>(&b).ok())
                .map(|t| (s.to_string(), t))
        });
        let (win_state, winner) = match &local {
            None => (incoming_state.to_string(), incoming),
            Some((local_state, local_task)) => {
                let (lp, ip) = (precedence(local_state), precedence(incoming_state));
                let doing_conflict_loss = ip == lp
                    && incoming_state == "doing"
                    && incoming.taken_by < local_task.taken_by;
                if ip > lp || doing_conflict_loss {
                    (incoming_state.to_string(), incoming)
                } else {
                    return Ok(()); // local copy stands
                }
            }
        };
        for state in STATES {
            let _ = fs::remove_file(self.task_dir(state).join(format!("{}.json", winner.id)));
        }
        let tmp = self.root().join("tmp").join(format!("merge-{}", winner.id));
        fs::write(&tmp, serde_json::to_vec(&winner)?)?;
        fs::rename(&tmp, self.task_dir(&win_state).join(format!("{}.json", winner.id)))?;
        Ok(())
    }

    fn find_task(&self, state: &str, id_prefix: &str) -> Result<(PathBuf, Task)> {
        self.task_init()?;
        let mut matches = Vec::new();
        for (_, task) in self.task_list(&[state])? {
            if task.id.starts_with(id_prefix) {
                let path = self.task_dir(state).join(format!("{}.json", task.id));
                matches.push((path, task));
            }
        }
        match matches.len() {
            0 => bail!("no {} task matching '{}'", state, id_prefix),
            1 => Ok(matches.into_iter().next().unwrap()),
            n => bail!("'{}' is ambiguous ({} matches) — use more of the id", id_prefix, n),
        }
    }
}
