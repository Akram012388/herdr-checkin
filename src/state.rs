//! Persisted queue state: `state.json` under `HERDR_PLUGIN_STATE_DIR`, guarded by a lockfile.
//! All mutations go through [`StateStore::update`] — a delta under the lock, never a full
//! write-back.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) const STATE_FILE_NAME: &str = "state.json";
const LOCK_FILE_NAME: &str = "state.lock";
const STATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WaitStatus {
    Blocked,
    Done,
}

impl WaitStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            WaitStatus::Blocked => "blocked",
            WaitStatus::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct QueueEntry {
    pub(crate) pane_id: String,
    pub(crate) workspace_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) display_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) title: Option<String>,
    pub(crate) status: WaitStatus,
    pub(crate) enqueued_at_ms: u64,
    /// When this entry was last enqueued or refreshed (an upsert bumps it; `enqueued_at_ms` stays
    /// put). Prune guards compare `max(enqueued_at_ms, last_touched_ms)` against the pre-lock
    /// snapshot, so a concurrent refresh of a persisted entry — its `enqueued_at_ms` predating the
    /// snapshot — is not mistaken for stale and dropped. `serde(default)` keeps pre-0.2.x state
    /// files (which lack the field) loadable: they read `0`, and `max` falls back to
    /// `enqueued_at_ms`, exactly reproducing the old behavior until the entry is next touched.
    #[serde(default)]
    pub(crate) last_touched_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedState {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    entries: Vec<QueueEntry>,
}

pub(crate) struct LoadedState {
    pub(crate) entries: Vec<QueueEntry>,
    needs_repair: bool,
}

pub(crate) struct StateStore {
    state_dir: PathBuf,
}

impl StateStore {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// Load the queue under an exclusive lock, apply `change`, and persist the
    /// result if it changed (or if the on-disk form needed repair).
    pub(crate) fn update<T>(
        &self,
        change: impl FnOnce(Vec<QueueEntry>) -> (Vec<QueueEntry>, T),
    ) -> Result<T, PluginError> {
        fs::create_dir_all(&self.state_dir).map_err(|error| {
            PluginError::new(format!(
                "failed to create plugin state directory {}: {error}",
                self.state_dir.display()
            ))
        })?;

        let _lock = StateLock::acquire(&self.state_dir.join(LOCK_FILE_NAME))?;
        let loaded = read_state(&self.state_dir.join(STATE_FILE_NAME))?;
        let previous = loaded.entries.clone();
        let (next, result) = change(loaded.entries);

        if loaded.needs_repair || next != previous {
            write_state(&self.state_dir.join(STATE_FILE_NAME), &next)?;
        }

        Ok(result)
    }
}

struct StateLock {
    file: File,
}

impl StateLock {
    fn acquire(path: &Path) -> Result<Self, PluginError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to open plugin state lock {}: {error}",
                    path.display()
                ))
            })?;

        file.lock_exclusive().map_err(|error| {
            PluginError::new(format!(
                "failed to lock plugin state {}: {error}",
                path.display()
            ))
        })?;

        Ok(Self { file })
    }
}

impl Drop for StateLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

pub(crate) fn read_state(path: &Path) -> Result<LoadedState, PluginError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedState {
                entries: Vec::new(),
                needs_repair: false,
            });
        }
        Err(error) => {
            return Err(PluginError::new(format!(
                "failed to read plugin state {}: {error}",
                path.display()
            )));
        }
    };

    match serde_json::from_str::<PersistedState>(&contents) {
        Ok(state) => Ok(LoadedState {
            // Rewrite (to stamp the current version) only when the file is from an older schema.
            // A forward-version file — written by a newer plugin — is left as-is here rather than
            // silently rewritten down to our version, so we don't strip fields we can't model.
            needs_repair: state.version < STATE_VERSION,
            entries: state.entries,
        }),
        Err(_) => Ok(LoadedState {
            entries: Vec::new(),
            needs_repair: true,
        }),
    }
}

fn write_state(path: &Path, entries: &[QueueEntry]) -> Result<(), PluginError> {
    let parent = path.parent().ok_or_else(|| {
        PluginError::new(format!(
            "plugin state path has no parent directory: {}",
            path.display()
        ))
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        PluginError::new(format!(
            "failed to create plugin state directory {}: {error}",
            parent.display()
        ))
    })?;

    let temp_path = parent.join(format!(
        ".{STATE_FILE_NAME}.tmp.{}.{}",
        std::process::id(),
        current_unix_ms()
    ));
    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| {
            PluginError::new(format!(
                "failed to create temporary plugin state {}: {error}",
                temp_path.display()
            ))
        })?;
    let state = PersistedState {
        version: STATE_VERSION,
        entries: entries.to_vec(),
    };
    // Any failure after the temp file exists must not leave it behind as litter in the state dir.
    serde_json::to_writer_pretty(&mut temp_file, &state).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to serialize plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.write_all(b"\n").map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to write plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.sync_all().map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to sync plugin state {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to replace plugin state {}: {error}",
            path.display()
        ))
    })
}

/// Read the current queue for read-only display (the status pane). Reads without the lock —
/// writes are atomic temp+rename, so a reader always sees a complete file — and degrades to an
/// empty queue on any error. Mutations must still go through [`StateStore::update`].
pub(crate) fn load_entries(state_dir: &Path) -> Vec<QueueEntry> {
    read_state(&state_dir.join(STATE_FILE_NAME))
        .map(|loaded| loaded.entries)
        .unwrap_or_default()
}

pub(crate) fn current_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginError {
    message: String,
}

impl PluginError {
    pub(crate) fn new(message: String) -> Self {
        Self { message }
    }
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PluginError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{feed_status, load, temp_state_dir};
    use std::fs;

    #[test]
    fn state_file_without_last_touched_loads_and_defaults_to_zero() {
        // Backward compatibility: a pre-0.2.x state.json has no last_touched_ms field.
        let dir = temp_state_dir("legacy-state");
        fs::write(
            dir.join(STATE_FILE_NAME),
            r#"{"version":1,"entries":[{"pane_id":"w1:p1","workspace_id":"w1","status":"blocked","enqueued_at_ms":1000}]}"#,
        )
        .expect("legacy state should write");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].enqueued_at_ms, 1_000);
        assert_eq!(entries[0].last_touched_ms, 0, "missing field defaults to 0");
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn malformed_state_is_repaired_to_empty() {
        let dir = temp_state_dir("malformed");
        fs::write(dir.join(STATE_FILE_NAME), "not json").expect("write malformed state");
        feed_status(&dir, 1_000, "w1:p1", "w1", "blocked", "x");
        let entries = load(&dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pane_id, "w1:p1");
        let _ = fs::remove_dir_all(dir);
    }
}
