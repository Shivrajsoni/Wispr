#!/bin/bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Start Wispr
# @raycast.mode compact

# Optional parameters:
# @raycast.icon 🎙️
# @raycast.packageName Wispr

# Documentation:
# @raycast.description Start voice dictation in the background (also starts at login)
# @raycast.author Shivraj

CTL="$HOME/Library/Application Support/wispr/wisprctl"
if [[ ! -x "$CTL" ]]; then
    echo "Wispr isn't installed: run scripts/wisprctl install in the wispr repo"
    exit 1
fi
exec "$CTL" start
