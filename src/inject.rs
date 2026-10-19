//! Delivers text into the focused app.
//!
//! **Paste** (default): snapshot every item and type on the general pasteboard, write the text,
//! press Cmd+V, wait for the target app to read it, then restore the snapshot, but only if the
//! pasteboard still holds our text (if the user copied something meanwhile, theirs wins). The
//! text is tagged `org.nspasteboard.TransientType` so clipboard managers don't record it.
//!
//! **Type**: synthesizes the text as keystrokes; slower, but never touches the pasteboard.
//! It is also the automatic fallback when pasting fails.
//!
//! When secure input is on (a password field is focused), nothing is injected; the text is left
//! on the pasteboard so the dictation is not lost.

use crate::config::{InjectMethod, OutputConfig};
use crate::macos;
use anyhow::{Result, anyhow};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{NSPasteboard, NSPasteboardItem, NSPasteboardTypeString, NSPasteboardWriting};
use objc2_foundation::{NSArray, NSData, NSString};
use std::thread;
use std::time::{Duration, Instant};

const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Pasted,
    Typed,
    /// Secure input was on; the text was left on the pasteboard instead.
    LeftOnClipboard,
}

pub struct Injector {
    enigo: Enigo,
    method: InjectMethod,
    restore: bool,
    restore_delay: Duration,
}

/// Every representation of every pasteboard item, so images, files, and rich text survive.
type Snapshot = Vec<Vec<(Retained<NSString>, Retained<NSData>)>>;

impl Injector {
    pub fn new(cfg: &OutputConfig) -> Result<Self> {
        let enigo = Enigo::new(&Settings::default())
            .map_err(|e| anyhow!("cannot create the keystroke synthesizer: {e:?}"))?;
        Ok(Self {
            enigo,
            method: cfg.method,
            restore: cfg.restore_clipboard,
            restore_delay: Duration::from_millis(cfg.restore_delay_ms),
        })
    }

    /// Returns how the text was delivered and how long it took until it reached the app
    /// (excluding the pasteboard restore that happens afterwards).
    pub fn inject(&mut self, text: &str) -> Result<(Outcome, Duration)> {
        let start = Instant::now();
        if text.is_empty() {
            return Ok((Outcome::Typed, Duration::ZERO));
        }
        if macos::secure_input_enabled() {
            leave_on_clipboard(text);
            return Ok((Outcome::LeftOnClipboard, start.elapsed()));
        }
        let typed = |this: &mut Self| {
            this.type_text(text)
                .map(|()| (Outcome::Typed, start.elapsed()))
        };
        match self.method {
            InjectMethod::Type => typed(self),
            InjectMethod::Paste => match self.paste(text, start) {
                Ok(delivered) => Ok((Outcome::Pasted, delivered)),
                Err(paste_err) => typed(self).map_err(|type_err| {
                    anyhow!("paste failed ({paste_err}); typing failed ({type_err})")
                }),
            },
        }
    }

    fn paste(&mut self, text: &str, start: Instant) -> Result<Duration> {
        let pb = NSPasteboard::generalPasteboard();
        let saved = self.restore.then(|| snapshot(&pb));
        if !write_text(&pb, text, true) {
            return Err(anyhow!("could not write to the pasteboard"));
        }
        let ours = pb.changeCount();

        let pressed = self.cmd_v();
        let delivered = start.elapsed();
        if let Some(saved) = saved {
            thread::sleep(self.restore_delay);
            if pb.changeCount() == ours {
                restore(&pb, saved);
            }
        }
        pressed.map(|()| delivered)
    }

    fn cmd_v(&mut self) -> Result<()> {
        let k = |r: enigo::InputResult<()>| r.map_err(|e| anyhow!("keystroke failed: {e:?}"));
        k(self.enigo.key(Key::Meta, Direction::Press))?;
        let v = k(self.enigo.key(Key::Unicode('v'), Direction::Click));
        // Always release Cmd, even if the V click failed, so the modifier never sticks.
        k(self.enigo.key(Key::Meta, Direction::Release))?;
        v
    }

    fn type_text(&mut self, text: &str) -> Result<()> {
        self.enigo
            .text(text)
            .map_err(|e| anyhow!("typing failed: {e:?}"))
    }
}

fn snapshot(pb: &NSPasteboard) -> Snapshot {
    let Some(items) = pb.pasteboardItems() else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| {
            item.types()
                .iter()
                .filter_map(|ty| item.dataForType(&ty).map(|data| (ty, data)))
                .collect()
        })
        .collect()
}

fn restore(pb: &NSPasteboard, saved: Snapshot) {
    pb.clearContents();
    if saved.is_empty() {
        return;
    }
    let items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = saved
        .into_iter()
        .map(|reps| {
            let item = NSPasteboardItem::new();
            for (ty, data) in reps {
                item.setData_forType(&data, &ty);
            }
            ProtocolObject::from_retained(item)
        })
        .collect();
    pb.writeObjects(&NSArray::from_retained_slice(&items));
}

fn write_text(pb: &NSPasteboard, text: &str, transient: bool) -> bool {
    let item = NSPasteboardItem::new();
    // SAFETY: NSPasteboardTypeString is an immutable AppKit constant.
    let string_type = unsafe { NSPasteboardTypeString };
    if !item.setString_forType(&NSString::from_str(text), string_type) {
        return false;
    }
    if transient {
        item.setData_forType(&NSData::new(), &NSString::from_str(TRANSIENT_TYPE));
    }
    pb.clearContents();
    let items = [ProtocolObject::from_retained(item)];
    pb.writeObjects(&NSArray::from_retained_slice(&items))
}

/// Puts `text` on the pasteboard and leaves it there, so a dictation is never lost.
pub fn leave_on_clipboard(text: &str) {
    write_text(&NSPasteboard::generalPasteboard(), text, false);
}

/// Measures the real pasteboard path without pressing any keys: (snapshot + write, which
/// happens before Cmd+V; restore, which happens after the text has landed).
pub fn benchmark_pasteboard() -> (Duration, Duration) {
    let pb = NSPasteboard::generalPasteboard();
    let t = Instant::now();
    let saved = snapshot(&pb);
    write_text(&pb, "wispr benchmark", true);
    let before = t.elapsed();
    let t = Instant::now();
    restore(&pb, saved);
    (before, t.elapsed())
}
