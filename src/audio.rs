#![allow(deprecated)]
#![cfg_attr(not(feature = "whisper"), allow(dead_code))]

use anyhow::{anyhow, bail, Context, Result};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, Stream, StreamConfig,
};
use crossbeam_channel::Sender;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

#[derive(Clone, Debug)]
pub struct AudioDeviceInfo {
    pub index: usize,
    pub name: String,
    pub is_default: bool,
    pub config: String,
}

pub struct AudioCapture {
    pub _stream: Stream,
    pub sample_rate: u32,
    pub device_name: String,
}

#[derive(Clone)]
struct CaptureSenders {
    audio_tx: Sender<Vec<f32>>,
    level_tx: Sender<f32>,
    error_tx: Sender<String>,
    stats: AudioCallbackStats,
}

#[derive(Clone, Debug)]
pub struct AudioCallbackStats {
    dropped_audio_chunks: Arc<AtomicU64>,
    dropped_level_updates: Arc<AtomicU64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AudioCallbackStatsSnapshot {
    pub dropped_audio_chunks: u64,
    pub dropped_level_updates: u64,
}

impl AudioCallbackStats {
    pub fn snapshot(&self) -> AudioCallbackStatsSnapshot {
        AudioCallbackStatsSnapshot {
            dropped_audio_chunks: self.dropped_audio_chunks.load(Ordering::Relaxed),
            dropped_level_updates: self.dropped_level_updates.load(Ordering::Relaxed),
        }
    }

    fn record_dropped_audio_chunk(&self) {
        self.dropped_audio_chunks.fetch_add(1, Ordering::Relaxed);
    }

    fn record_dropped_level_update(&self) {
        self.dropped_level_updates.fetch_add(1, Ordering::Relaxed);
    }
}

impl Default for AudioCallbackStats {
    fn default() -> Self {
        Self {
            dropped_audio_chunks: Arc::new(AtomicU64::new(0)),
            dropped_level_updates: Arc::new(AtomicU64::new(0)),
        }
    }
}

pub fn input_devices() -> Result<Vec<AudioDeviceInfo>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());
    let devices = host
        .input_devices()
        .context("Could not enumerate input devices")?;

    let mut out = Vec::new();
    let mut default_seen = false;
    for (index, device) in devices.enumerate() {
        let name = device
            .name()
            .unwrap_or_else(|_| format!("Input device {}", index + 1));
        let config = device
            .default_input_config()
            .map(|config| {
                format!(
                    "{} Hz, {} ch, {:?}",
                    config.sample_rate(),
                    config.channels(),
                    config.sample_format()
                )
            })
            .unwrap_or_else(|_| "unavailable".to_string());
        let is_default = !default_seen
            && default_name
                .as_ref()
                .map(|default| default == &name)
                .unwrap_or(false);
        default_seen |= is_default;
        out.push(AudioDeviceInfo {
            index,
            name,
            is_default,
            config,
        });
    }
    Ok(out)
}

pub fn start_capture(
    selected_name: Option<&str>,
    audio_tx: Sender<Vec<f32>>,
    level_tx: Sender<f32>,
    error_tx: Sender<String>,
    stats: AudioCallbackStats,
) -> Result<AudioCapture> {
    let host = cpal::default_host();
    let device = match selected_name {
        Some(name) => host
            .input_devices()
            .context("Could not enumerate input devices")?
            .find(|device| {
                device
                    .name()
                    .map(|device_name| device_name == name)
                    .unwrap_or(false)
            })
            .ok_or_else(|| anyhow!("Selected microphone was not found: {name}"))?,
        None => host
            .default_input_device()
            .ok_or_else(|| anyhow!("No default microphone is available"))?,
    };

    let device_name = device.name().unwrap_or_else(|_| "Microphone".to_string());
    let supported_config = device
        .default_input_config()
        .with_context(|| format!("Could not read default input config for {device_name}"))?;
    let sample_format = supported_config.sample_format();
    let sample_rate = supported_config.sample_rate();
    let channels = supported_config.channels() as usize;
    let config: StreamConfig = supported_config.into();
    let senders = CaptureSenders {
        audio_tx,
        level_tx,
        error_tx,
        stats,
    };

    let stream = match sample_format {
        SampleFormat::F32 => build_stream(&device, &config, channels, senders, |v: f32| v),
        SampleFormat::F64 => build_stream(&device, &config, channels, senders, |v: f64| v as f32),
        SampleFormat::I8 => build_stream(&device, &config, channels, senders, |v: i8| {
            v as f32 / i8::MAX as f32
        }),
        SampleFormat::I16 => build_stream(&device, &config, channels, senders, |v: i16| {
            v as f32 / i16::MAX as f32
        }),
        SampleFormat::I32 => build_stream(&device, &config, channels, senders, |v: i32| {
            v as f32 / i32::MAX as f32
        }),
        SampleFormat::U8 => build_stream(&device, &config, channels, senders, |v: u8| {
            (v as f32 / u8::MAX as f32) * 2.0 - 1.0
        }),
        SampleFormat::U16 => build_stream(&device, &config, channels, senders, |v: u16| {
            (v as f32 / u16::MAX as f32) * 2.0 - 1.0
        }),
        SampleFormat::U32 => build_stream(&device, &config, channels, senders, |v: u32| {
            (v as f32 / u32::MAX as f32) * 2.0 - 1.0
        }),
        other => bail!("Unsupported input sample format: {other:?}"),
    }?;

    stream
        .play()
        .with_context(|| format!("Could not start microphone stream for {device_name}"))?;

    Ok(AudioCapture {
        _stream: stream,
        sample_rate,
        device_name,
    })
}

fn build_stream<T, F>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    senders: CaptureSenders,
    convert: F,
) -> Result<Stream>
where
    T: cpal::SizedSample + Copy + Send + 'static,
    F: Fn(T) -> f32 + Copy + Send + 'static,
{
    let err_sender = senders.error_tx.clone();
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            if channels == 0 {
                return;
            }

            let mut mono = Vec::with_capacity(data.len() / channels);
            let mut peak = 0.0_f32;
            for frame in data.chunks(channels) {
                let sample = frame.iter().copied().map(convert).sum::<f32>() / channels as f32;
                peak = peak.max(sample.abs());
                mono.push(sample.clamp(-1.0, 1.0));
            }

            if senders.level_tx.try_send(peak.min(1.0)).is_err() {
                senders.stats.record_dropped_level_update();
            }
            if senders.audio_tx.try_send(mono).is_err() {
                senders.stats.record_dropped_audio_chunk();
            }
        },
        move |err| {
            let _ = err_sender.try_send(format!("Microphone stream error: {err}"));
        },
        None,
    )?;
    Ok(stream)
}
