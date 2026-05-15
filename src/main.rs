use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use std::{
    env,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

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
    prelude::{Backend, CrosstermBackend, Frame, Terminal},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use sha1::{Digest, Sha1};

const APP_DIR_NAME: &str = ".whispercli";
const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Debug, Parser)]
#[command(name = "whispercli")]
#[command(about = "Lightweight local Whisper transcription CLI")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the transcription TUI.
    Live(LiveArgs),
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

#[derive(Debug, Parser, Clone)]
struct LiveArgs {
    /// Output transcript path.
    #[arg(long, default_value = "meeting.md")]
    out: String,

    /// Whisper model name.
    #[arg(long, default_value = "tiny")]
    model: String,

    /// Recognition language.
    #[arg(long, default_value = "ja")]
    lang: String,
}

impl Default for LiveArgs {
    fn default() -> Self {
        Self {
            out: "meeting.md".to_string(),
            model: "tiny".to_string(),
            lang: "ja".to_string(),
        }
    }
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
        /// Model name: tiny or base.
        #[arg(default_value = "tiny")]
        model: String,
    },
}

#[derive(Clone, Copy, Debug)]
struct ModelInfo {
    name: &'static str,
    file_name: &'static str,
    size: &'static str,
    sha1: &'static str,
}

const MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        file_name: "ggml-tiny.bin",
        size: "75 MiB",
        sha1: "bd577a113a864445d4c299885e0cb97d4ba92b5f",
    },
    ModelInfo {
        name: "base",
        file_name: "ggml-base.bin",
        size: "142 MiB",
        sha1: "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
    },
];

#[derive(Debug)]
struct AppPaths {
    root: PathBuf,
    bin: PathBuf,
    models: PathBuf,
    transcripts: PathBuf,
    logs: PathBuf,
}

impl AppPaths {
    fn new() -> Result<Self> {
        let home = home_dir().context("Could not determine your home directory")?;
        let root = home.join(APP_DIR_NAME);

        Ok(Self {
            bin: root.join("bin"),
            models: root.join("models"),
            transcripts: root.join("transcripts"),
            logs: root.join("logs"),
            root,
        })
    }

