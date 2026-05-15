mod app;
mod audio;
mod settings;
mod transcriber;

use anyhow::{Context, Result};
use app::{
    add_current_exe_dir_to_path, install_model, is_model_installed, model_by_name, model_state,
    verify_sha1, AppPaths, MODELS,
};
use audio::input_devices;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::{CrosstermBackend, Frame, Terminal},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Wrap},
};
use settings::{TranscriptFormat, UserConfig};
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    time::{Duration, Instant},
};
use transcriber::{
    start_session, transcribe_wav_file, SessionCommand, SessionEvent, SessionHandle, SessionStatus,
    TranscriptLine,
};

#[derive(Debug, Parser)]
#[command(name = "whispercli", version)]
#[command(about = "Local real-time transcription with whisper.cpp")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Open the live transcription TUI.
    Live(LiveArgs),
    /// Transcribe an audio file.
    File(FileArgs),
    /// Open or edit settings.
    #[command(args_conflicts_with_subcommands = true)]
    Config(ConfigArgs),
    /// List available input devices.
    Devices,
    /// Create user directories and print install diagnostics.
    Init(InitArgs),
    /// Inspect paths and platform setup.
    Doctor(DoctorArgs),
    /// Manage whisper.cpp ggml models.
    Models {
        #[command(subcommand)]
        command: ModelCommand,
    },
}

#[derive(Debug, Parser, Clone, Default)]
struct LiveArgs {
    /// Output transcript file name or path.
    #[arg(long)]
    out: Option<String>,

    /// Print transcript events to stdout instead of opening the TUI.
    #[arg(long)]
    plain: bool,

    /// Print line-delimited JSON events to stdout. Implies --plain.
    #[arg(long)]
    jsonl: bool,

    /// Whisper model name.
    #[arg(long)]
    model: Option<String>,

    /// Recognition language, for example ja, en, or auto.
    #[arg(long)]
    lang: Option<String>,

    /// Input device name or index from `whispercli devices`.
    #[arg(long)]
    device: Option<String>,

    /// Output transcript format: md, txt, srt, json, or jsonl.
    #[arg(long, value_parser = parse_transcript_format)]
    format: Option<TranscriptFormat>,
}

#[derive(Debug, Parser, Clone)]
struct FileArgs {
    /// Audio file to transcribe. WAV PCM/f32 input is supported.
    audio: PathBuf,

    /// Output transcript file name or path.
    #[arg(long)]
    out: Option<String>,

    /// Whisper model name.
    #[arg(long)]
    model: Option<String>,

    /// Recognition language, for example ja, en, or auto.
    #[arg(long)]
    lang: Option<String>,

    /// Output transcript format: md, txt, srt, json, or jsonl.
    #[arg(long, value_parser = parse_transcript_format)]
    format: Option<TranscriptFormat>,
}

#[derive(Debug, Parser)]
struct ConfigArgs {
    #[command(subcommand)]
    command: Option<ConfigCommand>,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    /// Print the current config, or a single key.
    Get {
        /// Config key to print.
        key: Option<String>,
    },
    /// Set a config value.
    Set {
        /// Config key.
        key: String,
        /// New value.
        value: String,
    },
}

#[derive(Debug, Parser)]
struct InitArgs {
    /// Add the current executable directory to the user PATH on Windows.
    #[arg(long)]
    add_to_path: bool,

    /// Download a model during init. Defaults to tiny when no value is given.
    #[arg(long, value_name = "MODEL", num_args = 0..=1, default_missing_value = "tiny")]
    download: Option<String>,
}

