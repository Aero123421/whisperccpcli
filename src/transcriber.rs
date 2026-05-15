#![cfg_attr(
    not(feature = "whisper"),
    allow(dead_code, unused_imports, unused_variables)
)]

use crate::audio;
use crate::{
    app::{is_model_installed, model_by_name, AppPaths},
    settings::{TranscriptFormat, UserConfig},
};
use anyhow::{anyhow, Context, Result};
#[cfg(feature = "whisper")]
use crossbeam_channel::bounded;
use crossbeam_channel::{unbounded, Receiver, Sender};
use std::{
    fs::{self, File},
    io::{BufWriter, Seek, SeekFrom, Write},
    ops::Range,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
#[cfg(feature = "whisper")]
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;
const MIN_TRANSCRIBE_RMS: f32 = 0.006;
const STOP_FLUSH_MIN_SAMPLES: usize = WHISPER_SAMPLE_RATE as usize;

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
    AudioDropped(u64),
    Saved(PathBuf),
    Error(String),
}

#[derive(Clone, Debug)]
pub enum SessionCommand {
    SetPaused(bool),
    SaveNow,
    Stop,
}

#[cfg(feature = "whisper")]
enum InferenceCommand {
    Transcribe { audio: Vec<f32>, start_sample: u64 },
    SaveNow,
    Stop,
}