    fn ensure(&self) -> Result<()> {
        for dir in [&self.root, &self.bin, &self.models, &self.transcripts, &self.logs] {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScreenMode {
    SetupRequired,
    EnginePending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseAction {
    InstallModel,
    Quit,
}

#[derive(Debug, Clone, Copy)]
struct MouseTarget {
    area: Rect,
    action: MouseAction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunOutcome {
    Quit,
    InstallModel,
}

#[derive(Debug)]
struct App {
    args: LiveArgs,
    paths: AppPaths,
    started_at: Instant,
    running: bool,
    outcome: RunOutcome,
    screen_mode: ScreenMode,
    selected_model: ModelInfo,
    mouse_targets: Vec<MouseTarget>,
}

impl App {
    fn new(args: LiveArgs, paths: AppPaths) -> Result<Self> {
        let selected_model = model_by_name(&args.model).ok_or_else(|| {
            anyhow!(
                "Unknown model '{}'. Supported models: tiny, base",
                args.model
            )
        })?;
        let screen_mode = if paths.models.join(selected_model.file_name).exists() {
            ScreenMode::EnginePending
        } else {
            ScreenMode::SetupRequired
        };

        Ok(Self {
            args,
            paths,
            started_at: Instant::now(),
            running: true,
            outcome: RunOutcome::Quit,
            screen_mode,
            selected_model,
            mouse_targets: Vec::new(),
        })
    }

    fn elapsed(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    }

    fn request_install(&mut self) {
        if self.screen_mode == ScreenMode::SetupRequired {
            self.outcome = RunOutcome::InstallModel;
            self.running = false;
        }
    }

    fn quit(&mut self) {
        self.outcome = RunOutcome::Quit;
        self.running = false;
    }

    fn set_mouse_targets(&mut self, targets: Vec<MouseTarget>) {
        self.mouse_targets = targets;
    }
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
        Some(Commands::Live(args)) => run_tui(args),
        Some(Commands::Init(args)) => init(args),
        Some(Commands::Doctor) => doctor(),
        Some(Commands::Models { command }) => models(command),
        None => run_tui(LiveArgs::default()),
    }
}

fn init(args: InitArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    println!("created {}", paths.root.display());
    println!("models  {}", paths.models.display());
    println!("bin     {}", paths.bin.display());

    if args.add_to_path {
        add_current_exe_dir_to_path()?;
    }

    Ok(())
}

fn doctor() -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    println!("whisperCLI paths");
    println!("root        {}", paths.root.display());
    println!("bin         {}", paths.bin.display());
    println!("models      {}", paths.models.display());
    println!("transcripts {}", paths.transcripts.display());
    println!("logs        {}", paths.logs.display());
    println!();
    println!("platform    {}", env::consts::OS);
    println!("arch        {}", env::consts::ARCH);
    println!("exe         {}", env::current_exe()?.display());

    Ok(())
}

fn models(command: ModelCommand) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    match command {
        ModelCommand::List => {
            for model in MODELS {
                let path = paths.models.join(model.file_name);
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

fn install_model(paths: &AppPaths, requested: &str) -> Result<()> {
    let model = model_by_name(requested)
        .ok_or_else(|| anyhow!("Unknown model '{requested}'. Supported models: tiny, base"))?;
    let target = paths.models.join(model.file_name);
    if target.exists() {
        verify_sha1(&target, model.sha1)
            .with_context(|| format!("Installed model is corrupt: {}", target.display()))?;
        println!(
            "model '{}' already installed at {}",
            model.name,
            target.display()
        );
        return Ok(());
    }

    let url = format!("{MODEL_BASE_URL}/{}", model.file_name);
    let temp = target.with_extension("bin.part");
    println!("downloading {} ({})", model.name, model.size);
    println!("{url}");

    let mut response = reqwest::blocking::get(&url)
        .with_context(|| format!("Failed to start download from {url}"))?
        .error_for_status()
        .with_context(|| format!("Model server returned an error for {url}"))?;
    let mut file =
        File::create(&temp).with_context(|| format!("Failed to create {}", temp.display()))?;
    io::copy(&mut response, &mut file)
        .with_context(|| format!("Failed to write {}", temp.display()))?;
    file.flush()?;

    verify_sha1(&temp, model.sha1).with_context(|| "Downloaded model checksum did not match")?;
    fs::rename(&temp, &target)
        .with_context(|| format!("Failed to move model into {}", target.display()))?;

    println!("installed {}", target.display());
    Ok(())
}

fn verify_sha1(path: &Path, expected: &str) -> Result<()> {
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let actual = format!("{:x}", Sha1::digest(&bytes));
    if actual != expected {
        bail!(
            "SHA1 mismatch for {}: expected {}, got {}",
            path.display(),
            expected,
            actual
        );
    }
    Ok(())
}

fn model_by_name(name: &str) -> Option<ModelInfo> {
    MODELS.iter().copied().find(|model| model.name == name)
}

fn run_tui(args: LiveArgs) -> Result<()> {
    let paths = AppPaths::new()?;
    paths.ensure()?;

    let outcome = {
        let _guard = TerminalGuard::enter()?;
        let backend = CrosstermBackend::new(io::stdout());
        let mut terminal = Terminal::new(backend).context("Failed to create terminal backend")?;
        let outcome = run_app(&mut terminal, App::new(args.clone(), paths)?)?;
        terminal.show_cursor().ok();
        outcome
    };

    if outcome == RunOutcome::InstallModel {
        let paths = AppPaths::new()?;
        paths.ensure()?;
        install_model(&paths, &args.model)?;
        println!();
        println!("model installed. Run `whispercli` to return to the TUI.");
    }

    Ok(())
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> Result<RunOutcome> {
    let tick_rate = Duration::from_millis(250);

    while app.running {
        terminal.draw(|frame| render(frame, &mut app))?;

        if event::poll(tick_rate)? {
            match event::read()? {
                Event::Key(key) => handle_key(key, &mut app),
                Event::Mouse(mouse) => handle_mouse(mouse, &mut app),
                Event::Resize(_, _) | Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
            }
        }
    }

    Ok(app.outcome)
}

fn handle_key(key: KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.quit(),
        KeyCode::Char('q') | KeyCode::Esc => app.quit(),
        KeyCode::Char('i') => app.request_install(),
        _ => {}
    }
}

fn handle_mouse(mouse: MouseEvent, app: &mut App) {
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) {
        return;
    }

    let action = app
        .mouse_targets
        .iter()
        .find(|target| contains(target.area, mouse.column, mouse.row))
        .map(|target| target.action);

    match action {
        Some(MouseAction::InstallModel) => app.request_install(),
        Some(MouseAction::Quit) => app.quit(),
        None => {}
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.x && x < area.x + area.width && y >= area.y && y < area.y + area.height
}

fn render(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    let shell = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(4),
        ])
        .split(area);

    frame.render_widget(header(app), shell[0]);
    let targets = render_body(frame, app, shell[1]);
    frame.render_widget(commands(app), shell[2]);
    app.set_mouse_targets(targets);
}

fn render_body(frame: &mut Frame<'_>, app: &App, area: Rect) -> Vec<MouseTarget> {
    match app.screen_mode {
        ScreenMode::SetupRequired => render_setup(frame, app, area),
        ScreenMode::EnginePending => render_engine_pending(frame, app, area),
    }
}

fn render_setup(frame: &mut Frame<'_>, app: &App, area: Rect) -> Vec<MouseTarget> {
    let chunks = if area.width >= 94 && area.height >= 16 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(9), Constraint::Length(7)])
            .split(area)
    };

    frame.render_widget(setup_message(app), chunks[0]);
    render_actions(frame, app, chunks[1])
}

fn render_engine_pending(frame: &mut Frame<'_>, app: &App, area: Rect) -> Vec<MouseTarget> {
    let chunks = if area.width >= 104 && area.height >= 18 {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(9), Constraint::Length(8)])
            .split(area)
    };

    frame.render_widget(engine_message(app), chunks[0]);
    render_actions(frame, app, chunks[1])
}

