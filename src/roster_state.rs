//! Persisted roster registry: `roster.json` under `HERDR_PLUGIN_STATE_DIR`, guarded by its own
//! lockfile — a **separate store from `state.json`** (design §5). It holds only the time-in-state
//! registry. All mutations go through [`RosterStore::update`] — a delta
//! under the lock, temp+rename, exactly like [`crate::state::StateStore`].
//!
//! **Invariant #7 — this file is a prunable observation cache.** Nothing correctness-critical may
//! live *only* here: deleting `roster.json` must merely degrade timers, never lose a ping. The
//! queue's durability lives in `state.json`; this store is best-effort throughout (every writer
//! ignores its errors), so a corrupt/absent registry simply renders honest `~` timers.
//!
//! **Provenance (design §4 — the correctness fix).** The popup is a summon-and-glance modal, not
//! running most of the day, so its poll loop *cannot* be the observer of state transitions — it
//! would render a fabricated `0s` at open. Instead the **`status-changed` event binary** (which
//! fires on every transition, popup open or not, with a wall clock in hand) stamps
//! `status_since_ms` via [`stamp_status`]. The pane sampler only **reads** the timer and
//! **back-fills** the session uuid ([`reconcile_pane`]); it resets the timer only on a genuine
//! identity change (a reused pane slot), never on a mere status difference.

use crate::roster::RosterAgent;
use crate::state::{current_unix_ms, PluginError};
use crate::RuntimeEnv;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub(crate) const ROSTER_FILE_NAME: &str = "roster.json";
const LOCK_FILE_NAME: &str = "roster.lock";
const ROSTER_VERSION: u32 = 1;

/// One pane's observed state in the registry, keyed by `pane_id` in [`Registry`]. `agent_session`
/// (the stable session uuid) is `None` until the pane sampler back-fills it — the event payload that
/// stamps transitions carries no uuid (design §4). `status_since_ms` is the wall clock of the last
/// transition into `status`; `first_seen_ms`/`last_seen_ms` bracket the pane's observed lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RegistryEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) agent_session: Option<String>,
    pub(crate) status: String,
    pub(crate) status_since_ms: u64,
    pub(crate) first_seen_ms: u64,
    pub(crate) last_seen_ms: u64,
}

/// The time-in-state registry: `pane_id -> RegistryEntry`. A `BTreeMap` so the on-disk JSON is
/// deterministic (sorted keys) — stable diffs, stable tests.
pub(crate) type Registry = BTreeMap<String, RegistryEntry>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedRoster {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    agents: Registry,
}

struct LoadedRoster {
    registry: Registry,
    needs_repair: bool,
}

pub(crate) struct RosterStore {
    state_dir: PathBuf,
}

impl RosterStore {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            state_dir: state_dir.to_path_buf(),
        }
    }

    /// Load the registry under an exclusive lock, apply `change`, and persist the result only if it
    /// changed (or the on-disk form needed repair) — the same delta-under-lock discipline as
    /// [`crate::state::StateStore::update`], on `roster.json`'s own lock.
    pub(crate) fn update<T>(
        &self,
        change: impl FnOnce(Registry) -> (Registry, T),
    ) -> Result<T, PluginError> {
        fs::create_dir_all(&self.state_dir).map_err(|error| {
            PluginError::new(format!(
                "failed to create plugin state directory {}: {error}",
                self.state_dir.display()
            ))
        })?;

        let _lock = RosterLock::acquire(&self.state_dir.join(LOCK_FILE_NAME))?;
        let loaded = read_roster(&self.state_dir.join(ROSTER_FILE_NAME))?;
        let previous = loaded.registry.clone();
        let (next, result) = change(loaded.registry);

        if loaded.needs_repair || next != previous {
            write_roster(&self.state_dir.join(ROSTER_FILE_NAME), &next)?;
        }

        Ok(result)
    }
}

struct RosterLock {
    file: File,
}

impl RosterLock {
    fn acquire(path: &Path) -> Result<Self, PluginError> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|error| {
                PluginError::new(format!(
                    "failed to open roster lock {}: {error}",
                    path.display()
                ))
            })?;

        file.lock_exclusive().map_err(|error| {
            PluginError::new(format!("failed to lock roster {}: {error}", path.display()))
        })?;

        Ok(Self { file })
    }
}

