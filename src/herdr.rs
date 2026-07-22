//! The herdr CLI seam (the `Herdr` trait and its `herdr pane list`/`agent focus`/`notification
//! show` implementation) and the JSON parsing for both `pane list` responses and plugin event
//! payloads.

use crate::state::{PluginError, WaitStatus};
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Output};

/// A pane as reported by `herdr pane list` — the subset of `PaneInfo` we seed the queue from.
/// Same fields an event carries, so a scan can build full-fidelity queue entries. We deliberately
/// do NOT carry `focused`: the seed queues every blocked/done pane regardless of which pane herdr
/// mechanically restored focus to on restart (that is not a user "I looked at it" action, so it
/// must not suppress the ping). Focus-based eviction stays where it belongs — the event path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneInfo {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    pub(crate) agent_status: String,
    pub(crate) agent: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) title: Option<String>,
}

pub(crate) trait Herdr {
    /// Map of live `pane_id -> agent_status` from `herdr pane list`.
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError>;
    /// Full `pane list` info, used by the startup hook to re-seed the queue.
    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError>;
    /// Bring the agent in the given pane into focus (jumps workspace/tab/pane).
    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError>;
    /// Submit `text` as a reply into the agent in the given pane (routes into its session).
    /// The target is the stored `pane_id` (verified: the `agent_session` uuid is not accepted).
    /// Fire-and-forget: no `--wait` (its settled-state gate is unreliable from a non-working start).
    fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError>;
    /// Show a herdr toast.
    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError>;
}

pub(crate) struct CliHerdr {
    pub(crate) bin_path: PathBuf,
}

impl CliHerdr {
    /// Run `herdr pane list` and return its raw stdout, or an error if the command failed.
    /// Shared by [`pane_status_map`](Self::pane_status_map) and [`pane_infos`](Self::pane_infos)
    /// so both parse the same response without duplicating the spawn/error handling.
    fn pane_list_stdout(&self) -> Result<Vec<u8>, PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("pane")
            .arg("list")
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH pane list ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if !output.status.success() {
            return Err(command_failure("HERDR_BIN_PATH pane list", &output));
        }

        Ok(output.stdout)
    }
}

impl Herdr for CliHerdr {
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError> {
        parse_pane_status_map(&self.pane_list_stdout()?)
    }

    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError> {
        parse_pane_infos(&self.pane_list_stdout()?)
    }

    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("focus")
            .arg(pane_id)
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent focus {pane_id} ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH agent focus", &output))
        }
    }

    fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError> {
        let output = Command::new(&self.bin_path)
            .arg("agent")
            .arg("prompt")
            .arg(pane_id)
            .arg(text)
            .output()
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to run HERDR_BIN_PATH agent prompt {pane_id} ({}): {error}",
                    self.bin_path.display()
                ))
            })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH agent prompt", &output))
        }
    }

    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError> {
        let mut command = Command::new(&self.bin_path);
        command.arg("notification").arg("show").arg(title);
        if let Some(body) = body {
            command.arg("--body").arg(body);
        }
        command.arg("--sound").arg(sound);

        let output = command.output().map_err(|error| {
            PluginError::new(format!(
                "failed to run HERDR_BIN_PATH notification show ({}): {error}",
                self.bin_path.display()
            ))
        })?;

        if output.status.success() {
            Ok(())
        } else {
            Err(command_failure("HERDR_BIN_PATH notification show", &output))
        }
    }
}

fn command_failure(command: &str, output: &Output) -> PluginError {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        output.status.to_string()
    };
    PluginError::new(format!("{command} failed: {detail}"))
}

fn parse_pane_status_map(stdout: &[u8]) -> Result<HashMap<String, String>, PluginError> {
    Ok(parse_pane_infos(stdout)?
        .into_iter()
        .map(|pane| (pane.pane_id, pane.agent_status))
        .collect())
}

/// Parse `herdr pane list` into the fields the queue needs. Preserves the panes' returned order
/// (a `Vec`, not a map) so a re-seed is deterministic. Panes without a `pane_id` are skipped;
/// missing `agent_status` falls back to `"unknown"` (which the seed ignores — not a wait status).
fn parse_pane_infos(stdout: &[u8]) -> Result<Vec<PaneInfo>, PluginError> {
    let value: Value = serde_json::from_slice(stdout).map_err(|error| {
        PluginError::new(format!(
            "failed to parse HERDR_BIN_PATH pane list JSON: {error}"
        ))
    })?;

    if let Some(error) = herdr_error_message(&value) {
        return Err(PluginError::new(format!(
            "HERDR_BIN_PATH pane list returned an error: {error}"
        )));
    }

    let panes = value
        .get("result")
        .and_then(|result| result.get("panes"))
        .and_then(Value::as_array)
        .ok_or_else(|| {
            PluginError::new("HERDR_BIN_PATH pane list returned an unexpected response".to_string())
        })?;

    let mut infos = Vec::with_capacity(panes.len());
    for pane in panes {
        let Some(pane_id) = non_empty_string(pane, "pane_id") else {
            continue;
        };
        infos.push(PaneInfo {
            pane_id,
            workspace_id: non_empty_string(pane, "workspace_id").unwrap_or_default(),
            agent_status: non_empty_string(pane, "agent_status")
                .unwrap_or_else(|| "unknown".to_string()),
            agent: non_empty_string(pane, "agent"),
            display_agent: non_empty_string(pane, "display_agent"),
            title: non_empty_string(pane, "title"),
        });
    }
    Ok(infos)
}

