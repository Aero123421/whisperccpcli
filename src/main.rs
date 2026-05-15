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
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    prelude::{Backend, CrosstermBackend, Frame, Terminal},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, Paragraph, Sparkline, Wrap},
};
use sha1::{Digest, Sha1};

const APP_DIR_NAME: &str = ".whispercli";
const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

#[derive(Debug, Parser)]
#[command(name = "whispercli")]
#[command(about = "Lightweight local Whisper transcription CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Start the real-time transcription TUI.
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

#[derive(Debug)]
struct App {
    args: LiveArgs,
    paths: AppPaths,
    started_at: Instant,
    level: u16,
    tick: u64,
    waveform: Vec<u64>,
    running: bool,
    model_status: String,
}

impl App {
    fn new(args: LiveArgs, paths: AppPaths) -> Self {
        let model_status = model_by_name(&args.model)
            .map(|model| {
                if paths.models.join(model.file_name).exists() {
                    "installed".to_string()
                } else {
                    "missing".to_string()
                }
            })
            .unwrap_or_else(|| "unknown".to_string());

        Self {
            args,
            paths,
            started_at: Instant::now(),
            level: 28,
            tick: 0,
            waveform: vec![1, 3, 4, 2, 7, 9, 6, 5, 3, 4, 8, 10, 7, 4, 2, 1],
            running: true,
            model_status,
        }
    }

    fn elapsed(&self) -> String {
        let secs = self.started_at.elapsed().as_secs();
        format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    }

    fn on_tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        let phase = self.tick % 32;
        self.level = 18 + ((phase * 7 + phase.pow(2)) % 58) as u16;

        let next = 1 + ((self.tick * 5 + self.tick.pow(2)) % 10);
        self.waveform.remove(0);
        self.waveform.push(next);
    }
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen)
            .context("Failed to enter alternate terminal screen")?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
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
        Commands::Live(args) => run_tui(args),
        Commands::Init(args) => init(args),
        Commands::Doctor => doctor(),
        Commands::Models { command } => models(command),
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
                let state = if path.exists() { "installed" } else { "available" };
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
    let model = model_by_name(requested).ok_or_else(|| {
        anyhow!("Unknown model '{requested}'. Supported models: tiny, base")
    })?;
    let target = paths.models.join(model.file_name);
    if target.exists() {
        verify_sha1(&target, model.sha1)
            .with_context(|| format!("Installed model is corrupt: {}", target.display()))?;
        println!("model '{}' already installed at {}", model.name, target.display());
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
    let mut file = File::create(&temp)
        .with_context(|| format!("Failed to create {}", temp.display()))?;
    io::copy(&mut response, &mut file)
        .with_context(|| format!("Failed to write {}", temp.display()))?;
    file.flush()?;

    verify_sha1(&temp, model.sha1)
        .with_context(|| "Downloaded model checksum did not match")?;
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

    let _guard = TerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("Failed to create terminal backend")?;
    let result = run_app(&mut terminal, App::new(args, paths));
    terminal.show_cursor().ok();
    result
}

fn run_app<B: Backend>(terminal: &mut Terminal<B>, mut app: App) -> Result<()> {
    let tick_rate = Duration::from_millis(250);

    while app.running {
        terminal.draw(|frame| render(frame, &app))?;

        if event::poll(tick_rate)? {
            if let Event::Key(key) = event::read()? {
                handle_key(key, &mut app);
            }
        }

        app.on_tick();
    }

    Ok(())
}

fn handle_key(key: KeyEvent, app: &mut App) {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => app.running = false,
        KeyCode::Char('q') | KeyCode::Esc => app.running = false,
        _ => {}
    }
}

fn render(frame: &mut Frame<'_>, app: &App) {
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
    render_body(frame, app, shell[1]);
    frame.render_widget(commands(), shell[2]);
}

