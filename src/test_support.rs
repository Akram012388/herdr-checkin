//! Shared test doubles and fixtures used across more than one module's test suite: the fake
//! [`Herdr`] implementation and the state-dir/event-JSON builders. Test-only — never compiled into
//! the release binary.

use crate::herdr::{Herdr, PaneInfo};
use crate::state::{read_state, PluginError, QueueEntry, STATE_FILE_NAME};
use crate::RuntimeEnv;
use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct FakeHerdr {
    live: HashMap<String, String>,
    panes: Vec<PaneInfo>,
    focus_fails: bool,
    prompt_fails: bool,
    pub(crate) focused: RefCell<Vec<String>>,
    /// Every `prompt_agent` call, as `(pane_id, text)`, in order.
    pub(crate) prompts: RefCell<Vec<(String, String)>>,
    pub(crate) notifications: RefCell<Vec<(String, Option<String>, String)>>,
}

impl FakeHerdr {
    pub(crate) fn new(panes: &[(&str, &str)]) -> Self {
        Self {
            live: panes
                .iter()
                .map(|(pane_id, status)| (pane_id.to_string(), status.to_string()))
                .collect(),
            // Mirror `pane list`: derive full PaneInfos so `pane_infos()` and
            // `pane_status_map()` stay consistent. Workspace is the pane-id prefix (as herdr's
            // `wS:pN` ids encode it); agent/title are absent unless a test overrides `panes`.
            panes: panes
                .iter()
                .map(|(pane_id, status)| PaneInfo {
                    pane_id: pane_id.to_string(),
                    workspace_id: pane_id.split(':').next().unwrap_or("").to_string(),
                    agent_status: status.to_string(),
                    agent: None,
                    display_agent: None,
                    title: None,
                })
                .collect(),
            focus_fails: false,
            prompt_fails: false,
            focused: RefCell::new(Vec::new()),
            prompts: RefCell::new(Vec::new()),
            notifications: RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn with_failing_focus(mut self) -> Self {
        self.focus_fails = true;
        self
    }

    pub(crate) fn with_failing_prompt(mut self) -> Self {
        self.prompt_fails = true;
        self
    }

    /// Override the `pane list` result with hand-built PaneInfos (for field-fidelity tests).
    pub(crate) fn with_panes(mut self, panes: Vec<PaneInfo>) -> Self {
        self.panes = panes;
        self
    }
}

impl Herdr for FakeHerdr {
    fn pane_status_map(&self) -> Result<HashMap<String, String>, PluginError> {
        Ok(self.live.clone())
    }

    fn pane_infos(&self) -> Result<Vec<PaneInfo>, PluginError> {
        Ok(self.panes.clone())
    }

    fn focus_agent(&self, pane_id: &str) -> Result<(), PluginError> {
        if self.focus_fails {
            return Err(PluginError::new(format!("focus refused for {pane_id}")));
        }
        self.focused.borrow_mut().push(pane_id.to_string());
        Ok(())
    }

    fn prompt_agent(&self, pane_id: &str, text: &str) -> Result<(), PluginError> {
        if self.prompt_fails {
            return Err(PluginError::new(format!("prompt refused for {pane_id}")));
        }
        self.prompts
            .borrow_mut()
            .push((pane_id.to_string(), text.to_string()));
        Ok(())
    }

    fn show_notification(
        &self,
        title: &str,
        body: Option<&str>,
        sound: &str,
    ) -> Result<(), PluginError> {
        self.notifications.borrow_mut().push((
            title.to_string(),
            body.map(str::to_owned),
            sound.to_string(),
        ));
        Ok(())
    }

    fn popup_close(&self) -> Result<(), PluginError> {
        // No socket in tests; the pane's popup dismissal is exercised by the manual E2E, not here.
        Ok(())
    }
}

pub(crate) fn runtime(state_dir: PathBuf, now_ms: u64) -> RuntimeEnv {
    RuntimeEnv {
        state_dir,
        event_json: None,
        now_ms,
    }
}

pub(crate) fn temp_state_dir(label: &str) -> PathBuf {
    let path = env::temp_dir().join(format!(
        "herdr-checkin-{label}-{}-{}",
        std::process::id(),
        crate::current_unix_ms()
    ));
    fs::create_dir_all(&path).expect("temp state directory should be created");
    path
}

pub(crate) fn load(state_dir: &Path) -> Vec<QueueEntry> {
    read_state(&state_dir.join(STATE_FILE_NAME))
        .expect("state should load")
        .entries
}

pub(crate) fn status_event_json(
    pane_id: &str,
    workspace_id: &str,
    status: &str,
    title: &str,
) -> String {
    format!(
        r#"{{"event":"pane_agent_status_changed","data":{{"type":"pane_agent_status_changed","pane_id":"{pane_id}","workspace_id":"{workspace_id}","agent_status":"{status}","agent":"claude","display_agent":"Claude","title":"{title}"}}}}"#
    )
}

pub(crate) fn pane_event_json(kind: &str, pane_id: &str, workspace_id: &str) -> String {
    format!(
        r#"{{"event":"{kind}","data":{{"type":"{kind}","pane_id":"{pane_id}","workspace_id":"{workspace_id}"}}}}"#
    )
}

pub(crate) fn feed_status(
    state_dir: &Path,
    now_ms: u64,
    pane: &str,
    ws: &str,
    status: &str,
    title: &str,
) {
    let mut rt = runtime(state_dir.to_path_buf(), now_ms);
    rt.event_json = Some(status_event_json(pane, ws, status, title));
    crate::queue::on_status_changed(&rt).expect("status-changed should succeed");
}