impl Drop for RosterLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn read_roster(path: &Path) -> Result<LoadedRoster, PluginError> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedRoster {
                registry: Registry::new(),
                needs_repair: false,
            });
        }
        Err(error) => {
            return Err(PluginError::new(format!(
                "failed to read roster {}: {error}",
                path.display()
            )));
        }
    };

    match serde_json::from_str::<PersistedRoster>(&contents) {
        Ok(state) => Ok(LoadedRoster {
            needs_repair: state.version < ROSTER_VERSION,
            registry: state.agents,
        }),
        // A prunable cache: a corrupt file degrades to an empty registry (honest `~` timers), never
        // an error that could stall the event/queue path that best-effort-calls us.
        Err(_) => Ok(LoadedRoster {
            registry: Registry::new(),
            needs_repair: true,
        }),
    }
}

fn write_roster(path: &Path, registry: &Registry) -> Result<(), PluginError> {
    let parent = path.parent().ok_or_else(|| {
        PluginError::new(format!(
            "roster path has no parent directory: {}",
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
        ".{ROSTER_FILE_NAME}.tmp.{}.{}",
        std::process::id(),
        current_unix_ms()
    ));
    let mut temp_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp_path)
        .map_err(|error| {
            PluginError::new(format!(
                "failed to create temporary roster {}: {error}",
                temp_path.display()
            ))
        })?;
    let state = PersistedRoster {
        version: ROSTER_VERSION,
        agents: registry.clone(),
    };
    // Any failure after the temp file exists must not leave it behind as litter in the state dir.
    serde_json::to_writer_pretty(&mut temp_file, &state).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to serialize roster {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.write_all(b"\n").map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to write roster {}: {error}",
            temp_path.display()
        ))
    })?;
    temp_file.sync_all().map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to sync roster {}: {error}",
            temp_path.display()
        ))
    })?;
    drop(temp_file);

    fs::rename(&temp_path, path).map_err(|error| {
        let _ = fs::remove_file(&temp_path);
        PluginError::new(format!(
            "failed to replace roster {}: {error}",
            path.display()
        ))
    })
}

/// Read the registry without the lock — writes are atomic temp+rename, so a reader always sees a
/// complete file — degrading to an empty registry on any error (invariant #7). The analogue of
/// [`crate::state::load_entries`]. Currently only the tests read the registry out-of-band: production
/// reads happen inside [`reconcile_roster`]'s locked update (which both reads and back-fills in one
/// pass), so this is gated to the test build to keep the release binary warning-clean.
#[cfg(test)]
pub(crate) fn load_registry(state_dir: &Path) -> Registry {
    read_roster(&state_dir.join(ROSTER_FILE_NAME))
        .map(|loaded| loaded.registry)
        .unwrap_or_default()
}

// --- pure registry transitions ---------------------------------------------

/// Stamp a status transition for `pane_id` (the event binary's job). A *new* pane is inserted with
/// its timer starting now; an *existing* pane resets `status_since_ms` only when the status actually
/// changed (a re-emitted identical status just refreshes `last_seen_ms`, never restarting the
/// clock). The session uuid is left untouched — the event payload carries none, so the pane sampler
/// back-fills it ([`reconcile_pane`]).
pub(crate) fn stamp_status(registry: &mut Registry, pane_id: &str, status: &str, now_ms: u64) {
    match registry.get_mut(pane_id) {
        Some(entry) => {
            if entry.status != status {
                entry.status = status.to_string();
                entry.status_since_ms = now_ms;
            }
            entry.last_seen_ms = now_ms;
        }
        None => {
            registry.insert(
                pane_id.to_string(),
                RegistryEntry {
                    agent_session: None,
                    status: status.to_string(),
                    status_since_ms: now_ms,
                    first_seen_ms: now_ms,
                    last_seen_ms: now_ms,
                },
            );
        }
    }
}

/// Seed a pane into the registry **additively** for `startup` (invariant #4): a missing pane gets a
/// fresh entry timed from now, but an existing entry is left entirely untouched — its
/// `status_since_ms` (which may predate a herdr restart, or have been event-stamped since) is never
/// reset. That makes `startup` idempotent: a second run finds every pane present and changes nothing.
pub(crate) fn seed_status(registry: &mut Registry, pane_id: &str, status: &str, now_ms: u64) {
    registry
        .entry(pane_id.to_string())
        .or_insert_with(|| RegistryEntry {
            agent_session: None,
            status: status.to_string(),
            status_since_ms: now_ms,
            first_seen_ms: now_ms,
            last_seen_ms: now_ms,
        });
}

