//! `wispr` command-line entry point. Without flags it runs the dictation daemon.

use anyhow::{Context, Result, anyhow, bail};
use std::io::{BufRead, IsTerminal};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{fs, thread};
use wispr::asr::deepgram::{self, StreamingSession};
use wispr::asr::whisper::LocalWhisper;
use wispr::audio::{self, Microphone, f32_to_pcm16, resample::Resampler, wav};
use wispr::config::{Config, EnginePreference, user_config_path};
use wispr::context::AppClass;
use wispr::daemon::{self, Daemon, Engines, Signal};
use wispr::hud::Hud;
use wispr::pipeline::Event;
use wispr::text::PostProcessor;
use wispr::{hotkey, info, inject, macos};

const HELP: &str = "\
wispr: press a hotkey, speak, and polished text is typed into the focused app.

USAGE:
    wispr [--config <path>] [COMMAND]

COMMANDS (default: run the dictation daemon):
    --check-hardware          Check system, microphone, permissions, and engines
    --list-devices            List audio input devices
    --download-models [NAME]  Download a local Whisper model (small.en, base.en, large-v3-turbo)
    --benchmark               Measure end-to-end latency with synthetic speech
    --transcribe <file.wav>   Transcribe a WAV file and print the raw and formatted text
    -h, --help                Show this help
    -V, --version             Show the version

OPTIONS:
    --config <path>           Use this config file (default: ./config.toml, then
                              ~/Library/Application Support/wispr/config.toml)
    --no-hud                  Don't show the on-screen indicator
    --dry-run                 Log transcripts instead of typing them into apps

ENVIRONMENT:
    DEEPGRAM_API_KEY          Enables Deepgram streaming (also read from .env)
    WISPR_LOG_TEXT=1          Include dictated text in logs (release builds redact it)
";

enum Mode {
    Run,
    CheckHardware,
    ListDevices,
    Download(Vec<String>),
    Benchmark,
    Transcribe(PathBuf),
    Help,
    Version,
}

struct Args {
    config: Option<PathBuf>,
    hud: bool,
    dry_run: bool,
    command: Mode,
}

