//! Speech recognition engines.
//!
//! * [`deepgram`]: streaming WebSocket client. Audio is sent while you speak, so on release only
//!   the last fraction of a second still needs recognizing.
//! * [`whisper`]: whisper.cpp on Metal. Used when there is no API key, when offline, or when
//!   Deepgram fails or times out, so a dictation is never lost.

pub mod breaker;
pub mod deepgram;
pub mod whisper;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Deepgram,
    Local,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Deepgram => "Deepgram",
            Source::Local => "local Whisper",
        })
    }
}
