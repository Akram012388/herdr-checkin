#!/bin/sh
# Prepare a disposable Herdr environment for the VHS recording. This script is sourced by the tape
# while recording is hidden; it never talks to the user's live Herdr socket or config.

demo_session_root="$(mktemp -d "${TMPDIR:-/tmp}/herdr-checkin-vhs.XXXXXX")"
export XDG_CONFIG_HOME="$demo_session_root/config"
export XDG_STATE_HOME="$demo_session_root/state"
export HERDR_SOCKET_PATH="$demo_session_root/runtime/herdr.sock"
export PATH="$PWD/../../herdr/target/release:$PATH"

# VHS is launched from the developer's real Herdr pane, so its shell initially inherits Herdr's
# nested-session guard and live pane identity. Remove those values before starting the disposable
# monolithic demo; the unique XDG roots and socket below remain the isolation boundary.
unset HERDR_ENV HERDR_SESSION HERDR_CLIENT_SOCKET_PATH
unset HERDR_WORKSPACE_ID HERDR_TAB_ID HERDR_PANE_ID HERDR_BIN_PATH

mkdir -p "$XDG_CONFIG_HOME/herdr" "$XDG_STATE_HOME" "$demo_session_root/runtime"
cp "$PWD/demo/herdr-session-config.toml" "$XDG_CONFIG_HOME/herdr/config.toml"
herdr plugin link "$PWD/demo/plugin" --enabled >/dev/null