fn parse_args() -> Result<Args> {
    let mut args = std::env::args().skip(1).peekable();
    let mut out = Args {
        config: None,
        hud: true,
        dry_run: false,
        command: Mode::Run,
    };
    let set = |cmd: Mode, current: &mut Mode| -> Result<()> {
        if !matches!(current, Mode::Run) {
            bail!("only one command may be given");
        }
        *current = cmd;
        Ok(())
    };
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--config" | "-c" => {
                out.config = Some(PathBuf::from(
                    args.next()
                        .ok_or_else(|| anyhow!("--config needs a path"))?,
                ));
            }
            "--no-hud" => out.hud = false,
            "--dry-run" => out.dry_run = true,
            "--check-hardware" | "--doctor" => set(Mode::CheckHardware, &mut out.command)?,
            "--list-devices" => set(Mode::ListDevices, &mut out.command)?,
            "--benchmark" => set(Mode::Benchmark, &mut out.command)?,
            "--download-models" => {
                let mut names = Vec::new();
                while let Some(n) = args.next_if(|a| !a.starts_with('-')) {
                    names.push(n);
                }
                set(Mode::Download(names), &mut out.command)?;
            }
            "--transcribe" => {
                let p = args
                    .next()
                    .ok_or_else(|| anyhow!("--transcribe needs a WAV file"))?;
                set(Mode::Transcribe(PathBuf::from(p)), &mut out.command)?;
            }
            "-h" | "--help" => set(Mode::Help, &mut out.command)?,
            "-V" | "--version" => set(Mode::Version, &mut out.command)?,
            other => bail!("unknown argument {other:?} (see --help)"),
        }
    }
    Ok(out)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn real_main() -> Result<()> {
    let args = parse_args()?;
    match args.command {
        Mode::Help => {
            print!("{HELP}");
            Ok(())
        }
        Mode::Version => {
            println!("wispr {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Mode::ListDevices => list_devices(),
        command => {
            let cfg = Config::load(args.config.as_deref())?;
            match command {
                Mode::Run => run_daemon(cfg, args.hud, args.dry_run),
                Mode::CheckHardware => check_hardware(&cfg),
                Mode::Download(names) => download_models(&cfg, &names),
                Mode::Benchmark => benchmark(&cfg),
                Mode::Transcribe(path) => transcribe_file(&cfg, &path),
                Mode::Help | Mode::Version | Mode::ListDevices => unreachable!(),
            }
        }
    }
}

fn run_daemon(cfg: Config, show_hud: bool, dry_run: bool) -> Result<()> {
    let trigger = hotkey::parse_key(&cfg.hotkey.key)?;
    let cancel = hotkey::parse_key(&cfg.hotkey.cancel_key)?;
    let _instance = acquire_instance_lock()?;

    info!("wispr {} starting", env!("CARGO_PKG_VERSION"));
    match &cfg.source {
        Some(p) => info!("config: {}", p.display()),
        None => info!("config: built-in defaults (no config file found)"),
    }

    let t = Instant::now();
    let engines = Engines::load(&cfg)?;
    match (&engines.deepgram, &engines.local) {
        (Some(dg), Some(_)) => info!(
            "engine: Deepgram {} (local Whisper fallback ready)",
            dg.model
        ),
        (Some(dg), None) => info!("engine: Deepgram {} (no offline fallback)", dg.model),
        (None, Some(_)) => info!(
            "engine: local Whisper ({})",
            cfg.local_model_path().display()
        ),
        (None, None) => unreachable!("Engines::load guarantees at least one engine"),
    }
    info!("engines ready in {} ms", t.elapsed().as_millis());

    let hud = if show_hud {
        Hud::spawn()
    } else {
        Hud::disabled()
    };
    if show_hud && !hud.is_active() {
        wispr::warn!(
            "on-screen indicator unavailable (wispr-overlay not found next to the binary)"
        );
    }

    let (tx, rx) = mpsc::channel();
    hotkey::spawn(trigger, cancel, tx.clone());
    if std::io::stdin().is_terminal() {
        spawn_terminal_controls(tx);
    }

    let verb = match cfg.hotkey.mode {
        wispr::config::HotkeyMode::Toggle => "tap to start, tap again to insert",
        wispr::config::HotkeyMode::Hold => "hold to talk, release to insert",
    };
    let ready = format!(
        "ready: {} ({verb}); {} cancels",
        cfg.hotkey.key, cfg.hotkey.cancel_key
    );
    let mut daemon = Daemon::new(cfg, engines, hud)?;
    daemon.set_dry_run(dry_run);
    if let Err(e) = daemon.prepare_mic() {
        wispr::warn!("microphone not ready yet ({e:#}); will retry on the first dictation");
    }
    // Without permissions the hotkey is dead, so don't claim to be ready: the daemon logs what
    // is missing instead, and restarts itself (logging "ready") once both are granted.
    if macos::permissions_granted() {
        info!("{ready}");
    }
    if std::io::stdin().is_terminal() {
        info!("terminal: Enter toggles recording, q + Enter quits");
    }
    match daemon.run(rx)? {
        daemon::Exit::Quit => {
            // Native libraries (Metal, CoreAudio) tear down in exit handlers; never let a stuck
            // teardown keep a "quit" process alive.
            thread::spawn(|| {
                thread::sleep(Duration::from_secs(3));
                macos::force_exit(0);
            });
            info!("quit");
            Ok(())
        }
        daemon::Exit::Restart => {
            drop(_instance);
            restart_self()
        }
    }
}

/// Replaces this process with a fresh copy of itself (same pid, so launchd keeps tracking it).
fn restart_self() -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe()?;
    let err = Command::new(exe).args(std::env::args_os().skip(1)).exec();
    Err(anyhow!("restart failed: {err}"))
}

/// Two daemons would both paste every dictation, so only one may run per user. The OS releases
/// the lock when the process exits, even after a crash.
fn acquire_instance_lock() -> Result<fs::File> {
    let dir = user_config_path()
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("wispr.lock"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(fs::TryLockError::WouldBlock) => bail!(
            "wispr is already running (stop the background service with `wisprctl stop` or Raycast's \"Stop Wispr\")"
        ),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

fn spawn_terminal_controls(tx: mpsc::Sender<Signal>) {
    thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let signal = if line.trim().eq_ignore_ascii_case("q") {
                Signal::Quit
            } else {
                Signal::Input(Event::Toggle)
            };
            if tx.send(signal).is_err() {
                break;
            }
        }
    });
}

