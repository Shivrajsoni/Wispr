#!/usr/bin/env bash
# End-to-end tests: real Fn key events, real Deepgram / Whisper, real paste into a real window.
#
#   scripts/e2e/run.sh
#
# Speech comes from `say` through WISPR_TEST_AUDIO (played at real-time speed in place of the
# microphone), keys are posted through the macOS HID event tap, and the text lands in a small
# "sink" window that saves what it received. Needs the terminal to have Accessibility and Input
# Monitoring. The background Wispr service is paused during the run and restarted afterwards.

set -uo pipefail
cd "$(dirname "$0")/../.."

E="target/e2e"
W="target/release/wispr"
CTL="$HOME/Library/Application Support/wispr/wisprctl"
mkdir -p "$E/history"
PASS=0
FAIL=0
WPID=""

cargo build --release --quiet || exit 1
[[ -x "$E/keypost" ]] || swiftc -O -suppress-warnings scripts/e2e/keypost.swift -o "$E/keypost"
# The sink is a real .app launched with `open`, because macOS only brings LaunchServices-launched
# apps to the front; a bare binary started from a terminal stays behind the current app.
SINK_APP="$E/WisprE2ESink.app"
if [[ ! -x "$SINK_APP/Contents/MacOS/sink" || scripts/e2e/sink.swift -nt "$SINK_APP/Contents/MacOS/sink" ]]; then
    mkdir -p "$SINK_APP/Contents/MacOS"
    swiftc -O -suppress-warnings scripts/e2e/sink.swift -o "$SINK_APP/Contents/MacOS/sink"
    cat >"$SINK_APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleIdentifier</key><string>local.wispr.e2e-sink</string>
  <key>CFBundleName</key><string>WisprE2ESink</string>
  <key>CFBundleExecutable</key><string>sink</string>
  <key>CFBundlePackageType</key><string>APPL</string>
</dict></plist>
PLIST
fi

# Test config: the user's settings, but history goes to a scratch note and Obsidian stays closed.
SRC_CFG="$HOME/Library/Application Support/wispr/config.toml"
[[ -f "$SRC_CFG" ]] || SRC_CFG="config.example.toml"
sed -e "s|^path = .*|path = \"$PWD/$E/history/Dictations.md\"|" \
    -e "s|^open_obsidian = .*|open_obsidian = false|" "$SRC_CFG" >"$E/config.toml"
# Dictations are driven with F9: some apps (e.g. Antigravity IDE) grab focus when Fn is pressed,
# which would move the paste target. Fn detection itself is covered by its own case below.
sed -e 's|^key = .*|key = "Fn"|' "$E/config.toml" >"$E/config-fn.toml"
sed -i '' -e 's|^key = .*|key = "F9"|' "$E/config.toml"
grep -q '^\[history\]' "$E/config.toml" ||
    printf '\n[history]\npath = "%s"\nopen_obsidian = false\n' "$PWD/$E/history/Dictations.md" >>"$E/config.toml"

speech() { say -o "$E/$1.wav" --data-format=LEI16@16000 "$2"; }
speech correction "Um, so let's meet at four, no actually five, and then review the pull request."
speech question "How do we optimize the database query"
speech silence "[[slnc 2500]]"
seconds() { afinfo "$E/$1.wav" | awk '/estimated duration/ { print $3 }'; }

restore_service() {
    [[ -n "$WPID" ]] && kill "$WPID" 2>/dev/null
    [[ -x "$CTL" ]] && "$CTL" start >/dev/null 2>&1
}
trap restore_service EXIT
[[ -x "$CTL" ]] && "$CTL" stop >/dev/null 2>&1

start_wispr() { # <log> [ENV=value...]
    local log="$1"; shift
    rm -f "$E/history/Dictations.md"
    env "$@" WISPR_LOG_TEXT=1 "$W" --config "${CONFIG:-$E/config.toml}" --no-hud </dev/null >"$log" 2>&1 &
    WPID=$!
    for _ in $(seq 1 50); do grep -q "ready:" "$log" 2>/dev/null && return 0; sleep 0.2; done
    echo "wispr did not start:"; cat "$log"; return 1
}
stop_wispr() { kill "$WPID" 2>/dev/null; wait "$WPID" 2>/dev/null; WPID=""; }
tap_fn() { "$E/keypost" key 101; } # F9, the hotkey in the test config

front_app() { lsappinfo info -only name "$(lsappinfo front)" 2>/dev/null | sed -E 's/.*"([^"]*)".*/\1/'; }