#[derive(Debug, Parser)]
struct DoctorArgs {
    /// Print diagnostics as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Show installable and installed models.
    List,
    /// Download a model into ~/.whispercli/models.
    Install {
        /// Model name: tiny, base, small, or recommended.
        #[arg(default_value = "tiny")]
        model: String,
    },
    /// Verify installed model checksums.
    Verify {
        /// Optional model name. Omit to verify all models.
        model: Option<String>,
    },
    /// Remove an installed model.
    Remove {
        /// Model name: tiny, base, or small.
        model: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveAction {
    DownloadModel,
    Settings,
    Pause,
    Save,
    Quit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ConfigAction {
    Section(usize),
    Model(usize),
    DownloadModel(usize),
    Device(usize),
    OutputDir(usize),
    Format(usize),
    Language(usize),
    Back,
    Save,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TargetAction {
    Live(LiveAction),
    Config(ConfigAction),
}

#[derive(Clone, Debug)]
struct MouseTarget {
    id: usize,
    area: Rect,
    enabled: bool,
    action: TargetAction,
}

struct TerminalGuard {
    raw: bool,
    alternate: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        let mut guard = Self {
            raw: false,
            alternate: false,
        };
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        guard.raw = true;
        execute!(io::stdout(), EnterAlternateScreen)
            .context("Failed to enter alternate terminal screen")?;
        guard.alternate = true;
        execute!(io::stdout(), EnableMouseCapture).context("Failed to enable mouse capture")?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.raw {
            let _ = disable_raw_mode();
        }
        if self.alternate {
            let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
        }
    }
}

fn main() {
    if let Err(error) = try_main() {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

fn try_main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Commands::Live(args)) => run_live(args),
        Some(Commands::File(args)) => run_file(args),
        Some(Commands::Config(args)) => config_command(args),
        Some(Commands::Devices) => devices(),
        Some(Commands::Init(args)) => init(args),
        Some(Commands::Doctor(args)) => doctor(args),
        Some(Commands::Models { command }) => models(command),
        None => run_live(LiveArgs::default()),
    }
}

fn init(args: InitArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;
    let config = UserConfig::load_or_create(&paths)?;

    println!("created {}", paths.root.display());
    println!("models  {}", paths.models.display());
    println!("bin     {}", paths.bin.display());
    println!("config  {}", paths.config.display());
    println!("output  {}", config.output_dir.display());

    if args.add_to_path {
        add_current_exe_dir_to_path()?;
    }

    if let Some(model) = args.download {
        install_model(&paths, &model)?;
    }

    Ok(())
}

fn doctor(args: DoctorArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;
    let config = UserConfig::load_or_create(&paths)?;
    let devices = input_devices();

    if args.json {
        let model_values = MODELS
            .iter()
            .map(|model| {
                let state = model_state(&paths, *model);
                let path = paths.model_path(*model).display().to_string();
                serde_json::json!({
                    "name": model.name,
                    "file": model.file_name,
                    "size": model.size,
                    "state": state.label(),
                    "path": path,
                })
            })
            .collect::<Vec<_>>();
        let microphone_values = devices
            .as_ref()
            .map(|devices| {
                devices
                    .iter()
                    .map(|device| {
                        serde_json::json!({
                            "index": device.index,
                            "name": device.name,
                            "is_default": device.is_default,
                            "config": device.config,
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let report = serde_json::json!({
            "version": env!("CARGO_PKG_VERSION"),
            "paths": {
                "root": paths.root.display().to_string(),
                "bin": paths.bin.display().to_string(),
                "models": paths.models.display().to_string(),
                "transcripts": paths.transcripts.display().to_string(),
                "logs": paths.logs.display().to_string(),
                "config": paths.config.display().to_string(),
            },
            "platform": env::consts::OS,
            "arch": env::consts::ARCH,
            "exe": env::current_exe()?.display().to_string(),
            "settings": {
                "model": config.model,
                "language": config.language,
                "microphone": config.microphone,
                "output": config.output_dir.display().to_string(),
                "format": config.output_format.extension(),
                "chunk_seconds": config.chunk_seconds,
                "threads": config.threads,
                "start_paused": config.start_paused,
            },
            "models": model_values,
            "microphones": microphone_values,
            "microphone_error": devices.as_ref().err().map(|error| format!("{error:#}")),
        });
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("whisperCLI paths");
    println!("root        {}", paths.root.display());
    println!("bin         {}", paths.bin.display());
    println!("models      {}", paths.models.display());
    println!("transcripts {}", paths.transcripts.display());
    println!("logs        {}", paths.logs.display());
    println!("config      {}", paths.config.display());
    println!();
    println!("platform    {}", env::consts::OS);
    println!("arch        {}", env::consts::ARCH);
    println!("exe         {}", env::current_exe()?.display());
    println!();
    println!("settings");
    println!("model       {}", config.model);
    println!("language    {}", config.language);
    println!(
        "microphone  {}",
        config.microphone.as_deref().unwrap_or("default")
    );
    println!("output      {}", config.output_dir.display());
    println!("format      {}", config.output_format.label());
    println!();
    println!("models");
    for model in MODELS {
        let state = model_state(&paths, *model);
        println!("{:<6} {:<10} {}", model.name, model.size, state.label());
    }
    println!();
    println!("microphones");
    match devices {
        Ok(devices) => {
            for device in devices {
                let marker = if device.is_default { "*" } else { " " };
                println!(
                    "{} {:<2} {:<40} {}",
                    marker, device.index, device.name, device.config
                );
            }
        }
        Err(error) => {
            println!("Could not enumerate input devices:");
            println!("  {error:#}");
            println!("On macOS, check System Settings > Privacy & Security > Microphone.");
            println!("On Windows, check Settings > Privacy > Microphone.");
        }
    }

    Ok(())
}

fn devices() -> Result<()> {
    for device in input_devices()? {
        let marker = if device.is_default { "*" } else { " " };
        println!(
            "{} {:<2} {:<40} {}",
            marker, device.index, device.name, device.config
        );
    }
    Ok(())
}

fn models(command: ModelCommand) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    match command {
        ModelCommand::List => {
            for model in MODELS {
                let path = paths.model_path(*model);
                let state = model_state(&paths, *model);
                println!(
                    "{:<6} {:<10} {:<10} {}",
                    model.name,
                    model.size,
                    state.label(),
                    path.display()
                );
            }
            Ok(())
        }
        ModelCommand::Install { model } => install_model(&paths, &model),
        ModelCommand::Verify { model } => verify_models(&paths, model.as_deref()),
        ModelCommand::Remove { model } => remove_model(&paths, &model),
    }
}

fn verify_models(paths: &AppPaths, requested: Option<&str>) -> Result<()> {
    let models = if let Some(requested) = requested {
        vec![model_by_name(requested).ok_or_else(|| {
            anyhow::anyhow!(
                "Unknown model '{requested}'. Supported models: tiny, base, small, recommended"
            )
        })?]
    } else {
        MODELS.to_vec()
    };

    let mut failed = false;
    for model in models {
        let path = paths.model_path(model);
        if !path.exists() {
            println!("{:<6} missing  {}", model.name, path.display());
            failed = true;
            continue;
        }

        match verify_sha1(&path, model.sha1) {
            Ok(()) => println!("{:<6} ok       {}", model.name, path.display()),
            Err(error) => {
                println!("{:<6} corrupt  {}", model.name, path.display());
                println!("       {error:#}");
                failed = true;
            }
        }
    }

    if failed {
        anyhow::bail!("one or more models failed verification");
    }
    Ok(())
}

fn remove_model(paths: &AppPaths, requested: &str) -> Result<()> {
    let model = model_by_name(requested).ok_or_else(|| {
        anyhow::anyhow!(
            "Unknown model '{requested}'. Supported models: tiny, base, small, recommended"
        )
    })?;
    let path = paths.model_path(model);
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("Failed to remove {}", path.display()))?;
        println!("removed {}", path.display());
    } else {
        println!("model '{}' is not installed", model.name);
    }
    Ok(())
}

fn effective_config(paths: &AppPaths, args: &LiveArgs) -> Result<UserConfig> {
    let mut config = UserConfig::load_or_create(paths)?;
    if let Some(model) = &args.model {
        config.model = model_by_name(model)
            .ok_or_else(|| anyhow::anyhow!("Unknown model '{model}'"))?
            .name
            .to_string();
    }
    if let Some(lang) = &args.lang {
        config.language = lang.clone();
    }
    if let Some(device) = &args.device {
        config.microphone = Some(resolve_device_name(device)?);
    }
    if let Some(format) = args.format {
        config.output_format = format;
    } else if let Some(out) = &args.out {
        if let Some(format) = TranscriptFormat::from_path_extension(Path::new(out)) {
            config.output_format = format;
        }
    }
    Ok(config)
}

fn run_live(args: LiveArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    if args.plain || args.jsonl {
        let config = effective_config(&paths, &args)?;
        return run_live_plain(
            paths,
            config,
            args.out.clone(),
            args.format.is_some(),
            args.jsonl,
        );
    }

    loop {
        let config = effective_config(&paths, &args)?;
        let outcome = run_live_tui(
            paths.clone(),
            config,
            args.out.clone(),
            args.format.is_some(),
        )?;
        match outcome {
            LiveOutcome::Quit => return Ok(()),
            LiveOutcome::OpenConfig => run_config_loop()?,
            LiveOutcome::InstallModel(model) => install_model(&paths, &model)?,
        }
    }
}

fn run_file(args: FileArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;
    let mut config = UserConfig::load_or_create(&paths)?;
    if let Some(model) = &args.model {
        config.model = model_by_name(model)
            .ok_or_else(|| anyhow::anyhow!("Unknown model '{model}'"))?
            .name
            .to_string();
    }
    if let Some(lang) = &args.lang {
        config.language = lang.clone();
    }
    if let Some(format) = args.format {
        config.output_format = format;
    } else if let Some(out) = &args.out {
        if let Some(format) = TranscriptFormat::from_path_extension(Path::new(out)) {
            config.output_format = format;
        }
    }
    let model = model_by_name(&config.model)
        .ok_or_else(|| anyhow::anyhow!("Unknown model '{}'", config.model))?;
    if !is_model_installed(&paths, model) {
        anyhow::bail!("Model '{}' is not installed", model.name);
    }
    let output_path = transcriber_output_path(&config, args.out.clone(), args.format.is_some());
    let lines = transcribe_wav_file(args.audio, config, output_path.clone())?;
    println!(
        "wrote {} segments to {}",
        lines.len(),
        output_path.display()
    );
    Ok(())
}

fn transcriber_output_path(
    config: &UserConfig,
    out_override: Option<String>,
    force_format_extension: bool,
) -> PathBuf {
    let file_name = out_override.unwrap_or_else(|| "transcript".to_string());
    let mut path = PathBuf::from(file_name);
    if force_format_extension || path.extension().is_none() {
        path.set_extension(config.output_format.extension());
    }
    if path.is_absolute() {
        path
    } else {
        config.output_dir.join(path)
    }
}

fn resolve_device_name(value: &str) -> Result<String> {
    if let Ok(index) = value.parse::<usize>() {
        let devices = input_devices()?;
        if let Some(device) = devices.into_iter().find(|device| device.index == index) {
            return Ok(device.name);
        }
        anyhow::bail!("No input device has index {index}");
    }
    Ok(value.to_string())
}

fn run_live_plain(
    paths: AppPaths,
    config: UserConfig,
    out_override: Option<String>,
    force_format_extension: bool,
    jsonl: bool,
) -> Result<()> {
    let mut session = start_session(paths, config, out_override, force_format_extension)?;
    eprintln!("writing transcript to {}", session.output_path.display());
    eprintln!("press Ctrl+C to stop");

    while let Ok(event) = session.events.recv() {
        match event {
            SessionEvent::Segment(line) => {
                if jsonl {
                    println!(
                        "{}",
                        serde_json::json!({
                            "type": "segment",
                            "timestamp": line.timestamp,
                            "text": line.text,
                        })
                    );
                } else {
                    println!("[{}] {}", line.timestamp, line.text);
                }
            }
            SessionEvent::Saved(path) if jsonl => {
                println!(
                    "{}",
                    serde_json::json!({
                        "type": "saved",
                        "path": path.display().to_string(),
                    })
                );
            }
            SessionEvent::Error(error) => {
                if jsonl {
                    println!(
                        "{}",
                        serde_json::json!({
                            "type": "error",
                            "message": error,
                        })
                    );
                } else {
                    eprintln!("error: {error}");
                }
            }
            SessionEvent::Status(SessionStatus::Stopped) => break,
            _ => {}
        }
    }

    session.stop_and_wait();
    Ok(())
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum LiveOutcome {
    Quit,
    OpenConfig,
    InstallModel(String),
}

struct LiveUi {
    config: UserConfig,
    session: Option<SessionHandle>,
    status: SessionStatus,
    transcript: Vec<TranscriptLine>,
    scrollback: usize,
    output_path: Option<PathBuf>,
    microphone: String,
    level: f32,
    dropped_audio_chunks: u64,
    paused: bool,
    started_at: Instant,
    message: String,
    targets: Vec<MouseTarget>,
    hovered: Option<usize>,
    focused: usize,
}

fn run_live_tui(
    paths: AppPaths,
    config: UserConfig,
    out_override: Option<String>,
    force_format_extension: bool,
) -> Result<LiveOutcome> {
    let mut startup_error = String::new();
    let session = match model_by_name(&config.model) {
        Some(model) if is_model_installed(&paths, model) => {
            match start_session(
                paths.clone(),
                config.clone(),
                out_override,
                force_format_extension,
            ) {
                Ok(handle) => Some(handle),
                Err(error) => {
                    startup_error = format!("{error:#}");
                    None
                }
            }
        }
        _ => None,
    };

    let start_paused = config.start_paused;
    let mut ui = LiveUi {
        output_path: session.as_ref().map(|session| session.output_path.clone()),
        session,
        config,
        status: SessionStatus::Loading,
        transcript: Vec::new(),
        scrollback: 0,
        microphone: "default".to_string(),
        level: 0.0,
        dropped_audio_chunks: 0,
        paused: start_paused,
        started_at: Instant::now(),
        message: String::new(),
        targets: Vec::new(),
        hovered: None,
        focused: 0,
    };

    if ui.session.is_none() {
        ui.status = SessionStatus::Error;
        ui.message = if startup_error.is_empty() {
            "Install a model and choose a microphone in Settings.".to_string()
        } else {
            startup_error
        };
    }

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to create terminal backend")?;
    let tick_rate = Duration::from_millis(100);

    loop {
        drain_session_events(&mut ui);
        terminal.draw(|frame| render_live(frame, &mut ui))?;

        if event::poll(tick_rate)? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(outcome) = handle_live_key(key, &mut ui) {
                        stop_session(&mut ui);
                        return Ok(outcome);
                    }
                }
                Event::Mouse(mouse) => {
                    if let Some(outcome) = handle_live_mouse(mouse, &mut ui) {
                        stop_session(&mut ui);
                        return Ok(outcome);
                    }
                }
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
            }
        }
    }
}

fn drain_session_events(ui: &mut LiveUi) {
    if let Some(session) = &ui.session {
        while let Ok(event) = session.events.try_recv() {
            match event {
                SessionEvent::Status(status) => ui.status = status,
                SessionEvent::Level(level) => ui.level = level,
                SessionEvent::Microphone(name) => ui.microphone = name,
                SessionEvent::Segment(line) => {
                    ui.transcript.push(line);
                    if ui.scrollback == 0 && ui.transcript.len() > 10_000 {
                        let overflow = ui.transcript.len() - 10_000;
                        ui.transcript.drain(0..overflow);
                    }
                }
                SessionEvent::AudioDropped(total) => {
                    ui.dropped_audio_chunks = total;
                    ui.message = format!("Audio queue dropped {total} chunks while busy.");
                }
                SessionEvent::Saved(path) => {
                    ui.output_path = Some(path.clone());
                    ui.message = format!("Saved {}", app::short_home_path(&path));
                }
                SessionEvent::Error(error) => {
                    ui.message = error.clone();
                    ui.status = SessionStatus::Error;
                }
            }
        }
    }
}

fn stop_session(ui: &mut LiveUi) {
    if let Some(session) = &mut ui.session {
        session.stop_and_wait();
    }
}

fn copy_output_path(ui: &mut LiveUi) {
    let Some(path) = ui.output_path.as_ref() else {
        ui.message = "No output path is available yet.".to_string();
        return;
    };
    let path_text = path.display().to_string();
    let result = match env::consts::OS {
        "windows" => ProcessCommand::new("powershell")
            .args(["-NoProfile", "-Command", "Set-Clipboard -Value $args[0]"])
            .arg(&path_text)
            .status(),
        "macos" => ProcessCommand::new("sh")
            .arg("-c")
            .arg("printf %s \"$1\" | pbcopy")
            .arg("sh")
            .arg(&path_text)
            .status(),
        _ => ProcessCommand::new("sh")
            .arg("-c")
            .arg(
                "if command -v wl-copy >/dev/null 2>&1; then printf %s \"$1\" | wl-copy; else printf %s \"$1\" | xclip -selection clipboard; fi",
            )
            .arg("sh")
            .arg(&path_text)
            .status(),
    };

    ui.message = match result {
        Ok(status) if status.success() => "Copied output path.".to_string(),
        _ => format!("Output path: {path_text}"),
    };
}

fn open_output_folder(ui: &mut LiveUi) {
    let folder = ui
        .output_path
        .as_ref()
        .and_then(|path| path.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| ui.config.output_dir.clone());
    let result = match env::consts::OS {
        "windows" => ProcessCommand::new("explorer").arg(&folder).status(),
        "macos" => ProcessCommand::new("open").arg(&folder).status(),
        _ => ProcessCommand::new("xdg-open").arg(&folder).status(),
    };

    ui.message = match result {
        Ok(status) if status.success() => format!("Opened {}", app::short_home_path(&folder)),
        _ => format!("Output folder: {}", folder.display()),
    };
}

fn handle_live_key(key: KeyEvent, ui: &mut LiveUi) -> Option<LiveOutcome> {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Some(LiveOutcome::Quit)
        }
        KeyCode::Char('q') | KeyCode::Esc => Some(LiveOutcome::Quit),
        KeyCode::Char(',') => Some(LiveOutcome::OpenConfig),
        KeyCode::Char('s') | KeyCode::Char('S') => {
            if let Some(session) = &ui.session {
                let _ = session.controls.send(SessionCommand::SaveNow);
            }
            None
        }
        KeyCode::Char('C') => {
            copy_output_path(ui);
            None
        }
        KeyCode::Char('O') => {
            open_output_folder(ui);
            None
        }
        KeyCode::PageUp => {
            ui.scrollback = (ui.scrollback + 10).min(ui.transcript.len().saturating_sub(1));
            None
        }
        KeyCode::PageDown => {
            ui.scrollback = ui.scrollback.saturating_sub(10);
            None
        }
        KeyCode::End => {
            ui.scrollback = 0;
            None
        }
        KeyCode::Char(' ') => {
            if let Some(session) = &ui.session {
                ui.paused = !ui.paused;
                let _ = session.controls.send(SessionCommand::SetPaused(ui.paused));
            }
            None
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
            focus_next(ui);
            None
        }
        KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
            focus_prev(ui);
            None
        }
        KeyCode::Enter => activate_live_focused(ui),
        KeyCode::Char('i') => Some(LiveOutcome::InstallModel(ui.config.model.clone())),
        _ => None,
    }
}

fn handle_live_mouse(mouse: MouseEvent, ui: &mut LiveUi) -> Option<LiveOutcome> {
    ui.hovered = ui
        .targets
        .iter()
        .find(|target| contains(target.area, mouse.column, mouse.row))
        .map(|target| target.id);

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return None;
    }

