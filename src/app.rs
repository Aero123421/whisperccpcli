use anyhow::{anyhow, bail, Context, Result};
use sha1::{Digest, Sha1};
use std::{
    env,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf, MAIN_SEPARATOR},
    process::Command,
    time::Duration,
};

pub const APP_DIR_NAME: &str = ".whispercli";
const MODEL_BASE_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";
const RECOMMENDED_MODEL: &str = "large-v3-turbo-q5_0";

#[derive(Clone, Copy, Debug)]
pub struct ModelInfo {
    pub name: &'static str,
    pub file_name: &'static str,
    pub size: &'static str,
    pub sha1: &'static str,
    pub description: &'static str,
}

pub const MODELS: &[ModelInfo] = &[
    ModelInfo {
        name: "tiny",
        file_name: "ggml-tiny.bin",
        size: "75 MiB",
        sha1: "bd577a113a864445d4c299885e0cb97d4ba92b5f",
        description: "Fastest",
    },
    ModelInfo {
        name: "base",
        file_name: "ggml-base.bin",
        size: "142 MiB",
        sha1: "465707469ff3a37a2b9b8d8f89f2f99de7299dac",
        description: "Balanced",
    },
    ModelInfo {
        name: "small",
        file_name: "ggml-small.bin",
        size: "466 MiB",
        sha1: "55356645c2b361a969dfd0ef2c5a50d530afd8d5",
        description: "Better accuracy",
    },
    ModelInfo {
        name: "large-v3-turbo-q5_0",
        file_name: "ggml-large-v3-turbo-q5_0.bin",
        size: "547 MiB",
        sha1: "e050f7970618a659205450ad97eb95a18d69c9ee",
        description: "Recommended quality",
    },
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModelState {
    Installed,
    Missing,
    Corrupt,
}

impl ModelState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub root: PathBuf,
    pub bin: PathBuf,
    pub models: PathBuf,
    pub transcripts: PathBuf,
    pub logs: PathBuf,
    pub config: PathBuf,
}

impl AppPaths {
    pub fn new() -> Result<Self> {
        let home = home_dir().context("Could not determine your home directory")?;
        let root = home.join(APP_DIR_NAME);

        Ok(Self {
            bin: root.join("bin"),
            models: root.join("models"),
            transcripts: root.join("transcripts"),
            logs: root.join("logs"),
            config: root.join("config.toml"),
            root,
        })
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.root,
            &self.bin,
            &self.models,
            &self.transcripts,
            &self.logs,
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
        }
        Ok(())
    }

    pub fn model_path(&self, model: ModelInfo) -> PathBuf {
        self.models.join(model.file_name)
    }
}

pub fn model_by_name(name: &str) -> Option<ModelInfo> {
    let name = if name == "recommended" {
        RECOMMENDED_MODEL
    } else {
        name
    };
    MODELS.iter().copied().find(|model| model.name == name)
}

pub fn supported_model_names() -> String {
    let mut names = MODELS.iter().map(|model| model.name).collect::<Vec<_>>();
    names.push("recommended");
    names.join(", ")
}

pub fn is_model_installed(paths: &AppPaths, model: ModelInfo) -> bool {
    paths.model_path(model).exists()
}

pub fn model_state(paths: &AppPaths, model: ModelInfo) -> ModelState {
    let path = paths.model_path(model);
    if !path.exists() {
        return ModelState::Missing;
    }

    match verify_sha1(&path, model.sha1) {
        Ok(()) => ModelState::Installed,
        Err(_) => ModelState::Corrupt,
    }
}

