//! On-screen status pill, rendered by the `wispr-overlay` helper (macos/overlay.swift).
//!
//! The helper is one long-lived child process controlled with line commands on stdin
//! (`listening`, `level <rms>`, `processing`, `notice <text>`, `alert <text>`, `hide`). It never
//! takes focus and never sees dictated text: notices are fixed status messages only, and `level`
//! carries just the loudness of the latest audio so the waveform can follow your voice. When wispr exits, the helper's stdin
//! closes and it exits too. If the helper is missing or dies, the HUD silently disables itself.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

/// How often the microphone level is sent while listening (about 30 updates a second).
const LEVEL_INTERVAL: Duration = Duration::from_millis(33);

pub struct Hud {
    child: Option<(Child, ChildStdin)>,
    level_sum: f64,
    level_count: usize,
    level_sent: Option<Instant>,
}

impl Hud {
    pub fn spawn() -> Self {
        let child = overlay_path().and_then(|path| {
            let mut child = Command::new(path)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()?;
            let stdin = child.stdin.take()?;
            Some((child, stdin))
        });
        Self::with(child)
    }

    pub fn disabled() -> Self {
        Self::with(None)
    }

    fn with(child: Option<(Child, ChildStdin)>) -> Self {
        Self {
            child,
            level_sum: 0.0,
            level_count: 0,
            level_sent: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.child.is_some()
    }

    pub fn listening(&mut self) {
        self.level_sum = 0.0;
        self.level_count = 0;
        self.level_sent = None;
        self.send("listening");
    }

    /// Feeds freshly captured samples; sends their RMS at most every [`LEVEL_INTERVAL`].
    pub fn level(&mut self, samples: &[f32]) {
        if self.child.is_none() {
            return;
        }
        self.level_sum += samples.iter().map(|&s| f64::from(s * s)).sum::<f64>();
        self.level_count += samples.len();
        if self
            .level_sent
            .is_some_and(|t| t.elapsed() < LEVEL_INTERVAL)
            || self.level_count == 0
        {
            return;
        }
        let rms = (self.level_sum / self.level_count as f64).sqrt();
        self.level_sum = 0.0;
        self.level_count = 0;
        self.level_sent = Some(Instant::now());
        self.send(&format!("level {rms:.5}"));
    }

    pub fn processing(&mut self) {
        self.send("processing");
    }

    pub fn hide(&mut self) {
        self.send("hide");
    }

    pub fn notice(&mut self, message: &str) {
        self.send(&format!("notice {}", message.replace('\n', " ")));
    }

    /// A problem the user should act on; shown longer and marked as a warning.
    pub fn alert(&mut self, message: &str) {
        self.send(&format!("alert {}", message.replace('\n', " ")));
    }

    fn send(&mut self, line: &str) {
        let Some((_, stdin)) = self.child.as_mut() else {
            return;
        };
        if writeln!(stdin, "{line}")
            .and_then(|()| stdin.flush())
            .is_err()
            && let Some((mut child, _)) = self.child.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for Hud {
    fn drop(&mut self) {
        if let Some((mut child, stdin)) = self.child.take() {
            drop(stdin);
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn overlay_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let path = exe.parent()?.join("wispr-overlay");
    path.is_file().then_some(path)
}