    let target = ui
        .targets
        .iter()
        .find(|target| contains(target.area, mouse.column, mouse.row) && target.enabled)
        .cloned()?;
    ui.focused = target.id;
    match target.action {
        TargetAction::Live(action) => run_live_action(ui, action),
        TargetAction::Config(_) => None,
    }
}

fn activate_live_focused(ui: &mut LiveUi) -> Option<LiveOutcome> {
    let action = ui
        .targets
        .iter()
        .find(|target| target.id == ui.focused && target.enabled)
        .map(|target| target.action);
    match action {
        Some(TargetAction::Live(action)) => run_live_action(ui, action),
        _ => None,
    }
}

fn run_live_action(ui: &mut LiveUi, action: LiveAction) -> Option<LiveOutcome> {
    match action {
        LiveAction::DownloadModel => Some(LiveOutcome::InstallModel(ui.config.model.clone())),
        LiveAction::Settings => Some(LiveOutcome::OpenConfig),
        LiveAction::Pause => {
            if let Some(session) = &ui.session {
                ui.paused = !ui.paused;
                let _ = session.controls.send(SessionCommand::SetPaused(ui.paused));
            }
            None
        }
        LiveAction::Save => {
            if let Some(session) = &ui.session {
                let _ = session.controls.send(SessionCommand::SaveNow);
            }
            None
        }
        LiveAction::Quit => Some(LiveOutcome::Quit),
    }
}