fn list_devices() -> Result<()> {
    for d in audio::list_input_devices()? {
        println!(
            "{} {}  [{}]",
            if d.is_default { "*" } else { " " },
            d.name,
            d.format
        );
    }
    println!(
        "\n* = system default. Set [audio] device = \"<name>\" in config.toml to choose another."
    );
    Ok(())
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn check_hardware(cfg: &Config) -> Result<()> {
    let ok = |b: bool| if b { "ok" } else { "MISSING" };
    let mem_gb = sh("sysctl", &["-n", "hw.memsize"])
        .parse::<u64>()
        .unwrap_or(0)
        / (1 << 30);
    println!("System");
    println!(
        "  macOS        {} ({})",
        sh("sw_vers", &["-productVersion"]),
        sh("sw_vers", &["-buildVersion"])
    );
    println!(
        "  chip         {} ({} cores, {} GB)",
        sh("sysctl", &["-n", "machdep.cpu.brand_string"]),
        sh("sysctl", &["-n", "hw.ncpu"]),
        mem_gb
    );

    println!("\nConfig");
    match &cfg.source {
        Some(p) => println!("  file         {}", p.display()),
        None => println!(
            "  file         none (defaults); create ./config.toml or {}",
            user_config_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        ),
    }
    println!(
        "  hotkey       {} ({:?}), cancel {}",
        cfg.hotkey.key, cfg.hotkey.mode, cfg.hotkey.cancel_key
    );
    hotkey::parse_key(&cfg.hotkey.key)?;
    hotkey::parse_key(&cfg.hotkey.cancel_key)?;

    println!("\nAudio");
    let devices = audio::list_input_devices()?;
    match devices.iter().find(|d| d.is_default) {
        Some(d) => println!("  default mic  {} [{}]", d.name, d.format),
        None => println!("  default mic  MISSING"),
    }
    println!("  inputs       {}", devices.len());

    println!("\nPermissions");
    println!("  accessibility  {}", ok(macos::accessibility_trusted()));
    println!("  input monitor  {}", ok(macos::input_monitoring_granted()));
    println!(
        "  secure input   {}",
        if macos::secure_input_enabled() {
            "ON (typing is blocked right now)"
        } else {
            "off"
        }
    );
    println!("  microphone     checked on first recording (macOS prompts once)");

    println!("\nEngines");
    match cfg.deepgram_api_key() {
        Some(key) => {
            let masked = format!("...{}", &key[key.len().saturating_sub(4)..]);
            match deepgram::check_connectivity(&daemon::deepgram_options(
                cfg,
                key,
                &daemon::deepgram_host(cfg),
            )) {
                Ok(t) => println!(
                    "  deepgram     ok ({}, key {masked}, connect {} ms)",
                    cfg.deepgram.model,
                    t.as_millis()
                ),
                Err(e) => println!("  deepgram     FAILED: {e:#}"),
            }
        }
        None => println!("  deepgram     not configured (set DEEPGRAM_API_KEY)"),
    }
    let model = cfg.local_model_path();
    match fs::metadata(&model) {
        Ok(m) => println!(
            "  whisper      ok ({}, {} MB)",
            model.display(),
            m.len() / 1_000_000
        ),
        Err(_) => println!(
            "  whisper      MISSING {} (run --download-models)",
            model.display()
        ),
    }
    let overlay = std::env::current_exe()?.with_file_name("wispr-overlay");
    println!("  overlay      {}", ok(overlay.is_file()));

    println!("\nHistory");
    match cfg.history_path() {
        Some(p) => println!(
            "  note         {} (last {} dictations{})",
            p.display(),
            cfg.history.max_entries,
            if p.is_file() {
                ""
            } else {
                ", created on first dictation"
            }
        ),
        None => println!("  note         off (set [history] path to log dictations to a note)"),
    }
    Ok(())
}

const MODELS: &[(&str, &str, &str)] = &[
    (
        "small.en",
        "ggml-small.en-q5_1.bin",
        "190 MB, fast offline fallback (default)",
    ),
    (
        "base.en",
        "ggml-base.en-q5_1.bin",
        "60 MB, smallest and fastest, less accurate",
    ),
    (
        "large-v3-turbo",
        "ggml-large-v3-turbo-q5_0.bin",
        "574 MB, most accurate, slower",
    ),
];

fn download_models(cfg: &Config, names: &[String]) -> Result<()> {
    let names: Vec<&str> = if names.is_empty() {
        vec!["small.en"]
    } else {
        names.iter().map(String::as_str).collect()
    };
    let dir = cfg
        .local_model_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("models"));
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;

    for name in names {
        let Some((_, file, about)) = MODELS.iter().find(|(n, _, _)| *n == name) else {
            let known: Vec<_> = MODELS
                .iter()
                .map(|(n, _, d)| format!("{n} ({d})"))
                .collect();
            bail!("unknown model {name:?}; available: {}", known.join(", "));
        };
        let dest = dir.join(file);
        if dest.is_file() {
            println!("{name}: already present at {}", dest.display());
            continue;
        }
        println!("{name}: downloading {about} to {}", dest.display());
        let partial = dest.with_extension("bin.part");
        let url = format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{file}");
        let status = Command::new("curl")
            .args(["-L", "--fail", "--progress-bar", "-o"])
            .arg(&partial)
            .arg(&url)
            .status()
            .context("running curl")?;
        if !status.success() {
            let _ = fs::remove_file(&partial);
            bail!("download of {name} failed");
        }
        fs::rename(&partial, &dest)?;
        println!("{name}: done");
    }
    Ok(())
}

