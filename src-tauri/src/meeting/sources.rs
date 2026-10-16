//! The two sources of a call: the microphone, and the sound the call app plays.
//!
//! The call's sound comes from a Core Audio process tap of everything except
//! Murmela ([`super::macos_tap`]), which needs macOS 14.4 or later.

use std::any::Any;

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use serde::Serialize;

use super::recorder::{AudioSource, BlockSink};

/// Whether this computer can record the call's sound, and why not.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemAudioSupport {
    pub supported: bool,
    pub reason: Option<String>,
}

pub fn system_audio_support() -> SystemAudioSupport {
    match crate::model_metadata::macos_version() {
        Some((major, minor)) if (major, minor) < (14, 4) => SystemAudioSupport {
            supported: false,
            reason: Some(format!(
                "Recording a call's sound needs macOS 14.4 or later. This Mac runs {major}.{minor}."
            )),
        },
        _ => SystemAudioSupport { supported: true, reason: None },
    }
}

/// The microphone chosen in Settings, or the default one.
pub struct MicrophoneSource {
    preferred: Option<String>,
    name: String,
}

impl MicrophoneSource {
    pub fn new(preferred: Option<String>) -> Self {
        Self {
            preferred,
            name: "Microphone".into(),
        }
    }
}

impl AudioSource for MicrophoneSource {
    fn open(&mut self, sink: BlockSink) -> Result<Box<dyn Any>> {
        let host = cpal::default_host();
        let device = crate::audio_capture::select_input_device(&host, self.preferred.as_deref())?;
        self.name = crate::audio_capture::describe_device(&device);
        let config = device
            .default_input_config()
            .context("Could not read the microphone's format")?;
        let stream = open_stream(&device, config.sample_format(), &config.into(), sink)?;
        Ok(Box::new(stream))
    }

    fn describe(&self) -> String {
        self.name.clone()
    }
}

/// Everything the Mac plays, apart from Murmela itself.
pub struct SystemSource;

impl AudioSource for SystemSource {
    fn open(&mut self, sink: BlockSink) -> Result<Box<dyn Any>> {
        open_system(sink)
    }

    fn describe(&self) -> String {
        "Sound from other apps".into()
    }
}

fn open_system(sink: BlockSink) -> Result<Box<dyn Any>> {
    if !system_audio_support().supported {
        return Err(anyhow!(
            "SYSTEM_AUDIO_UNSUPPORTED: {}",
            system_audio_support().reason.unwrap_or_default()
        ));
    }
    let tap = super::macos_tap::SystemAudioTap::create()?;
    let host = cpal::default_host();
    let id = cpal::DeviceId(cpal::HostId::CoreAudio, tap.device_uid.clone());
    // A new aggregate device can take a moment to appear in the device list.
    let mut device = None;
    for _ in 0..20 {
        device = host.device_by_id(&id);
        if device.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let device = device.ok_or_else(|| {
        anyhow!(
            "SYSTEM_AUDIO_UNAVAILABLE: macOS created the call audio device but did not list it."
        )
    })?;
    let config = device
        .default_input_config()
        .context("Could not read the call audio format")?;
    let stream = open_stream(&device, config.sample_format(), &config.into(), sink)?;
    // The stream must stop before the tap goes away; tuple fields drop in order.
    Ok(Box::new((stream, tap)))
}

fn open_stream(
    device: &cpal::Device,
    format: SampleFormat,
    config: &StreamConfig,
    sink: BlockSink,
) -> Result<Stream> {
    let rate = config.sample_rate;
    let channels = config.channels;
    let error_sink = sink.clone();
    let on_error = move |error: cpal::StreamError| error_sink.fail(error.to_string());
    let stream = match format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            move |data: &[f32], _: &_| sink.send(data.to_vec(), rate, channels),
            on_error,
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            move |data: &[i16], _: &_| {
                sink.send(
                    data.iter().map(|v| *v as f32 / i16::MAX as f32).collect(),
                    rate,
                    channels,
                )
            },
            on_error,
            None,
        )?,
        SampleFormat::I32 => device.build_input_stream(
            config,
            move |data: &[i32], _: &_| {
                sink.send(
                    data.iter().map(|v| *v as f32 / i32::MAX as f32).collect(),
                    rate,
                    channels,
                )
            },
            on_error,
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            move |data: &[u16], _: &_| {
                sink.send(
                    data.iter()
                        .map(|v| (*v as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect(),
                    rate,
                    channels,
                )
            },
            on_error,
            None,
        )?,
        other => return Err(anyhow!("Unsupported audio sample format {other:?}")),
    };
    stream.play().context("Could not start the audio stream")?;
    Ok(stream)
}