fn render_live(frame: &mut Frame<'_>, ui: &mut LiveUi) {
    let area = frame.area();
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);

    frame.render_widget(live_header(ui), shell[0]);
    let targets = render_live_body(frame, ui, shell[1]);
    frame.render_widget(live_footer(), shell[2]);
    ui.targets = targets;
    clamp_focus(ui);
}

fn live_header(ui: &LiveUi) -> Paragraph<'_> {
    let status = status_label(&ui.status);
    let output = ui
        .output_path
        .as_ref()
        .map(|path| app::short_home_path(path))
        .unwrap_or_else(|| ui.config.output_dir.display().to_string());
    Paragraph::new(Line::from(vec![
        Span::styled(
            "whisperCLI ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("{status}  "), Style::default().fg(Color::White)),
        Span::styled(
            format!("{}  ", elapsed(ui.started_at)),
            Style::default().fg(Color::Gray),
        ),
        Span::raw(format!(
            "model {}   mic {}   out {}",
            ui.config.model, ui.microphone, output
        )),
    ]))
    .block(base_block())
}

fn render_live_body(frame: &mut Frame<'_>, ui: &LiveUi, area: Rect) -> Vec<MouseTarget> {
    if area.width >= 108 && area.height >= 20 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area);
        frame.render_widget(transcript_panel(ui, cols[0].height), cols[0]);
        render_live_sidebar(frame, ui, cols[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(10)])
            .split(area);
        frame.render_widget(transcript_panel(ui, rows[0].height), rows[0]);
        render_live_sidebar(frame, ui, rows[1])
    }
}

fn transcript_panel(ui: &LiveUi, height: u16) -> Paragraph<'_> {
    let mut lines = Vec::new();
    if ui.transcript.is_empty() {
        lines.push(Line::from(Span::styled(
            empty_state_title(ui),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(empty_state_body(ui)));
        if !ui.message.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::styled("Status: ", muted()),
                Span::raw(ui.message.as_str()),
            ]));
        }
    } else {
        let visible = height.saturating_sub(2).max(1) as usize;
        let end = ui.transcript.len().saturating_sub(ui.scrollback);
        let start = end.saturating_sub(visible);
        if ui.scrollback > 0 {
            lines.push(Line::from(vec![
                Span::styled("Viewing history. ", muted()),
                Span::raw("End returns to latest."),
            ]));
        }
        for line in &ui.transcript[start..end] {
            lines.push(Line::from(vec![
                Span::styled(format!("{}  ", line.timestamp), muted()),
                Span::raw(line.text.as_str()),
            ]));
        }
    }

    Paragraph::new(lines)
        .block(base_block().title(" Transcript "))
        .wrap(Wrap { trim: false })
}