/// The pane sampler's per-pane reconcile: back-fill the live session uuid and detect a reused pane
/// slot, returning the `status_since_ms` the row should trust (or `None` — render `~` — when the
/// pane has no registry entry). This never fabricates a transition time from a status difference
/// (design §4); it resets the timer *only* when the recorded uuid disagrees with the live one, which
/// means a **different agent** now occupies this positional pane id and the old timer is not ours.
pub(crate) fn reconcile_pane(
    registry: &mut Registry,
    pane_id: &str,
    live_session: Option<&str>,
    live_status: &str,
    now_ms: u64,
) -> Option<u64> {
    let entry = registry.get_mut(pane_id)?;
    match (entry.agent_session.as_deref(), live_session) {
        // Reused slot: a different session now. Reset to an honest "seen since now" for the new
        // agent, adopting its live status so the registry stays coherent until its next event.
        (Some(stored), Some(live)) if stored != live => {
            *entry = RegistryEntry {
                agent_session: Some(live.to_string()),
                status: live_status.to_string(),
                status_since_ms: now_ms,
                first_seen_ms: now_ms,
                last_seen_ms: now_ms,
            };
        }
        // First time the pane learns its uuid (the event stamps carry none): back-fill only, keeping
        // the event-stamped timer intact.
        (None, Some(live)) => entry.agent_session = Some(live.to_string()),
        // Matching uuid, or a session-less agent we can't disambiguate: trust the recorded timer.
        _ => {}
    }
    Some(entry.status_since_ms)
}

// --- runtime bridges (best-effort throughout — invariant #7) ---------------

/// Stamp the roster registry from a `status-changed` event, best-effort. Called from the dispatch
/// layer *after* the queue mutation, so `queue.rs` never learns the registry exists (mirroring how
/// `enrich_location` is injected). Unlike the queue path this fires for **every** `agent_status` —
/// `idle`/`working` included — because the Agents view times every agent, not just waiters. An
/// absent/unparseable event or a `roster.json` failure is swallowed: a timer is never worth failing
/// the event that keeps the durable queue correct.
pub(crate) fn stamp_status_changed(runtime: &RuntimeEnv) {
    let Some(event) = runtime
        .event_json
        .as_deref()
        .and_then(crate::herdr::parse_status_event)
    else {
        return;
    };
    let now_ms = runtime.now_ms;
    let _ = RosterStore::new(&runtime.state_dir).update(|mut registry| {
        stamp_status(&mut registry, &event.pane_id, &event.agent_status, now_ms);
        (registry, ())
    });
}

/// Seed the registry from `startup`'s `pane list`, additively (invariant #4), best-effort. Runs
/// after the queue re-seed so a `roster.json` problem can never abort re-seeding the durable queue.
pub(crate) fn seed_registry<'a>(
    runtime: &RuntimeEnv,
    panes: impl IntoIterator<Item = (&'a str, &'a str)>,
) {
    let now_ms = runtime.now_ms;
    let _ = RosterStore::new(&runtime.state_dir).update(|mut registry| {
        for (pane_id, status) in panes {
            seed_status(&mut registry, pane_id, status, now_ms);
        }
        (registry, ())
    });
}

