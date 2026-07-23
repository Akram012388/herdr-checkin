#!/bin/sh
set -u

exec "$HERDR_BIN_PATH" plugin pane open \
  --plugin Akram012388.checkin-demo \
  --entrypoint queue \
  --placement popup \
  --width 50% \
  --height 50% \
  --env HERDR_CHECKIN_POPUP=1 \
  --focus