fn render_live_sidebar(frame: &mut Frame<'_>, ui: &LiveUi, area: Rect) -> Vec<MouseTarget> {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8),
            Constraint::Length(4),
            Constraint::Min(3),
        ])
        .split(area);

    frame.render_widget(session_panel(ui), rows[0]);
    frame.render_widget(level_panel(ui), rows[1]);

    let actions = if ui.session.is_some() {
        vec![
            (
                LiveAction::Pause,
                if ui.paused { "Resume" } else { "Pause" },
                "Space",
            ),
            (LiveAction::Save, "Save now", "S"),
            (LiveAction::Settings, "Settings", ","),
            (LiveAction::Quit, "Quit", "Q"),
        ]
    } else {
        vec![
            (LiveAction::DownloadModel, "Download model", "I"),
            (LiveAction::Settings, "Settings", ","),
            (LiveAction::Quit, "Quit", "Q"),
        ]
    };

    render_action_buttons(frame, ui, rows[2], actions)
}

fn session_panel(ui: &LiveUi) -> Paragraph<'_> {
    let output = ui
        .output_path
        .as_ref()
        .map(|path| app::short_home_path(path))
        .unwrap_or_else(|| app::short_home_path(&ui.config.output_dir));
    let lines = vec![
        Line::from(vec![
            Span::styled("status ", muted()),
            Span::raw(status_label(&ui.status)),
        ]),
        Line::from(vec![
            Span::styled("model  ", muted()),
            Span::raw(ui.config.model.as_str()),
        ]),
        Line::from(vec![
            Span::styled("lang   ", muted()),
            Span::raw(ui.config.language.as_str()),
        ]),
        Line::from(vec![
            Span::styled("format ", muted()),
            Span::raw(ui.config.output_format.label()),
        ]),
        Line::from(vec![
            Span::styled("drops  ", muted()),
            Span::raw(ui.dropped_audio_chunks.to_string()),
        ]),
        Line::from(vec![Span::styled("output ", muted()), Span::raw(output)]),
    ];
    Paragraph::new(lines)
        .block(base_block().title(" Session "))
        .wrap(Wrap { trim: true })
}

fn level_panel(ui: &LiveUi) -> Gauge<'_> {
    Gauge::default()
        .block(base_block().title(" Input "))
        .gauge_style(Style::default().fg(Color::White).bg(Color::Black))
        .ratio(ui.level as f64)
        .label(if ui.session.is_some() {
            format!("{:>3}%", (ui.level * 100.0).round() as u8)
        } else {
            "not ready".to_string()
        })
}

fn render_action_buttons(
    frame: &mut Frame<'_>,
    ui: &LiveUi,
    area: Rect,
    actions: Vec<(LiveAction, &'static str, &'static str)>,
) -> Vec<MouseTarget> {
    let visible_rows = actions
        .len()
        .min(area.height.saturating_div(3).max(1) as usize);
    let constraints = vec![Constraint::Length(3); visible_rows];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let mut targets = Vec::new();
    for (index, (action, label, hint)) in actions.into_iter().enumerate() {
        if index >= rows.len() {
            break;
        }
        let id = index;
        let target = MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Live(action),
        };
        frame.render_widget(
            button(label, hint, ui.hovered == Some(id), ui.focused == id, true),
            rows[index],
        );
        targets.push(target);
    }
    targets
}

fn live_footer() -> Paragraph<'static> {
    Paragraph::new(
        "Space pause/resume   S save   PgUp/PgDn scroll   End latest   C copy path   O folder   Q quit",
    )
        .block(base_block().title(" Commands "))
        .wrap(Wrap { trim: true })
}

fn status_label(status: &SessionStatus) -> &'static str {
    match status {
        SessionStatus::Loading => "LOADING",
        SessionStatus::Listening => "LISTENING",
        SessionStatus::Paused => "PAUSED",
        SessionStatus::Processing => "PROCESSING",
        SessionStatus::Saving => "SAVING",
        SessionStatus::Error => "NEEDS SETUP",
        SessionStatus::Stopped => "STOPPED",
    }
}

fn empty_state_title(ui: &LiveUi) -> &'static str {
    if ui.session.is_some() {
        match ui.status {
            SessionStatus::Listening => "Listening",
            SessionStatus::Paused => "Paused",
            SessionStatus::Processing => "Processing audio",
            _ => "Preparing session",
        }
    } else {
        "Setup required"
    }
}

fn empty_state_body(ui: &LiveUi) -> String {
    if ui.session.is_some() {
        match ui.status {
            SessionStatus::Listening => {
                "Speak normally. Transcript text will appear here after each processed audio chunk."
            }
            SessionStatus::Paused => {
                "Audio capture is paused. Press Space or click Resume to continue."
            }
            SessionStatus::Processing => "Whisper is processing the current audio chunk.",
            _ => "Loading the model and microphone.",
        }
        .to_string()
    } else {
        format!(
            "Model '{}' is missing. Press I to download it, or run: whispercli models install {}",
            ui.config.model, ui.config.model
        )
    }
}

fn config_command(args: ConfigArgs) -> Result<()> {
    match args.command {
        Some(ConfigCommand::Get { key }) => config_get(key.as_deref()),
        Some(ConfigCommand::Set { key, value }) => config_set(&key, &value),
        None => run_config_loop(),
    }
}

fn config_get(key: Option<&str>) -> Result<()> {
    let paths = AppPaths::new()?;
    let config = UserConfig::load_or_create(&paths)?;
    match key {
        Some("model") => println!("{}", config.model),
        Some("language") | Some("lang") => println!("{}", config.language),
        Some("microphone") | Some("device") => {
            println!("{}", config.microphone.as_deref().unwrap_or("default"))
        }
        Some("output_dir") | Some("output") => println!("{}", config.output_dir.display()),
        Some("output_format") | Some("format") => println!("{}", config.output_format.extension()),
        Some("chunk_seconds") => println!("{}", config.chunk_seconds),
        Some("threads") => println!("{}", config.threads),
        Some("start_paused") => println!("{}", config.start_paused),
        Some(key) => anyhow::bail!("Unknown config key '{key}'"),
        None => {
            println!("model={}", config.model);
            println!("language={}", config.language);
            println!(
                "microphone={}",
                config.microphone.as_deref().unwrap_or("default")
            );
            println!("output_dir={}", config.output_dir.display());
            println!("output_format={}", config.output_format.extension());
            println!("chunk_seconds={}", config.chunk_seconds);
            println!("threads={}", config.threads);
            println!("start_paused={}", config.start_paused);
        }
    }
    Ok(())
}