pub fn install_model(paths: &AppPaths, requested: &str) -> Result<()> {
    let model = model_by_name(requested).ok_or_else(|| {
        anyhow!(
            "Unknown model '{requested}'. Supported models: {}",
            supported_model_names()
        )
    })?;
    let target = paths.model_path(model);
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

    fs::create_dir_all(&paths.models)
        .with_context(|| format!("Failed to create {}", paths.models.display()))?;

    let url = format!("{MODEL_BASE_URL}/{}", model.file_name);
    let temp = target.with_extension("bin.part");
    if temp.exists() {
        fs::remove_file(&temp)
            .with_context(|| format!("Failed to remove stale {}", temp.display()))?;
    }

    println!("downloading {} ({})", model.name, model.size);
    println!("{url}");

    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(60 * 30))
        .build()
        .context("Failed to create HTTP client")?;
    let mut response = client
        .get(&url)
        .send()
        .with_context(|| format!("Failed to start download from {url}"))?
        .error_for_status()
        .with_context(|| format!("Model server returned an error for {url}"))?;

    let total = response.content_length();
    let mut file =
        File::create(&temp).with_context(|| format!("Failed to create {}", temp.display()))?;
    let mut hasher = Sha1::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 1024 * 256];

    loop {
        let read = response
            .read(&mut buffer)
            .with_context(|| format!("Failed to read from {url}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])
            .with_context(|| format!("Failed to write {}", temp.display()))?;
        hasher.update(&buffer[..read]);
        downloaded += read as u64;

        if let Some(total) = total {
            let pct = (downloaded as f64 / total as f64 * 100.0).min(100.0);
            print!(
                "\r{pct:5.1}%  {}/{} MiB",
                downloaded / 1024 / 1024,
                total / 1024 / 1024
            );
        } else {
            print!("\r{} MiB", downloaded / 1024 / 1024);
        }
        let _ = io::stdout().flush();
    }
    println!();
    file.flush()?;

    let actual = format!("{:x}", hasher.finalize());
    if actual != model.sha1 {
        let _ = fs::remove_file(&temp);
        bail!(
            "SHA1 mismatch for {}: expected {}, got {}",
            model.name,
            model.sha1,
            actual
        );
    }

    fs::rename(&temp, &target)
        .with_context(|| format!("Failed to move model into {}", target.display()))?;

    println!("installed {}", target.display());
    Ok(())
}

pub fn verify_sha1(path: &Path, expected: &str) -> Result<()> {
    let mut file =
        File::open(path).with_context(|| format!("Failed to read {}", path.display()))?;
    let mut hasher = Sha1::new();
    let mut buffer = [0_u8; 1024 * 256];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = format!("{:x}", hasher.finalize());
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

pub fn home_dir() -> Result<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("Neither USERPROFILE nor HOME is set"))
}

pub fn documents_dir() -> Option<PathBuf> {
    home_dir().ok().map(|home| home.join("Documents"))
}

pub fn short_home_path(path: &Path) -> String {
    let home = match home_dir() {
        Ok(home) => home,
        Err(_) => return path.display().to_string(),
    };

    if let Ok(stripped) = path.strip_prefix(home) {
        let rest = stripped.display().to_string();
        if rest.is_empty() {
            "~".to_string()
        } else {
            format!(
                "~{}{}",
                MAIN_SEPARATOR,
                rest.trim_start_matches(['\\', '/'])
            )
        }
    } else {
        path.display().to_string()
    }
}

pub fn add_current_exe_dir_to_path() -> Result<()> {
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
        .args([
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, time::SystemTime};

    #[test]
    fn model_lookup_accepts_recommended_alias() {
        assert_eq!(
            model_by_name("recommended").unwrap().name,
            "large-v3-turbo-q5_0"
        );
        assert_eq!(
            model_by_name("large-v3-turbo-q5_0").unwrap().sha1,
            "e050f7970618a659205450ad97eb95a18d69c9ee"
        );
        assert!(model_by_name("missing").is_none());
    }

    #[test]
    fn verify_sha1_detects_valid_and_invalid_files() {
        let path = env::temp_dir().join(format!(
            "whispercli-sha1-test-{}",
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"abc").unwrap();
        assert!(verify_sha1(&path, "a9993e364706816aba3e25717850c26c9cd0d89d").is_ok());
        assert!(verify_sha1(&path, "0000000000000000000000000000000000000000").is_err());
        let _ = fs::remove_file(path);
    }
}
