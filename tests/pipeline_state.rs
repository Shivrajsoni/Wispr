//! State-machine integration tests: Idle -> Listening -> Processing -> Injecting -> Idle,
//! plus cancellation, failure, and hotkey-spam paths in both hotkey modes.

use wispr::config::HotkeyMode;
use wispr::pipeline::{Action, Event, Machine, State};

fn text(s: &str) -> Event {
    Event::TranscriptReady(s.to_string())
}

#[test]
fn toggle_happy_path() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    assert_eq!(
        m.handle(Event::HotkeyPress),
        vec![Action::StartRecording, Action::ShowListening]
    );
    assert_eq!(m.state(), State::Listening);
    assert_eq!(
        m.handle(Event::HotkeyRelease),
        vec![],
        "release is meaningless in toggle mode"
    );
    assert_eq!(
        m.handle(Event::HotkeyPress),
        vec![Action::StopRecording, Action::ShowProcessing]
    );
    assert_eq!(m.state(), State::Processing);
    assert_eq!(
        m.handle(text("Hello there. ")),
        vec![Action::Inject("Hello there. ".into())]
    );
    assert_eq!(m.state(), State::Injecting);
    assert_eq!(m.handle(Event::Injected), vec![Action::Hide]);
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn hold_happy_path() {
    let mut m = Machine::new(HotkeyMode::Hold);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(Event::HotkeyPress),
        vec![],
        "a second press while held is ignored"
    );
    assert_eq!(
        m.handle(Event::HotkeyRelease),
        vec![Action::StopRecording, Action::ShowProcessing]
    );
    m.handle(text("Ship it."));
    m.handle(Event::Injected);
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn stray_release_while_idle_is_ignored() {
    let mut m = Machine::new(HotkeyMode::Hold);
    assert_eq!(m.handle(Event::HotkeyRelease), vec![]);
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn cancel_while_listening_discards_audio() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(Event::Cancel),
        vec![Action::DiscardRecording, Action::Hide]
    );
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn cancel_while_processing_discards_transcript() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(Event::Cancel),
        vec![Action::DiscardTranscription, Action::Hide]
    );
    // A late transcript after cancelling must not be injected.
    assert_eq!(m.handle(text("too late")), vec![]);
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn cancel_while_idle_does_nothing() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    assert_eq!(m.handle(Event::Cancel), vec![]);
}

#[test]
fn hotkey_spam_while_processing_is_ignored() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    for _ in 0..10 {
        assert_eq!(m.handle(Event::HotkeyPress), vec![]);
        assert_eq!(m.handle(Event::HotkeyRelease), vec![]);
        assert_eq!(m.handle(Event::Toggle), vec![]);
    }
    assert_eq!(m.state(), State::Processing);
    assert_eq!(m.handle(text("ok")), vec![Action::Inject("ok".into())]);
}

#[test]
fn hotkey_spam_while_injecting_is_ignored() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    m.handle(text("ok"));
    assert_eq!(m.handle(Event::HotkeyPress), vec![]);
    assert_eq!(m.handle(Event::Injected), vec![Action::Hide]);
    assert_eq!(
        m.handle(Event::HotkeyPress),
        vec![Action::StartRecording, Action::ShowListening]
    );
}

#[test]
fn empty_transcript_notifies_and_returns_to_idle() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(text("   ")),
        vec![Action::Notice("No speech detected")]
    );
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn transcription_failure_returns_to_idle() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(Event::TranscriptionFailed),
        vec![Action::Alert("Transcription failed")]
    );
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn injection_failure_returns_to_idle() {
    let mut m = Machine::new(HotkeyMode::Toggle);
    m.handle(Event::HotkeyPress);
    m.handle(Event::HotkeyPress);
    m.handle(text("hello"));
    let actions = m.handle(Event::InjectionFailed);
    assert!(matches!(actions.as_slice(), [Action::Alert(_)]));
    assert_eq!(m.state(), State::Idle);
}

#[test]
fn microphone_failure_on_start_returns_to_idle() {
    let mut m = Machine::new(HotkeyMode::Hold);
    m.handle(Event::HotkeyPress);
    assert_eq!(
        m.handle(Event::RecordingFailed),
        vec![Action::Alert("Microphone unavailable")]
    );
    assert_eq!(m.state(), State::Idle);
    // The user's eventual key release must not start processing.
    assert_eq!(m.handle(Event::HotkeyRelease), vec![]);
}

#[test]
fn force_stop_transcribes_in_either_mode() {
    for mode in [HotkeyMode::Toggle, HotkeyMode::Hold] {
        let mut m = Machine::new(mode);
        m.handle(Event::HotkeyPress);
        assert_eq!(
            m.handle(Event::ForceStop),
            vec![Action::StopRecording, Action::ShowProcessing]
        );
    }
}

#[test]
fn terminal_toggle_works_in_hold_mode() {
    let mut m = Machine::new(HotkeyMode::Hold);
    assert_eq!(
        m.handle(Event::Toggle),
        vec![Action::StartRecording, Action::ShowListening]
    );
    assert_eq!(
        m.handle(Event::Toggle),
        vec![Action::StopRecording, Action::ShowProcessing]
    );
}

#[test]
fn no_speech_timeout_discards_and_alerts() {
    for mode in [HotkeyMode::Toggle, HotkeyMode::Hold] {
        let mut m = Machine::new(mode);
        m.handle(Event::HotkeyPress);
        assert_eq!(
            m.handle(Event::NoSpeechTimeout),
            vec![
                Action::DiscardRecording,
                Action::Alert("No speech detected")
            ]
        );
        assert_eq!(m.state(), State::Idle);
        assert_eq!(m.handle(Event::HotkeyRelease), vec![]);
    }
}
