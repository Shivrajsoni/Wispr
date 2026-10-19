//! The control loop: owns every piece of mutable state and is the only thread that touches it.
//!
//! Each iteration waits for a [`Signal`] (hotkey, terminal, quit) with a short timeout while
//! busy, or a long one while idle (so an idle daemon uses ~0% CPU), then:
//! 1. drains the microphone ring, resamples to 16 kHz, appends to the utterance buffer and
//!    streams PCM16 to the Deepgram session;
//! 2. enforces the max recording length and reacts to microphone loss;
//! 3. polls the in-flight transcription, falling back to local Whisper on any Deepgram failure.
//!
//! ## Online/offline routing
//!
//! * A circuit breaker ([`Breaker`]) decides per dictation whether to try Deepgram. After a
//!   failure it stays open (local only, no network attempt) for 30 s doubling to 5 min, or an
//!   hour for a rejected key; the first dictation after the cooldown probes Deepgram again.
//! * If Deepgram has not even finished connecting when you release the key, waiting would only
//!   add round trips, so the audio goes to local Whisper immediately.
//! * If it is connected but the final transcript takes longer than `finalize_timeout_ms`, or
//!   any error occurs, the same in-RAM audio is transcribed locally. Nothing is lost.
//!
//! Audio lives only in RAM and is cleared at the start of every recording; nothing is written
//! to disk. Dictated text is redacted from logs in release builds.

use crate::asr::Source;
use crate::asr::breaker::{Breaker, Transition};
use crate::asr::deepgram::{
    DeepgramOptions, EU_HOST, FailureKind, SessionError, StreamingSession, US_HOST, fastest_host,
};
use crate::asr::whisper::LocalWhisper;
use crate::audio::resample::Resampler;
use crate::audio::{Microphone, f32_to_pcm16};
use crate::config::{Config, EnginePreference, Region};
use crate::context::{FrontApp, frontmost_app};
use crate::history::{Entry, History};
use crate::hud::Hud;
use crate::inject::{Injector, Outcome};
use crate::log::Redacted;
use crate::pipeline::{Action, Event, Machine, State};
use crate::text::PostProcessor;
use crate::{info, macos, vad, warn};
use anyhow::{Result, anyhow, bail};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

pub enum Exit {
    Quit,
    Restart,
}

pub enum Signal {
    Input(Event),
    HotkeyFailed(String),
    Quit,
}

/// Stream audio to Deepgram in ~100 ms frames (3200 bytes of 16 kHz PCM16).
const STREAM_CHUNK_BYTES: usize = 3_200;
const BUSY_POLL: Duration = Duration::from_millis(5);
const IDLE_POLL: Duration = Duration::from_secs(1);

pub struct Engines {
    pub deepgram: Option<DeepgramOptions>,
    pub local: Option<LocalWhisper>,
}

impl Engines {
    /// Resolves the configured engine preference against what is actually available.
    pub fn load(cfg: &Config) -> Result<Self> {
        let key = cfg.deepgram_api_key();
        let use_deepgram = match cfg.transcription.engine {
            EnginePreference::Auto => key.is_some(),
            EnginePreference::Deepgram if key.is_none() => {
                bail!(
                    "engine is \"deepgram\" but no DEEPGRAM_API_KEY was found (environment, config, or .env)"
                )
            }
            EnginePreference::Deepgram => true,
            EnginePreference::Local => false,
        };
        let deepgram = match (use_deepgram, key) {
            (true, Some(api_key)) => Some(deepgram_options(cfg, api_key, &deepgram_host(cfg))),
            _ => None,
        };

        let local = if deepgram.is_none() || cfg.local.fallback {
            let path = cfg.local_model_path();
            match LocalWhisper::load(&path, cfg.local.threads, &cfg.transcription.language) {
                Ok(w) => Some(w),
                Err(e) if deepgram.is_some() => {
                    warn!("offline fallback disabled: {e:#}");
                    None
                }
                Err(e) => return Err(e),
            }
        } else {
            None
        };
        Ok(Self { deepgram, local })
    }
}

/// The Deepgram endpoint to use; for `region = "auto"`, whichever answers a TCP connect faster.
pub fn deepgram_host(cfg: &Config) -> String {
    // Escape hatch for proxies and for testing the offline path.
    if let Ok(host) = std::env::var("WISPR_DEEPGRAM_HOST")
        && !host.trim().is_empty()
    {
        return host.trim().to_string();
    }
    match cfg.deepgram.region {
        Region::Us => US_HOST.into(),
        Region::Eu => EU_HOST.into(),
        Region::Auto => {
            let (best, probes) = fastest_host(Duration::from_millis(1500));
            let timings: Vec<String> = probes
                .iter()
                .map(|(h, d)| match d {
                    Some(d) => format!("{h} {} ms", d.as_millis()),
                    None => format!("{h} unreachable"),
                })
                .collect();
            info!("deepgram region: {best} ({})", timings.join(", "));
            best.into()
        }
    }
}

