#![cfg_attr(
    not(feature = "whisper"),
    allow(dead_code, unused_imports, unused_variables)
)]

#[cfg(feature = "whisper")]
use crate::audio;
use crate::{
    app::{is_model_installed, model_by_name, AppPaths},
    settings::{TranscriptFormat, UserConfig},
};
use anyhow::{anyhow, Context, Result};
#[cfg(feature = "whisper")]
use crossbeam_channel::bounded;
use crossbeam_channel::{unbounded, Receiver, Sender};
#[cfg(feature = "whisper")]
use std::time::Instant;
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    thread,
    time::Duration,
};
#[cfg(feature = "whisper")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;

#[derive(Clone, Debug)]
pub struct TranscriptLine {
    pub timestamp: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum SessionStatus {
    Loading,
    Listening,
    Paused,
    Processing,
    Saving,
    Error,
    Stopped,
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Status(SessionStatus),
    Level(f32),
    Microphone(String),
    Segment(TranscriptLine),
    Saved(PathBuf),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum SessionCommand {
    SetPaused(bool),
    SaveNow,
    Stop,
}

pub struct SessionHandle {
    pub events: Receiver<SessionEvent>,
    pub controls: Sender<SessionCommand>,
    pub output_path: PathBuf,
}

pub fn start_session(
    paths: AppPaths,
    config: UserConfig,
    out_override: Option<String>,
) -> Result<SessionHandle> {
    let model =
        model_by_name(&config.model).ok_or_else(|| anyhow!("Unknown model '{}'", config.model))?;
    if !is_model_installed(&paths, model) {
        return Err(anyhow!("Model '{}' is not installed", model.name));
    }

    let output_path = resolve_output_path(&config, out_override);
    let (event_tx, event_rx) = unbounded();
    let (command_tx, command_rx) = unbounded();
    let worker_paths = paths.clone();
    let worker_config = config.clone();
    let worker_output = output_path.clone();

    thread::spawn(move || {
        if let Err(error) = run_worker(
            worker_paths,
            worker_config,
            worker_output,
            command_rx,
            event_tx.clone(),
        ) {
            let _ = event_tx.send(SessionEvent::Error(format!("{error:#}")));
            let _ = event_tx.send(SessionEvent::Status(SessionStatus::Error));
        }
    });

    Ok(SessionHandle {
        events: event_rx,
        controls: command_tx,
        output_path,
    })
}

fn run_worker(
    paths: AppPaths,
    config: UserConfig,
    output_path: PathBuf,
    command_rx: Receiver<SessionCommand>,
    event_tx: Sender<SessionEvent>,
) -> Result<()> {
    let model =
        model_by_name(&config.model).ok_or_else(|| anyhow!("Unknown model '{}'", config.model))?;
    let model_path = paths.model_path(model);

    event_tx
        .send(SessionEvent::Status(SessionStatus::Loading))
        .ok();

    #[cfg(not(feature = "whisper"))]
    {
        let _ = event_tx.send(SessionEvent::Status(SessionStatus::Error));
        Ok(())
    }

    #[cfg(feature = "whisper")]
    {
        let context =
            WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
                .with_context(|| format!("Failed to load model {}", model_path.display()))?;
        let mut state = context
            .create_state()
            .context("Failed to create whisper state")?;

        let (audio_tx, audio_rx) = bounded::<Vec<f32>>(64);
        let (level_tx, level_rx) = bounded::<f32>(8);
        let (stream_error_tx, stream_error_rx) = bounded::<String>(8);
        let capture = audio::start_capture(
            config.microphone.as_deref(),
            audio_tx,
            level_tx,
            stream_error_tx,
        )?;
        event_tx
            .send(SessionEvent::Microphone(capture.device_name.clone()))
            .ok();

        let mut paused = config.start_paused;
        let mut lines = Vec::<TranscriptLine>::new();
        let mut pending = Vec::<f32>::new();
        let mut last_text = String::new();
        let started_at = Instant::now();
        let chunk_samples = (config.chunk_seconds.max(2) as usize) * WHISPER_SAMPLE_RATE as usize;

        event_tx
            .send(SessionEvent::Status(if paused {
                SessionStatus::Paused
            } else {
                SessionStatus::Listening
            }))
            .ok();

        loop {
            while let Ok(command) = command_rx.try_recv() {
                match command {
                    SessionCommand::SetPaused(next) => {
                        paused = next;
                        event_tx
                            .send(SessionEvent::Status(if paused {
                                SessionStatus::Paused
                            } else {
                                SessionStatus::Listening
                            }))
                            .ok();
                    }
                    SessionCommand::SaveNow => {
                        event_tx
                            .send(SessionEvent::Status(SessionStatus::Saving))
                            .ok();
                        write_transcript(&output_path, config.output_format, &lines)?;
                        event_tx.send(SessionEvent::Saved(output_path.clone())).ok();
                        event_tx
                            .send(SessionEvent::Status(if paused {
                                SessionStatus::Paused
                            } else {
                                SessionStatus::Listening
                            }))
                            .ok();
                    }
                    SessionCommand::Stop => {
                        write_transcript(&output_path, config.output_format, &lines)?;
                        event_tx.send(SessionEvent::Saved(output_path.clone())).ok();
                        event_tx
                            .send(SessionEvent::Status(SessionStatus::Stopped))
                            .ok();
                        return Ok(());
                    }
                }
            }

            while let Ok(level) = level_rx.try_recv() {
                event_tx.send(SessionEvent::Level(level)).ok();
            }

            while let Ok(error) = stream_error_rx.try_recv() {
                event_tx.send(SessionEvent::Error(error)).ok();
            }

            let Ok(chunk) = audio_rx.recv_timeout(Duration::from_millis(80)) else {
                continue;
            };

            if paused {
                continue;
            }

            pending.extend(resample_to_16k(&chunk, capture.sample_rate));
            while pending.len() >= chunk_samples {
                let rest = pending.split_off(chunk_samples);
                let audio_chunk = std::mem::replace(&mut pending, rest);

                if rms(&audio_chunk) < 0.006 {
                    continue;
                }

                event_tx
                    .send(SessionEvent::Status(SessionStatus::Processing))
                    .ok();
                match transcribe_chunk(&mut state, &config, &audio_chunk) {
                    Ok(text) => {
                        let text = text.trim().to_string();
                        if !text.is_empty() && !is_duplicate(&last_text, &text) {
                            last_text = text.clone();
                            let line = TranscriptLine {
                                timestamp: format_elapsed(started_at.elapsed()),
                                text,
                            };
                            lines.push(line.clone());
                            write_transcript(&output_path, config.output_format, &lines)?;
                            event_tx.send(SessionEvent::Segment(line)).ok();
                            event_tx.send(SessionEvent::Saved(output_path.clone())).ok();
                        }
                    }
                    Err(error) => {
                        event_tx
                            .send(SessionEvent::Error(format!("{error:#}")))
                            .ok();
                    }
                }
                event_tx
                    .send(SessionEvent::Status(SessionStatus::Listening))
                    .ok();
            }
        }
    }
}

#[cfg(feature = "whisper")]
fn transcribe_chunk(
    state: &mut whisper_rs::WhisperState,
    config: &UserConfig,
    samples: &[f32],
) -> Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(config.language.as_str()));
    params.set_n_threads(config.threads as i32);
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_single_segment(true);
    params.set_no_context(true);

