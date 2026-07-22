#!/bin/sh
# Idempotent launcher for the status pane, run by the `open-pane` action.
#
# "Open-or-focus, toggle on repeat", scoped to the current tab:
#   - no status pane in the current tab       -> open a split (focused)
#   - a status pane exists but isn't focused    -> focus it
#   - the focused pane IS the status pane       -> close it (herdr has no hide; reopening just
#                                                  re-reads state.json, so this is cheap)
#
# herdr actions run a command (there is no declarative "open this pane" field), so this shells
# out via the injected $HERDR_BIN_PATH (falling back to `herdr` on PATH). The OPEN/FOCUS/CLOSE
# decision is computed in-process by the binary (`herdr-checkin pane-decision`, fed `pane list`
# JSON on stdin) so it is unit-tested and the returned pane id is already validated flag-safe.
# Any failure degrades to OPEN, preserving the always-open behavior.
set -u

herdr_bin="${HERDR_BIN_PATH:-herdr}"
plugin_root="${HERDR_PLUGIN_ROOT:-.}"
viewer_bin="$plugin_root/target/release/herdr-checkin"

decision="OPEN"
if [ -x "$viewer_bin" ]; then
  panes="$("$herdr_bin" pane list 2>/dev/null || true)"
  if [ -n "$panes" ]; then
    decision="$(printf '%s' "$panes" | "$viewer_bin" pane-decision 2>/dev/null || echo OPEN)"
  fi
fi

open_pane() {
  exec "$herdr_bin" plugin pane open \
    --plugin Akram012388.checkin \
    --entrypoint queue \
    --placement split \
    --direction right \
    --focus
}

# The decision is computed from a snapshot; the target pane can vanish before we act on it. If a
# FOCUS/CLOSE fails (e.g. the pane was closed in the race window), fall back to opening one rather
# than failing the action.
case "$decision" in
  "FOCUS "*)
    "$herdr_bin" plugin pane focus "${decision#FOCUS }" && exit 0
    open_pane
    ;;
  "CLOSE "*)
    "$herdr_bin" plugin pane close "${decision#CLOSE }" && exit 0
    exit 0
    ;;
  *)
    open_pane
    ;;
esac
