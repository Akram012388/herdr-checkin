#!/bin/sh
set -u

# Herdr protects and injects HERDR_BIN_PATH for plugin panes. Override it inside this demo-only
# wrapper so the Check-in roster reads deterministic fixture data; popup creation itself still went
# through the real isolated Herdr process.
export HERDR_BIN_PATH="$HERDR_PLUGIN_ROOT/fake-herdr.sh"
exec "$HERDR_PLUGIN_ROOT/../../target/release/herdr-checkin" pane