fn herdr_error_message(value: &Value) -> Option<String> {
    let error = value.get("error")?;
    // A present-but-null `error` is a success shape, not a failure.
    if error.is_null() {
        return None;
    }
    let code = error.get("code").and_then(Value::as_str);
    let message = error.get("message").and_then(Value::as_str);

    match (code, message) {
        (Some(code), Some(message)) => Some(format!("{code}: {message}")),
        (Some(code), None) => Some(code.to_string()),
        (None, Some(message)) => Some(message.to_string()),
        (None, None) => Some(error.to_string()),
    }
}

pub(crate) struct StatusEvent {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    pub(crate) agent_status: String,
    pub(crate) agent: Option<String>,
    pub(crate) display_agent: Option<String>,
    pub(crate) title: Option<String>,
}

impl StatusEvent {
    pub(crate) fn wait_status(&self) -> Option<WaitStatus> {
        match self.agent_status.as_str() {
            "blocked" => Some(WaitStatus::Blocked),
            "done" => Some(WaitStatus::Done),
            _ => None,
        }
    }

    pub(crate) fn is_working(&self) -> bool {
        self.agent_status == "working"
    }
}

/// The plugin event JSON is `{ "event": ..., "data": { "type": ..., <fields> } }`.
/// Fields are read from `data`, falling back to the top-level object.
pub(crate) fn parse_status_event(raw: &str) -> Option<StatusEvent> {
    let value: Value = serde_json::from_str(raw).ok()?;
    let data = event_data(&value);
    Some(StatusEvent {
        pane_id: non_empty_string(data, "pane_id")?,
        workspace_id: non_empty_string(data, "workspace_id").unwrap_or_default(),
        agent_status: non_empty_string(data, "agent_status")?,
        agent: non_empty_string(data, "agent"),
        display_agent: non_empty_string(data, "display_agent"),
        title: non_empty_string(data, "title"),
    })
}

pub(crate) fn parse_event_string(raw: &str, key: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    non_empty_string(event_data(&value), key)
}

fn event_data(value: &Value) -> &Value {
    value.get("data").unwrap_or(value)
}

fn non_empty_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pane_infos_extracts_wait_fields_and_skips_idless_panes() {
        let json = br#"{"result":{"type":"pane_list","panes":[
            {"pane_id":"wA:p1","workspace_id":"wA","agent_status":"blocked","agent":"claude","display_agent":"Claude","title":"needs input","focused":true},
            {"pane_id":"","agent_status":"done"},
            {"agent_status":"done"}
        ]}}"#;
        let infos = parse_pane_infos(json).expect("pane list should parse");
        assert_eq!(infos.len(), 1, "panes without a pane_id are skipped");
        assert_eq!(infos[0].pane_id, "wA:p1");
        assert_eq!(infos[0].workspace_id, "wA");
        assert_eq!(infos[0].agent_status, "blocked");
        assert_eq!(infos[0].title.as_deref(), Some("needs input"));
    }

    #[test]
    fn null_error_field_is_treated_as_success() {
        let value: Value = serde_json::from_str(r#"{"result":{"panes":[]},"error":null}"#).unwrap();
        assert_eq!(herdr_error_message(&value), None);
    }

    // A throwaway `herdr` that logs each argv entry on its own line (so a multi-word arg proves it
    // was passed as ONE argument, not shell-split) and exits with `exit_code`.
    #[cfg(unix)]
    fn fake_agent_prompt_herdr(
        dir: &std::path::Path,
        log: &std::path::Path,
        exit_code: i32,
    ) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(format!("fake-herdr-{exit_code}.sh"));
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{log}\"\nprintf 'prompt refused\\n' >&2\nexit {exit_code}\n",
            log = log.display()
        );
        std::fs::write(&path, script).expect("fake herdr should write");
        let mut permissions = std::fs::metadata(&path)
            .expect("fake herdr metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("fake herdr should be executable");
        path
    }

    #[test]
    #[cfg(unix)]
    fn cli_prompt_agent_shapes_the_command() {
        let dir = crate::test_support::temp_state_dir("prompt-ok");
        let log = dir.join("argv.log");
        let herdr = CliHerdr {
            bin_path: fake_agent_prompt_herdr(&dir, &log, 0),
        };

        herdr
            .prompt_agent("wA:p1", "use option B")
            .expect("prompt should succeed");

        let argv = std::fs::read_to_string(&log).expect("argv log should exist");
        let lines: Vec<&str> = argv.lines().collect();
        // The reply text arrives as a single argv entry — not split on its spaces.
        assert_eq!(lines, ["agent", "prompt", "wA:p1", "use option B"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn cli_prompt_agent_maps_a_nonzero_exit_to_err() {
        let dir = crate::test_support::temp_state_dir("prompt-err");
        let log = dir.join("argv.log");
        let herdr = CliHerdr {
            bin_path: fake_agent_prompt_herdr(&dir, &log, 1),
        };

        let error = herdr
            .prompt_agent("wA:p1", "hi")
            .expect_err("a nonzero exit should map to an error");
        assert!(
            error.to_string().contains("agent prompt"),
            "error was: {error}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // The shared fake records prompt calls in order — the contract the reply-mode slices assert on.
    #[test]
    fn fake_herdr_records_prompt_calls() {
        let herdr = crate::test_support::FakeHerdr::new(&[("wA:p1", "blocked")]);
        herdr
            .prompt_agent("wA:p1", "yes")
            .expect("fake prompt should succeed");
        assert_eq!(
            herdr.prompts.into_inner(),
            vec![("wA:p1".to_string(), "yes".to_string())]
        );
    }
}