fn render_body(frame: &mut Frame<'_>, app: &App, area: Rect) {
    if area.width >= 104 && area.height >= 20 {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
            .split(area);

        frame.render_widget(transcript(), columns[0]);

        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(9),
                Constraint::Length(6),
                Constraint::Min(5),
            ])
            .split(columns[1]);

        frame.render_widget(session(app), right[0]);
        frame.render_widget(audio(app), right[1]);
        frame.render_widget(timeline(app), right[2]);
    } else {
        let rows = if area.height >= 22 {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Min(8),
                    Constraint::Length(6),
                    Constraint::Length(5),
                ])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(6), Constraint::Length(5)])
                .split(area)
        };

        frame.render_widget(transcript(), rows[0]);

        if rows.len() == 3 {
            frame.render_widget(session(app), rows[1]);
            frame.render_widget(audio(app), rows[2]);
        } else {
            frame.render_widget(compact_status(app), rows[1]);
        }
    }
}

fn header(app: &App) -> Paragraph<'_> {
    let title = Span::styled(
        "whisperCLI",
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let rec = Span::styled(" REC ", Style::default().fg(Color::Gray));
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

    Paragraph::new(Line::from(vec![title, rec, meta]))
        .block(base_block())
        .alignment(Alignment::Left)
}

fn transcript() -> Paragraph<'static> {
    let lines = vec![
        Line::from("今日はRustで軽量な文字起こしCLIを作る話をしています。"),
        Line::from("whisper.cppを使って、オフラインでもリアルタイムに保存できます。"),
        Line::from("まずはTUIの見た目と操作感を固めて、あとから音声入力を接続します。"),
        Line::from(""),
        Line::from(Span::styled(
            "listening...",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    Paragraph::new(lines)
        .block(base_block().title(" Live Transcript "))
        .wrap(Wrap { trim: false })
}

fn session(app: &App) -> Paragraph<'_> {
    let rows = vec![
        Line::from(vec![
            Span::styled("device ", muted()),
            Span::raw("Microphone Array"),
        ]),
        Line::from(vec![
            Span::styled("model  ", muted()),
            Span::raw(app.args.model.as_str()),
        ]),
        Line::from(vec![
            Span::styled("status ", muted()),
            Span::raw(app.model_status.as_str()),
        ]),
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
            Span::raw(app.paths.root.display().to_string()),
        ]),
    ];

    Paragraph::new(rows)
        .block(base_block().title(" Session "))
        .wrap(Wrap { trim: true })
}

fn audio(app: &App) -> Gauge<'_> {
    Gauge::default()
        .block(base_block().title(" Audio "))
        .gauge_style(Style::default().fg(Color::Gray).bg(Color::Black))
        .ratio(app.level as f64 / 100.0)
        .label(format!("level {:02}%", app.level))
}

fn timeline(app: &App) -> Sparkline<'_> {
    Sparkline::default()
        .block(base_block().title(" Timeline "))
        .data(&app.waveform)
        .style(Style::default().fg(Color::Gray))
        .bar_set(symbols::bar::NINE_LEVELS)
}

fn compact_status(app: &App) -> Paragraph<'_> {
    let lines = vec![
        Line::from(vec![
            Span::styled("Audio ", muted()),
            Span::raw(format!("level {:02}%   ", app.level)),
            Span::styled("model ", muted()),
            Span::raw(format!("{} ({})", app.args.model, app.model_status)),
        ]),
        Line::from(vec![
            Span::styled("output ", muted()),
            Span::raw(app.args.out.as_str()),
            Span::raw("   "),
            Span::styled("home ", muted()),
            Span::raw(app.paths.root.display().to_string()),
        ]),
    ];

    Paragraph::new(lines)
        .block(base_block().title(" Status "))
        .wrap(Wrap { trim: true })
}

fn commands() -> Paragraph<'static> {
    Paragraph::new(Line::from(vec![
        Span::styled("Commands  ", muted()),
        Span::raw("Ctrl+S save   Ctrl+P pause   Ctrl+M model   "),
        Span::styled("q/Esc/Ctrl+C finish", Style::default().fg(Color::White)),
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

fn home_dir() -> Result<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Neither USERPROFILE nor HOME is set"))
}

fn add_current_exe_dir_to_path() -> Result<()> {
    if env::consts::OS != "windows" {
        println!("--add-to-path is currently implemented for Windows only.");
        println!("Add this directory to PATH manually: {}", current_exe_dir()?.display());
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
