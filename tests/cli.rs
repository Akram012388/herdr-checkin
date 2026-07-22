//! End-to-end tests that run the built binary the way herdr does: one process
//! per event/action, with a fake `herdr` on `HERDR_BIN_PATH`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "herdr-checkin-cli-{label}-{}-{}",
            std::process::id(),
            current_unix_ms()
        ));
        fs::create_dir_all(&path).expect("temp directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[test]
#[cfg(unix)]
fn enqueue_via_events_then_next_focuses_oldest() {
    let temp = TempDir::new("next");
    let state_dir = temp.path().join("state");
    let pane_list_path = temp.path().join("panes.json");
    let focus_log_path = temp.path().join("focus.log");
    let notify_log_path = temp.path().join("notify.log");
    let herdr_path = write_fake_herdr(temp.path());

    let plugin = Plugin {
        binary: env!("CARGO_BIN_EXE_herdr-checkin").into(),
        herdr_path,
        state_dir,
        pane_list_path,
        focus_log_path,
        notify_log_path,
    };

    plugin
        .run("status-changed")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            status_event("wA:p1", "wA", "blocked", "needs input"),
        )
        .assert_success();
    plugin
        .run("status-changed")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            status_event("wB:p1", "wB", "done", "finished"),
        )
        .assert_success();

    // Both panes are live; next should focus the oldest (wA:p1) and pop it.
    plugin.write_pane_list(&[("wA:p1", "blocked"), ("wB:p1", "done")]);
    plugin.run("next").assert_success();
    assert_eq!(plugin.focus_log(), vec!["wA:p1"]);

    // Running next again focuses the remaining waiter.
    plugin.write_pane_list(&[("wB:p1", "done")]);
    plugin.run("next").assert_success();
    assert_eq!(plugin.focus_log(), vec!["wA:p1", "wB:p1"]);

    // Queue now empty: next is a clean no-op, no extra focus call.
    plugin.write_pane_list(&[]);
    plugin.run("next").assert_success();
    assert_eq!(plugin.focus_log(), vec!["wA:p1", "wB:p1"]);
}

#[test]
#[cfg(unix)]
fn focus_event_evicts_before_next() {
    let temp = TempDir::new("evict");
    let plugin = Plugin::in_dir(&temp);

    plugin
        .run("status-changed")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            status_event("wA:p1", "wA", "blocked", "x"),
        )
        .assert_success();
    // User focuses the pane themselves.
    plugin
        .run("focused")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            pane_event("pane_focused", "wA:p1", "wA"),
        )
        .assert_success();

    plugin.write_pane_list(&[("wA:p1", "blocked")]);
    plugin.run("next").assert_success();
    assert!(plugin.focus_log().is_empty());
}

#[test]
#[cfg(unix)]
fn peek_writes_a_toast_listing_the_queue() {
    let temp = TempDir::new("peek");
    let plugin = Plugin::in_dir(&temp);

    plugin
        .run("status-changed")
        .env(
            "HERDR_PLUGIN_EVENT_JSON",
            status_event("wA:p1", "wA", "blocked", "needs input"),
        )
        .assert_success();

    plugin.write_pane_list(&[("wA:p1", "blocked")]);
    plugin.run("peek").assert_success();

    let notifications = plugin.notify_log();
    assert_eq!(notifications.len(), 1);
    assert!(
        notifications[0].contains("1 agent waiting"),
        "notification was: {}",
        notifications[0]
    );
}

