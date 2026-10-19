#!/bin/bash

# Required parameters:
# @raycast.schemaVersion 1
# @raycast.title Wispr Status
# @raycast.mode fullOutput

# Optional parameters:
# @raycast.icon 🎙️
# @raycast.packageName Wispr

# Documentation:
# @raycast.description Show whether Wispr is running, its engine, and recent problems
# @raycast.author Shivraj

CTL="$HOME/Library/Application Support/wispr/wisprctl"
if [[ ! -x "$CTL" ]]; then
    echo "Wispr isn't installed: run scripts/wisprctl install in the wispr repo"
    exit 1
fi
exec "$CTL" status
