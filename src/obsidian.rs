//! Opens the history note in Obsidian when Wispr starts, and installs the note's styling.
//!
//! Everything here is best effort and off the critical path (runs on its own thread at startup):
//!
//! 1. Find the vault: the nearest ancestor of the note that contains `.obsidian/`.
//! 2. Install a CSS snippet that wraps long dictations instead of scrolling them sideways. It is
//!    scoped to notes with `cssclasses: wispr-history`, so the rest of the vault is unaffected.
//! 3. If Obsidian is not running: make sure the vault is in Obsidian's vault list (otherwise the
//!    `obsidian://` link cannot resolve it), then open the note with `open -g`, which launches
//!    Obsidian in the background without stealing focus from what you are doing.
//!
//! Obsidian's own config files are only edited while Obsidian is closed, because a running
//! Obsidian rewrites them from memory and would discard the change.

use crate::history::{CSS_CLASS, write_atomically};
use crate::{info, warn};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;

const APP: &str = "/Applications/Obsidian.app";
const SNIPPET: &str = "/* Installed by Wispr: wrap long dictations in the history note. */
.wispr-history pre,
.wispr-history pre code,
.wispr-history .HyperMD-codeblock {
  white-space: pre-wrap !important;
  overflow-wrap: anywhere;
}
";

/// Opens `note` in Obsidian in the background (if Obsidian is installed and not already running).
pub fn launch_in_background(note: PathBuf) {
    thread::Builder::new()
        .name("obsidian".into())
        .spawn(move || {
            if let Err(e) = launch(&note) {
                warn!("could not open the history note in Obsidian: {e}");
            }
        })
        .ok();
}

fn launch(note: &Path) -> Result<(), String> {
    if !Path::new(APP).is_dir() {
        return Err("Obsidian is not installed".into());
    }
    let Some(vault) = vault_root(note) else {
        return Err(format!(
            "{} is not inside an Obsidian vault",
            note.display()
        ));
    };
    if is_running() {
        return Ok(());
    }
    install_snippet(&vault).map_err(|e| format!("installing CSS snippet: {e}"))?;
    if let Some(registry) = registry_path() {
        register_vault(&registry, &vault).map_err(|e| format!("registering vault: {e}"))?;
    }
    let url = format!(
        "obsidian://open?path={}",
        percent_encode(&note.to_string_lossy())
    );
    let ok = Command::new("open")
        .args(["-g", &url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if ok {
        info!("opened {} in Obsidian", note.display());
        Ok(())
    } else {
        Err("`open` failed".into())
    }
}

pub fn vault_root(note: &Path) -> Option<PathBuf> {
    note.ancestors()
        .skip(1)
        .find(|dir| dir.join(".obsidian").is_dir())
        .map(Path::to_path_buf)
}

fn is_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "Obsidian"])
        .stdout(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn registry_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("Library/Application Support/obsidian/obsidian.json"))
}

fn install_snippet(vault: &Path) -> std::io::Result<()> {
    let config = vault.join(".obsidian");
    let snippet = config.join("snippets").join(format!("{CSS_CLASS}.css"));
    if fs::read_to_string(&snippet).ok().as_deref() != Some(SNIPPET) {
        fs::create_dir_all(snippet.parent().unwrap_or(&config))?;
        write_atomically(&snippet, SNIPPET)?;
    }
    let appearance = config.join("appearance.json");
    let mut json: Value = fs::read_to_string(&appearance)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    if enable_snippet(&mut json) {
        write_atomically(&appearance, &pretty(&json))?;
        info!("enabled the {CSS_CLASS} CSS snippet in {}", vault.display());
    }
    Ok(())
}

/// Adds the snippet to `enabledCssSnippets`; returns whether anything changed.
fn enable_snippet(appearance: &mut Value) -> bool {
    let Some(obj) = appearance.as_object_mut() else {
        return false;
    };
    let list = obj.entry("enabledCssSnippets").or_insert_with(|| json!([]));
    let Some(list) = list.as_array_mut() else {
        return false;
    };
    if list.iter().any(|v| v == CSS_CLASS) {
        return false;
    }
    list.push(json!(CSS_CLASS));
    true
}