#[derive(Clone, Debug, Default)]
pub struct TranscriberStats {
    audio_callback: audio::AudioCallbackStats,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TranscriberStatsSnapshot {
    pub dropped_audio_chunks: u64,
    pub dropped_level_updates: u64,
}

impl TranscriberStats {
    pub fn snapshot(&self) -> TranscriberStatsSnapshot {
        let audio = self.audio_callback.snapshot();
        TranscriberStatsSnapshot {
            dropped_audio_chunks: audio.dropped_audio_chunks,
            dropped_level_updates: audio.dropped_level_updates,
        }
    }
}

pub struct SessionHandle {
    pub events: Receiver<SessionEvent>,
    pub controls: Sender<SessionCommand>,
    pub output_path: PathBuf,
    worker: Option<JoinHandle<()>>,
}

impl SessionHandle {
    pub fn stop_and_wait(&mut self) {
        let _ = self.controls.send(SessionCommand::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub fn start_session(
    paths: AppPaths,
    config: UserConfig,
    out_override: Option<String>,
    force_format_extension: bool,
) -> Result<SessionHandle> {
    let model =
        model_by_name(&config.model).ok_or_else(|| anyhow!("Unknown model '{}'", config.model))?;
    if !is_model_installed(&paths, model) {
        return Err(anyhow!("Model '{}' is not installed", model.name));
    }

    let output_path = resolve_output_path(&config, out_override, force_format_extension);
    let (event_tx, event_rx) = unbounded();
    let (command_tx, command_rx) = unbounded();
    let worker_paths = paths.clone();
    let worker_config = config.clone();
    let worker_output = output_path.clone();
    let stats = TranscriberStats::default();
    let worker_stats = stats.clone();

    let worker = thread::spawn(move || {
        if let Err(error) = run_worker(
            worker_paths,
            worker_config,
            worker_output,
            command_rx,
            event_tx.clone(),
            worker_stats,
        ) {
            let _ = event_tx.send(SessionEvent::Error(format!("{error:#}")));
            let _ = event_tx.send(SessionEvent::Status(SessionStatus::Error));
        }
    });

    Ok(SessionHandle {
        events: event_rx,
        controls: command_tx,
        output_path,
        worker: Some(worker),
    })
}

fn run_worker(
    paths: AppPaths,
    config: UserConfig,
    output_path: PathBuf,
    command_rx: Receiver<SessionCommand>,
    event_tx: Sender<SessionEvent>,
    stats: TranscriberStats,
) -> Result<()> {
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
        let (inference_tx, inference_rx) = unbounded::<InferenceCommand>();
        let (ready_tx, ready_rx) = bounded(1);
        let paused_state = Arc::new(AtomicBool::new(config.start_paused));
        let inference_paused_state = paused_state.clone();
        let inference_paths = paths.clone();
        let inference_config = config.clone();
        let inference_output_path = output_path.clone();
        let inference_event_tx = event_tx.clone();
        let inference_worker = thread::spawn(move || {
            if let Err(error) = run_inference_worker(
                inference_paths,
                inference_config,
                inference_output_path,
                inference_rx,
                inference_event_tx.clone(),
                inference_paused_state,
                ready_tx,
            ) {
                let _ = inference_event_tx.send(SessionEvent::Error(format!("{error:#}")));
                let _ = inference_event_tx.send(SessionEvent::Status(SessionStatus::Error));
            }
        });

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                let _ = inference_worker.join();
                return Err(anyhow!(message));
            }
            Err(_) => {
                let _ = inference_worker.join();
                return Err(anyhow!("Inference worker exited during startup"));
            }
        }

        let (audio_tx, audio_rx) = bounded::<Vec<f32>>(512);
        let (level_tx, level_rx) = bounded::<f32>(8);
        let (stream_error_tx, stream_error_rx) = bounded::<String>(8);
        let mut capture = Some(audio::start_capture(
            config.microphone.as_deref(),
            audio_tx,
            level_tx,
            stream_error_tx,
            stats.audio_callback.clone(),
        )?);
        let source_rate = capture
            .as_ref()
            .map(|capture| capture.sample_rate)
            .unwrap_or(WHISPER_SAMPLE_RATE);
        event_tx
            .send(SessionEvent::Microphone(
                capture
                    .as_ref()
                    .map(|capture| capture.device_name.clone())
                    .unwrap_or_else(|| "Microphone".to_string()),
            ))
            .ok();

        let mut paused = config.start_paused;
        let mut pending = Vec::<f32>::new();
        let mut pending_start_sample = 0_u64;
        let mut last_reported_audio_drops = 0_u64;
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
                        paused_state.store(paused, Ordering::Relaxed);
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
                        inference_tx
                            .send(InferenceCommand::SaveNow)
                            .context("Inference worker stopped before saving")?;
                    }
                    SessionCommand::Stop => {
                        event_tx
                            .send(SessionEvent::Status(SessionStatus::Saving))
                            .ok();
                        capture.take();
                        if !paused {
                            drain_audio_queue(&audio_rx, &mut pending, source_rate);
                        }
                        queue_ready_chunks(
                            &mut pending,
                            &mut pending_start_sample,
                            chunk_samples,
                            &inference_tx,
                        )?;
                        if pending.len() >= STOP_FLUSH_MIN_SAMPLES {
                            let audio_chunk = std::mem::take(&mut pending);
                            let chunk_start_sample = pending_start_sample;
                            queue_audio_chunk(&inference_tx, audio_chunk, chunk_start_sample)?;
                        }
                        inference_tx
                            .send(InferenceCommand::Stop)
                            .context("Inference worker stopped before finalizing")?;
                        inference_worker
                            .join()
                            .map_err(|_| anyhow!("Inference worker panicked"))?;
                        report_audio_drop_stats(&stats, &event_tx, &mut last_reported_audio_drops);
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
            report_audio_drop_stats(&stats, &event_tx, &mut last_reported_audio_drops);

            let Ok(chunk) = audio_rx.recv_timeout(Duration::from_millis(80)) else {
                continue;
            };

            if paused {
                continue;
            }

            pending.extend(resample_to_16k(&chunk, source_rate));
            drain_audio_queue(&audio_rx, &mut pending, source_rate);
            queue_ready_chunks(
                &mut pending,
                &mut pending_start_sample,
                chunk_samples,
                &inference_tx,
            )?;
        }
    }
}

