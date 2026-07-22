#!/bin/sh
# Offline harness for scripts/pane-demo.tape: seed a throwaway queue and a fake `herdr` so the
# status pane can be recorded without any real agents. SOURCE this (it exports env into the shell
# vhs records); it is also safe to run standalone to inspect the seeded state.
#
#   . scripts/pane-demo-setup.sh && herdr-checkin pane
#
# No `set -e`: this is sourced into an interactive shell, so a stray failure must not kill it.

DEMO_DIR="${DEMO_DIR:-${TMPDIR:-/tmp}/herdr-checkin-pane-demo}"
STATE_DIR="$DEMO_DIR/state"
rm -rf "$DEMO_DIR"
mkdir -p "$STATE_DIR"

# Fake herdr: `agent focus` (what `Enter` calls) reports success so the jump reads cleanly; every
# other subcommand is a harmless ok. The pane never shells out for its list — it reads state.json.
FAKE_HERDR="$DEMO_DIR/fake-herdr.sh"
cat > "$FAKE_HERDR" <<'FAKE'
#!/bin/sh
case "$1 $2" in
  "agent focus")      printf '{"id":"fake","result":{"type":"agent_info"}}\n' ;;
  "notification show") printf '{"id":"fake","result":{"type":"ok"}}\n' ;;
  "pane list")        printf '{"id":"fake","result":{"type":"pane_list","panes":[]}}\n' ;;
  *)                  printf '{"id":"fake","result":{"type":"ok"}}\n' ;;
esac
FAKE
chmod +x "$FAKE_HERDR"

# Four waiters with staggered wait times (enqueued_at = now - N minutes), so the "waited" column
# shows a natural spread rather than every row reading "just now".
NOW_MS=$(( $(date +%s) * 1000 ))
ago() { echo $(( NOW_MS - $1 * 60000 )); }
cat > "$STATE_DIR/state.json" <<STATE
{"version":1,"entries":[
 {"pane_id":"api:p1","workspace_id":"api","agent":"claude","display_agent":"Claude","title":"migrate auth to JWT","status":"blocked","enqueued_at_ms":$(ago 8),"last_touched_ms":$(ago 8)},
 {"pane_id":"web:p3","workspace_id":"web","agent":"claude","display_agent":"Claude","title":"fix flaky snapshot test","status":"done","enqueued_at_ms":$(ago 5),"last_touched_ms":$(ago 5)},
 {"pane_id":"infra:p2","workspace_id":"infra","agent":"codex","display_agent":"Codex","title":"review terraform plan","status":"blocked","enqueued_at_ms":$(ago 3),"last_touched_ms":$(ago 3)},
 {"pane_id":"docs:p1","workspace_id":"docs","agent":"claude","display_agent":"Claude","title":"rewrite README intro","status":"done","enqueued_at_ms":$(ago 1),"last_touched_ms":$(ago 1)}
]}
STATE

# Put the built binary on PATH so the on-camera command reads as the real `herdr-checkin pane`,
# and point the plugin at the throwaway state + fake herdr.
export PATH="$PWD/target/release:$PATH"
export HERDR_PLUGIN_STATE_DIR="$STATE_DIR"
export HERDR_BIN_PATH="$FAKE_HERDR"
