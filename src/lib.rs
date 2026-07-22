//! herdr-checkin — a durable attention queue for agent panes.
//!
//! Herdr's native `open_notification_target` only jumps to the toast that is
//! currently on screen, so a ping is lost once its toast fades and multiple
//! pings cannot queue. This plugin remembers them: agents that go `blocked`
//! (need input) or `done` (finished) are enqueued FIFO, and the `next` action
//! jumps to the oldest waiter and pops it. Entries are evicted when their pane
//! is focused, returns to `working`, or closes.
//!
//! herdr invokes this binary once per event and once per action; all state
//! lives in `state.json` under `HERDR_PLUGIN_STATE_DIR`, guarded by a lockfile.

use std::env;
use std::path::PathBuf;

mod actions;
mod herdr;
mod pane;
mod queue;
mod roster;
mod state;
#[cfg(test)]
pub(crate) mod test_support;

use actions::{next, peek, roster, startup};
use herdr::{enrich_location, CliHerdr};
use queue::{on_closed, on_focused, on_status_changed};

pub(crate) use actions::{agent_label, clear, entry_destination, entry_detail};
pub(crate) use herdr::Herdr;
pub(crate) use queue::evict;
pub(crate) use state::{
    current_unix_ms, load_entries, PluginError, QueueEntry, StateStore, WaitStatus,
};

/// Entrypoint used by `main`: parse argv, read the herdr-provided environment,
/// dispatch, and translate the result into a process exit code.
pub fn run_from_env() -> i32 {
    let subcommand = match parse_subcommand(env::args().skip(1)) {
        Ok(subcommand) => subcommand,
        Err(ParseCommandError::Usage(message)) => {
            eprintln!("{message}");
            return 2;
        }
    };

    let state_dir = match env_path("HERDR_PLUGIN_STATE_DIR") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    let herdr_bin_path = match env_path("HERDR_BIN_PATH") {
        Ok(path) => path,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };

    let runtime = RuntimeEnv {
        state_dir,
        event_json: env::var("HERDR_PLUGIN_EVENT_JSON").ok(),
        now_ms: current_unix_ms(),
        herdr_bin_path: herdr_bin_path.clone(),
    };
    let herdr = CliHerdr {
        bin_path: herdr_bin_path,
    };

    match run(subcommand, &runtime, &herdr) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("{error}");
            1
        }
    }
}

fn run(subcommand: Subcommand, runtime: &RuntimeEnv, herdr: &dyn Herdr) -> Result<(), PluginError> {
    match subcommand {
        Subcommand::StatusChanged => {
            on_status_changed(runtime, |event| enrich_location(herdr, event))
        }
        Subcommand::Focused => on_focused(runtime),
        Subcommand::Closed => on_closed(runtime),
        Subcommand::Next => next(runtime, herdr),
        Subcommand::Peek => peek(runtime, herdr),
        Subcommand::Clear => clear(runtime),
        Subcommand::Startup => startup(runtime, herdr),
        Subcommand::Pane => pane::run(runtime, herdr),
        Subcommand::Roster => roster(runtime, herdr),
    }
}

// --- subcommand parsing ----------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Subcommand {
    StatusChanged,
    Focused,
    Closed,
    Next,
    Peek,
    Clear,
    Startup,
    Pane,
    /// Hidden dev-only subcommand: dump the live agent roster as text (Agents-view data path).
    /// Deliberately absent from `usage()` — it is not a user-facing action.
    Roster,
}

enum ParseCommandError {
    Usage(String),
}

fn parse_subcommand<I>(args: I) -> Result<Subcommand, ParseCommandError>
where
    I: IntoIterator<Item = String>,
{
    let mut args = args.into_iter();
    let Some(raw_subcommand) = args.next() else {
        return Err(ParseCommandError::Usage(usage()));
    };
    if args.next().is_some() {
        return Err(ParseCommandError::Usage(usage()));
    }

    match raw_subcommand.as_str() {
        "status-changed" => Ok(Subcommand::StatusChanged),
        "focused" => Ok(Subcommand::Focused),
        "closed" => Ok(Subcommand::Closed),
        "next" => Ok(Subcommand::Next),
        "peek" => Ok(Subcommand::Peek),
        "clear" => Ok(Subcommand::Clear),
        "startup" => Ok(Subcommand::Startup),
        "pane" => Ok(Subcommand::Pane),
        // Hidden: parseable but intentionally left out of `usage()` (dev-only roster dump).
        "roster" => Ok(Subcommand::Roster),
        "help" | "--help" | "-h" => Err(ParseCommandError::Usage(usage())),
        other => Err(ParseCommandError::Usage(format!(
            "unknown subcommand: {other}\n{}",
            usage()
        ))),
    }
}

fn usage() -> String {
    "usage: herdr-checkin <status-changed|focused|closed|next|peek|clear|startup|pane>".to_string()
}

fn env_path(name: &str) -> Result<PathBuf, PluginError> {
    env::var_os(name)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .ok_or_else(|| {
            PluginError::new(format!(
                "{name} is not set; herdr should provide this environment variable to plugin commands"
            ))
        })
}

struct RuntimeEnv {
    state_dir: PathBuf,
    event_json: Option<String>,
    now_ms: u64,
    /// The `herdr` binary path (`HERDR_BIN_PATH`), carried so the pane's roster sampler thread can
    /// build its own [`CliHerdr`] to poll `agent list` off the render tick (the borrowed
    /// `&dyn Herdr` handed to each subcommand is neither `Send` nor `'static`, so it can't move into
    /// the worker). Every subcommand's live `Herdr` is still constructed from this same path.
    herdr_bin_path: PathBuf,
}