#[test]
#[cfg(unix)]
fn concurrent_status_events_keep_state_json_valid() {
    let temp = TempDir::new("concurrent");
    let plugin = Plugin::in_dir(&temp);

    let handles = (0..12)
        .map(|index| {
            let plugin = plugin.clone();
            thread::spawn(move || {
                let pane = format!("wX:p{index}");
                plugin
                    .run("status-changed")
                    .env(
                        "HERDR_PLUGIN_EVENT_JSON",
                        status_event(&pane, "wX", "blocked", "x"),
                    )
                    .assert_success();
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        handle.join().expect("event process should join");
    }

    let state = read_state_json(&plugin.state_dir);
    let entries = state["entries"]
        .as_array()
        .expect("entries should be an array");
    assert_eq!(entries.len(), 12);
    assert_eq!(state["version"].as_u64(), Some(1));
}

#[derive(Clone)]
struct Plugin {
    binary: PathBuf,
    herdr_path: PathBuf,
    state_dir: PathBuf,
    pane_list_path: PathBuf,
    focus_log_path: PathBuf,
    notify_log_path: PathBuf,
}

impl Plugin {
    #[cfg(unix)]
    fn in_dir(temp: &TempDir) -> Self {
        Self {
            binary: env!("CARGO_BIN_EXE_herdr-checkin").into(),
            herdr_path: write_fake_herdr(temp.path()),
            state_dir: temp.path().join("state"),
            pane_list_path: temp.path().join("panes.json"),
            focus_log_path: temp.path().join("focus.log"),
            notify_log_path: temp.path().join("notify.log"),
        }
    }

    fn run(&self, subcommand: &str) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .arg(subcommand)
            .env("HERDR_BIN_PATH", &self.herdr_path)
            .env("HERDR_PLUGIN_STATE_DIR", &self.state_dir)
            .env("FAKE_HERDR_PANE_LIST", &self.pane_list_path)
            .env("FAKE_HERDR_FOCUS_LOG", &self.focus_log_path)
            .env("FAKE_HERDR_NOTIFY_LOG", &self.notify_log_path);
        command
    }

    fn write_pane_list(&self, panes: &[(&str, &str)]) {
        let panes = panes
            .iter()
            .map(|(pane_id, status)| {
                serde_json::json!({
                    "pane_id": pane_id,
                    "workspace_id": pane_id.split(':').next().unwrap_or(""),
                    "agent_status": status,
                    "focused": false
                })
            })
            .collect::<Vec<_>>();
        let response = serde_json::json!({
            "id": "cli:pane:list",
            "result": { "type": "pane_list", "panes": panes }
        });
        fs::write(
            &self.pane_list_path,
            serde_json::to_string(&response).expect("pane list should serialize"),
        )
        .expect("pane list should write");
    }

    fn focus_log(&self) -> Vec<String> {
        read_lines(&self.focus_log_path)
    }

    fn notify_log(&self) -> Vec<String> {
        read_lines(&self.notify_log_path)
    }
}

trait CommandAssertions {
    fn assert_success(&mut self);
}

impl CommandAssertions for Command {
    fn assert_success(&mut self) {
        let output = self.output().expect("plugin command should run");
        assert!(
            output.status.success(),
            "expected success, got status {:?}\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[cfg(unix)]
fn write_fake_herdr(root: &Path) -> PathBuf {
    let path = root.join("fake-herdr.sh");
    fs::write(
        &path,
        r#"#!/bin/sh
set -eu

if [ "$1" = "pane" ] && [ "$2" = "list" ]; then
  cat "$FAKE_HERDR_PANE_LIST"
  exit 0
fi

if [ "$1" = "agent" ] && [ "$2" = "focus" ]; then
  printf '%s\n' "$3" >> "$FAKE_HERDR_FOCUS_LOG"
  printf '{"id":"fake","result":{"type":"agent_info"}}\n'
  exit 0
fi

if [ "$1" = "notification" ] && [ "$2" = "show" ]; then
  printf '%s\n' "$3" >> "$FAKE_HERDR_NOTIFY_LOG"
  printf '{"id":"fake","result":{"type":"ok"}}\n'
  exit 0
fi

printf 'unexpected fake herdr command: %s %s\n' "${1:-}" "${2:-}" >&2
exit 64
"#,
    )
    .expect("fake herdr script should write");

    let mut permissions = fs::metadata(&path)
        .expect("fake herdr script metadata should load")
        .permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("fake herdr script should be executable");
    path
}

fn read_lines(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect()
}

fn read_state_json(state_dir: &Path) -> Value {
    let contents = fs::read_to_string(state_dir.join("state.json")).expect("state should exist");
    serde_json::from_str(&contents).expect("state should be valid JSON")
}

fn status_event(pane_id: &str, workspace_id: &str, status: &str, title: &str) -> String {
    format!(
        r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"{pane_id}","workspace_id":"{workspace_id}","agent_status":"{status}","agent":"claude","display_agent":"Claude","title":"{title}"}}}}"#
    )
}

fn pane_event(kind: &str, pane_id: &str, workspace_id: &str) -> String {
    format!(
        r#"{{"event":"{kind}","data":{{"type":"{kind}","pane_id":"{pane_id}","workspace_id":"{workspace_id}"}}}}"#
    )
}

fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}