fn config_set(key: &str, value: &str) -> Result<()> {
    let paths = AppPaths::new()?;
    let mut config = UserConfig::load_or_create(&paths)?;
    match key {
        "model" => {
            let model = model_by_name(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "Unknown model '{value}'. Supported models: tiny, base, small, recommended"
                )
            })?;
            config.model = model.name.to_string();
        }
        "language" | "lang" => config.language = value.to_string(),
        "microphone" | "device" => {
            config.microphone = if value == "default" || value.is_empty() {
                None
            } else {
                Some(resolve_device_name(value)?)
            };
        }
        "output_dir" | "output" => config.output_dir = PathBuf::from(value),
        "output_format" | "format" => {
            config.output_format = TranscriptFormat::parse(value)
                .ok_or_else(|| anyhow::anyhow!("Unsupported format '{value}'"))?;
        }
        "chunk_seconds" => config.chunk_seconds = value.parse::<u64>()?.max(2),
        "threads" => config.threads = value.parse::<usize>()?.max(1),
        "start_paused" => config.start_paused = parse_bool(value)?,
        _ => anyhow::bail!("Unknown config key '{key}'"),
    }
    config.save(&paths)?;
    println!("saved {key}");
    Ok(())
}

fn parse_bool(value: &str) -> Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "y" | "on" => Ok(true),
        "0" | "false" | "no" | "n" | "off" => Ok(false),
        _ => anyhow::bail!("Expected a boolean value, got '{value}'"),
    }
}

fn run_config_loop() -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;
    loop {
        let config = UserConfig::load_or_create(&paths)?;
        match run_config_tui(paths.clone(), config)? {
            ConfigOutcome::Back => return Ok(()),
            ConfigOutcome::InstallModel(model) => install_model(&paths, &model)?,
        }
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
enum ConfigOutcome {
    Back,
    InstallModel(String),
}

struct ConfigUi {
    paths: AppPaths,
    config: UserConfig,
    devices: Vec<audio::AudioDeviceInfo>,
    device_error: Option<String>,
    output_dirs: Vec<PathBuf>,
    section: usize,
    message: String,
    targets: Vec<MouseTarget>,
    hovered: Option<usize>,
    focused: usize,
}

fn run_config_tui(paths: AppPaths, config: UserConfig) -> Result<ConfigOutcome> {
    let (devices, device_error) = match input_devices() {
        Ok(devices) => (devices, None),
        Err(error) => (Vec::new(), Some(format!("{error:#}"))),
    };
    let output_dirs = output_dir_choices(&paths, &config);
    let mut ui = ConfigUi {
        paths,
        config,
        devices,
        device_error,
        output_dirs,
        section: 0,
        message: "Select values with mouse, arrows, or Enter. Changes are saved immediately."
            .to_string(),
        targets: Vec::new(),
        hovered: None,
        focused: 0,
    };

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to create terminal backend")?;

    loop {
        terminal.draw(|frame| render_config(frame, &mut ui))?;
        if event::poll(Duration::from_millis(100))? {
            match event::read()? {
                Event::Key(key) => {
                    if let Some(outcome) = handle_config_key(key, &mut ui)? {
                        return Ok(outcome);
                    }
                }
                Event::Mouse(mouse) => {
                    if let Some(outcome) = handle_config_mouse(mouse, &mut ui)? {
                        return Ok(outcome);
                    }
                }
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
            }
        }
    }
}

fn output_dir_choices(paths: &AppPaths, config: &UserConfig) -> Vec<PathBuf> {
    let mut dirs = vec![config.output_dir.clone(), paths.transcripts.clone()];
    if let Some(documents) = app::documents_dir() {
        dirs.push(documents.join("whisperCLI"));
    }
    if let Ok(current) = env::current_dir() {
        dirs.push(current);
    }
    dirs.sort();
    dirs.dedup();
    dirs
}

fn handle_config_key(key: KeyEvent, ui: &mut ConfigUi) -> Result<Option<ConfigOutcome>> {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            Ok(Some(ConfigOutcome::Back))
        }
        KeyCode::Esc | KeyCode::Char('q') => Ok(Some(ConfigOutcome::Back)),
        KeyCode::Char('s') | KeyCode::Char('S') => {
            ui.config.save(&ui.paths)?;
            ui.message = "Settings saved.".to_string();
            Ok(None)
        }
        KeyCode::Tab | KeyCode::Down | KeyCode::Right => {
            focus_next_config(ui);
            Ok(None)
        }
        KeyCode::BackTab | KeyCode::Up | KeyCode::Left => {
            focus_prev_config(ui);
            Ok(None)
        }
        KeyCode::Enter => activate_config_focused(ui),
        _ => Ok(None),
    }
}

fn handle_config_mouse(mouse: MouseEvent, ui: &mut ConfigUi) -> Result<Option<ConfigOutcome>> {
    ui.hovered = ui
        .targets
        .iter()
        .find(|target| contains(target.area, mouse.column, mouse.row))
        .map(|target| target.id);

    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return Ok(None);
    }

    let Some(target) = ui
        .targets
        .iter()
        .find(|target| contains(target.area, mouse.column, mouse.row) && target.enabled)
        .cloned()
    else {
        return Ok(None);
    };
    ui.focused = target.id;
    match target.action {
        TargetAction::Config(action) => run_config_action(ui, action),
        TargetAction::Live(_) => Ok(None),
    }
}

fn activate_config_focused(ui: &mut ConfigUi) -> Result<Option<ConfigOutcome>> {
    let action = ui
        .targets
        .iter()
        .find(|target| target.id == ui.focused && target.enabled)
        .map(|target| target.action);
    match action {
        Some(TargetAction::Config(action)) => run_config_action(ui, action),
        _ => Ok(None),
    }
}

