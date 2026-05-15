use crate::app::{documents_dir, AppPaths};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptFormat {
    Md,
    Txt,
}

impl TranscriptFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Md => "md",
            Self::Txt => "txt",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Md => "Markdown",
            Self::Txt => "Text",
        }
    }

    pub fn all() -> &'static [TranscriptFormat] {
        &[TranscriptFormat::Md, TranscriptFormat::Txt]
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct UserConfig {
    pub model: String,
    pub language: String,
    pub microphone: Option<String>,
    pub output_dir: PathBuf,
    pub output_format: TranscriptFormat,
    pub chunk_seconds: u64,
    pub threads: usize,
    pub start_paused: bool,
}

impl UserConfig {
    pub fn default_for(paths: &AppPaths) -> Self {
        Self {
            model: "tiny".to_string(),
            language: "ja".to_string(),
            microphone: None,
            output_dir: documents_dir()
                .map(|documents| documents.join("whisperCLI"))
                .unwrap_or_else(|| paths.transcripts.clone()),
            output_format: TranscriptFormat::Md,
            chunk_seconds: 5,
            threads: std::thread::available_parallelism()
                .map(|threads| threads.get().clamp(1, 8))
                .unwrap_or(4),
            start_paused: false,
        }
    }

    pub fn load_or_create(paths: &AppPaths) -> Result<Self> {
        paths.ensure()?;
        if !paths.config.exists() {
            let config = Self::default_for(paths);
            config.save(paths)?;
            return Ok(config);
        }

        let raw = fs::read_to_string(&paths.config)
            .with_context(|| format!("Failed to read {}", paths.config.display()))?;
        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", paths.config.display()))?;
        if config.chunk_seconds < 2 {
            config.chunk_seconds = 2;
        }
        if config.threads == 0 {
            config.threads = 1;
        }
        if config.microphone.as_deref() == Some("") {
            config.microphone = None;
        }
        Ok(config)
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure()?;
        if let Some(parent) = paths.config.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let raw = toml::to_string_pretty(self).context("Failed to serialize config")?;
        fs::write(&paths.config, raw)
            .with_context(|| format!("Failed to write {}", paths.config.display()))?;
        Ok(())
    }
}