fn render_actions(frame: &mut Frame<'_>, app: &App, area: Rect) -> Vec<MouseTarget> {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(3),
        ])
        .split(area);

    frame.render_widget(session(app), rows[0]);

    let mut targets = Vec::new();
    if app.screen_mode == ScreenMode::SetupRequired {
        frame.render_widget(
            button("Install model", "click or press i"),
            rows[1],
        );
        targets.push(MouseTarget {
            area: rows[1],
            action: MouseAction::InstallModel,
        });
    } else {
        frame.render_widget(button("Waiting", "audio engine pending"), rows[1]);
    }

    frame.render_widget(button("Quit", "click or press q"), rows[2]);
    targets.push(MouseTarget {
        area: rows[2],
        action: MouseAction::Quit,
    });
    targets
}

fn header(app: &App) -> Paragraph<'_> {
    let title = Span::styled(
        "whisperCLI",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let state = match app.screen_mode {
        ScreenMode::SetupRequired => " SETUP ",
        ScreenMode::EnginePending => " ENGINE PENDING ",
    };
    let meta = Span::styled(
        format!(
            " {}   model {}   lang {}   out {}",
            app.elapsed(),
            app.args.model,
            app.args.lang,
            app.args.out
        ),
        Style::default().fg(Color::Gray),
    );

    Paragraph::new(Line::from(vec![
        title,
        Span::styled(state, Style::default().fg(Color::Gray)),
        meta,
    ]))
    .block(base_block())
    .alignment(Alignment::Left)
}

