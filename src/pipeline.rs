//! The dictation state machine.
//!
//! ```text
//!            press (toggle) / press (hold) / toggle
//!   Idle ───────────────────────────────────────────▶ Listening
//!    ▲                                                  │  press (toggle) / release (hold) /
//!    │ cancel                                           │  toggle / force-stop
//!    ├──────────────────────────────────────────────────┤
//!    │                                                  ▼
//!    │ cancel / transcript empty / transcription failed Processing
//!    ├──────────────────────────────────────────────────┤
//!    │                                                  │ transcript ready
//!    │ injected / injection failed                      ▼
//!    └──────────────────────────────────────────────── Injecting
//! ```
//!
//! The machine is pure: [`Machine::handle`] maps (state, event) to a new state plus a list of
//! [`Action`]s for the daemon to perform. Events that make no sense in the current state (a key
//! press while processing, a stray release) are ignored, which is what makes rapid hotkey
//! spamming harmless.

use crate::config::HotkeyMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Listening,
    Processing,
    Injecting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    HotkeyPress,
    HotkeyRelease,
    /// Mode-independent start/stop (terminal Enter key).
    Toggle,
    Cancel,
    /// Stop listening without a key: max duration reached or the microphone disappeared.
    ForceStop,
    /// The microphone could not be opened.
    RecordingFailed,
    TranscriptReady(String),
    TranscriptionFailed,
    Injected,
    InjectionFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    StartRecording,
    /// Stop capturing and begin transcription.
    StopRecording,
    /// Stop capturing and throw the audio away.
    DiscardRecording,
    /// Abandon an in-flight transcription.
    DiscardTranscription,
    Inject(String),
    ShowListening,
    ShowProcessing,
    Hide,
    Notice(&'static str),
    /// Like [`Action::Notice`], but for a failure the user should notice.
    Alert(&'static str),
}

pub struct Machine {
    state: State,
    mode: HotkeyMode,
}

impl Machine {
    pub fn new(mode: HotkeyMode) -> Self {
        Self {
            state: State::Idle,
            mode,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn handle(&mut self, event: Event) -> Vec<Action> {
        use Action::*;
        use State::*;

        let starts = |e: &Event| matches!(e, Event::HotkeyPress | Event::Toggle);
        let stops = |e: &Event, mode: HotkeyMode| match e {
            Event::Toggle | Event::ForceStop => true,
            Event::HotkeyPress => mode == HotkeyMode::Toggle,
            Event::HotkeyRelease => mode == HotkeyMode::Hold,
            _ => false,
        };

        let (next, actions) = match (self.state, event) {
            (Idle, e) if starts(&e) => (Listening, vec![StartRecording, ShowListening]),
            (Listening, Event::Cancel) => (Idle, vec![DiscardRecording, Hide]),
            (Listening, Event::RecordingFailed) => (Idle, vec![Alert("Microphone unavailable")]),
            (Listening, e) if stops(&e, self.mode) => {
                (Processing, vec![StopRecording, ShowProcessing])
            }
            (Processing, Event::Cancel) => (Idle, vec![DiscardTranscription, Hide]),
            (Processing, Event::TranscriptReady(text)) if text.trim().is_empty() => {
                (Idle, vec![Notice("No speech detected")])
            }
            (Processing, Event::TranscriptReady(text)) => (Injecting, vec![Inject(text)]),
            (Processing, Event::TranscriptionFailed) => (Idle, vec![Alert("Transcription failed")]),
            (Injecting, Event::Injected) => (Idle, vec![Hide]),
            (Injecting, Event::InjectionFailed) => (
                Idle,
                vec![Alert("Couldn't type the text; it's on your clipboard")],
            ),
            (state, _) => (state, vec![]),
        };
        self.state = next;
        actions
    }
}
