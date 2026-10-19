//! Global hotkey listener.
//!
//! `rdev` reports raw key-down/up events system-wide (needs Accessibility + Input Monitoring).
//! We track the trigger key's own up/down state, so OS auto-repeat never produces duplicate
//! presses; the state machine decides what a press or release means (toggle vs. hold).
//! The cancel key is observed but not swallowed, so the focused app also receives it.

use crate::daemon::Signal;
use crate::pipeline::Event;
use anyhow::{Result, bail};
use rdev::{EventType, Key};
use std::sync::mpsc::Sender;
use std::thread;

pub fn parse_key(name: &str) -> Result<Key> {
    let key = match name
        .trim()
        .to_ascii_lowercase()
        .replace(['_', '-', ' '], "")
        .as_str()
    {
        "rightoption" | "rightalt" | "altgr" => Key::AltGr,
        "leftoption" | "option" | "alt" => Key::Alt,
        "rightcommand" | "rightcmd" => Key::MetaRight,
        "rightcontrol" | "rightctrl" => Key::ControlRight,
        "rightshift" => Key::ShiftRight,
        "fn" | "function" => Key::Function,
        "capslock" => Key::CapsLock,
        "escape" | "esc" => Key::Escape,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => bail!(
            "unknown key {name:?}; use RightOption, RightCommand, RightControl, RightShift, LeftOption, Fn, CapsLock, Escape, or F1-F12"
        ),
    };
    Ok(key)
}

/// Spawns the listener thread. Events arrive on `tx`; if the OS refuses the event tap, a
/// [`Signal::HotkeyFailed`] is sent instead.
pub fn spawn(trigger: Key, cancel: Key, tx: Sender<Signal>) {
    thread::Builder::new()
        .name("hotkey".into())
        .spawn(move || {
            let mut tracker = Tracker::new(trigger, cancel);
            let events = tx.clone();
            let result = rdev::listen(move |ev| {
                if let Some(event) = tracker.on(ev.event_type) {
                    let _ = events.send(Signal::Input(event));
                }
            });
            if let Err(e) = result {
                let _ = tx.send(Signal::HotkeyFailed(format!("{e:?}")));
            }
        })
        .expect("spawning the hotkey thread");
}

/// Turns raw key events into hotkey events.
///
/// Modifier-style triggers (Fn, Option, Command, Control, Shift) double as shortcut modifiers:
/// Fn+Delete, Fn+arrow keys, Fn+F1 and so on. If any other key goes down while the trigger is
/// held, that was a shortcut, not a dictation, so the recording is cancelled.
pub struct Tracker {
    trigger: Key,
    cancel: Key,
    modifier_trigger: bool,
    down: bool,
}

impl Tracker {
    pub fn new(trigger: Key, cancel: Key) -> Self {
        let modifier_trigger = matches!(
            trigger,
            Key::Function
                | Key::Alt
                | Key::AltGr
                | Key::MetaLeft
                | Key::MetaRight
                | Key::ControlLeft
                | Key::ControlRight
                | Key::ShiftLeft
                | Key::ShiftRight
        );
        Self {
            trigger,
            cancel,
            modifier_trigger,
            down: false,
        }
    }

    pub fn on(&mut self, event: EventType) -> Option<Event> {
        match event {
            EventType::KeyPress(k) if k == self.trigger => {
                // Auto-repeat sends more presses while held; only the first one counts.
                (!std::mem::replace(&mut self.down, true)).then_some(Event::HotkeyPress)
            }
            EventType::KeyRelease(k) if k == self.trigger => {
                std::mem::replace(&mut self.down, false).then_some(Event::HotkeyRelease)
            }
            EventType::KeyPress(k) if k == self.cancel => Some(Event::Cancel),
            EventType::KeyPress(_) if self.down && self.modifier_trigger => Some(Event::Cancel),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_names_leniently() {
        assert_eq!(parse_key("RightOption").unwrap(), Key::AltGr);
        assert_eq!(parse_key("right_option").unwrap(), Key::AltGr);
        assert_eq!(parse_key("F9").unwrap(), Key::F9);
        assert_eq!(parse_key("esc").unwrap(), Key::Escape);
        assert!(parse_key("Hyper").is_err());
        assert_eq!(parse_key("fn").unwrap(), Key::Function);
    }

    #[test]
    fn tracker_ignores_auto_repeat() {
        let mut t = Tracker::new(Key::F9, Key::Escape);
        assert_eq!(t.on(EventType::KeyPress(Key::F9)), Some(Event::HotkeyPress));
        assert_eq!(t.on(EventType::KeyPress(Key::F9)), None);
        assert_eq!(
            t.on(EventType::KeyRelease(Key::F9)),
            Some(Event::HotkeyRelease)
        );
        assert_eq!(t.on(EventType::KeyRelease(Key::F9)), None);
    }

    #[test]
    fn fn_shortcuts_cancel_instead_of_dictating() {
        let mut t = Tracker::new(Key::Function, Key::Escape);
        assert_eq!(
            t.on(EventType::KeyPress(Key::Function)),
            Some(Event::HotkeyPress)
        );
        assert_eq!(
            t.on(EventType::KeyPress(Key::Backspace)),
            Some(Event::Cancel),
            "Fn+Delete"
        );
        assert_eq!(
            t.on(EventType::KeyRelease(Key::Function)),
            Some(Event::HotkeyRelease)
        );
        // Typing while Fn is up (e.g. during a toggle-mode dictation) is not a shortcut.
        assert_eq!(t.on(EventType::KeyPress(Key::KeyA)), None);
    }

    #[test]
    fn non_modifier_triggers_allow_other_keys() {
        let mut t = Tracker::new(Key::F9, Key::Escape);
        t.on(EventType::KeyPress(Key::F9));
        assert_eq!(t.on(EventType::KeyPress(Key::KeyA)), None);
        assert_eq!(t.on(EventType::KeyPress(Key::Escape)), Some(Event::Cancel));
    }
}
