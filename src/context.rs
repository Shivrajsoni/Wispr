//! Which app will receive the text, and what kind of app it is.
//!
//! Uses the frontmost window's owning application. Only the app name and bundle path are read;
//! window titles are never read, so no Screen Recording permission is needed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppClass {
    /// Shells: no trailing period, no auto-capitalization.
    Terminal,
    /// Editors and IDEs: text is kept exactly as transcribed.
    Code,
    /// Messaging: one-line messages lose their trailing period (configurable).
    Chat,
    /// Everything else: normal prose.
    Prose,
}

#[derive(Debug, Clone)]
pub struct FrontApp {
    pub name: String,
    pub class: AppClass,
}

pub fn frontmost_app() -> FrontApp {
    match active_win_pos_rs::get_active_window() {
        Ok(w) => FrontApp {
            class: classify(&w.app_name, &w.process_path.to_string_lossy()),
            name: w.app_name,
        },
        Err(()) => FrontApp {
            name: "unknown".into(),
            class: AppClass::Prose,
        },
    }
}

const TERMINALS: &[&str] = &[
    "terminal",
    "iterm",
    "ghostty",
    "alacritty",
    "kitty",
    "wezterm",
    "warp",
    "hyper",
    "tabby",
    "rio",
];
const EDITORS: &[&str] = &[
    "visual studio code",
    "code",
    "cursor",
    "windsurf",
    "zed",
    "xcode",
    "sublime text",
    "nova",
    "intellij",
    "pycharm",
    "webstorm",
    "goland",
    "rustrover",
    "clion",
    "android studio",
    "neovide",
    "fleet",
    "bbedit",
    "textmate",
];
const CHATS: &[&str] = &[
    "slack",
    "discord",
    "messages",
    "whatsapp",
    "telegram",
    "signal",
    "microsoft teams",
    "teams",
    "messenger",
    "beeper",
    "element",
];

pub fn classify(app_name: &str, process_path: &str) -> AppClass {
    let name = app_name.trim().to_lowercase();
    // The bundle name in the path catches helpers whose process name differs from the app.
    let bundle = process_path
        .split('/')
        .find(|seg| seg.ends_with(".app"))
        .map(|seg| seg.trim_end_matches(".app").to_lowercase())
        .unwrap_or_default();
    let matches = |list: &[&str]| {
        list.iter().any(|&k| {
            name == k
                || bundle == k
                || name.starts_with(&format!("{k} "))
                || bundle.starts_with(&format!("{k} "))
        })
    };
    if matches(TERMINALS) {
        AppClass::Terminal
    } else if matches(EDITORS) {
        AppClass::Code
    } else if matches(CHATS) {
        AppClass::Chat
    } else {
        AppClass::Prose
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_apps() {
        assert_eq!(
            classify(
                "Ghostty",
                "/Applications/Ghostty.app/Contents/MacOS/ghostty"
            ),
            AppClass::Terminal
        );
        assert_eq!(classify("Terminal", ""), AppClass::Terminal);
        assert_eq!(
            classify(
                "Code",
                "/Applications/Visual Studio Code.app/Contents/MacOS/Electron"
            ),
            AppClass::Code
        );
        assert_eq!(classify("Cursor", ""), AppClass::Code);
        assert_eq!(classify("IntelliJ IDEA", ""), AppClass::Code);
        assert_eq!(classify("Slack", ""), AppClass::Chat);
        assert_eq!(classify("Messages", ""), AppClass::Chat);
        assert_eq!(classify("Mail", ""), AppClass::Prose);
        assert_eq!(classify("Notes", ""), AppClass::Prose);
    }

    #[test]
    fn avoids_substring_false_positives() {
        // "code" must not match "Xcode"-like names by accident, nor "Kitty Litter Tracker" etc.
        assert_eq!(classify("Barcode Scanner", ""), AppClass::Prose);
        assert_eq!(classify("Notion", ""), AppClass::Prose);
        assert_eq!(classify("Teamspeak", ""), AppClass::Prose);
    }
}