fn register_vault(registry: &Path, vault: &Path) -> std::io::Result<()> {
    let mut json: Value = fs::read_to_string(registry)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({ "vaults": {} }));
    if add_vault(&mut json, vault, now_ms()) {
        // Keep a copy of Obsidian's original file the first time we touch it.
        let backup = registry.with_extension("json.wispr-backup");
        if registry.is_file() && !backup.exists() {
            fs::copy(registry, &backup)?;
        }
        write_atomically(registry, &pretty(&json))?;
        info!("added {} to Obsidian's vault list", vault.display());
    }
    Ok(())
}

/// Ensures `vault` is listed and opened at launch; stale entries whose folder no longer exists
/// stop auto-opening (they would only show a "vault not found" window). Returns whether anything
/// changed.
fn add_vault(registry: &mut Value, vault: &Path, ts: u64) -> bool {
    let vault_str = vault.to_string_lossy().to_string();
    let Some(root) = registry.as_object_mut() else {
        return false;
    };
    let vaults = root.entry("vaults").or_insert_with(|| json!({}));
    let Some(vaults) = vaults.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    for entry in vaults.values_mut() {
        let path = entry["path"].as_str().map(PathBuf::from);
        if path.as_ref().is_some_and(|p| !p.exists()) && entry["open"] == json!(true) {
            entry["open"] = json!(false);
            changed = true;
        }
    }
    if !vaults
        .values()
        .any(|e| e["path"].as_str() == Some(vault_str.as_str()))
    {
        let id = format!("{:016x}", ts.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        vaults.insert(id, json!({ "path": vault_str, "ts": ts, "open": true }));
        changed = true;
    }
    changed
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_vault_root() {
        let dir = std::env::temp_dir().join(format!("wispr-vault-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join(".obsidian")).ok();
        fs::create_dir_all(dir.join("Sub")).ok();
        assert_eq!(vault_root(&dir.join("Sub/Note.md")), Some(dir.clone()));
        assert_eq!(vault_root(Path::new("/definitely/not/a/vault/x.md")), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snippet_is_enabled_once_and_other_settings_survive() {
        let mut a = json!({ "cssTheme": "AnuPpuccin", "enabledCssSnippets": ["mine"] });
        assert!(enable_snippet(&mut a));
        assert!(!enable_snippet(&mut a), "idempotent");
        assert_eq!(a["cssTheme"], "AnuPpuccin");
        assert_eq!(a["enabledCssSnippets"], json!(["mine", CSS_CLASS]));
        let mut empty = json!({});
        assert!(enable_snippet(&mut empty));
    }

    #[test]
    fn vault_registration_adds_once_and_parks_stale_vaults() {
        let mut r =
            json!({ "vaults": { "abc": { "path": "/no/such/vault", "ts": 1, "open": true } } });
        let vault = std::env::temp_dir();
        assert!(add_vault(&mut r, &vault, 42));
        assert!(!add_vault(&mut r, &vault, 43), "idempotent");
        let vaults = r["vaults"].as_object().cloned().unwrap_or_default();
        assert_eq!(vaults.len(), 2);
        assert_eq!(
            vaults["abc"]["open"], false,
            "missing vault no longer auto-opens"
        );
        assert_eq!(vaults["abc"]["path"], "/no/such/vault", "but is kept");
        assert!(
            vaults
                .values()
                .any(|v| v["path"] == vault.to_string_lossy().as_ref() && v["open"] == true)
        );
    }

    #[test]
    fn obsidian_url_encoding() {
        assert_eq!(
            percent_encode("/Users/me/My Vault/Wispr Dictations.md"),
            "/Users/me/My%20Vault/Wispr%20Dictations.md"
        );
    }
}
