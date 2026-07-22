#!/bin/sh
# Launcher for the status pane, run by the `open-pane` action.
#
# Opens the pane as a centered, session-modal herdr popup (`--placement popup`) — the same class
# of floating modal as herdr's own prefix+s settings, drawn over the whole session rather than
# inside a tab. A popup is a session-level singleton, so no open/focus/close toggle is needed:
# the summon key opens it, and the pane dismisses itself on `q`/`Esc` (it calls the `popup.close`
# socket method on exit, gated on HERDR_CHECKIN_POPUP so only a popup-launched pane does so).
#
# herdr actions run a command, so this shells out via the injected $HERDR_BIN_PATH (falling back
# to `herdr` on PATH). `--width`/`--height` take a "NN%" percentage of the available area (herdr's
# PopupSize), so the modal scales to roughly half the screen like the settings popup.
set -u

herdr_bin="${HERDR_BIN_PATH:-herdr}"

exec "$herdr_bin" plugin pane open \
  --plugin Akram012388.checkin \
  --entrypoint queue \
  --placement popup \
  --width 60% \
  --height 55% \
  --env HERDR_CHECKIN_POPUP=1 \
  --focus
