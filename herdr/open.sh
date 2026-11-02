#!/bin/sh
# Opens one of this plugin's panes (picker | panel) next to the focused pane.
set -e
herdr="${HERDR_BIN_PATH:-herdr}"
case "$1" in
  panel) exec "$herdr" plugin pane open --plugin agent-clip --entrypoint panel --direction right ;;
  *) exec "$herdr" plugin pane open --plugin agent-clip --entrypoint picker ;;
esac
