//! Rolling Markdown log of recent dictations (e.g. a note in your Obsidian vault).
//!
//! The file *is* the interface: Obsidian watches its vault with FSEvents and reloads a note the
//! moment it changes, so no plugin, socket, or server is needed. To keep that safe and fast:
//!
//! * Writes happen on a dedicated thread fed by a channel, so a slow or iCloud-synced disk can
//!   never delay dictation. Recording an entry is one channel send.
//! * Each update is written to a hidden temp file in the same directory and then `rename`d over
//!   the note. Rename is atomic on APFS, so Obsidian sees either the old or the new note, never
//!   a half-written one.
//! * The newest `max_entries` dictations are kept, newest first; the oldest drops off. On startup
//!   the existing note is parsed back (and rewritten in the current template), so the window
//!   survives restarts.
//!
//! Each dictation is rendered as its own fenced code block, because Obsidian gives every code
//! block a native **Copy** button. The fence is always longer than any backtick run in the
//! text, so dictated text can never break out of its block.
//!
//! This is opt-in (`[history] path`), because unlike audio, these texts are stored on disk.

use crate::{info, warn};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

/// CSS class set via frontmatter; scopes the wrap-long-lines snippet to this note only.
pub const CSS_CLASS: &str = "wispr-history";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Local time, `YYYY-MM-DD HH:MM:SS`.
    pub when: String,
    pub app: String,
    pub offline: bool,
    pub text: String,
}

pub struct History {
    tx: Sender<Entry>,
}

impl History {
    /// Loads the existing note, rewrites it in the current template (so it exists right away,
    /// e.g. for Obsidian to open), and starts the writer thread. Failures are logged, never fatal.
    pub fn start(path: PathBuf, max_entries: usize) -> Self {
        let max = max_entries.max(1);
        let mut entries: VecDeque<Entry> = fs::read_to_string(&path)
            .map(|md| parse(&md))
            .unwrap_or_default()
            .into();
        entries.truncate(max);
        info!(
            "history: {} ({} saved entries)",
            path.display(),
            entries.len()
        );
        save(&path, &mut entries, max);

        let (tx, rx) = mpsc::channel::<Entry>();
        thread::Builder::new()
            .name("history".into())
            .spawn(move || {
                while let Ok(entry) = rx.recv() {
                    entries.push_front(entry);
                    // Coalesce bursts into a single write.
                    while let Ok(more) = rx.try_recv() {
                        entries.push_front(more);
                    }
                    entries.truncate(max);
                    save(&path, &mut entries, max);
                }
            })
            .expect("spawning the history thread");
        Self { tx }
    }

    pub fn record(&self, entry: Entry) {
        let _ = self.tx.send(entry);
    }
}

fn save(path: &Path, entries: &mut VecDeque<Entry>, max: usize) {
    let note = render(entries.make_contiguous(), max);
    if let Err(e) = write_atomically(path, &note) {
        warn!("could not update history note {}: {e}", path.display());
    }
}

pub fn write_atomically(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    // Dot-prefixed so Obsidian ignores it during the instant it exists.
    let tmp = dir.join(format!(".{name}.wispr-tmp"));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(content.as_bytes())?;
        f.sync_data()?;
    }
    fs::rename(&tmp, path)
}

pub fn render(entries: &[Entry], max: usize) -> String {
    let mut out = format!(
        "---\ncssclasses:\n  - {CSS_CLASS}\n---\n\
         # Wispr dictations\n\n\
         > [!tip] Your latest {max} dictations, newest first\n\
         > Hover an entry and click **Copy**. Wispr rewrites this note after every dictation and \
         drops the oldest entry once there are {max}, so edits here are not kept.\n"
    );
    if entries.is_empty() {
        out.push_str("\n*Nothing dictated yet. Tap your hotkey and start talking.*\n");
    }
    for e in entries {
        out.push_str("\n## ");
        out.push_str(&e.when);
        out.push_str(" · ");
        out.push_str(&e.app.replace(['\n', '·'], " "));
        if e.offline {
            out.push_str(" · offline");
        }
        out.push('\n');
        let fence = fence_for(&e.text);
        out.push_str(&fence);
        out.push_str("text\n");
        out.push_str(e.text.trim());
        out.push('\n');
        out.push_str(&fence);
        out.push('\n');
    }
    out
}

/// A backtick fence longer than any backtick run inside `text` (at least three).
fn fence_for(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat((longest + 1).max(3))
}