fn run_config_action(ui: &mut ConfigUi, action: ConfigAction) -> Result<Option<ConfigOutcome>> {
    match action {
        ConfigAction::Section(section) => ui.section = section,
        ConfigAction::Model(index) => {
            ui.config.model = MODELS[index].name.to_string();
            ui.config.save(&ui.paths)?;
            ui.message = format!("Selected model {}.", MODELS[index].name);
        }
        ConfigAction::DownloadModel(index) => {
            return Ok(Some(ConfigOutcome::InstallModel(
                MODELS[index].name.to_string(),
            )))
        }
        ConfigAction::Device(index) => {
            ui.config.microphone = ui.devices.get(index).map(|device| device.name.clone());
            ui.config.save(&ui.paths)?;
            ui.message = "Microphone selected.".to_string();
        }
        ConfigAction::OutputDir(index) => {
            if let Some(path) = ui.output_dirs.get(index) {
                ui.config.output_dir = path.clone();
                ui.config.save(&ui.paths)?;
                ui.message = "Output folder selected.".to_string();
            }
        }
        ConfigAction::Format(index) => {
            if let Some(format) = TranscriptFormat::all().get(index) {
                ui.config.output_format = *format;
                ui.config.save(&ui.paths)?;
                ui.message = format!("Output format set to {}.", format.label());
            }
        }
        ConfigAction::Language(index) => {
            let languages = languages();
            if let Some(language) = languages.get(index) {
                ui.config.language = language.to_string();
                ui.config.save(&ui.paths)?;
                ui.message = format!("Language set to {language}.");
            }
        }
        ConfigAction::Save => {
            ui.config.save(&ui.paths)?;
            ui.message = "Settings saved.".to_string();
        }
        ConfigAction::Back => return Ok(Some(ConfigOutcome::Back)),
    }
    Ok(None)
}

fn render_config(frame: &mut Frame<'_>, ui: &mut ConfigUi) {
    let area = frame.area();
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(4),
        ])
        .split(area);
    frame.render_widget(config_header(), shell[0]);

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(22), Constraint::Min(40)])
        .split(shell[1]);

    let mut targets = render_config_sections(frame, ui, cols[0]);
    targets.extend(render_config_detail(frame, ui, cols[1], targets.len()));
    frame.render_widget(config_footer(ui), shell[2]);
    ui.targets = targets;
    clamp_focus_config(ui);
}

fn config_header() -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled(
            "whisperCLI Settings",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "   configure model, microphone, output, and language",
            muted(),
        ),
    ]))
    .block(base_block())
}

fn render_config_sections(frame: &mut Frame<'_>, ui: &ConfigUi, area: Rect) -> Vec<MouseTarget> {
    let sections = ["Model", "Microphone", "Output", "General"];
    let visible_rows = sections
        .len()
        .min(area.height.saturating_div(3).max(1) as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); visible_rows])
        .split(area);
    let mut targets = Vec::new();
    for (index, section) in sections.iter().enumerate() {
        if index >= rows.len() {
            break;
        }
        let id = targets.len();
        let focused = ui.focused == id;
        let hovered = ui.hovered == Some(id);
        let selected = ui.section == index;
        frame.render_widget(
            selectable_row(section, selected, hovered, focused),
            rows[index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::Section(index)),
        });
    }
    targets
}

fn render_config_detail(
    frame: &mut Frame<'_>,
    ui: &ConfigUi,
    area: Rect,
    id_offset: usize,
) -> Vec<MouseTarget> {
    match ui.section {
        0 => render_model_settings(frame, ui, area, id_offset),
        1 => render_microphone_settings(frame, ui, area, id_offset),
        2 => render_output_settings(frame, ui, area, id_offset),
        _ => render_general_settings(frame, ui, area, id_offset),
    }
}

fn render_model_settings(
    frame: &mut Frame<'_>,
    ui: &ConfigUi,
    area: Rect,
    id_offset: usize,
) -> Vec<MouseTarget> {
    let item_count = (MODELS.len() + 1).min(area.height.saturating_div(3).max(1) as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); item_count])
        .split(area);
    let mut targets = Vec::new();
    for (index, model) in MODELS.iter().enumerate() {
        if index >= rows.len() {
            return targets;
        }
        let id = id_offset + targets.len();
        let state = model_state(&ui.paths, *model);
        let selected = ui.config.model == model.name;
        let label = format!(
            "{} {}   {}   {}   {}",
            if selected { "●" } else { "○" },
            model.name,
            model.size,
            model.description,
            state.label()
        );
        frame.render_widget(
            selectable_row(&label, selected, ui.hovered == Some(id), ui.focused == id),
            rows[index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::Model(index)),
        });
    }

    let selected_index = MODELS
        .iter()
        .position(|model| model.name == ui.config.model)
        .unwrap_or(0);
    if targets.len() >= rows.len() {
        return targets;
    }
    let id = id_offset + targets.len();
    let row_index = targets.len();
    frame.render_widget(
        button(
            "Download selected model",
            "Enter or click",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[row_index],
    );
    targets.push(MouseTarget {
        id,
        area: rows[row_index],
        enabled: true,
        action: TargetAction::Config(ConfigAction::DownloadModel(selected_index)),
    });
    targets
}

fn render_microphone_settings(
    frame: &mut Frame<'_>,
    ui: &ConfigUi,
    area: Rect,
    id_offset: usize,
) -> Vec<MouseTarget> {
    if ui.devices.is_empty() {
        let message = if let Some(error) = &ui.device_error {
            format!(
                "Could not enumerate input devices:\n\n{error}\n\nmacOS: System Settings > Privacy & Security > Microphone\nWindows: Settings > Privacy > Microphone"
            )
        } else {
            "No input devices found. Check microphone permissions and reconnect your microphone."
                .to_string()
        };
        frame.render_widget(
            Paragraph::new(message)
                .block(base_block().title(" Microphone "))
                .wrap(Wrap { trim: false }),
            area,
        );
        return Vec::new();
    }

    let visible_rows = ui
        .devices
        .len()
        .min(area.height.saturating_div(3).max(1) as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); visible_rows])
        .split(area);
    let mut targets = Vec::new();
    for (index, device) in ui.devices.iter().enumerate() {
        if index >= rows.len() {
            break;
        }
        let id = id_offset + targets.len();
        let selected = ui
            .config
            .microphone
            .as_ref()
            .map(|name| name == &device.name)
            .unwrap_or(device.is_default);
        let label = format!(
            "{} {}{}",
            if selected { "●" } else { "○" },
            device.name,
            if device.is_default { "  default" } else { "" }
        );
        frame.render_widget(
            selectable_row(&label, selected, ui.hovered == Some(id), ui.focused == id),
            rows[index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::Device(index)),
        });
    }
    targets
}

fn render_output_settings(
    frame: &mut Frame<'_>,
    ui: &ConfigUi,
    area: Rect,
    id_offset: usize,
) -> Vec<MouseTarget> {
    let item_count = ui.output_dirs.len() + TranscriptFormat::all().len();
    let visible_rows = area.height.saturating_div(3).max(1) as usize;
    let item_count = item_count.min(visible_rows);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); item_count])
        .split(area);
    let mut targets = Vec::new();

    for (index, dir) in ui.output_dirs.iter().enumerate() {
        if targets.len() >= rows.len() {
            return targets;
        }
        let id = id_offset + targets.len();
        let selected = ui.config.output_dir == *dir;
        let label = format!(
            "{} folder {}",
            if selected { "●" } else { "○" },
            app::short_home_path(dir)
        );
        frame.render_widget(
            selectable_row(&label, selected, ui.hovered == Some(id), ui.focused == id),
            rows[index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::OutputDir(index)),
        });
    }

    for (index, format) in TranscriptFormat::all().iter().enumerate() {
        if targets.len() >= rows.len() {
            return targets;
        }
        let row_index = targets.len();
        let id = id_offset + targets.len();
        let selected = ui.config.output_format == *format;
        let label = format!(
            "{} format {}",
            if selected { "●" } else { "○" },
            format.label()
        );
        frame.render_widget(
            selectable_row(&label, selected, ui.hovered == Some(id), ui.focused == id),
            rows[row_index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[row_index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::Format(index)),
        });
    }
    targets
}