#[cfg(feature = "whisper")]
fn run_inference_worker(
    paths: AppPaths,
    config: UserConfig,
    output_path: PathBuf,
    command_rx: Receiver<InferenceCommand>,
    event_tx: Sender<SessionEvent>,
    paused_state: Arc<AtomicBool>,
    ready_tx: Sender<std::result::Result<(), String>>,
) -> Result<()> {
    let model =
        model_by_name(&config.model).ok_or_else(|| anyhow!("Unknown model '{}'", config.model))?;
    let model_path = paths.model_path(model);
    let context =
        match WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
            .with_context(|| format!("Failed to load model {}", model_path.display()))
        {
            Ok(context) => context,
            Err(error) => {
                let _ = ready_tx.send(Err(format!("{error:#}")));
                return Err(error);
            }
        };
    let mut state = match context
        .create_state()
        .context("Failed to create whisper state")
    {
        Ok(state) => state,
        Err(error) => {
            let _ = ready_tx.send(Err(format!("{error:#}")));
            return Err(error);
        }
    };
    let _ = ready_tx.send(Ok(()));

    let mut lines = Vec::<TranscriptLine>::new();
    let mut writer = TranscriptWriter::new(output_path.clone(), config.output_format);
    let mut last_text = String::new();

    while let Ok(command) = command_rx.recv() {
        match command {
            InferenceCommand::Transcribe {
                audio,
                start_sample,
            } => {
                process_audio_chunk(
                    &mut state,
                    &config,
                    &output_path,
                    &event_tx,
                    &mut writer,
                    &mut lines,
                    &mut last_text,
                    &audio,
                    start_sample,
                )?;
                event_tx
                    .send(SessionEvent::Status(current_session_status(&paused_state)))
                    .ok();
            }
            InferenceCommand::SaveNow => {
                writer.flush()?;
                event_tx.send(SessionEvent::Saved(output_path.clone())).ok();
                event_tx
                    .send(SessionEvent::Status(current_session_status(&paused_state)))
                    .ok();
            }
            InferenceCommand::Stop => {
                writer.flush()?;
                event_tx.send(SessionEvent::Saved(output_path.clone())).ok();
                event_tx
                    .send(SessionEvent::Status(SessionStatus::Stopped))
                    .ok();
                return Ok(());
            }
        }
    }

    Ok(())
}

#[cfg(feature = "whisper")]
fn current_session_status(paused_state: &AtomicBool) -> SessionStatus {
    if paused_state.load(Ordering::Relaxed) {
        SessionStatus::Paused
    } else {
        SessionStatus::Listening
    }
}

#[cfg(feature = "whisper")]
fn report_audio_drop_stats(
    stats: &TranscriberStats,
    event_tx: &Sender<SessionEvent>,
    last_reported_audio_drops: &mut u64,
) {
    let dropped_audio_chunks = stats.snapshot().dropped_audio_chunks;
    if dropped_audio_chunks > *last_reported_audio_drops {
        *last_reported_audio_drops = dropped_audio_chunks;
        event_tx
            .send(SessionEvent::AudioDropped(dropped_audio_chunks))
            .ok();
    }
}

#[cfg(feature = "whisper")]
fn drain_audio_queue(audio_rx: &Receiver<Vec<f32>>, pending: &mut Vec<f32>, source_rate: u32) {
    while let Ok(chunk) = audio_rx.try_recv() {
        pending.extend(resample_to_16k(&chunk, source_rate));
    }
}

#[cfg(feature = "whisper")]
fn queue_ready_chunks(
    pending: &mut Vec<f32>,
    pending_start_sample: &mut u64,
    chunk_samples: usize,
    inference_tx: &Sender<InferenceCommand>,
) -> Result<()> {
    while pending.len() >= chunk_samples {
        let rest = pending.split_off(chunk_samples);
        let audio_chunk = std::mem::replace(pending, rest);
        let chunk_start_sample = *pending_start_sample;
        *pending_start_sample += audio_chunk.len() as u64;
        queue_audio_chunk(inference_tx, audio_chunk, chunk_start_sample)?;
    }
    Ok(())
}

#[cfg(feature = "whisper")]
fn queue_audio_chunk(
    inference_tx: &Sender<InferenceCommand>,
    audio_chunk: Vec<f32>,
    chunk_start_sample: u64,
) -> Result<()> {
    if rms(&audio_chunk) < MIN_TRANSCRIBE_RMS {
        return Ok(());
    }
    inference_tx
        .send(InferenceCommand::Transcribe {
            audio: audio_chunk,
            start_sample: chunk_start_sample,
        })
        .context("Inference worker stopped while queueing audio")
}

