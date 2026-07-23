#!/bin/sh
# Deterministic read-only Herdr CLI fixture used only inside the demo popup.

case "$1 $2" in
  "agent list")
    printf '%s\n' '{"id":"demo","result":{"type":"agent_list","agents":[{"agent":"codex","agent_session":{"value":"00000000-0000-0000-0000-000000000001"},"agent_status":"idle","focused":true,"pane_id":"w1:p1","tab_id":"w1:t1","terminal_title":"demo skill","workspace_id":"w1"},{"agent":"codex","agent_session":{"value":"00000000-0000-0000-0000-000000000002"},"agent_status":"idle","focused":false,"pane_id":"w1:p2","tab_id":"w1:t2","terminal_title":"configuration","workspace_id":"w1"},{"agent":"codex","agent_session":{"value":"00000000-0000-0000-0000-000000000003"},"agent_status":"idle","focused":false,"pane_id":"w2:pA","tab_id":"w2:t1","terminal_title":"popup demo","workspace_id":"w2"}]}}'
    ;;
  "agent read")
    case "$3" in
      "w1:p1") printf '%s\n' 'Invoke it with $demo-gif.' ;;
      "w1:p2") printf '%s\n' 'The Herdr setup is ready for review.' ;;
      "w2:pA") printf '%s\n' 'The popup now inherits the active theme.' ;;
    esac
    ;;
  "workspace list")
    printf '%s\n' '{"id":"demo","result":{"type":"workspace_list","workspaces":[{"workspace_id":"w1","label":"home"},{"workspace_id":"w2","label":"herdr-checkin"}]}}'
    ;;
  "tab list")
    printf '%s\n' '{"id":"demo","result":{"type":"tab_list","tabs":[{"tab_id":"w1:t1","label":"~"},{"tab_id":"w1:t2","label":"herdr-config"},{"tab_id":"w2:t1","label":"codex"}]}}'
    ;;
  "pane list")
    printf '%s\n' '{"id":"demo","result":{"type":"pane_list","panes":[{"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1","agent_status":"idle","agent":"codex"},{"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t2","agent_status":"idle","agent":"codex"},{"pane_id":"w2:pA","workspace_id":"w2","tab_id":"w2:t1","agent_status":"idle","agent":"codex"}]}}'
    ;;
  "agent focus"|"agent prompt"|"notification show"|"popup close")
    printf '%s\n' '{"id":"demo","result":{"type":"ok"}}'
    ;;
  *)
    printf '%s\n' '{"id":"demo","result":{"type":"ok"}}'
    ;;
esac
