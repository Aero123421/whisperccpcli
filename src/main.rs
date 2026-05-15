mod app;
mod audio;
mod settings;
mod transcriber;

use anyhow::{Context, Result};
use app::{
    add_current_exe_dir_to_path, install_model, is_model_installed, model_by_name, AppPaths, MODELS,
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
    env, io,
    path::PathBuf,
    time::{Duration, Instant},
};
use transcriber::{
    start_session, SessionCommand, SessionEvent, SessionHandle, SessionStatus, TranscriptLine,
};

#[derive(Debug, Parser)]
#[command(name = "whispercli")]
#[command(about = "Local real-time transcription with whisper.cpp")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Open the live transcription TUI.
    Live(LiveArgs),
    /// Open the settings TUI.
    Config,
    /// List available input devices.
    Devices,
    /// Create user directories and print install diagnostics.
    Init(InitArgs),
    /// Inspect paths and platform setup.
    Doctor,
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

    /// Whisper model name.
    #[arg(long)]
    model: Option<String>,

    /// Recognition language, for example ja, en, or auto.
    #[arg(long)]
    lang: Option<String>,
}

#[derive(Debug, Parser)]
struct InitArgs {
    /// Add the current executable directory to the user PATH on Windows.
    #[arg(long)]
    add_to_path: bool,
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    /// Show installable and installed models.
    List,
    /// Download a model into ~/.whispercli/models.
    Install {
        /// Model name: tiny, base, or small.
        #[arg(default_value = "tiny")]
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

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)
            .context("Failed to enter terminal UI mode")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
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
        Some(Commands::Config) => run_config_loop(),
        Some(Commands::Devices) => devices(),
        Some(Commands::Init(args)) => init(args),
        Some(Commands::Doctor) => doctor(),
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

    Ok(())
}

fn doctor() -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;
    let config = UserConfig::load_or_create(&paths)?;

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
        let state = if is_model_installed(&paths, *model) {
            "installed"
        } else {
            "missing"
        };
        println!("{:<6} {:<10} {}", model.name, model.size, state);
    }
    println!();
    println!("microphones");
    for device in input_devices()? {
        let marker = if device.is_default { "*" } else { " " };
        println!(
            "{} {:<2} {:<40} {}",
            marker, device.index, device.name, device.config
        );
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
                let state = if path.exists() {
                    "installed"
                } else {
                    "available"
                };
                println!(
                    "{:<6} {:<10} {:<10} {}",
                    model.name,
                    model.size,
                    state,
                    path.display()
                );
            }
            Ok(())
        }
        ModelCommand::Install { model } => install_model(&paths, &model),
    }
}

fn effective_config(paths: &AppPaths, args: &LiveArgs) -> Result<UserConfig> {
    let mut config = UserConfig::load_or_create(paths)?;
    if let Some(model) = &args.model {
        config.model = model.clone();
    }
    if let Some(lang) = &args.lang {
        config.language = lang.clone();
    }
    Ok(config)
}

fn run_live(args: LiveArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    loop {
        let config = effective_config(&paths, &args)?;
        let outcome = run_live_tui(paths.clone(), config, args.out.clone())?;
        match outcome {
            LiveOutcome::Quit => return Ok(()),
            LiveOutcome::OpenConfig => run_config_loop()?,
            LiveOutcome::InstallModel(model) => install_model(&paths, &model)?,
        }
    }
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
    output_path: Option<PathBuf>,
    microphone: String,
    level: f32,
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
) -> Result<LiveOutcome> {
    let mut startup_error = String::new();
    let session = match model_by_name(&config.model) {
        Some(model) if is_model_installed(&paths, model) => {
            match start_session(paths.clone(), config.clone(), out_override) {
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
        microphone: "default".to_string(),
        level: 0.0,
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
                SessionEvent::Segment(line) => ui.transcript.push(line),
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
        frame.render_widget(transcript_panel(ui), cols[0]);
        render_live_sidebar(frame, ui, cols[1])
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(8), Constraint::Length(10)])
            .split(area);
        frame.render_widget(transcript_panel(ui), rows[0]);
        render_live_sidebar(frame, ui, rows[1])
    }
}