#[cfg(feature = "whisper")]
#[allow(clippy::too_many_arguments)]
fn process_audio_chunk(
    state: &mut whisper_rs::WhisperState,
    config: &UserConfig,
    output_path: &Path,
    event_tx: &Sender<SessionEvent>,
    writer: &mut TranscriptWriter,
    lines: &mut Vec<TranscriptLine>,
    last_text: &mut String,
    audio_chunk: &[f32],
    chunk_start_sample: u64,
) -> Result<()> {
    if rms(audio_chunk) < MIN_TRANSCRIBE_RMS {
        return Ok(());
    }

    event_tx
        .send(SessionEvent::Status(SessionStatus::Processing))
        .ok();
    match transcribe_chunk(state, config, audio_chunk, last_text.as_str()) {
        Ok(text) => {
            let text = text.trim().to_string();
            if !text.is_empty() && !is_duplicate(last_text, &text) {
                *last_text = text.clone();
                let line = TranscriptLine {
                    timestamp: format_sample_timestamp(chunk_start_sample),
                    text,
                };
                lines.push(line.clone());
                writer.append_line(&line)?;
                writer.flush()?;
                event_tx.send(SessionEvent::Segment(line)).ok();
                event_tx
                    .send(SessionEvent::Saved(output_path.to_path_buf()))
                    .ok();
            }
        }
        Err(error) => {
            event_tx
                .send(SessionEvent::Error(format!("{error:#}")))
                .ok();
        }
    }
    Ok(())
}

#[cfg(feature = "whisper")]
fn transcribe_chunk(
    state: &mut whisper_rs::WhisperState,
    config: &UserConfig,
    samples: &[f32],
    initial_prompt: &str,
) -> Result<String> {
    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
    let language = config.language.trim();
    if language.eq_ignore_ascii_case("auto") || language.is_empty() {
        params.set_language(None);
    } else {
        params.set_language(Some(language));
    }
    params.set_n_threads(config.threads as i32);
    params.set_translate(false);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_timestamps(true);
    params.set_single_segment(false);
    params.set_no_context(false);
    let initial_prompt = initial_prompt.trim();
    if !initial_prompt.is_empty() && !initial_prompt.contains('\0') {
        params.set_initial_prompt(initial_prompt);
    }

    state
        .full(params, samples)
        .context("Whisper inference failed")?;

    let mut text = String::new();
    for segment in state.as_iter() {
        append_segment_text(&mut text, segment.to_string().trim());
    }
    Ok(text)
}

