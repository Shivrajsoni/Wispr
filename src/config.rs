//! Configuration: one TOML file, plus `DEEPGRAM_API_KEY` from the environment or a `.env` file.
//!
//! Lookup order for the config file: `--config <path>`, `$WISPR_CONFIG`, `./config.toml`,
//! then `~/Library/Application Support/wispr/config.toml`. Every field has a default, so
//! running with no config file at all is valid. Relative paths inside the file (such as the
//! Whisper model) resolve against the config file's directory first, then the working directory.

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub hotkey: HotkeyConfig,
    pub transcription: TranscriptionConfig,
    pub deepgram: DeepgramConfig,
    pub local: LocalConfig,
    pub audio: AudioConfig,
    pub output: OutputConfig,
    pub history: HistoryConfig,
    /// Canonical spelling -> spoken/misheard variants. Keys are also sent to Deepgram as keyterms.
    pub vocabulary: BTreeMap<String, Vec<String>>,
    #[serde(skip)]
    pub source: Option<PathBuf>,
    #[serde(skip)]
    pub base_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HotkeyMode {
    /// Tap to start, tap again to finish.
    Toggle,
    /// Hold to talk, release to finish.
    Hold,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HotkeyConfig {
    pub key: String,
    pub mode: HotkeyMode,
    pub cancel_key: String,
}

impl Default for HotkeyConfig {
    fn default() -> Self {
        Self {
            key: "Fn".into(),
            mode: HotkeyMode::Toggle,
            cancel_key: "Escape".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnginePreference {
    /// Deepgram when an API key is available, otherwise local Whisper.
    Auto,
    Deepgram,
    Local,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TranscriptionConfig {
    pub engine: EnginePreference,
    pub language: String,
    pub max_recording_seconds: u64,
}

impl Default for TranscriptionConfig {
    fn default() -> Self {
        Self {
            engine: EnginePreference::Auto,
            language: "en".into(),
            max_recording_seconds: 300,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DeepgramConfig {
    pub model: String,
    /// Which Deepgram endpoint to use.
    pub region: Region,
    /// Prefer the `DEEPGRAM_API_KEY` environment variable or `.env`; this is a last resort.
    pub api_key: Option<String>,
    pub connect_timeout_ms: u64,
    /// How long to wait for the final transcript after you stop speaking before falling back.
    pub finalize_timeout_ms: u64,
    pub smart_format: bool,
    /// Convert spoken "comma", "period", "new paragraph" into punctuation.
    pub spoken_punctuation: bool,
}

impl Default for DeepgramConfig {
    fn default() -> Self {
        Self {
            model: "nova-3".into(),
            region: Region::Auto,
            api_key: None,
            connect_timeout_ms: 3000,
            finalize_timeout_ms: 1500,
            smart_format: true,
            spoken_punctuation: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Region {
    /// Probe both endpoints at startup and use the one with the shorter round trip.
    Auto,
    /// api.deepgram.com
    Us,
    /// api.eu.deepgram.com (audio is processed in the EU)
    Eu,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    /// Markdown note that keeps your latest dictations, e.g. inside an Obsidian vault.
    /// Empty disables it. A leading `~/` is expanded.
    pub path: String,
    pub max_entries: usize,
    /// When the note is inside an Obsidian vault, open it in Obsidian (in the background) when
    /// Wispr starts, unless Obsidian is already running.
    pub open_obsidian: bool,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            path: String::new(),
            max_entries: 50,
            open_obsidian: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub model_path: PathBuf,
    pub threads: u8,
    /// Load Whisper at startup and use it when Deepgram is unreachable or times out.
    pub fallback: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from("models/ggml-small.en-q5_1.bin"),
            threads: 4,
            fallback: true,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AudioConfig {
    /// Input device name (see `wispr --list-devices`). Empty means the system default.
    pub device: String,
    /// Keep the microphone open this long after a dictation so back-to-back dictations
    /// start instantly. 0 closes it immediately (the mic indicator turns off right away).
    pub keep_warm_seconds: u64,
    /// Cancel a dictation when no speech is heard for this many seconds after it starts.
    /// 0 disables the timeout.
    pub no_speech_timeout_seconds: u64,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            device: String::new(),
            keep_warm_seconds: 0,
            no_speech_timeout_seconds: 6,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InjectMethod {
    /// Put the text on the pasteboard, press Cmd+V, then restore the previous pasteboard.
    Paste,
    /// Synthesize the text as keystrokes. Slower, but never touches the pasteboard.
    Type,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub method: InjectMethod,
    pub restore_clipboard: bool,
    pub restore_delay_ms: u64,
    pub trailing_space: bool,
    pub resolve_corrections: bool,
    /// Drop the trailing period of one-sentence messages in chat apps.
    pub casual_chat: bool,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            method: InjectMethod::Paste,
            restore_clipboard: true,
            restore_delay_ms: 250,
            trailing_space: true,
            resolve_corrections: true,
            casual_chat: true,
        }
    }
}

impl Config {
    /// Finds and parses the config file. A missing file yields defaults; a broken one is an error.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let path = match explicit {
            Some(p) => {
                if !p.exists() {
                    bail!("config file {} does not exist", p.display());
                }
                Some(p.to_path_buf())
            }
            None => Self::candidates().into_iter().find(|p| p.is_file()),
        };

        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let Some(path) = path else {
            return Ok(Self {
                base_dir: cwd,
                ..Self::default()
            });
        };

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let mut cfg =
            Self::parse(&content).with_context(|| format!("parsing {}", path.display()))?;
        cfg.base_dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or(cwd);
        cfg.source = Some(path);
        Ok(cfg)
    }

    pub fn parse(content: &str) -> Result<Self> {
        let cfg: Self = toml::from_str(content)?;
        if cfg.local.threads == 0 {
            bail!("local.threads must be at least 1");
        }
        Ok(cfg)
    }

    fn candidates() -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(p) = std::env::var_os("WISPR_CONFIG") {
            out.push(PathBuf::from(p));
        }
        out.push(PathBuf::from("config.toml"));
        if let Some(p) = user_config_path() {
            out.push(p);
        }
        out
    }

    /// Resolves a possibly-relative path from the config file, falling back to the working directory.
    pub fn resolve(&self, p: &Path) -> PathBuf {
        if p.is_absolute() {
            return p.to_path_buf();
        }
        let from_config = self.base_dir.join(p);
        if from_config.exists() {
            return from_config;
        }
        let from_cwd = PathBuf::from(p);
        if from_cwd.exists() {
            from_cwd
        } else {
            from_config
        }
    }

    pub fn local_model_path(&self) -> PathBuf {
        self.resolve(&self.local.model_path)
    }

    /// `DEEPGRAM_API_KEY` from the environment, then `deepgram.api_key`, then a `.env` file
    /// next to the config file or in the working directory.
    pub fn deepgram_api_key(&self) -> Option<String> {
        const VAR: &str = "DEEPGRAM_API_KEY";
        if let Ok(v) = std::env::var(VAR)
            && !v.trim().is_empty()
        {
            return Some(v.trim().to_string());
        }
        if let Some(k) = self
            .deepgram
            .api_key
            .as_ref()
            .filter(|k| !k.trim().is_empty())
        {
            return Some(k.trim().to_string());
        }
        [self.base_dir.join(".env"), PathBuf::from(".env")]
            .iter()
            .filter_map(|p| std::fs::read_to_string(p).ok())
            .find_map(|content| dotenv_value(&content, VAR))
    }

    pub fn history_path(&self) -> Option<PathBuf> {
        let p = self.history.path.trim();
        (!p.is_empty()).then(|| crate::history::expand_home(p))
    }

    pub fn audio_device(&self) -> Option<&str> {
        let d = self.audio.device.trim();
        (!d.is_empty()).then_some(d)
    }
}

pub fn user_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(|home| PathBuf::from(home).join("Library/Application Support/wispr/config.toml"))
}

/// Reads `KEY=value` from dotenv-formatted text, tolerating `export`, quotes, and comments.
pub fn dotenv_value(content: &str, key: &str) -> Option<String> {
    content.lines().find_map(|line| {
        let line = line.trim();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .or_else(|| v.strip_prefix('\'').and_then(|s| s.strip_suffix('\'')))
            .unwrap_or(v);
        (!v.is_empty()).then(|| v.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let cfg = Config::parse(include_str!("../config.example.toml")).unwrap();
        assert_eq!(cfg.hotkey.mode, HotkeyMode::Toggle);
        assert_eq!(cfg.deepgram.model, "nova-3");
        assert!(!cfg.vocabulary.is_empty());
    }

    #[test]
    fn empty_config_uses_defaults() {
        let cfg = Config::parse("").unwrap();
        assert_eq!(cfg.transcription.engine, EnginePreference::Auto);
        assert_eq!(cfg.output.method, InjectMethod::Paste);
    }

    #[test]
    fn typos_are_rejected() {
        assert!(Config::parse("[hotkey]\nmdoe = \"hold\"").is_err());
    }

    #[test]
    fn dotenv_parsing() {
        let env = "# comment\nOTHER=1\nexport DEEPGRAM_API_KEY=\"abc123\"\n";
        assert_eq!(
            dotenv_value(env, "DEEPGRAM_API_KEY").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            dotenv_value("DEEPGRAM_API_KEY='x y'", "DEEPGRAM_API_KEY").as_deref(),
            Some("x y")
        );
        assert_eq!(dotenv_value("DEEPGRAM_API_KEY=", "DEEPGRAM_API_KEY"), None);
        assert_eq!(dotenv_value("NOPE=1", "DEEPGRAM_API_KEY"), None);
    }
}
