//! Tiny logging layer. Everything goes to stderr.
//!
//! Dictated text must never reach logs in release builds: wrap it in [`Redacted`].
//! Set `WISPR_LOG_TEXT=1` to opt in to seeing transcripts while debugging a release build.

use std::fmt;

#[macro_export]
macro_rules! info {
    ($($arg:tt)*) => { eprintln!("[wispr] {}", format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($arg:tt)*) => { eprintln!("[wispr] warning: {}", format_args!($($arg)*)) };
}

/// Displays dictated text only in debug builds (or with `WISPR_LOG_TEXT=1`).
pub struct Redacted<'a>(pub &'a str);

impl fmt::Display for Redacted<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if text_logging_enabled() {
            write!(f, "{:?}", self.0)
        } else {
            write!(f, "<{} chars>", self.0.chars().count())
        }
    }
}

fn text_logging_enabled() -> bool {
    cfg!(debug_assertions) || std::env::var_os("WISPR_LOG_TEXT").is_some_and(|v| v == "1")
}
