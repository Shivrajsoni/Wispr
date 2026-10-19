//! # wispr
//!
//! Press a hotkey, speak, and polished text is typed into whatever app has focus.
//! That is the whole product: there is no assistant, chat, or search mode.
//!
//! ## Pipeline
//!
//! ```text
//!  CoreAudio thread         control thread (daemon)                     session thread
//!  ────────────────         ───────────────────────────────────────     ──────────────
//!  cpal callback ──SPSC──▶  drain ─▶ downmix ─▶ HPF ─▶ FIR ─▶ 16 kHz ─┬─▶ PCM16 ─▶ Deepgram WS
//!  (no locks, no allocs)    ring                                      │              │
//!                                                                     └─▶ utterance  │ final text
//!                                                                         buffer     ▼
//!                           hotkey ─▶ state machine ─▶ stop ─▶ transcript (or local Whisper)
//!                                                              ─▶ post-process ─▶ inject ─▶ app
//! ```
//!
//! ## Concurrency model
//!
//! * **CoreAudio thread**: the `cpal` callback only copies samples into a lock-free
//!   ring buffer ([`audio::Microphone`]). It never blocks, locks, or allocates.
//! * **Control thread**: [`daemon::Daemon`] owns all mutable state. It drains audio,
//!   drives the [`pipeline::Machine`] state machine, and performs injection. Input
//!   events reach it over a single `mpsc` channel, so state is never shared.
//! * **Hotkey thread**: `rdev` global listener; it turns key transitions into events.
//! * **Session thread**: one per utterance; owns the Deepgram WebSocket
//!   ([`asr::deepgram::StreamingSession`]) so network stalls never touch the audio path.
//!
//! The microphone is opened only while listening, so the macOS mic indicator is
//! only lit while you dictate.

#![cfg(target_os = "macos")]

pub mod log;

pub mod asr;
pub mod audio;
pub mod config;
pub mod context;
pub mod daemon;
pub mod history;
pub mod hotkey;
pub mod hud;
pub mod inject;
pub mod macos;
pub mod obsidian;
pub mod pipeline;
pub mod text;
pub mod vad;