pub fn deepgram_options(cfg: &Config, api_key: String, host: &str) -> DeepgramOptions {
    DeepgramOptions {
        host: host.into(),
        api_key,
        model: cfg.deepgram.model.clone(),
        language: cfg.transcription.language.clone(),
        keyterms: cfg.vocabulary.keys().cloned().collect(),
        smart_format: cfg.deepgram.smart_format,
        spoken_punctuation: cfg.deepgram.spoken_punctuation,
        connect_timeout: Duration::from_millis(cfg.deepgram.connect_timeout_ms),
        finalize_timeout: Duration::from_millis(cfg.deepgram.finalize_timeout_ms),
    }
}

/// Initial prompt that biases Whisper toward the user's vocabulary.
pub fn whisper_prompt(cfg: &Config) -> String {
    if cfg.vocabulary.is_empty() {
        String::new()
    } else {
        let terms: Vec<&str> = cfg.vocabulary.keys().map(String::as_str).collect();
        format!("Glossary: {}.", terms.join(", "))
    }
}

pub struct Daemon {
    cfg: Config,
    machine: Machine,
    hud: Hud,
    injector: Injector,
    post: PostProcessor,
    deepgram: Option<DeepgramOptions>,
    local: Option<LocalWhisper>,
    whisper_prompt: String,

    mic: Option<Microphone>,
    resampler: Option<Resampler>,
    mic_idle_since: Option<Instant>,
    device_check_due: bool,
    raw: Vec<f32>,
    fresh: Vec<f32>,
    pcm: Vec<u8>,
    utterance: Vec<f32>,

    session: Option<StreamingSession>,
    breaker: Breaker,
    history: Option<History>,
    recording_since: Option<Instant>,
    stopped_at: Option<Instant>,
    target: Option<FrontApp>,
    transcript_info: Option<(Source, Duration, Duration)>,
    notice_after_inject: Option<&'static str>,
    dry_run: bool,
    test_audio: Option<TestAudio>,
}

/// A WAV file played into the pipeline in place of the microphone (`WISPR_TEST_AUDIO`), for
/// end-to-end tests without a human speaking.
struct TestAudio {
    samples: Vec<f32>,
    pos: usize,
}

fn load_test_audio() -> Option<TestAudio> {
    let path = std::env::var_os("WISPR_TEST_AUDIO")?;
    let result = std::fs::read(&path)
        .map_err(anyhow::Error::from)
        .and_then(|b| crate::audio::wav::parse(&b));
    match result {
        Ok(w) => {
            let mut samples = Vec::new();
            Resampler::new(w.sample_rate, w.channels).process(&w.samples, &mut samples);
            info!(
                "test audio: {:.1} s from {} replaces the microphone",
                samples.len() as f64 / 16_000.0,
                std::path::Path::new(&path).display()
            );
            Some(TestAudio { samples, pos: 0 })
        }
        Err(e) => {
            warn!("ignoring WISPR_TEST_AUDIO: {e:#}");
            None
        }
    }
}

impl Daemon {
    pub fn new(cfg: Config, engines: Engines, hud: Hud) -> Result<Self> {
        let injector = Injector::new(&cfg.output)?;
        let post = PostProcessor::new(&cfg.vocabulary, &cfg.output);
        let whisper_prompt = whisper_prompt(&cfg);
        let history = cfg.history_path().map(|p| {
            let history = History::start(p.clone(), cfg.history.max_entries);
            if cfg.history.open_obsidian {
                crate::obsidian::launch_in_background(p);
            }
            history
        });
        Ok(Self {
            machine: Machine::new(cfg.hotkey.mode),
            hud,
            injector,
            post,
            deepgram: engines.deepgram,
            local: engines.local,
            whisper_prompt,
            cfg,
            mic: None,
            resampler: None,
            mic_idle_since: None,
            device_check_due: false,
            raw: Vec::with_capacity(16_384),
            fresh: Vec::with_capacity(8_192),
            pcm: Vec::with_capacity(STREAM_CHUNK_BYTES * 2),
            utterance: Vec::new(),
            session: None,
            breaker: Breaker::default(),
            history,
            recording_since: None,
            stopped_at: None,
            target: None,
            transcript_info: None,
            notice_after_inject: None,
            dry_run: false,
            test_audio: load_test_audio(),
        })
    }