fn transcript_panel(ui: &LiveUi) -> Paragraph<'_> {
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
        for line in ui.transcript.iter().rev().take(18).rev() {
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
    let constraints = vec![Constraint::Length(3); actions.len()];
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);
    let mut targets = Vec::new();
    for (index, (action, label, hint)) in actions.into_iter().enumerate() {
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
    Paragraph::new("Space pause/resume   S save   , settings   Tab focus   Enter select   Q quit")
        .block(base_block().title(" Commands "))
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

fn empty_state_body(ui: &LiveUi) -> &'static str {
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
    } else {
        "Open Settings to choose a microphone and output folder, or download the selected model."
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
    output_dirs: Vec<PathBuf>,
    section: usize,
    message: String,
    targets: Vec<MouseTarget>,
    hovered: Option<usize>,
    focused: usize,
}

fn run_config_tui(paths: AppPaths, config: UserConfig) -> Result<ConfigOutcome> {
    let devices = input_devices().unwrap_or_default();
    let output_dirs = output_dir_choices(&paths, &config);
    let mut ui = ConfigUi {
        paths,
        config,
        devices,
        output_dirs,
        section: 0,
        message: "Select values with mouse, arrows, or Enter. Changes are saved with S."
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); sections.len()])
        .split(area);
    let mut targets = Vec::new();
    for (index, section) in sections.iter().enumerate() {
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); MODELS.len() + 1])
        .split(area);
    let mut targets = Vec::new();
    for (index, model) in MODELS.iter().enumerate() {
        let id = id_offset + targets.len();
        let installed = is_model_installed(&ui.paths, *model);
        let selected = ui.config.model == model.name;
        let label = format!(
            "{} {}   {}   {}   {}",
            if selected { "●" } else { "○" },
            model.name,
            model.size,
            model.description,
            if installed { "installed" } else { "download" }
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
    let id = id_offset + targets.len();
    frame.render_widget(
        button(
            "Download selected model",
            "Enter or click",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[MODELS.len()],
    );
    targets.push(MouseTarget {
        id,
        area: rows[MODELS.len()],
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
        frame.render_widget(
            Paragraph::new("No input devices found. Check Windows microphone permissions and reconnect your microphone.")
                .block(base_block().title(" Microphone "))
                .wrap(Wrap { trim: false }),
            area,
        );
        return Vec::new();
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); ui.devices.len()])
        .split(area);
    let mut targets = Vec::new();
    for (index, device) in ui.devices.iter().enumerate() {
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); item_count])
        .split(area);
    let mut targets = Vec::new();

    for (index, dir) in ui.output_dirs.iter().enumerate() {
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
        let row_index = ui.output_dirs.len() + index;
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
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(3); langs.len() + 2])
        .split(area);
    let mut targets = Vec::new();
    for (index, language) in langs.iter().enumerate() {
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

    let id = id_offset + targets.len();
    frame.render_widget(
        button(
            "Save settings",
            "S",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[langs.len()],
    );
    targets.push(MouseTarget {
        id,
        area: rows[langs.len()],
        enabled: true,
        action: TargetAction::Config(ConfigAction::Save),
    });

    let id = id_offset + targets.len();
    frame.render_widget(
        button(
            "Back to transcription",
            "Esc",
            ui.hovered == Some(id),
            ui.focused == id,
            true,
        ),
        rows[langs.len() + 1],
    );
    targets.push(MouseTarget {
        id,
        area: rows[langs.len() + 1],
        enabled: true,
        action: TargetAction::Config(ConfigAction::Back),
    });
    targets
}

fn config_footer(ui: &ConfigUi) -> Paragraph<'_> {
    Paragraph::new(Line::from(vec![
        Span::styled("Message  ", muted()),
        Span::raw(ui.message.as_str()),
        Span::styled("    Tab focus   Enter select   S save   Esc back", muted()),
    ]))
    .block(base_block().title(" Commands "))
    .wrap(Wrap { trim: true })
}

fn languages() -> &'static [&'static str] {
    &["ja", "auto", "en"]
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
