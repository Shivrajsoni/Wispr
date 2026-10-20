//! Which app will receive the text, and what kind of app it is.
//!
//! The focused app comes from the Accessibility API (`AXFocusedApplication`), which Wispr is
//! already allowed to use for typing. Its executable path then gives the app name. The window
//! list (`active-win-pos-rs`) is only a fallback: under launchd it fails without the Screen
//! Recording permission, which made every app "unknown". Window titles are never read.

use std::ffi::{CString, c_char, c_void};

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
    if let Some(path) = focused_app_pid().and_then(executable_path) {
        let name = app_name_from_path(&path);
        return FrontApp {
            class: classify(&name, &path),
            name,
        };
    }
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

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXUIElementCreateSystemWide() -> *const c_void;
    fn AXUIElementCopyAttributeValue(
        element: *const c_void,
        attribute: *const c_void,
        value: *mut *const c_void,
    ) -> i32;
    fn AXUIElementGetPid(element: *const c_void, pid: *mut i32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const c_char,
        encoding: u32,
    ) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

unsafe extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, size: u32) -> i32;
}

const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
const PROC_PIDPATHINFO_MAXSIZE: usize = 4096;

/// PID of the app that currently has keyboard focus, via the Accessibility API.
fn focused_app_pid() -> Option<i32> {
    let attribute = CString::new("AXFocusedApplication").ok()?;
    // SAFETY: every CF object created or copied here is released exactly once below; the
    // out-pointers are valid locals; null results are checked before use.
    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return None;
        }
        let name = CFStringCreateWithCString(
            std::ptr::null(),
            attribute.as_ptr(),
            CF_STRING_ENCODING_UTF8,
        );
        let mut app: *const c_void = std::ptr::null();
        let err = if name.is_null() {
            -1
        } else {
            AXUIElementCopyAttributeValue(system, name, &mut app)
        };
        let mut pid = 0;
        let ok = err == 0 && !app.is_null() && AXUIElementGetPid(app, &mut pid) == 0 && pid > 0;
        if !app.is_null() {
            CFRelease(app);
        }
        if !name.is_null() {
            CFRelease(name);
        }
        CFRelease(system);
        ok.then_some(pid)
    }
}

fn executable_path(pid: i32) -> Option<String> {
    let mut buf = vec![0u8; PROC_PIDPATHINFO_MAXSIZE];
    // SAFETY: the buffer is valid for `buf.len()` bytes; proc_pidpath writes at most that many.
    let len = unsafe { proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if len <= 0 {
        return None;
    }
    buf.truncate(len as usize);
    String::from_utf8(buf).ok()
}

/// "Visual Studio Code" from ".../Visual Studio Code.app/Contents/MacOS/Electron"; the outermost
/// bundle wins, so helper apps nested inside another app report the parent app.
pub fn app_name_from_path(path: &str) -> String {
    path.split('/')
        .find_map(|seg| seg.strip_suffix(".app"))
        .or_else(|| path.rsplit('/').next())
        .unwrap_or("unknown")
        .to_string()
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
    "antigravity",
    "antigravity ide",
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
        assert_eq!(classify("Antigravity IDE", ""), AppClass::Code);
        assert_eq!(classify("IntelliJ IDEA", ""), AppClass::Code);
        assert_eq!(classify("Slack", ""), AppClass::Chat);
        assert_eq!(classify("Messages", ""), AppClass::Chat);
        assert_eq!(classify("Mail", ""), AppClass::Prose);
        assert_eq!(classify("Notes", ""), AppClass::Prose);
    }

    #[test]
    fn app_names_from_executable_paths() {
        assert_eq!(
            app_name_from_path("/Applications/Visual Studio Code.app/Contents/MacOS/Electron"),
            "Visual Studio Code"
        );
        assert_eq!(
            app_name_from_path(
                "/Applications/Slack.app/Contents/Frameworks/Slack Helper.app/Contents/MacOS/Slack Helper"
            ),
            "Slack"
        );
        assert_eq!(app_name_from_path("/usr/local/bin/nvim"), "nvim");
        let path = "/Applications/Ghostty.app/Contents/MacOS/ghostty";
        assert_eq!(
            classify(&app_name_from_path(path), path),
            AppClass::Terminal
        );
    }

    #[test]
    fn avoids_substring_false_positives() {
        // "code" must not match "Xcode"-like names by accident, nor "Kitty Litter Tracker" etc.
        assert_eq!(classify("Barcode Scanner", ""), AppClass::Prose);
        assert_eq!(classify("Notion", ""), AppClass::Prose);
        assert_eq!(classify("Teamspeak", ""), AppClass::Prose);
    }
}
