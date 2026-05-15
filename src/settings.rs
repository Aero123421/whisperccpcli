use crate::app::{documents_dir, AppPaths};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptFormat {
    Md,
    Txt,
    Srt,
    Json,
    Jsonl,
}

impl TranscriptFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Md => "md",
            Self::Txt => "txt",
            Self::Srt => "srt",
            Self::Json => "json",
            Self::Jsonl => "jsonl",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Md => "Markdown",
            Self::Txt => "Text",
            Self::Srt => "SRT",
            Self::Json => "JSON",
            Self::Jsonl => "JSONL",
        }
    }

    pub fn all() -> &'static [TranscriptFormat] {
        &[
            TranscriptFormat::Md,
            TranscriptFormat::Txt,
            TranscriptFormat::Srt,
            TranscriptFormat::Json,
            TranscriptFormat::Jsonl,
        ]
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "md" | "markdown" => Some(Self::Md),
            "txt" | "text" => Some(Self::Txt),
            "srt" => Some(Self::Srt),
            "json" => Some(Self::Json),
            "jsonl" | "ndjson" => Some(Self::Jsonl),
            _ => None,
        }
    }

    pub fn from_path_extension(path: &Path) -> Option<Self> {
        path.extension()
            .and_then(|extension| extension.to_str())
            .and_then(Self::parse)
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
        let config: Self = toml::from_str(&raw)
            .with_context(|| format!("Failed to parse {}", paths.config.display()))?;
        Ok(config.normalized())
    }

    pub fn normalized(mut self) -> Self {
        if self.chunk_seconds < 2 {
            self.chunk_seconds = 2;
        }
        if self.threads == 0 {
            self.threads = 1;
        }
        if self.microphone.as_deref() == Some("") {
            self.microphone = None;
        }
        self
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_parser_accepts_aliases() {
        assert_eq!(
            TranscriptFormat::parse("markdown"),
            Some(TranscriptFormat::Md)
        );
        assert_eq!(TranscriptFormat::parse("text"), Some(TranscriptFormat::Txt));
        assert_eq!(
            TranscriptFormat::parse("jsonl"),
            Some(TranscriptFormat::Jsonl)
        );
        assert_eq!(TranscriptFormat::parse("nope"), None);
    }

    #[test]
    fn config_normalization_repairs_old_values() {
        let paths = AppPaths {
            root: PathBuf::from("root"),
            bin: PathBuf::from("bin"),
            models: PathBuf::from("models"),
            transcripts: PathBuf::from("transcripts"),
            logs: PathBuf::from("logs"),
            config: PathBuf::from("config.toml"),
        };
        let mut config = UserConfig::default_for(&paths);
        config.chunk_seconds = 0;
        config.threads = 0;
        config.microphone = Some(String::new());

        let config = config.normalized();
        assert_eq!(config.chunk_seconds, 2);
        assert_eq!(config.threads, 1);
        assert_eq!(config.microphone, None);
    }
}