    state
        .full(params, samples)
        .context("Whisper inference failed")?;

    let mut text = String::new();
    for segment in state.as_iter() {
        text.push_str(segment.to_string().trim());
    }
    Ok(text)
}

fn resample_to_16k(input: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == WHISPER_SAMPLE_RATE || input.is_empty() {
        return input.to_vec();
    }

    let out_len =
        ((input.len() as f64) * (WHISPER_SAMPLE_RATE as f64 / source_rate as f64)).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * source_rate as f64 / WHISPER_SAMPLE_RATE as f64;
        let idx = src_pos.floor() as usize;
        let frac = (src_pos - idx as f64) as f32;
        let a = input.get(idx).copied().unwrap_or(0.0);
        let b = input.get(idx + 1).copied().unwrap_or(a);
        out.push(a + (b - a) * frac);
    }
    out
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples.iter().map(|sample| sample * sample).sum::<f32>();
    (sum / samples.len() as f32).sqrt()
}

fn is_duplicate(previous: &str, current: &str) -> bool {
    if previous == current {
        return true;
    }
    previous.len() > 8 && current.len() > 8 && previous.ends_with(current)
}

fn resolve_output_path(config: &UserConfig, out_override: Option<String>) -> PathBuf {
    let file_name = out_override.unwrap_or_else(|| "meeting".to_string());
    let mut path = PathBuf::from(file_name);
    path.set_extension(config.output_format.extension());

    if path.is_absolute() {
        path
    } else {
        config.output_dir.join(path)
    }
}

fn write_transcript(
    path: &PathBuf,
    format: TranscriptFormat,
    lines: &[TranscriptLine],
) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("Failed to create {}", parent.display()))?;
    }

    let mut file =
        File::create(path).with_context(|| format!("Failed to create {}", path.display()))?;
    match format {
        TranscriptFormat::Md => {
            writeln!(file, "# Transcript")?;
            writeln!(file)?;
            for line in lines {
                writeln!(file, "- [{}] {}", line.timestamp, line.text)?;
            }
        }
        TranscriptFormat::Txt => {
            for line in lines {
                writeln!(file, "[{}] {}", line.timestamp, line.text)?;
            }
        }
    }
    Ok(())
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    format!("{:02}:{:02}", (secs / 60) % 60, secs % 60)
}
