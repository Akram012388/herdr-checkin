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

# Fake herdr: deterministic Queue liveness plus a live Agents roster, label maps, terminal tails,
# and successful prompt/focus actions. No real agents or user paths are involved.
FAKE_HERDR="$DEMO_DIR/fake-herdr.sh"
cat > "$FAKE_HERDR" <<'FAKE'
#!/bin/sh
case "$1 $2" in
  "agent list")
    printf '%s\n' '{"id":"fake","result":{"type":"agent_list","agents":[{"agent":"claude","agent_session":{"value":"00000000-0000-0000-0000-000000000001"},"agent_status":"blocked","focused":false,"pane_id":"wA:p1","tab_id":"wA:t1","terminal_title":"auth migration","workspace_id":"wA"},{"agent":"codex","agent_session":{"value":"00000000-0000-0000-0000-000000000002"},"agent_status":"working","focused":true,"pane_id":"wA:p2","tab_id":"wA:t2","terminal_title":"reviewing API changes","workspace_id":"wA"},{"agent":"amp","agent_session":{"value":"00000000-0000-0000-0000-000000000003"},"agent_status":"done","focused":false,"pane_id":"wI:p3","tab_id":"wI:t1","terminal_title":"deployment plan","workspace_id":"wI"},{"agent":"codex","agent_session":{"value":"00000000-0000-0000-0000-000000000004"},"agent_status":"idle","focused":false,"pane_id":"wD:pA","tab_id":"wD:t1","terminal_title":"documentation pass","workspace_id":"wD"}]}}'
    ;;
  "agent read")
    case "$3" in
      "wA:p1") printf '%s\n' 'Need confirmation on token expiry.' ;;
      "wA:p2") printf '%s\n' 'Review complete; tests are green.' ;;
      "wI:p3") printf '%s\n' 'Deployment plan ready.' ;;
      "wD:pA") printf '%s\n' 'Waiting for the next task.' ;;
    esac
    ;;
  "agent focus"|"agent prompt")
    printf '{"id":"fake","result":{"type":"agent_info"}}\n'
    ;;
  "notification show") printf '{"id":"fake","result":{"type":"ok"}}\n' ;;
  "workspace list")
    printf '%s\n' '{"id":"fake","result":{"type":"workspace_list","workspaces":[{"workspace_id":"wA","label":"API platform"},{"workspace_id":"wI","label":"Infrastructure"},{"workspace_id":"wD","label":"Documentation"}]}}'
    ;;
  "tab list")
    printf '%s\n' '{"id":"fake","result":{"type":"tab_list","tabs":[{"tab_id":"wA:t1","label":"auth"},{"tab_id":"wA:t2","label":"review"},{"tab_id":"wI:t1","label":"amp"},{"tab_id":"wD:t1","label":"docs"}]}}'
    ;;
  "pane list")
    printf '%s\n' '{"id":"fake","result":{"type":"pane_list","panes":[{"pane_id":"wA:p1","workspace_id":"wA","tab_id":"wA:t1","agent_status":"blocked","agent":"claude"},{"pane_id":"wA:p2","workspace_id":"wA","tab_id":"wA:t2","agent_status":"working","agent":"codex"},{"pane_id":"wI:p3","workspace_id":"wI","tab_id":"wI:t1","agent_status":"done","agent":"amp"},{"pane_id":"wD:pA","workspace_id":"wD","tab_id":"wD:t1","agent_status":"idle","agent":"codex"}]}}'
    ;;
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
 {"pane_id":"wA:p1","workspace_id":"wA","tab_id":"wA:t1","workspace_label":"API platform","tab_label":"auth","pane_label":null,"agent":"claude","display_agent":"Claude","title":"confirm token expiry","status":"blocked","enqueued_at_ms":$(ago 8),"last_touched_ms":$(ago 8)},
 {"pane_id":"wI:p3","workspace_id":"wI","tab_id":"wI:t1","workspace_label":"Infrastructure","tab_label":"amp","pane_label":null,"agent":"amp","display_agent":"Amp","title":"deployment plan ready","status":"done","enqueued_at_ms":$(ago 5),"last_touched_ms":$(ago 5)},
 {"pane_id":"wD:pA","workspace_id":"wD","tab_id":"wD:t1","workspace_label":"Documentation","tab_label":"docs","pane_label":null,"agent":"codex","display_agent":"Codex","title":"documentation pass complete","status":"done","enqueued_at_ms":$(ago 2),"last_touched_ms":$(ago 2)}
]}
STATE

# Seed trustworthy time-in-state values for the live roster.
cat > "$STATE_DIR/roster.json" <<ROSTER
{"version":1,"agents":{
 "wA:p1":{"agent_session":"00000000-0000-0000-0000-000000000001","status":"blocked","status_since_ms":$(ago 8),"first_seen_ms":$(ago 10),"last_seen_ms":$NOW_MS},
 "wA:p2":{"agent_session":"00000000-0000-0000-0000-000000000002","status":"working","status_since_ms":$(ago 3),"first_seen_ms":$(ago 9),"last_seen_ms":$NOW_MS},
 "wI:p3":{"agent_session":"00000000-0000-0000-0000-000000000003","status":"done","status_since_ms":$(ago 5),"first_seen_ms":$(ago 12),"last_seen_ms":$NOW_MS},
 "wD:pA":{"agent_session":"00000000-0000-0000-0000-000000000004","status":"idle","status_since_ms":$(ago 1),"first_seen_ms":$(ago 7),"last_seen_ms":$NOW_MS}
}}
ROSTER

# Put the built binary on PATH so the on-camera command reads as the real `herdr-checkin pane`,
# and point the plugin at the throwaway state + fake herdr.
export PATH="$PWD/target/release:$PATH"
export HERDR_PLUGIN_STATE_DIR="$STATE_DIR"
export HERDR_BIN_PATH="$FAKE_HERDR"
export HERDR_PLUGIN_PANE_THEME_JSON='{"schema_version":1,"name":"tokyonight-demo","palette":{"accent":{"kind":"rgb","r":122,"g":162,"b":247},"panel_bg":{"kind":"rgb","r":26,"g":27,"b":38},"surface0":{"kind":"rgb","r":36,"g":40,"b":59},"surface1":{"kind":"rgb","r":41,"g":46,"b":66},"surface_dim":{"kind":"rgb","r":22,"g":22,"b":30},"overlay0":{"kind":"rgb","r":86,"g":95,"b":137},"overlay1":{"kind":"rgb","r":120,"g":134,"b":180},"text":{"kind":"rgb","r":192,"g":202,"b":245},"subtext0":{"kind":"rgb","r":169,"g":177,"b":214},"mauve":{"kind":"rgb","r":187,"g":154,"b":247},"green":{"kind":"rgb","r":158,"g":206,"b":106},"yellow":{"kind":"rgb","r":224,"g":175,"b":104},"red":{"kind":"rgb","r":247,"g":118,"b":142},"blue":{"kind":"rgb","r":122,"g":162,"b":247},"teal":{"kind":"rgb","r":125,"g":207,"b":255},"peach":{"kind":"rgb","r":255,"g":158,"b":100}}}'
