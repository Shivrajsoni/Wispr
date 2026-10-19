# Wispr

**Tap `Fn`, speak, and clean text appears wherever you're typing.**
A fast, private, open-source take on Wispr Flow for macOS, written in Rust.

- **Fast**: about 0.4 s from stopping to text on screen.
- **Works offline**: if the internet or Deepgram is down, a local model on your Mac takes over.
- **Clean text**: drops "um" and "uh", fixes "four, no actually five" → "five", formats per app.
- **Never answers back**: it types what you said. A question stays a question.
- **History in Obsidian**: your last 50 dictations, each with a Copy button.

---

## 1. Install (5 minutes)

You need macOS on Apple Silicon, [Rust](https://rustup.rs), Xcode tools
(`xcode-select --install`), and CMake (`brew install cmake`).

```bash
git clone <this repo> && cd wispr
cp .env.example .env                  # paste your key: DEEPGRAM_API_KEY=...  (optional)
cargo run --release -- --download-models    # offline model, 190 MB
scripts/wisprctl install --raycast-dir ~/path/to/raycast/scripts
```

That builds **Wispr.app**, starts it now and at every login, and adds Raycast commands.

**Then allow two permissions** when macOS asks, or go to System Settings → Privacy & Security:
- **Accessibility**: lets Wispr type into apps
- **Input Monitoring**: lets Wispr see the `Fn` key

Wispr restarts itself once both are on. The first dictation also asks for **Microphone** access.

**One keyboard setting:** System Settings → Keyboard → *Press 🌐 key to* → **Do Nothing**.
Otherwise macOS opens emoji or its own dictation at the same time.

---

## 2. Use it

| Do this | Result |
|---|---|
| Tap `Fn`, speak, tap `Fn` again | Text is typed where your cursor is |
| `Esc` while speaking | Cancel |
| `Fn` + another key (Fn+Delete, Fn+←) | Treated as a shortcut, not a dictation |

A small pill at the bottom of the screen shows **Listening** → **Transcribing**.
Prefer push-to-talk (hold `Fn`, release to type)? Set `mode = "hold"` in the config.

**Raycast commands:** *Start Wispr*, *Stop Wispr*, *Wispr Status*.
Terminal: `wisprctl start | stop | restart | status | logs | config | uninstall`.

---

## 3. How it works

```
 Fn key ─▶ microphone ─▶ 16 kHz audio (kept in RAM) ─┬─▶ Deepgram (cloud, streams while you talk)
                                                      │
                                                      └─▶ Whisper (on your Mac, if cloud fails)
                                                                   │
                                     text ◀───────────────────────┘
                                       │
              clean up: remove fillers, fix self-corrections, your vocabulary, app style
                                       │
                         paste into the focused app (your clipboard is restored)
                                       │
                         add to the history note in Obsidian
```

**Step by step**

1. **Hotkey**: Wispr listens for `Fn` system-wide.
2. **Microphone**: already prepared at startup, so recording begins within ~50 ms.
   It only records while you dictate, so the mic indicator is off the rest of the time.
3. **Speech to text**: audio streams to **Deepgram** while you talk, so when you stop, the
   text is almost ready (~0.3 s). A copy of the audio stays in memory for the offline fallback.
4. **Clean-up**: simple rules, no AI model, so your words are never rewritten or answered:
   - fillers removed: "um, so let's go" → "So let's go"
   - corrections resolved: "port 8080, wait no, port 3000" → "port 3000"
   - your words fixed: "cube netties" → "Kubernetes"
   - per app: no trailing period in terminals or one-line chat messages
5. **Typing**: Wispr saves your clipboard, pastes the text, then puts your clipboard back
   (images and files too). In password fields it types nothing and copies the text instead.
6. **History**: the text is added to your Obsidian note.

---

## 4. What happens when you're offline

Wispr always keeps your recording in memory until it's turned into text, so **a dictation is never
lost**. If Deepgram can't be used, the same audio goes to **Whisper**, a speech-to-text model
running on your Mac's GPU.

| When you stop speaking… | Wispr does | You wait |
|---|---|---|
| Deepgram is connected and working | uses Deepgram | ~0.4 s |
| Deepgram is still connecting | uses Whisper right away | ~0.8 s |
| No internet, or the API key is rejected | uses Whisper right away, switches to offline mode | ~0.8 s |
| Deepgram is connected but too slow (> 1.5 s) | uses Whisper | up to 1.5 s extra, once |

**Offline mode** (a "circuit breaker"): after a failure, Wispr stops trying Deepgram for a while,
so you don't pay the wait every time. It waits 30 s, then 1, 2, 4, up to 5 min while failures continue
(1 hour for a bad key), then tries Deepgram again. One success switches back online. The pill
says *"Offline: using local model"* when this happens.

**Is the offline model an "embedding model"?** No. Embedding models turn text into numbers to
compare meanings (used for search). Wispr's offline engine is **Whisper**, a speech recognition
model: it turns the sound into a spectrogram, an *encoder* converts that into audio features, and a
*decoder* writes the words one token at a time. Wispr uses `small.en` (English, 190 MB, 5-bit
quantized), loaded into memory at startup and warmed up, so the first offline dictation is as fast
as the rest. Silence is skipped before it reaches the model, because Whisper invents text like
"Thank you." when fed silence.

**Which Deepgram server?** `region = "auto"` tests the US and EU servers at startup and uses the
closer one. (With `eu`, audio is processed in the EU.)

---

## 5. History in Obsidian

```toml
[history]
path = "~/Documents/MyVault/Wispr Dictations.md"
max_entries = 50
open_obsidian = true
```

- Newest first. Each entry shows the time, the app, and `offline` if Whisper was used.
- **Every dictation is its own code block, so Obsidian shows a Copy button on each one.**
- After 50 entries, the oldest one is removed automatically.
- When Wispr starts and Obsidian isn't open, Wispr opens it **in the background** on this note.

How it's done: the note is just a file. Obsidian notices file changes instantly, so no plugin is
needed. Wispr writes the file on a separate thread, so it never slows down typing, and replaces it
in one atomic step, so Obsidian never sees a half-written note. A small CSS snippet
(`.obsidian/snippets/wispr-history.css`) makes long dictations wrap. It only affects this note.

---

## 6. Settings

Your settings file: `~/Library/Application Support/wispr/config.toml` (open it with `wisprctl config`,
then `wisprctl restart`). Every option is explained in [`config.example.toml`](config.example.toml).

| Setting | What it does | Default |
|---|---|---|
| `hotkey.key` | `Fn`, `RightOption`, `RightCommand`, `F1`–`F12`, … | `Fn` |
| `hotkey.mode` | `toggle` (tap to start/stop) or `hold` (push-to-talk) | `toggle` |
| `transcription.engine` | `auto`, `deepgram`, or `local` (audio never leaves your Mac) | `auto` |
| `deepgram.region` | `auto`, `us`, `eu` | `auto` |
| `local.fallback` | keep Whisper loaded for offline use (~300 MB of memory) | `true` |
| `output.method` | `paste` (fast) or `type` (never touches the clipboard) | `paste` |
| `history.path` | Obsidian note for your history; empty turns it off | off |
| `[vocabulary]` | `"Kubernetes" = ["cube netties"]`: also helps Deepgram hear these words | — |

The Deepgram key lives in `~/Library/Application Support/wispr/.env` (`DEEPGRAM_API_KEY=...`).
New Deepgram accounts get $200 of free credit.

---

## 7. Speed (MacBook Air M3)

| Step | Time |
|---|---|
| Press `Fn` → microphone recording | ~50 ms |
| Connect to Deepgram | ~0.7 s, while you're still talking |
| Stop → text from Deepgram | ~0.3–0.4 s |
| Stop → text from Whisper (offline) | ~0.8 s |
| Clean-up + paste | ~12 ms |
| Idle CPU | 0% |

Measure it yourself: `wispr --benchmark`.

---

## 8. Privacy

- Audio is kept **only in memory** and never saved to disk.
- With `engine = "local"`, audio never leaves your Mac. Otherwise it goes to Deepgram while you speak.
- Logs never contain what you said (only its length).
- Dictated text is saved only if you turn on the history note.

---

## 9. Troubleshooting

| Problem | Fix |
|---|---|
| Nothing happens when I press `Fn` | Run *Wispr Status*. Usually Accessibility or Input Monitoring is off |
| Emoji picker opens with `Fn` | Keyboard → *Press 🌐 key to* → Do Nothing |
| "Offline: using local model" | Internet or Deepgram is down; it switches back by itself |
| A word is always misheard | Add it to `[vocabulary]` |
| Permissions asked again after an update | Normal after `wisprctl install` ships a new build; allow again |

Logs: `wisprctl logs`. Full check: `wispr --check-hardware`.

---

## 10. For developers

```bash
cargo test                    # 74 tests
cargo clippy --all-targets    # zero warnings
wispr --dry-run               # log text instead of typing it
WISPR_DEEPGRAM_HOST=offline.invalid wispr --dry-run   # simulate being offline
```

| File | What it does |
|---|---|
| `src/daemon.rs` | Main loop: records, picks Deepgram or Whisper, types the result |
| `src/pipeline.rs` | The states: Idle → Listening → Processing → Injecting |
| `src/hotkey.rs` | `Fn` detection, Fn-shortcut cancel |
| `src/audio/` | Microphone (lock-free), resampling to 16 kHz |
| `src/asr/deepgram.rs` | Deepgram streaming client |
| `src/asr/breaker.rs` | Offline mode (circuit breaker) |
| `src/asr/whisper.rs` | Local Whisper on the GPU |
| `src/vad.rs` | Detects silence |
| `src/text.rs` | Clean-up rules |
| `src/inject.rs` | Paste and restore your clipboard |
| `src/history.rs`, `src/obsidian.rs` | The Obsidian history note |
| `macos/overlay.swift` | The on-screen pill |
| `scripts/wisprctl`, `raycast/` | Background service and Raycast commands |