fn append_segment_text(output: &mut String, segment: &str) {
    if segment.is_empty() {
        return;
    }
    if output
        .chars()
        .last()
        .zip(segment.chars().next())
        .map(|(left, right)| left.is_ascii_alphanumeric() && right.is_ascii_alphanumeric())
        .unwrap_or(false)
    {
        output.push(' ');
    }
    output.push_str(segment);
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

pub(crate) fn transcribe_wav_file(
    path: PathBuf,
    config: UserConfig,
    output_path: PathBuf,
) -> Result<Vec<TranscriptLine>> {
    #[cfg(not(feature = "whisper"))]
    {
        let _ = (path, config, output_path);
        anyhow::bail!("This binary was built without whisper support");
    }

    #[cfg(feature = "whisper")]
    {
        let samples = read_wav_mono(&path)?;
        let mut writer = TranscriptWriter::new(output_path, config.output_format);
        let mut lines = Vec::<TranscriptLine>::new();
        let mut last_text = String::new();
        let chunk_samples = (config.chunk_seconds.max(2) as usize) * WHISPER_SAMPLE_RATE as usize;
        let paths = AppPaths::new()?;
        let model = model_by_name(&config.model)
            .ok_or_else(|| anyhow!("Unknown model '{}'", config.model))?;
        let model_path = paths.model_path(model);
        let context =
            WhisperContext::new_with_params(&model_path, WhisperContextParameters::default())
                .with_context(|| format!("Failed to load model {}", model_path.display()))?;
        let mut state = context
            .create_state()
            .context("Failed to create whisper state")?;

        for (index, audio_chunk) in samples.chunks(chunk_samples).enumerate() {
            if audio_chunk.len() < STOP_FLUSH_MIN_SAMPLES || rms(audio_chunk) < MIN_TRANSCRIBE_RMS {
                continue;
            }
            let text = transcribe_chunk(&mut state, &config, audio_chunk, &last_text)?
                .trim()
                .to_string();
            if text.is_empty() || is_duplicate(&last_text, &text) {
                continue;
            }
            last_text = text.clone();
            let line = TranscriptLine {
                timestamp: format_sample_timestamp((index * chunk_samples) as u64),
                text,
            };
            writer.append_line(&line)?;
            lines.push(line);
        }
        writer.flush()?;
        Ok(lines)
    }
}

fn read_wav_mono(path: &PathBuf) -> Result<Vec<f32>> {
    let bytes = fs::read(path).with_context(|| format!("Failed to read {}", path.display()))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        anyhow::bail!("Only RIFF/WAVE audio files are currently supported");
    }

    let mut cursor = 12_usize;
    let mut format_tag = 0_u16;
    let mut channels = 0_u16;
    let mut sample_rate = 0_u32;
    let mut bits_per_sample = 0_u16;
    let mut data_range: Option<Range<usize>> = None;

    while cursor + 8 <= bytes.len() {
        let id = &bytes[cursor..cursor + 4];
        let size = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap()) as usize;
        cursor += 8;
        if cursor + size > bytes.len() {
            anyhow::bail!("Invalid WAV chunk size in {}", path.display());
        }

        match id {
            b"fmt " => {
                if size < 16 {
                    anyhow::bail!("Invalid WAV fmt chunk in {}", path.display());
                }
                format_tag = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
                channels = u16::from_le_bytes(bytes[cursor + 2..cursor + 4].try_into().unwrap());
                sample_rate = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().unwrap());
                bits_per_sample =
                    u16::from_le_bytes(bytes[cursor + 14..cursor + 16].try_into().unwrap());
            }
            b"data" => data_range = Some(cursor..cursor + size),
            _ => {}
        }
        cursor += size + (size % 2);
    }

    let data_range = data_range.ok_or_else(|| anyhow!("WAV file has no data chunk"))?;
    if channels == 0 {
        anyhow::bail!("WAV file reports zero channels");
    }

    let channel_count = channels as usize;
    let mut mono = Vec::new();
    match (format_tag, bits_per_sample) {
        (1, 16) => {
            let frame_bytes = channel_count * 2;
            for frame in bytes[data_range].chunks_exact(frame_bytes) {
                let mut sum = 0.0_f32;
                for channel in 0..channel_count {
                    let offset = channel * 2;
                    let sample = i16::from_le_bytes([frame[offset], frame[offset + 1]]) as f32
                        / i16::MAX as f32;
                    sum += sample;
                }
                mono.push(sum / channel_count as f32);
            }
        }
        (3, 32) => {
            let frame_bytes = channel_count * 4;
            for frame in bytes[data_range].chunks_exact(frame_bytes) {
                let mut sum = 0.0_f32;
                for channel in 0..channel_count {
                    let offset = channel * 4;
                    let sample = f32::from_le_bytes([
                        frame[offset],
                        frame[offset + 1],
                        frame[offset + 2],
                        frame[offset + 3],
                    ]);
                    sum += sample.clamp(-1.0, 1.0);
                }
                mono.push(sum / channel_count as f32);
            }
        }
        _ => anyhow::bail!(
            "Unsupported WAV format: format {format_tag}, {bits_per_sample} bits. Use PCM 16-bit or float 32-bit WAV."
        ),
    }

    Ok(resample_to_16k(&mono, sample_rate))
}

fn rms(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum = samples.iter().map(|sample| sample * sample).sum::<f32>();
    (sum / samples.len() as f32).sqrt()
}