fn setup_message(app: &App) -> Paragraph<'_> {
    let lines = vec![
        Line::from(Span::styled(
            "Model is not installed",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("whisperCLI will not show fake transcript text or simulated audio."),
        Line::from("Install a local whisper.cpp model before starting transcription."),
        Line::from(""),
        Line::from(vec![
            Span::styled("Recommended: ", muted()),
            Span::raw(format!(
                "{} ({})",
                app.selected_model.name, app.selected_model.size
            )),
        ]),
        Line::from(vec![
            Span::styled("Command:     ", muted()),
            Span::raw(format!("whispercli models install {}", app.args.model)),
        ]),
    ];

    Paragraph::new(lines)
        .block(base_block().title(" Setup "))
        .wrap(Wrap { trim: false })
}

fn engine_message(app: &App) -> Paragraph<'_> {
    let lines = vec![
        Line::from(Span::styled(
            "Ready for the audio engine implementation",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("No transcript is shown until real microphone input and whisper.cpp decoding are connected."),
        Line::from("The model is installed, so the next implementation step is cpal input + whisper-rs streaming."),
        Line::from(""),
        Line::from(vec![
            Span::styled("Output path: ", muted()),
            Span::raw(app.args.out.as_str()),
        ]),
    ];

    Paragraph::new(lines)
        .block(base_block().title(" Live Transcript "))
        .wrap(Wrap { trim: false })
}

fn session(app: &App) -> Paragraph<'_> {
    let model_path = app.paths.models.join(app.selected_model.file_name);
    let model_state = if model_path.exists() {
        "installed"
    } else {
        "missing"
    };
    let rows = vec![
        Line::from(vec![
            Span::styled("model  ", muted()),
            Span::raw(app.args.model.as_str()),
        ]),
        Line::from(vec![Span::styled("state  ", muted()), Span::raw(model_state)]),
        Line::from(vec![
            Span::styled("lang   ", muted()),
            Span::raw(app.args.lang.as_str()),
        ]),
        Line::from(vec![
            Span::styled("output ", muted()),
            Span::raw(app.args.out.as_str()),
        ]),
        Line::from(vec![
            Span::styled("home   ", muted()),
            Span::raw(short_home_path(&app.paths.root)),
        ]),
    ];

    Paragraph::new(rows)
        .block(base_block().title(" Session "))
        .wrap(Wrap { trim: true })
}

fn button(label: &'static str, hint: &'static str) -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled(label, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        Span::styled(format!("  {hint}"), muted()),
    ]))
    .block(base_block())
    .alignment(Alignment::Center)
}

fn commands(app: &App) -> Paragraph<'static> {
    let line = match app.screen_mode {
        ScreenMode::SetupRequired => "Commands  i install model   q/Esc/Ctrl+C quit   mouse click supported",
        ScreenMode::EnginePending => {
            "Commands  q/Esc/Ctrl+C quit   mouse click supported   no fake transcript"
        }
    };

    Paragraph::new(Line::from(vec![
        Span::styled("Commands  ", muted()),
        Span::raw(line.trim_start_matches("Commands  ")),
    ]))
    .block(base_block())
    .wrap(Wrap { trim: true })
}

fn base_block() -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
}

fn muted() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn short_home_path(path: &Path) -> String {
    let home = match home_dir() {
        Ok(home) => home,
        Err(_) => return path.display().to_string(),
    };

    if let Ok(stripped) = path.strip_prefix(home) {
        let rest = stripped.display().to_string();
        if rest.is_empty() {
            "~".to_string()
        } else {
            format!("~\\{}", rest.trim_start_matches(['\\', '/']))
        }
    } else {
        path.display().to_string()
    }
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Neither USERPROFILE nor HOME is set"))
}

fn add_current_exe_dir_to_path() -> Result<()> {
    if env::consts::OS != "windows" {
        println!("--add-to-path is currently implemented for Windows only.");
        println!(
            "Add this directory to PATH manually: {}",
            current_exe_dir()?.display()
        );
        return Ok(());
    }

    let exe_dir = current_exe_dir()?;
    let exe_dir_text = exe_dir.display().to_string();

    let script = r#"
$dir = $args[0]
$current = [Environment]::GetEnvironmentVariable("Path", "User")
$entries = @()
if ($current) {
    $entries = $current.Split(";") | Where-Object { $_ -ne "" }
}
foreach ($entry in $entries) {
    if ($entry.TrimEnd("\") -ieq $dir.TrimEnd("\")) {
        Write-Host "PATH already contains $dir"
        exit 0
    }
}
$updated = if ($current) { "$current;$dir" } else { $dir }
[Environment]::SetEnvironmentVariable("Path", $updated, "User")
Write-Host "Added $dir to the user PATH"
"#;

    let status = Command::new("powershell")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-Command", script])
        .arg(&exe_dir_text)
        .status()
        .context("Failed to update the user PATH with PowerShell")?;

    if !status.success() {
        bail!("PowerShell PATH update failed with status {status}");
    }

    println!("Added {} to the user PATH.", exe_dir.display());
    println!("Open a new terminal before running whispercli by name.");
    Ok(())
}

fn current_exe_dir() -> Result<PathBuf> {
    let exe = env::current_exe().context("Failed to locate current executable")?;
    exe.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("Executable has no parent directory: {}", exe.display()))
}