/// Reads a WAV file and converts it to 16 kHz mono.
fn load_wav_16k(path: &Path) -> Result<Vec<f32>> {
    let w = wav::parse(&fs::read(path).with_context(|| format!("reading {}", path.display()))?)?;
    let mut out = Vec::new();
    Resampler::new(w.sample_rate, w.channels).process(&w.samples, &mut out);
    Ok(out)
}

fn stream_to_deepgram(
    opts: &deepgram::DeepgramOptions,
    audio: &[f32],
    pace: bool,
) -> Result<(String, Duration)> {
    let session = StreamingSession::start(opts.clone());
    for chunk in audio.chunks(1_600) {
        let mut pcm = Vec::new();
        f32_to_pcm16(chunk, &mut pcm);
        session.send_audio(pcm);
        if pace {
            thread::sleep(Duration::from_millis(100));
        }
    }
    let t = Instant::now();
    session.finish();
    let text =
        session.wait(opts.connect_timeout + opts.finalize_timeout + Duration::from_secs(1))?;
    Ok((text, t.elapsed()))
}

fn transcribe_file(cfg: &Config, path: &Path) -> Result<()> {
    let audio = load_wav_16k(path)?;
    let key = cfg
        .deepgram_api_key()
        .filter(|_| cfg.transcription.engine != EnginePreference::Local);
    let raw = match key {
        Some(key) => {
            let mut opts = daemon::deepgram_options(cfg, key, &daemon::deepgram_host(cfg));
            // A whole file arrives at once, so Deepgram needs time to catch up on the backlog.
            opts.finalize_timeout += Duration::from_secs_f64(audio.len() as f64 / 16_000.0);
            stream_to_deepgram(&opts, &audio, false)?.0
        }
        None => LocalWhisper::load(
            &cfg.local_model_path(),
            cfg.local.threads,
            &cfg.transcription.language,
        )?
        .transcribe(&audio, &daemon::whisper_prompt(cfg))?,
    };
    let formatted = PostProcessor::new(&cfg.vocabulary, &cfg.output).process(&raw, AppClass::Prose);
    println!("raw:       {raw}");
    println!("formatted: {}", formatted.trim_end());
    Ok(())
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

fn ms(d: Duration) -> String {
    format!("{:.0} ms", d.as_secs_f64() * 1000.0)
}

fn benchmark(cfg: &Config) -> Result<()> {
    const PHRASE: &str = "Um, so let's meet at four, no actually five, and then review the pull request for the cache layer.";
    const RUNS: usize = 3;

    // Synthetic speech only; it is deleted right after loading.
    let tmp = std::env::temp_dir().join(format!("wispr-bench-{}.wav", std::process::id()));
    let status = Command::new("say")
        .args(["-o"])
        .arg(&tmp)
        .args(["--data-format=LEI16@16000", PHRASE])
        .status()
        .context("running `say` to synthesize test speech")?;
    if !status.success() {
        bail!("`say` failed");
    }
    let audio = load_wav_16k(&tmp);
    let _ = fs::remove_file(&tmp);
    let audio = audio?;
    let secs = audio.len() as f64 / 16_000.0;

    println!(
        "wispr latency benchmark: {:.1} s of synthetic speech, {RUNS} runs each",
        secs
    );
    println!("{}", sh("sysctl", &["-n", "machdep.cpu.brand_string"]));
    println!();

    // The daemon prepares the stream at startup, so a hotkey press only has to start it.
    match Microphone::open(cfg.audio_device()) {
        Ok(mut mic) => {
            let t = Instant::now();
            let mut first = Vec::new();
            let started = mic.start().map(|()| {
                while first.is_empty() && t.elapsed() < Duration::from_secs(2) {
                    thread::sleep(Duration::from_micros(500));
                    mic.drain(&mut first);
                }
                t.elapsed()
            });
            mic.stop();
            match started {
                Ok(d) => println!(
                    "  hotkey -> first mic samples         {}  ({})",
                    ms(d),
                    mic.name
                ),
                Err(e) => println!("  microphone start                    failed: {e:#}"),
            }
        }
        Err(e) => println!("  microphone                          failed: {e:#}"),
    }

    let post = PostProcessor::new(&cfg.vocabulary, &cfg.output);
    let mut deepgram_final = None;
    if let Some(key) = cfg.deepgram_api_key() {
        let opts = daemon::deepgram_options(cfg, key, &daemon::deepgram_host(cfg));
        match deepgram::check_connectivity(&opts) {
            Ok(c) => println!("  deepgram connect (hidden by speech) {}", ms(c)),
            Err(e) => println!("  deepgram connect                    failed: {e:#}"),
        }
        let mut times = Vec::new();
        let mut transcript = String::new();
        for _ in 0..RUNS {
            match stream_to_deepgram(&opts, &audio, true) {
                Ok((text, d)) => {
                    times.push(d);
                    transcript = text;
                }
                Err(e) => println!("  deepgram run failed: {e:#}"),
            }
        }
        if !times.is_empty() {
            let m = median(times.clone());
            deepgram_final = Some(m);
            println!(
                "  deepgram release -> final text      {}  (runs: {})",
                ms(m),
                times.iter().map(|d| ms(*d)).collect::<Vec<_>>().join(", ")
            );
            println!("      raw:       {transcript}");
            println!(
                "      formatted: {}",
                post.process(&transcript, AppClass::Prose).trim_end()
            );
        }
    } else {
        println!("  deepgram                            skipped (no DEEPGRAM_API_KEY)");
    }

    let model = cfg.local_model_path();
    let mut local_time = None;
    if model.is_file() {
        let t = Instant::now();
        let mut whisper =
            LocalWhisper::load(&model, cfg.local.threads, &cfg.transcription.language)?;
        println!("  whisper load + warm-up (startup)    {}", ms(t.elapsed()));
        let prompt = daemon::whisper_prompt(cfg);
        let mut times = Vec::new();
        let mut transcript = String::new();
        for _ in 0..RUNS {
            let t = Instant::now();
            transcript = whisper.transcribe(&audio, &prompt)?;
            times.push(t.elapsed());
        }
        let m = median(times);
        local_time = Some(m);
        println!(
            "  whisper release -> text ({})  {}",
            model
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default(),
            ms(m)
        );
        println!("      raw:       {transcript}");
    } else {
        println!("  whisper                             skipped (model missing)");
    }

    let sample = "So let's meet at four. No. Actually, five. And then review the pull request.";
    let t = Instant::now();
    for _ in 0..1_000 {
        std::hint::black_box(post.process(std::hint::black_box(sample), AppClass::Prose));
    }
    let format_time = t.elapsed() / 1_000;
    println!(
        "  post-processing                     {:.3} ms",
        format_time.as_secs_f64() * 1000.0
    );

    let (paste, restore) = inject::benchmark_pasteboard();
    println!(
        "  pasteboard save + write (pre-paste) {:.2} ms; restore afterwards {:.2} ms",
        paste.as_secs_f64() * 1000.0,
        restore.as_secs_f64() * 1000.0
    );

    println!();
    for (label, asr) in [("Deepgram", deepgram_final), ("local Whisper", local_time)] {
        if let Some(asr) = asr {
            println!(
                "  estimated release -> text on screen via {label}: {}",
                ms(asr + format_time + paste)
            );
        }
    }
    Ok(())
}