pub(crate) fn is_duplicate(previous: &str, current: &str) -> bool {
    let previous = normalize_transcript_text(previous);
    let current = normalize_transcript_text(current);
    if previous.is_empty() || current.is_empty() {
        return false;
    }
    if previous == current {
        return true;
    }

    let previous_chars = previous.chars().count();
    let current_chars = current.chars().count();
    let min_chars = previous_chars.min(current_chars);
    let threshold = duplicate_threshold_chars(&previous, &current);
    if min_chars < threshold {
        return false;
    }

    if previous.contains(&current) || current.contains(&previous) {
        return true;
    }

    let overlap =
        suffix_prefix_overlap(&previous, &current).max(suffix_prefix_overlap(&current, &previous));
    overlap >= threshold && overlap * 100 >= min_chars * 80
}

fn normalize_transcript_text(text: &str) -> String {
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|ch| !ch.is_whitespace() && !is_transcript_punctuation(*ch))
        .collect()
}

fn is_transcript_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation()
        || matches!(
            ch,
            '、' | '。'
                | '，'
                | '．'
                | '！'
                | '？'
                | '：'
                | '；'
                | '「'
                | '」'
                | '『'
                | '』'
                | '（'
                | '）'
                | '［'
                | '］'
                | '｛'
                | '｝'
                | '【'
                | '】'
                | '・'
                | '…'
        )
}

fn duplicate_threshold_chars(previous: &str, current: &str) -> usize {
    if previous.is_ascii() && current.is_ascii() {
        8
    } else {
        4
    }
}

fn suffix_prefix_overlap(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let max_len = left.len().min(right.len());
    for len in (1..=max_len).rev() {
        if left[left.len() - len..] == right[..len] {
            return len;
        }
    }
    0
}

fn resolve_output_path(
    config: &UserConfig,
    out_override: Option<String>,
    force_format_extension: bool,
) -> PathBuf {
    let file_name = out_override.unwrap_or_else(|| format!("meeting-{}", timestamp_for_file()));
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

struct TranscriptWriter {
    path: PathBuf,
    format: TranscriptFormat,
    file: Option<BufWriter<File>>,
    segments_written: usize,
}

impl TranscriptWriter {
    fn new(path: PathBuf, format: TranscriptFormat) -> Self {
        Self {
            path,
            format,
            file: None,
            segments_written: 0,
        }
    }

    fn append_line(&mut self, line: &TranscriptLine) -> Result<()> {
        let format = self.format;
        let segment_index = self.segments_written + 1;
        let json = if matches!(format, TranscriptFormat::Json | TranscriptFormat::Jsonl) {
            Some(transcript_line_json(line)?)
        } else {
            None
        };
        let file = self.ensure_open()?;
        match format {
            TranscriptFormat::Md => writeln!(file, "- [{}] {}", line.timestamp, line.text)?,
            TranscriptFormat::Txt => writeln!(file, "[{}] {}", line.timestamp, line.text)?,
            TranscriptFormat::Srt => write_srt_segment(file, segment_index, line)?,
            TranscriptFormat::Json => {
                file.flush()?;
                file.seek(SeekFrom::End(-2))?;
                if segment_index > 1 {
                    writeln!(file, ",")?;
                }
                writeln!(
                    file,
                    "  {}",
                    json.expect("json transcript line must be encoded")
                )?;
                writeln!(file, "]")?;
            }
            TranscriptFormat::Jsonl => writeln!(
                file,
                "{}",
                json.expect("json transcript line must be encoded")
            )?,
        }
        self.segments_written = segment_index;
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.ensure_open()?.flush()?;
        Ok(())
    }

    fn ensure_open(&mut self) -> Result<&mut BufWriter<File>> {
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
            let file = File::create(&self.path)
                .with_context(|| format!("Failed to create {}", self.path.display()))?;
            let mut writer = BufWriter::new(file);
            match self.format {
                TranscriptFormat::Md => {
                    writeln!(writer, "# Transcript")?;
                    writeln!(writer)?;
                }
                TranscriptFormat::Json => {
                    writeln!(writer, "[")?;
                    writeln!(writer, "]")?;
                }
                TranscriptFormat::Txt | TranscriptFormat::Srt | TranscriptFormat::Jsonl => {}
            }
            self.file = Some(writer);
        }
        Ok(self.file.as_mut().expect("transcript writer must be open"))
    }
}