/// Reconcile a freshly sampled roster against the registry (on the sampler thread), filling each
/// agent's [`RosterAgent::status_since_ms`] for the row's time-in-state. Back-fills session uuids and
/// resets reused-slot timers as a delta — a no-op write in steady state (every uuid already recorded
/// and matching). Best-effort: a `roster.json` failure leaves every `status_since_ms` at `None`, so
/// the rows honestly show `~` rather than blanking (invariant #7).
pub(crate) fn reconcile_roster(state_dir: &Path, agents: &mut [RosterAgent], now_ms: u64) {
    let result = RosterStore::new(state_dir).update(|mut registry| {
        let sinces: Vec<Option<u64>> = agents
            .iter()
            .map(|agent| {
                reconcile_pane(
                    &mut registry,
                    &agent.pane_id,
                    agent.agent_session.as_deref(),
                    agent.agent_status.as_str(),
                    now_ms,
                )
            })
            .collect();
        (registry, sinces)
    });
    if let Ok(sinces) = result {
        for (agent, since) in agents.iter_mut().zip(sinces) {
            agent.status_since_ms = since;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::AgentStatus;
    use crate::test_support::temp_state_dir;
    use std::fs;

    fn entry(registry: &Registry, pane_id: &str) -> RegistryEntry {
        registry
            .get(pane_id)
            .cloned()
            .unwrap_or_else(|| panic!("expected a registry entry for {pane_id}"))
    }

    #[test]
    fn stamp_status_inserts_a_new_pane_timed_from_now() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.status, "blocked");
        assert_eq!(e.status_since_ms, 1_000);
        assert_eq!(e.first_seen_ms, 1_000);
        assert_eq!(e.agent_session, None, "the event carries no session uuid");
    }

    #[test]
    fn stamp_status_resets_the_clock_only_on_a_real_transition() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "working", 1_000);
        // Same status re-emitted: refresh last_seen but keep the clock.
        stamp_status(&mut registry, "w1:p1", "working", 2_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(
            e.status_since_ms, 1_000,
            "an identical status keeps the clock"
        );
        assert_eq!(e.last_seen_ms, 2_000, "but last_seen advances");
        // A genuine transition restarts the clock.
        stamp_status(&mut registry, "w1:p1", "blocked", 3_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.status, "blocked");
        assert_eq!(e.status_since_ms, 3_000);
        assert_eq!(e.first_seen_ms, 1_000, "first_seen never moves");
    }

    #[test]
    fn seed_status_is_additive_and_idempotent() {
        let mut registry = Registry::new();
        seed_status(&mut registry, "w1:p1", "blocked", 1_000);
        // A surviving entry is never reset by a later seed (invariant #4).
        seed_status(&mut registry, "w1:p1", "done", 9_000);
        let e = entry(&registry, "w1:p1");
        assert_eq!(
            e.status, "blocked",
            "seed never overwrites a surviving entry"
        );
        assert_eq!(e.status_since_ms, 1_000, "and never resets its clock");
    }

    #[test]
    fn reconcile_backfills_the_uuid_without_touching_the_timer() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let since = reconcile_pane(&mut registry, "w1:p1", Some("uuid-A"), "blocked", 5_000);
        assert_eq!(
            since,
            Some(1_000),
            "the event-stamped timer is trusted, not reset"
        );
        assert_eq!(
            entry(&registry, "w1:p1").agent_session.as_deref(),
            Some("uuid-A")
        );
    }

    #[test]
    fn reconcile_resets_the_timer_on_a_reused_pane_slot() {
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        reconcile_pane(&mut registry, "w1:p1", Some("uuid-A"), "blocked", 2_000);
        // A different session now occupies the same positional pane id: the old timer is not ours.
        let since = reconcile_pane(&mut registry, "w1:p1", Some("uuid-B"), "working", 8_000);
        assert_eq!(since, Some(8_000), "a reused slot resets to seen-since-now");
        let e = entry(&registry, "w1:p1");
        assert_eq!(e.agent_session.as_deref(), Some("uuid-B"));
        assert_eq!(
            e.status, "working",
            "and adopts the new agent's live status"
        );
    }

    #[test]
    fn reconcile_is_none_for_a_pane_with_no_registry_entry() {
        let mut registry = Registry::new();
        // No entry, no honest reading -> None -> the row renders `~`.
        assert_eq!(
            reconcile_pane(&mut registry, "w9:p9", Some("uuid-Z"), "idle", 1_000),
            None
        );
        assert!(registry.is_empty(), "reconcile never creates entries");
    }

    #[test]
    fn reconcile_trusts_a_sessionless_agents_timer() {
        // Some agents list no session uuid (seen live for a non-Claude/Codex agent). We can't detect
        // a reuse, so we trust the recorded timer rather than guessing.
        let mut registry = Registry::new();
        stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
        let since = reconcile_pane(&mut registry, "w1:p1", None, "blocked", 5_000);
        assert_eq!(since, Some(1_000));
    }

    #[test]
    fn store_round_trips_through_the_lock_and_repairs_junk() {
        let dir = temp_state_dir("roster-store");
        let store = RosterStore::new(&dir);
        store
            .update(|mut registry| {
                stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
                (registry, ())
            })
            .expect("first write should succeed");
        let loaded = load_registry(&dir);
        assert_eq!(
            loaded.get("w1:p1").map(|e| e.status.as_str()),
            Some("blocked")
        );

        // A corrupt file degrades to empty and is repaired on the next write (invariant #7).
        fs::write(dir.join(ROSTER_FILE_NAME), "not json").expect("clobber the roster");
        assert!(
            load_registry(&dir).is_empty(),
            "junk reads as an empty registry"
        );
        store
            .update(|mut registry| {
                stamp_status(&mut registry, "w2:p1", "done", 2_000);
                (registry, ())
            })
            .expect("write over junk should succeed");
        let loaded = load_registry(&dir);
        assert_eq!(loaded.len(), 1, "the junk was repaired, not merged");
        assert!(loaded.contains_key("w2:p1"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_roster_fills_status_since_and_survives_a_missing_file() {
        let dir = temp_state_dir("roster-reconcile");
        // Seed one pane via a transition stamp; leave a second pane unknown to the registry.
        RosterStore::new(&dir)
            .update(|mut registry| {
                stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
                (registry, ())
            })
            .expect("seed write");

        let mut agents = vec![
            sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A")),
            sample_agent("w2:p1", AgentStatus::Working, Some("uuid-B")),
        ];
        reconcile_roster(&dir, &mut agents, 5_000);
        assert_eq!(
            agents[0].status_since_ms,
            Some(1_000),
            "known pane trusts its timer"
        );
        assert_eq!(agents[1].status_since_ms, None, "unknown pane stays `~`");

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn stamp_status_changed_is_a_data_path_from_the_event_json() {
        // The dispatch bridge: a raw status-changed event JSON stamps the registry (acceptance #2 at
        // the lib level; tests/cli.rs proves the same through the built binary).
        let dir = temp_state_dir("roster-stamp-bridge");
        let mut rt = crate::test_support::runtime(dir.clone(), 7_000);
        rt.event_json = Some(crate::test_support::status_event_json(
            "w1:p1", "w1", "working", "building",
        ));
        stamp_status_changed(&rt);
        let entry = entry(&load_registry(&dir), "w1:p1");
        assert_eq!(entry.status, "working");
        assert_eq!(entry.status_since_ms, 7_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn seed_registry_is_additive_and_idempotent_across_runs() {
        // startup idempotence on roster.json (acceptance #3): a second seed, even at a later clock,
        // leaves every surviving entry's timer untouched.
        let dir = temp_state_dir("roster-seed-idempotent");
        let panes = [("w1:p1", "blocked"), ("w2:p1", "idle")];
        let rt_a = crate::test_support::runtime(dir.clone(), 1_000);
        seed_registry(&rt_a, panes.iter().map(|(p, s)| (*p, *s)));
        let rt_b = crate::test_support::runtime(dir.clone(), 9_000);
        seed_registry(&rt_b, panes.iter().map(|(p, s)| (*p, *s)));

        let registry = load_registry(&dir);
        assert_eq!(registry.len(), 2);
        assert_eq!(entry(&registry, "w1:p1").status_since_ms, 1_000);
        assert_eq!(entry(&registry, "w2:p1").status_since_ms, 1_000);
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn invariant_7_deleting_roster_json_only_degrades_timers() {
        // roster.json is a prunable observation cache: deleting it must lose no correctness — the
        // registry reads back empty, and reconcile then renders honest `~` (None) instead of a timer.
        let dir = temp_state_dir("roster-invariant-7");
        RosterStore::new(&dir)
            .update(|mut registry| {
                stamp_status(&mut registry, "w1:p1", "blocked", 1_000);
                (registry, ())
            })
            .expect("seed the registry");
        fs::remove_file(dir.join(ROSTER_FILE_NAME)).expect("delete roster.json");

        assert!(
            load_registry(&dir).is_empty(),
            "a missing file degrades to empty"
        );
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Blocked, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 5_000);
        assert_eq!(
            agents[0].status_since_ms, None,
            "with the file gone the row shows `~`, never a stale or fabricated timer"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn reconcile_roster_never_writes_state_json() {
        // The pane's roster path touches only roster.json (acceptance #1): reconciling must never
        // create or write state.json — the durable queue store is off-limits to the live view.
        let dir = temp_state_dir("roster-no-state-write");
        let mut agents = vec![sample_agent("w1:p1", AgentStatus::Working, Some("uuid-A"))];
        reconcile_roster(&dir, &mut agents, 1_000);
        assert!(
            !dir.join(crate::state::STATE_FILE_NAME).exists(),
            "the roster path must not write state.json"
        );
        let _ = fs::remove_dir_all(dir);
    }

    fn sample_agent(pane_id: &str, status: AgentStatus, session: Option<&str>) -> RosterAgent {
        RosterAgent {
            pane_id: pane_id.to_string(),
            workspace_id: pane_id.split(':').next().unwrap_or("w1").to_string(),
            tab_id: None,
            agent: Some("claude".to_string()),
            agent_status: status,
            agent_session: session.map(str::to_string),
            cwd: None,
            focused: false,
            terminal_title: None,
            status_since_ms: None,
            workspace_label: None,
            tab_label: None,
            pane_label: None,
            last_line: None,
        }
    }
}
