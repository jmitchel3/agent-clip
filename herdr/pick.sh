#!/bin/sh
# Herdr's server may have started without the install dir on PATH.
PATH="$PATH:$HOME/.local/bin"
exec agent-clip pick --herdr "$@"