fn format_sample_timestamp(sample_index: u64) -> String {
    let millis = sample_index.saturating_mul(1_000) / WHISPER_SAMPLE_RATE as u64;
    format_elapsed(Duration::from_millis(millis))
}

fn transcript_line_json(line: &TranscriptLine) -> Result<String> {
    serde_json::to_string(&serde_json::json!({
        "timestamp": line.timestamp,
        "text": line.text,
    }))
    .context("Failed to encode transcript line as JSON")
}

fn write_srt_segment(
    file: &mut BufWriter<File>,
    segment_index: usize,
    line: &TranscriptLine,
) -> Result<()> {
    let start = parse_timestamp_seconds(&line.timestamp).unwrap_or(0);
    let end = start + 3;
    writeln!(file, "{segment_index}")?;
    writeln!(file, "{} --> {}", srt_time(start), srt_time(end))?;
    writeln!(file, "{}", line.text)?;
    writeln!(file)?;
    Ok(())
}

fn parse_timestamp_seconds(timestamp: &str) -> Option<u64> {
    let parts = timestamp
        .split(':')
        .map(str::parse::<u64>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    if parts.len() != 3 {
        return None;
    }
    Some(parts[0] * 3600 + parts[1] * 60 + parts[2])
}

fn srt_time(seconds: u64) -> String {
    format!(
        "{:02}:{:02}:{:02},000",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    )
}

fn format_elapsed(duration: Duration) -> String {
    let secs = duration.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn timestamp_for_file() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    millis.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_ignores_case_spacing_and_punctuation() {
        assert!(is_duplicate("Hello, world!", "hello world"));
        assert!(is_duplicate("これはテストです。", "これはテストです"));
    }

    #[test]
    fn duplicate_detects_contained_transcript_text() {
        assert!(is_duplicate(
            "today we discussed the project timeline",
            "project timeline"
        ));
    }

    #[test]
    fn duplicate_keeps_short_or_different_text() {
        assert!(!is_duplicate("", ""));
        assert!(!is_duplicate("yes", "no"));
        assert!(!is_duplicate("alpha beta gamma", "gamma delta"));
    }

    #[test]
    fn sample_timestamp_uses_chunk_start_offset() {
        assert_eq!(
            format_sample_timestamp(WHISPER_SAMPLE_RATE as u64 * 65),
            "00:01:05"
        );
    }

    #[test]
    fn append_segment_text_keeps_words_readable() {
        let mut output = String::new();
        append_segment_text(&mut output, "hello");
        append_segment_text(&mut output, "world");
        assert_eq!(output, "hello world");

        let mut output = String::new();
        append_segment_text(&mut output, "これは");
        append_segment_text(&mut output, "テストです");
        assert_eq!(output, "これはテストです");
    }

    #[test]
    fn resolve_output_path_respects_explicit_extension() {
        let paths = AppPaths {
            root: PathBuf::from("root"),
            bin: PathBuf::from("bin"),
            models: PathBuf::from("models"),
            transcripts: PathBuf::from("transcripts"),
            logs: PathBuf::from("logs"),
            config: PathBuf::from("config.toml"),
        };
        let mut config = UserConfig::default_for(&paths);
        config.output_dir = PathBuf::from("out");
        config.output_format = TranscriptFormat::Md;

        let path = resolve_output_path(&config, Some("meeting.txt".to_string()), false);
        assert_eq!(path, PathBuf::from("out").join("meeting.txt"));

        let path = resolve_output_path(&config, Some("meeting.txt".to_string()), true);
        assert_eq!(path, PathBuf::from("out").join("meeting.md"));
    }

    #[test]
    fn srt_time_formats_seconds() {
        assert_eq!(srt_time(3_661), "01:01:01,000");
    }
}
