#!/bin/bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Stop Wispr
# @raycast.mode compact

# Optional parameters:
# @raycast.icon 🎙️
# @raycast.packageName Wispr

# Documentation:
# @raycast.description Stop voice dictation until you start it again or log in
# @raycast.author Shivraj

CTL="$HOME/Library/Application Support/wispr/wisprctl"
if [[ ! -x "$CTL" ]]; then
    echo "Wispr isn't installed: run scripts/wisprctl install in the wispr repo"
    exit 1
fi
exec "$CTL" stop