    /// Runs until quit. Returns [`Exit::Restart`] once missing permissions have been granted,
    /// because macOS only applies them to the hotkey listener of a freshly started process.
    pub fn run(mut self, signals: Receiver<Signal>) -> Result<Exit> {
        let mut awaiting_permissions = !macos::permissions_granted();
        if awaiting_permissions {
            warn!("waiting for permissions. {}", macos::PERMISSIONS_HELP);
            macos::request_missing_permissions();
            self.hud
                .alert("Allow Wispr in Accessibility & Input Monitoring");
        }
        loop {
            let idle = self.machine.state() == State::Idle
                && !self.mic.as_ref().is_some_and(Microphone::is_running);
            match signals.recv_timeout(if idle { IDLE_POLL } else { BUSY_POLL }) {
                Ok(Signal::Input(event)) => self.dispatch(event),
                Ok(Signal::HotkeyFailed(reason)) if macos::permissions_granted() => {
                    return Err(anyhow!(
                        "the global hotkey could not be registered ({reason})"
                    ));
                }
                Ok(Signal::HotkeyFailed(_)) => awaiting_permissions = true,
                Ok(Signal::Quit) | Err(RecvTimeoutError::Disconnected) => return Ok(Exit::Quit),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
            if awaiting_permissions && idle && macos::permissions_granted() {
                info!("permissions granted; restarting to activate the hotkey");
                return Ok(Exit::Restart);
            }
        }
    }

    fn tick(&mut self) {
        let state = self.machine.state();
        self.pump_audio(state == State::Listening);

        if state == State::Listening {
            let too_long = self.recording_since.is_some_and(|t| {
                t.elapsed() > Duration::from_secs(self.cfg.transcription.max_recording_seconds)
            });
            let mic_lost = self.mic.as_ref().is_some_and(Microphone::has_failed);
            if too_long {
                info!("maximum recording length reached; finishing");
                self.dispatch(Event::ForceStop);
            } else if mic_lost {
                warn!("microphone disconnected; transcribing what was captured");
                self.dispatch(Event::ForceStop);
            }
        }

        if self.machine.state() == State::Processing {
            self.poll_transcription();
        }

        if self.machine.state() == State::Idle {
            let warm_expired = self.mic_idle_since.is_some_and(|t| {
                t.elapsed() >= Duration::from_secs(self.cfg.audio.keep_warm_seconds)
            });
            if warm_expired {
                self.stop_mic();
            }
            if self.mic.as_ref().is_some_and(Microphone::has_failed) {
                warn!("microphone lost; it will be reopened on the next dictation");
                self.mic = None;
            }
            // Off the critical path: follow the system default input if it changed
            // (e.g. headphones plugged in), so the next dictation uses the right mic.
            if std::mem::take(&mut self.device_check_due)
                && self.cfg.audio_device().is_none()
                && self
                    .mic
                    .as_ref()
                    .is_some_and(|m| !m.is_running() && !m.is_system_default())
            {
                info!("default microphone changed");
                self.mic = None;
                if let Err(e) = self.prepare_mic() {
                    warn!("cannot open the new default microphone: {e:#}");
                }
            }
        }
    }

    fn dispatch(&mut self, event: Event) {
        for action in self.machine.handle(event) {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Action) {
        match action {
            Action::StartRecording => {
                if let Err(e) = self.begin_recording() {
                    warn!("cannot open the microphone: {e:#}");
                    self.dispatch(Event::RecordingFailed);
                }
            }
            Action::StopRecording => self.end_recording(),
            Action::DiscardRecording => {
                self.session = None;
                self.utterance.clear();
                self.park_mic();
                info!("cancelled");
            }
            Action::DiscardTranscription => {
                self.session = None;
                info!("cancelled");
            }
            Action::Inject(text) => self.inject(&text),
            Action::ShowListening => self.hud.listening(),
            Action::ShowProcessing => self.hud.processing(),
            Action::Hide => self.hud.hide(),
            Action::Notice(message) => {
                info!("{message}");
                self.hud.notice(message);
            }
            Action::Alert(message) => {
                info!("{message}");
                self.hud.alert(message);
            }
        }
    }

    /// Log transcripts instead of typing them (for testing without touching other apps).
    pub fn set_dry_run(&mut self, on: bool) {
        self.dry_run = on;
    }

    /// Builds the input stream without starting it, so a dictation only has to start it.
    pub fn prepare_mic(&mut self) -> Result<()> {
        if self.test_audio.is_some() {
            return Ok(());
        }
        let t = Instant::now();
        let mic = Microphone::open(self.cfg.audio_device())?;
        info!(
            "mic: {} ({} ch @ {} Hz), prepared in {} ms",
            mic.name,
            mic.channels,
            mic.sample_rate,
            t.elapsed().as_millis()
        );
        self.mic = Some(mic);
        Ok(())
    }

    fn begin_recording(&mut self) -> Result<()> {
        if let Some(test) = self.test_audio.as_mut() {
            test.pos = 0;
        } else {
            if self.mic.as_ref().is_none_or(Microphone::has_failed) {
                self.mic = None;
                self.prepare_mic()?;
            }
            let Some(mic) = self.mic.as_mut() else {
                bail!("no microphone");
            };
            mic.start()?;
            self.resampler = Some(Resampler::new(mic.sample_rate, mic.channels));
        }
        self.mic_idle_since = None;
        self.utterance.clear();
        self.pcm.clear();
        self.session = match &self.deepgram {
            Some(opts) if self.breaker.allows(Instant::now()) => {
                Some(StreamingSession::start(opts.clone()))
            }
            Some(_) => {
                info!("offline mode: this dictation uses local Whisper");
                None
            }
            None => None,
        };
        self.recording_since = Some(Instant::now());
        self.target = None;
        Ok(())
    }

    fn pump_audio(&mut self, capturing: bool) {
        self.fresh.clear();
        if let Some(test) = self.test_audio.as_mut() {
            if !capturing {
                return;
            }
            // Release samples at real-time speed, exactly as a microphone would.
            let elapsed = self.recording_since.map_or(Duration::ZERO, |t| t.elapsed());
            let due = ((elapsed.as_secs_f64() * 16_000.0) as usize).min(test.samples.len());
            self.fresh
                .extend_from_slice(&test.samples[test.pos.min(due)..due]);
            test.pos = due;
        } else {
            let Some(mic) = self.mic.as_mut() else { return };
            self.raw.clear();
            mic.drain(&mut self.raw);
            let dropped = mic.take_dropped();
            if dropped > 0 {
                warn!("audio buffer overflow: dropped {dropped} samples");
            }
            if !capturing || self.raw.is_empty() {
                return;
            }
            let Some(resampler) = self.resampler.as_mut() else {
                return;
            };
            resampler.process(&self.raw, &mut self.fresh);
        }
        if self.fresh.is_empty() {
            return;
        }
        self.utterance.extend_from_slice(&self.fresh);
        self.hud.level(&self.fresh);
        if let Some(session) = &self.session {
            f32_to_pcm16(&self.fresh, &mut self.pcm);
            if self.pcm.len() >= STREAM_CHUNK_BYTES {
                session.send_audio(std::mem::take(&mut self.pcm));
            }
        }
    }

    fn end_recording(&mut self) {
        self.pump_audio(true);
        self.stopped_at = Some(Instant::now());
        if let Some(session) = &self.session {
            if session.is_connected() {
                if !self.pcm.is_empty() {
                    session.send_audio(std::mem::take(&mut self.pcm));
                }
                session.finish();
            } else {
                // Not connected: waiting would add several round trips, so go local now. If the
                // attempt already failed (offline, bad key) or is hopelessly slow, trip the breaker
                // so the next dictations don't even try.
                let spoke_for = self.recording_since.map_or(Duration::ZERO, |t| t.elapsed());
                match session.try_result() {
                    Some(Err(e)) => self.on_deepgram_failure(&e),
                    _ if spoke_for
                        > Duration::from_millis(self.cfg.deepgram.connect_timeout_ms) =>
                    {
                        self.on_deepgram_failure(&SessionError {
                            kind: FailureKind::Network,
                            message: format!(
                                "still not connected after {} ms",
                                spoke_for.as_millis()
                            ),
                        });
                    }
                    _ => info!("Deepgram still connecting at release; using local Whisper"),
                }
                self.session = None;
            }
        }
        self.park_mic();
        // The text goes wherever focus is now, so that is the app we format for.
        self.target = Some(frontmost_app());

        if vad::is_digital_silence(&self.utterance) {
            self.session = None;
            self.dispatch(Event::TranscriptReady(String::new()));
            warn!(
                "the microphone delivered pure silence. {}",
                macos::PERMISSIONS_HELP
            );
            self.hud.alert("Microphone is silent: check permissions");
            return;
        }
        if self.session.is_none() {
            self.transcribe_locally();
        }
    }

    fn poll_transcription(&mut self) {
        let Some(session) = &self.session else { return };
        let deadline = Duration::from_millis(
            self.cfg.deepgram.connect_timeout_ms + self.cfg.deepgram.finalize_timeout_ms,
        );
        let result = match session.try_result() {
            Some(r) => r,
            None if self.stopped_at.is_some_and(|t| t.elapsed() > deadline) => Err(SessionError {
                kind: FailureKind::Network,
                message: format!("no response within {deadline:?}"),
            }),
            None => return,
        };
        self.session = None;
        match result {
            Ok(text) => {
                if self.breaker.record_success() == Transition::BackOnline {
                    info!("Deepgram is reachable again; back online");
                }
                self.finish_transcript(text, Source::Deepgram);
            }
            Err(e) => {
                self.on_deepgram_failure(&e);
                self.transcribe_locally();
            }
        }
    }

    fn on_deepgram_failure(&mut self, e: &SessionError) {
        warn!("Deepgram failed: {e}");
        if let Transition::WentOffline(cooldown) =
            self.breaker.record_failure(e.kind, Instant::now())
        {
            info!(
                "offline mode for {} s; dictations use local Whisper",
                cooldown.as_secs()
            );
            if self.local.is_some() {
                self.notice_after_inject = Some(if e.kind == FailureKind::Auth {
                    "Deepgram key rejected: using offline model"
                } else {
                    "Offline: using local model"
                });
            }
        }
    }

    fn transcribe_locally(&mut self) {
        let Some(whisper) = self.local.as_mut() else {
            self.dispatch(Event::TranscriptionFailed);
            return;
        };
        match whisper.transcribe(&self.utterance, &self.whisper_prompt) {
            Ok(text) => self.finish_transcript(text, Source::Local),
            Err(e) => {
                warn!("local transcription failed: {e:#}");
                self.dispatch(Event::TranscriptionFailed);
            }
        }
    }

    fn finish_transcript(&mut self, raw: String, source: Source) {
        let stt = self.stopped_at.map_or(Duration::ZERO, |t| t.elapsed());
        let class = self
            .target
            .as_ref()
            .map_or(crate::context::AppClass::Prose, |t| t.class);
        let t = Instant::now();
        let text = self.post.process(&raw, class);
        self.transcript_info = Some((source, stt, t.elapsed()));
        self.utterance.clear();
        self.dispatch(Event::TranscriptReady(text));
    }

    fn inject(&mut self, text: &str) {
        let app = self
            .target
            .as_ref()
            .map_or("unknown", |t| t.name.as_str())
            .to_string();
        let result = if self.dry_run {
            info!("dry run, not injecting: {}", Redacted(text));
            Ok((Outcome::Typed, Duration::ZERO))
        } else {
            self.injector.inject(text)
        };
        let info = self.transcript_info.take();
        if let Some(history) = &self.history {
            history.record(Entry {
                when: macos::local_timestamp(),
                app: app.clone(),
                offline: info.is_some_and(|(source, _, _)| source == Source::Local),
                text: text.trim().to_string(),
            });
        }
        match result {
            Ok((outcome, delivered)) => {
                if let Some((source, stt, format)) = info {
                    info!(
                        "{} -> {app} via {source} | release->text {} ms (transcribe {} ms, format {} us, inject {} ms) | {}",
                        match outcome {
                            Outcome::Pasted => "pasted",
                            Outcome::Typed => "typed",
                            Outcome::LeftOnClipboard => "copied",
                        },
                        (stt + format + delivered).as_millis(),
                        stt.as_millis(),
                        format.as_micros(),
                        delivered.as_millis(),
                        Redacted(text)
                    );
                }
                self.dispatch(Event::Injected);
                if outcome == Outcome::LeftOnClipboard {
                    self.hud
                        .notice("Secure input is on: text copied to clipboard");
                } else if let Some(n) = self.notice_after_inject.take() {
                    self.hud.notice(n);
                }
            }
            Err(e) => {
                warn!("injection failed: {e:#}");
                crate::inject::leave_on_clipboard(text);
                self.dispatch(Event::InjectionFailed);
            }
        }
        self.notice_after_inject = None;
    }

    /// Stops capture now, or keeps it running for `audio.keep_warm_seconds`.
    fn park_mic(&mut self) {
        self.device_check_due = true;
        if self.cfg.audio.keep_warm_seconds == 0 {
            self.stop_mic();
        } else {
            self.mic_idle_since = Some(Instant::now());
        }
    }

    fn stop_mic(&mut self) {
        if let Some(mic) = self.mic.as_mut() {
            mic.stop();
        }
        self.mic_idle_since = None;
    }
}