pub fn parse(md: &str) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut current: Option<Entry> = None;
    let mut fence: Option<String> = None;
    let mut fenced_once = false;
    for line in md.lines() {
        if let Some(f) = &fence {
            if line.trim_end() == f {
                fence = None;
            } else if let Some(e) = current.as_mut() {
                e.text.push_str(line);
                e.text.push('\n');
            }
            continue;
        }
        if let Some(heading) = line.strip_prefix("## ") {
            entries.extend(current.take());
            fenced_once = false;
            let mut parts = heading.split(" · ");
            let when = parts.next().unwrap_or_default().trim().to_string();
            let app = parts.next().unwrap_or_default().trim().to_string();
            let offline = parts.any(|p| p.trim() == "offline");
            current = Some(Entry {
                when,
                app,
                offline,
                text: String::new(),
            });
            continue;
        }
        let Some(e) = current.as_mut() else { continue };
        let ticks = line.len() - line.trim_start_matches('`').len();
        if ticks >= 3 && !fenced_once && e.text.is_empty() {
            fence = Some("`".repeat(ticks));
            fenced_once = true;
        } else if !fenced_once && (!e.text.is_empty() || !line.trim().is_empty()) {
            // Notes written by older versions stored plain text with '#' escaped.
            e.text
                .push_str(line.strip_prefix("\\#").map_or(line, |_| &line[1..]));
            e.text.push('\n');
        }
    }
    entries.extend(current);
    for e in &mut entries {
        e.text = e.text.trim_end().to_string();
    }
    entries
}

/// Expands a leading `~/` to the home directory.
pub fn expand_home(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(when: &str, app: &str, offline: bool, text: &str) -> Entry {
        Entry {
            when: when.into(),
            app: app.into(),
            offline,
            text: text.into(),
        }
    }

    #[test]
    fn each_entry_is_a_copyable_code_block() {
        let md = render(
            &[entry(
                "2026-10-20 14:03:11",
                "Slack",
                false,
                "So let's meet at five.",
            )],
            50,
        );
        assert!(md.starts_with("---\ncssclasses:\n  - wispr-history\n---\n"));
        assert!(
            md.contains("## 2026-10-20 14:03:11 · Slack\n```text\nSo let's meet at five.\n```\n")
        );
    }

    #[test]
    fn render_parse_round_trip() {
        let entries = vec![
            entry(
                "2026-10-20 14:03:11",
                "Slack",
                false,
                "So let's meet at five.",
            ),
            entry(
                "2026-10-20 13:58:02",
                "Cursor",
                true,
                "Line one.\n\n## not a heading\nUse ```rust fences``` here.",
            ),
        ];
        let md = render(&entries, 50);
        assert!(md.contains("## 2026-10-20 13:58:02 · Cursor · offline\n````text\n"));
        assert_eq!(parse(&md), entries);
        assert_eq!(render(&parse(&md), 50), md, "re-rendering is stable");
    }

    #[test]
    fn fence_outgrows_backticks_in_text() {
        assert_eq!(fence_for("plain"), "```");
        assert_eq!(fence_for("a ``` b"), "````");
        assert_eq!(fence_for("`````"), "``````");
    }

    #[test]
    fn reads_notes_from_the_previous_plain_format() {
        let old = "# Wispr dictations\n\nintro\n\n## 2026-10-20 10:00:00 · Notes\nHello.\n\\# hash line\n";
        let parsed = parse(old);
        assert_eq!(
            parsed,
            [entry(
                "2026-10-20 10:00:00",
                "Notes",
                false,
                "Hello.\n# hash line"
            )]
        );
    }

    #[test]
    fn empty_and_foreign_notes() {
        assert!(parse("").is_empty());
        assert!(parse("# Some other note\n\nhello").is_empty());
        assert!(parse(&render(&[], 50)).is_empty());
        assert!(render(&[], 50).contains("Nothing dictated yet"));
    }

    #[test]
    fn writer_keeps_a_rolling_window_and_creates_note_upfront() {
        let dir = std::env::temp_dir().join(format!("wispr-history-test-{}", std::process::id()));
        let path = dir.join("Dictations.md");
        let _ = fs::remove_dir_all(&dir);
        let h = History::start(path.clone(), 3);
        assert!(path.is_file(), "note exists before the first dictation");
        for i in 0..5 {
            h.record(entry(
                &format!("2026-10-20 10:00:0{i}"),
                "Notes",
                false,
                &format!("text {i}"),
            ));
        }
        drop(h);
        let mut md = String::new();
        for _ in 0..100 {
            md = fs::read_to_string(&path).unwrap_or_default();
            if md.contains("text 4") && parse(&md).len() == 3 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let texts: Vec<_> = parse(&md).into_iter().map(|e| e.text).collect();
        assert_eq!(
            texts,
            ["text 4", "text 3", "text 2"],
            "oldest entries dropped"
        );
        assert!(!dir.join(".Dictations.md.wispr-tmp").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn home_expansion() {
        let home = std::env::var("HOME").unwrap_or_default();
        assert_eq!(expand_home("~/x/y.md"), PathBuf::from(home).join("x/y.md"));
        assert_eq!(expand_home("/abs/y.md"), PathBuf::from("/abs/y.md"));
    }
}