fn render_general_settings(
    frame: &mut Frame<'_>,
    ui: &ConfigUi,
    area: Rect,
    id_offset: usize,
) -> Vec<MouseTarget> {
    let langs = languages();
    let item_count = (langs.len() + 2).min(area.height.saturating_div(3).max(1) as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); item_count])
        .split(area);
    let mut targets = Vec::new();
    for (index, language) in langs.iter().enumerate() {
        if index >= rows.len() {
            return targets;
        }
        let id = id_offset + targets.len();
        let selected = ui.config.language == *language;
        let label = format!("{} language {}", if selected { "●" } else { "○" }, language);
        frame.render_widget(
            selectable_row(&label, selected, ui.hovered == Some(id), ui.focused == id),
            rows[index],
        );
        targets.push(MouseTarget {
            id,
            area: rows[index],
            enabled: true,
            action: TargetAction::Config(ConfigAction::Language(index)),
        });
    }

    if targets.len() >= rows.len() {
        return targets;
    }
    let id = id_offset + targets.len();
    let row_index = targets.len();
    frame.render_widget(
        button(
            "Save settings",
            "S",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[row_index],
    );
    targets.push(MouseTarget {
        id,
        area: rows[row_index],
        enabled: true,
        action: TargetAction::Config(ConfigAction::Save),
    });

    if targets.len() >= rows.len() {
        return targets;
    }
    let id = id_offset + targets.len();
    let row_index = targets.len();
    frame.render_widget(
        button(
            "Back to transcription",
            "Esc",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[row_index],
    );
    targets.push(MouseTarget {
        id,
        area: rows[row_index],
        enabled: true,
        action: TargetAction::Config(ConfigAction::Back),
    });
    targets
}

fn config_footer(ui: &ConfigUi) -> Paragraph<'_> {
    Paragraph::new(Line::from(vec![
        Span::styled("Message  ", muted()),
        Span::raw(ui.message.as_str()),
        Span::styled("    Tab focus   Enter select   Esc back", muted()),
    ]))
    .block(base_block().title(" Commands "))
    .wrap(Wrap { trim: true })
}

fn languages() -> &'static [&'static str] {
    &["ja", "auto", "en"]
}

fn parse_transcript_format(value: &str) -> std::result::Result<TranscriptFormat, String> {
    TranscriptFormat::parse(value)
        .ok_or_else(|| "expected one of: md, txt, srt, json, jsonl".to_string())
}

fn focus_next(ui: &mut LiveUi) {
    if ui.targets.is_empty() {
        return;
    }
    ui.focused = (ui.focused + 1) % ui.targets.len();
}

fn focus_prev(ui: &mut LiveUi) {
    if ui.targets.is_empty() {
        return;
    }
    ui.focused = if ui.focused == 0 {
        ui.targets.len() - 1
    } else {
        ui.focused - 1
    };
}

fn clamp_focus(ui: &mut LiveUi) {
    if ui.targets.is_empty() {
        ui.focused = 0;
    } else if ui.focused >= ui.targets.len() {
        ui.focused = ui.targets.len() - 1;
    }
}

fn focus_next_config(ui: &mut ConfigUi) {
    if ui.targets.is_empty() {
        return;
    }
    ui.focused = (ui.focused + 1) % ui.targets.len();
}

fn focus_prev_config(ui: &mut ConfigUi) {
    if ui.targets.is_empty() {
        return;
    }
    ui.focused = if ui.focused == 0 {
        ui.targets.len() - 1
    } else {
        ui.focused - 1
    };
}

fn clamp_focus_config(ui: &mut ConfigUi) {
    if ui.targets.is_empty() {
        ui.focused = 0;
    } else if ui.focused >= ui.targets.len() {
        ui.focused = ui.targets.len() - 1;
    }
}

fn button(
    label: &'static str,
    hint: &'static str,
    hovered: bool,
    focused: bool,
    enabled: bool,
) -> Paragraph<'static> {
    let style = control_style(hovered, focused, enabled);
    Paragraph::new(Line::from(vec![
        Span::styled(
            label,
            Style::default()
                .fg(if enabled {
                    Color::White
                } else {
                    Color::DarkGray
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {hint}"), muted()),
    ]))
    .block(base_block().border_style(style))
    .alignment(Alignment::Center)
}

fn selectable_row(label: &str, selected: bool, hovered: bool, focused: bool) -> Paragraph<'_> {
    let marker_style = if selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    Paragraph::new(Line::from(vec![Span::styled(
        label.to_string(),
        marker_style,
    )]))
    .block(base_block().border_style(control_style(hovered, focused, true)))
}

fn control_style(hovered: bool, focused: bool, enabled: bool) -> Style {
    if !enabled {
        Style::default().fg(Color::DarkGray)
    } else if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else if hovered {
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

fn base_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}

fn elapsed(start: Instant) -> String {
    let secs = start.elapsed().as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_parses_live_plain_options() {
        let cli = Cli::try_parse_from([
            "whispercli",
            "live",
            "--plain",
            "--format",
            "txt",
            "--device",
            "0",
            "--lang",
            "auto",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Live(args)) => {
                assert!(args.plain);
                assert_eq!(args.format, Some(TranscriptFormat::Txt));
                assert_eq!(args.device.as_deref(), Some("0"));
                assert_eq!(args.lang.as_deref(), Some("auto"));
            }
            _ => panic!("expected live command"),
        }
    }

    #[test]
    fn cli_has_version_flag() {
        Cli::command().debug_assert();
        let error = Cli::try_parse_from(["whispercli", "--version"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
    }

    #[test]
    fn cli_parses_reviewed_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["whispercli", "doctor", "--json"])
                .unwrap()
                .command,
            Some(Commands::Doctor(DoctorArgs { json: true }))
        ));
        assert!(matches!(
            Cli::try_parse_from(["whispercli", "models", "verify", "tiny"])
                .unwrap()
                .command,
            Some(Commands::Models {
                command: ModelCommand::Verify { .. }
            })
        ));
        assert!(matches!(
            Cli::try_parse_from(["whispercli", "config", "set", "language", "ja"])
                .unwrap()
                .command,
            Some(Commands::Config(_))
        ));
    }
}