# Opens the sink window and waits until it is the frontmost app. Fails (and the case is skipped)
# if it never gets focus, so a test can never type into one of your real apps.
open_sink() { # <output-file> <seconds>
    rm -f "$1"
    open -n "$SINK_APP" --args "$PWD/$1" "$2"
    for _ in $(seq 1 40); do
        [[ "$(front_app)" == "WisprE2ESink" ]] && { sleep 0.3; return 0; }
        sleep 0.1
    done
    echo "  SKIP  test window did not get focus (front app: $(front_app)); not pressing Fn"
    return 1
}
wait_sink() { for _ in $(seq 1 200); do pgrep -f "WisprE2ESink.app" >/dev/null || return 0; sleep 0.1; done; }

# Refuses to press Fn unless the test window is in front.
safe_tap_fn() {
    if [[ "$(front_app)" != "WisprE2ESink" ]]; then
        echo "  ABORT focus moved to $(front_app); not pressing Fn"
        return 1
    fi
    tap_fn
}

check() { # <description> <command...>
    local what="$1"; shift
    if "$@"; then echo "  PASS  $what"; PASS=$((PASS + 1)); else echo "  FAIL  $what"; FAIL=$((FAIL + 1)); fi
}
has() { grep -qiE -- "$2" "$1" 2>/dev/null; }
lacks() { ! grep -qiE -- "$2" "$1" 2>/dev/null; }
empty() { [[ ! -s "$1" ]] || [[ -z "$(tr -d '[:space:]' <"$1")" ]]; }

dictate() { # <case> <audio> [ENV...] : start wispr, focus sink, tap Fn, play audio, tap Fn
    local name="$1" audio="$2"; shift 2
    local log="$E/$name.log" out="$E/$name.out" secs
    secs="$(seconds "$audio")"
    start_wispr "$log" WISPR_TEST_AUDIO="$E/$audio.wav" "$@" || return 1
    printf 'clipboard-sentinel-%s' "$name" | pbcopy
    if open_sink "$out" "$(echo "$secs + 6" | bc)"; then
        safe_tap_fn && sleep "$(echo "$secs + 0.4" | bc)" && safe_tap_fn
        wait_sink
    fi
    stop_wispr
}

echo "== 1. Online: Fn -> speech -> Deepgram -> clean-up -> paste -> history"
dictate online correction
check "pasted into the sink window (and nowhere else)" has "$E/online.log" "pasted -> WisprE2ESink"
check "text was pasted into the focused window" has "$E/online.out" "meet at five"
check "self-correction resolved (no 'four')" lacks "$E/online.out" "four"
check "filler removed (no 'Um')" lacks "$E/online.out" "^um"
check "transcribed by Deepgram" has "$E/online.log" "via Deepgram"
check "clipboard restored afterwards" test "$(pbpaste)" = "clipboard-sentinel-online"
check "history note has the dictation" has "$E/history/Dictations.md" "meet at five"
check "history entry is a copyable code block" has "$E/history/Dictations.md" '^```text$'
grep -h "release->text" "$E/online.log" | sed 's/^\[wispr\] /        /; s/ | ".*//'

echo "== 2. Offline: Deepgram unreachable -> local Whisper"
dictate offline correction WISPR_DEEPGRAM_HOST=offline.invalid
check "text was pasted" has "$E/offline.out" "meet at (five|5)"
check "fell back to local Whisper" has "$E/offline.log" "via local Whisper"
check "offline mode engaged" has "$E/offline.log" "offline mode for"
check "history entry tagged offline" has "$E/history/Dictations.md" "· offline"
grep -h "release->text" "$E/offline.log" | sed 's/^\[wispr\] /        /; s/ | ".*//'

echo "== 3. A dictated question is typed, never answered"
dictate question question
check "question typed verbatim" has "$E/question.out" "^How do we optimi[sz]e the database query\?"

echo "== 4. Silence -> nothing typed"
dictate silence silence
check "no speech detected" has "$E/silence.log" "No speech detected"
check "nothing typed" empty "$E/silence.out"

echo "== 5. Esc cancels"
start_wispr "$E/cancel.log" WISPR_TEST_AUDIO="$E/correction.wav"
if open_sink "$E/cancel.out" 6; then
    safe_tap_fn && sleep 2 && "$E/keypost" key 53
    wait_sink
fi
stop_wispr
check "recording cancelled" has "$E/cancel.log" "cancelled"
check "nothing typed" empty "$E/cancel.out"

echo "== 6. Fn + another key is a shortcut, not a dictation"
CONFIG="$E/config-fn.toml" start_wispr "$E/combo.log" WISPR_TEST_AUDIO="$E/correction.wav"
if open_sink "$E/combo.out" 4; then
    "$E/keypost" fn down; "$E/keypost" key 123; "$E/keypost" fn up
    wait_sink
fi
stop_wispr
check "Fn+Left cancelled the dictation" has "$E/combo.log" "cancelled"
check "nothing was transcribed" lacks "$E/combo.log" "release->text"

echo
echo "$PASS passed, $FAIL failed"
[[ $FAIL -eq 0 ]]
