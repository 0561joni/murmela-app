use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
#[cfg(test)]
use std::io::Cursor;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tauri::Manager;

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

#[cfg(test)]
const LISTEN_START_WAV: &[u8] = include_bytes!("../assets/sounds/listen_start.wav");
#[cfg(test)]
const LISTEN_STOP_WAV: &[u8] = include_bytes!("../assets/sounds/listen_stop.wav");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackCue {
    Start,
    Stop,
    Complete,
    Error,
}

enum SoundCommand {
    Shutdown,
    Play(FeedbackCue),
    PrepareCapture(bool, SyncSender<()>),
}

#[derive(Default)]
struct FeedbackPolicy {
    capturing: bool,
    capture_cues: bool,
    seen: HashSet<String>,
    order: VecDeque<String>,
    last_outcome: Option<Instant>,
}

impl FeedbackPolicy {
    fn accept_outcome(&mut self, key: &str, enabled: bool, now: Instant) -> bool {
        if !self.seen.insert(key.to_string()) {
            return false;
        }
        self.order.push_back(key.to_string());
        if self.order.len() > 2048 {
            if let Some(old) = self.order.pop_front() {
                self.seen.remove(&old);
            }
        }
        if !enabled || self.capturing {
            return false;
        }
        if self
            .last_outcome
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(750))
        {
            return false;
        }
        self.last_outcome = Some(now);
        true
    }
}

pub struct SoundPlayer {
    tx: Sender<SoundCommand>,
    policy: Mutex<FeedbackPolicy>,
    playback: Arc<Mutex<PlaybackState>>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl SoundPlayer {
    pub fn new() -> Result<Self> {
        let playback = Arc::new(Mutex::new(PlaybackState::new()));
        let thread_playback = Arc::clone(&playback);
        let (tx, rx) = mpsc::channel::<SoundCommand>();
        let (init_tx, init_rx) = mpsc::sync_channel::<Result<(), String>>(0);
        let thread = thread::Builder::new()
            .name("murmela-sound".into())
            .spawn(move || run_sound_thread(rx, init_tx, thread_playback))
            .map_err(|err| anyhow!("failed to spawn sound thread: {err}"))?;
        match init_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                tx,
                policy: Mutex::new(FeedbackPolicy::default()),
                playback,
                thread: Mutex::new(Some(thread)),
            }),
            Ok(Err(msg)) => Err(anyhow!("sound init failed: {msg}")),
            Err(_) => Err(anyhow!("sound thread exited before init")),
        }
    }

    pub fn shutdown(&self) {
        let _ = self.tx.send(SoundCommand::Shutdown);
        if let Some(thread) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = thread.join();
        }
    }

    /// Drain the boundary cue before opening capture. No feedback is queued
    /// while a microphone is open, including during a microphone test.
    pub fn prepare_capture(&self, enabled: bool) -> Result<()> {
        if let Ok(mut policy) = self.policy.lock() {
            if policy.capturing {
                return Err(anyhow!("A recording is already active."));
            }
            policy.capturing = true;
            policy.capture_cues = enabled;
        }
        if let Ok(mut playback) = self.playback.lock() {
            playback.muted = false;
        }
        let (tx, rx) = mpsc::sync_channel(1);
        if self
            .tx
            .send(SoundCommand::PrepareCapture(enabled, tx))
            .is_ok()
        {
            let _ = rx.recv_timeout(Duration::from_millis(800));
        }
        // Also gate the output callback: a failed audio driver must never play
        // a delayed boundary cue into an already-open microphone.
        if let Ok(mut playback) = self.playback.lock() {
            playback.muted = true;
            playback.samples.clear();
            playback.position = 0;
            playback.completion.take();
        }
        Ok(())
    }

    /// Call only after capture has stopped, so the stop tone cannot enter audio.
    pub fn finish_capture(&self, enabled: bool, canceled: bool) {
        if let Ok(mut policy) = self.policy.lock() {
            let play = policy.capturing && policy.capture_cues && enabled && !canceled;
            policy.capturing = false;
            policy.capture_cues = false;
            if let Ok(mut playback) = self.playback.lock() {
                playback.samples.clear();
                playback.position = 0;
                playback.muted = false;
            }
            if play {
                let _ = self.tx.send(SoundCommand::Play(FeedbackCue::Stop));
            }
        }
    }

    pub fn outcome(&self, cue: FeedbackCue, key: &str, enabled: bool) {
        if let Ok(mut policy) = self.policy.lock() {
            if policy.accept_outcome(key, enabled, Instant::now()) {
                let _ = self.tx.send(SoundCommand::Play(cue));
            }
        }
    }

    pub fn preview(&self, cue: FeedbackCue) -> Result<()> {
        let policy = self
            .policy
            .lock()
            .map_err(|_| anyhow!("Sound feedback unavailable"))?;
        if policy.capturing {
            return Err(anyhow!("Stop recording before previewing sounds."));
        }
        self.tx
            .send(SoundCommand::Play(cue))
            .map_err(|_| anyhow!("Sound output unavailable"))
    }
}

pub fn notify(app: &tauri::AppHandle, cue: FeedbackCue, key: &str) {
    if crate::shutdown::is_shutting_down() {
        return;
    }
    let Some(state) = app.try_state::<crate::app_state::AppState>() else {
        return;
    };
    let enabled = crate::storage::get_settings_from_db_path(&state.db_path)
        .map(|settings| settings.sounds_enabled)
        .unwrap_or(false);
    let capturing = state
        .recording_controller
        .status()
        .map(|status| {
            matches!(
                status.state,
                crate::audio_capture::RecordingOverlayState::Listening
            )
        })
        .unwrap_or(true);
    if let Some(player) = state.sound_player.as_ref().as_ref() {
        player.outcome(cue, key, enabled && !capturing);
    }
}

struct PlaybackState {
    samples: Vec<f32>,
    position: usize,
    completion: Option<SyncSender<()>>,
    muted: bool,
}

impl PlaybackState {
    fn new() -> Self {
        Self {
            samples: Vec::new(),
            position: 0,
            completion: None,
            muted: false,
        }
    }

    fn enqueue(&mut self, new_samples: &[f32]) {
        if self.position >= self.samples.len() {
            self.samples.clear();
            self.position = 0;
        }
        self.samples.extend_from_slice(new_samples);
    }

    fn pull(&mut self) -> Option<f32> {
        if self.muted {
            return None;
        }
        if self.position < self.samples.len() {
            let s = self.samples[self.position];
            self.position += 1;
            Some(s)
        } else {
            if let Some(done) = self.completion.take() {
                let _ = done.try_send(());
            }
            None
        }
    }
}

fn run_sound_thread(
    rx: mpsc::Receiver<SoundCommand>,
    init_tx: SyncSender<Result<(), String>>,
    state: Arc<Mutex<PlaybackState>>,
) {
    let host = cpal::default_host();
    let device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = init_tx.send(Err("no default output device".into()));
            return;
        }
    };

    let supported = match device.default_output_config() {
        Ok(c) => c,
        Err(err) => {
            let _ = init_tx.send(Err(format!("default_output_config: {err}")));
            return;
        }
    };

    let device_sample_rate = supported.sample_rate();
    let device_channels = supported.channels() as usize;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();

    let start_samples = cue_samples(FeedbackCue::Start, device_sample_rate);
    let stop_samples = cue_samples(FeedbackCue::Stop, device_sample_rate);
    let complete_samples = cue_samples(FeedbackCue::Complete, device_sample_rate);
    let error_samples = cue_samples(FeedbackCue::Error, device_sample_rate);

    let stream_state = Arc::clone(&state);

    let err_fn = |err| eprintln!("[sound] stream error: {err}");

    let stream_result: Result<Stream, cpal::BuildStreamError> = match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            &config,
            move |output: &mut [f32], _| {
                fill_f32(output, device_channels, &stream_state);
            },
            err_fn,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            &config,
            move |output: &mut [i16], _| {
                fill_i16(output, device_channels, &stream_state);
            },
            err_fn,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            &config,
            move |output: &mut [u16], _| {
                fill_u16(output, device_channels, &stream_state);
            },
            err_fn,
            None,
        ),
        other => {
            let _ = init_tx.send(Err(format!("unsupported sample format: {other:?}")));
            return;
        }
    };

    let stream = match stream_result {
        Ok(s) => s,
        Err(err) => {
            let _ = init_tx.send(Err(format!("build_output_stream: {err}")));
            return;
        }
    };

    if let Err(err) = stream.play() {
        let _ = init_tx.send(Err(format!("stream.play: {err}")));
        return;
    }

    let _ = init_tx.send(Ok(()));

    while let Ok(cmd) = rx.recv() {
        if matches!(cmd, SoundCommand::Shutdown) {
            break;
        }
        if let Ok(mut playback) = state.lock() {
            match cmd {
                SoundCommand::Shutdown => unreachable!(),
                SoundCommand::Play(cue) => {
                    if playback.muted {
                        continue;
                    }
                    let samples = match cue {
                        FeedbackCue::Start => &start_samples,
                        FeedbackCue::Stop => &stop_samples,
                        FeedbackCue::Complete => &complete_samples,
                        FeedbackCue::Error => &error_samples,
                    };
                    playback.enqueue(samples);
                }
                SoundCommand::PrepareCapture(enabled, done) => {
                    if playback.muted {
                        let _ = done.try_send(());
                        continue;
                    }
                    playback.samples.clear();
                    playback.position = 0;
                    if let Some(previous) = playback.completion.take() {
                        let _ = previous.try_send(());
                    }
                    if enabled {
                        playback.enqueue(&start_samples);
                    }
                    // Output-buffer tail is silence before capture begins.
                    playback
                        .samples
                        .extend(std::iter::repeat_n(0.0, device_sample_rate as usize / 12));
                    playback.completion = Some(done);
                }
            }
        }
    }

    drop(stream);
}

fn fill_f32(output: &mut [f32], channels: usize, state: &Arc<Mutex<PlaybackState>>) {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => {
            for s in output.iter_mut() {
                *s = 0.0;
            }
            return;
        }
    };
    for frame in output.chunks_mut(channels) {
        let v = guard.pull().unwrap_or(0.0);
        for s in frame.iter_mut() {
            *s = v;
        }
    }
}

fn fill_i16(output: &mut [i16], channels: usize, state: &Arc<Mutex<PlaybackState>>) {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => {
            for s in output.iter_mut() {
                *s = 0;
            }
            return;
        }
    };
    for frame in output.chunks_mut(channels) {
        let v = guard.pull().unwrap_or(0.0);
        let scaled = (v.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
        for s in frame.iter_mut() {
            *s = scaled;
        }
    }
}

fn fill_u16(output: &mut [u16], channels: usize, state: &Arc<Mutex<PlaybackState>>) {
    let mut guard = match state.lock() {
        Ok(g) => g,
        Err(_) => {
            for s in output.iter_mut() {
                *s = u16::MAX / 2;
            }
            return;
        }
    };
    for frame in output.chunks_mut(channels) {
        let v = guard.pull().unwrap_or(0.0);
        let scaled = ((v.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16;
        for s in frame.iter_mut() {
            *s = scaled;
        }
    }
}

#[cfg(test)]
fn decode_wav(bytes: &[u8]) -> Result<Vec<f32>> {
    let cursor = Cursor::new(bytes);
    let mut reader = hound::WavReader::new(cursor).map_err(|err| anyhow!("invalid WAV: {err}"))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        return Err(anyhow!("expected mono WAV, got {} channels", spec.channels));
    }
    if spec.sample_rate != 44_100 {
        return Err(anyhow!(
            "expected 44100 Hz WAV, got {} Hz",
            spec.sample_rate
        ));
    }
    let samples = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|s| s.map(|v| v as f32 / i16::MAX as f32))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|err| anyhow!("WAV sample read: {err}"))?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|err| anyhow!("WAV sample read: {err}"))?,
        other => return Err(anyhow!("unsupported WAV format: {other:?}")),
    };
    Ok(samples)
}

/// Soft "wooden tap" cues: short marimba-like strikes, rising to start, falling
/// to stop. Low pitches, a rounded attack and fast-fading overtones keep them
/// warm and unobtrusive for many dictations a day. `tools/gen_sounds.py` renders
/// the same design to WAV for listening.
fn cue_samples(cue: FeedbackCue, rate: u32) -> Vec<f32> {
    // (start offset in s, fundamental in Hz, relative loudness)
    let taps: &[(f32, f32, f32)] = match cue {
        FeedbackCue::Start => &[(0.0, 440.00, 0.85), (0.075, 587.33, 1.0)],
        FeedbackCue::Stop => &[(0.0, 587.33, 1.0), (0.075, 440.00, 0.8)],
        FeedbackCue::Complete => &[(0.0, 440.00, 0.8), (0.07, 554.37, 0.85), (0.14, 659.25, 0.9)],
        FeedbackCue::Error => &[(0.0, 329.63, 1.0), (0.11, 261.63, 0.9)],
    };
    const TAP_SECONDS: f32 = 0.22;
    const PEAK: f32 = 0.10;
    let rate_f = rate as f32;
    let length = taps
        .iter()
        .map(|(offset, _, _)| ((offset + TAP_SECONDS) * rate_f) as usize)
        .max()
        .unwrap_or(0);
    let mut samples = vec![0.0f32; length];
    for &(offset, frequency, loudness) in taps {
        let start = (offset * rate_f) as usize;
        let count = (TAP_SECONDS * rate_f) as usize;
        for index in 0..count {
            let t = index as f32 / rate_f;
            // 2 ms raised-cosine attack: no click, still a definite "tap".
            let attack = (t / 0.002).min(1.0);
            let attack = 0.5 - 0.5 * (attack * std::f32::consts::PI).cos();
            // Marimba-like partials (1 : 3.93 : 9.2) whose overtones die quickly.
            let value = [(1.0, 1.0, 0.060), (3.93, 0.22, 0.012), (9.2, 0.05, 0.004)]
                .iter()
                .map(|&(ratio, gain, decay): &(f32, f32, f32)| {
                    (t * frequency * ratio * std::f32::consts::TAU).sin() * gain * (-t / decay).exp()
                })
                .sum::<f32>();
            if let Some(sample) = samples.get_mut(start + index) {
                *sample += value * attack * loudness;
            }
        }
    }
    // One-pole low-pass around 3.5 kHz rounds off the strike.
    let alpha = 1.0 - (-std::f32::consts::TAU * 3_500.0 / rate_f).exp();
    let mut state = 0.0;
    for sample in samples.iter_mut() {
        state += alpha * (*sample - state);
        *sample = state;
    }
    let peak = samples.iter().fold(0.0f32, |peak, sample| peak.max(sample.abs()));
    if peak > 0.0 {
        let gain = PEAK / peak;
        samples.iter_mut().for_each(|sample| *sample *= gain);
    }
    // 5 ms fade-out so the buffer never ends on a step.
    let fade = ((0.005 * rate_f) as usize).min(samples.len());
    let len = samples.len();
    for (index, sample) in samples[len - fade..].iter_mut().enumerate() {
        *sample *= 1.0 - index as f32 / fade as f32;
    }
    samples
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_are_soft_short_and_start_and_end_silently() {
        for cue in [FeedbackCue::Start, FeedbackCue::Stop, FeedbackCue::Complete, FeedbackCue::Error] {
            let samples = cue_samples(cue, 48_000);
            let peak = samples.iter().fold(0.0f32, |peak, s| peak.max(s.abs()));
            assert!((peak - 0.10).abs() < 1e-3, "{cue:?} peak {peak}");
            assert!(samples.len() < 48_000 * 2 / 5, "{cue:?} longer than 0.4 s");
            assert!(samples[0].abs() < 1e-3 && samples.last().unwrap().abs() < 1e-4);
        }
    }

    #[test]
    fn feedback_deduplicates_coalesces_and_does_not_replay_muted_events() {
        let mut policy = FeedbackPolicy::default();
        let now = Instant::now();
        assert!(policy.accept_outcome("first", true, now));
        assert!(!policy.accept_outcome("first", true, now + Duration::from_secs(1)));
        assert!(!policy.accept_outcome("nearby", true, now + Duration::from_millis(100)));
        assert!(policy.accept_outcome("later", true, now + Duration::from_secs(2)));
        policy.capturing = true;
        assert!(!policy.accept_outcome("during-capture", true, now + Duration::from_secs(3)));
        policy.capturing = false;
        assert!(!policy.accept_outcome("during-capture", true, now + Duration::from_secs(4)));
        assert!(!policy.accept_outcome("muted", false, now + Duration::from_secs(5)));
        assert!(!policy.accept_outcome("muted", true, now + Duration::from_secs(6)));
    }

    #[test]
    fn boundary_completion_is_acknowledged_only_after_samples_are_drained() {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut playback = PlaybackState::new();
        playback.enqueue(&[0.1, 0.0, 0.0]);
        playback.completion = Some(tx);
        assert!(rx.try_recv().is_err());
        assert_eq!(playback.pull(), Some(0.1));
        assert_eq!(playback.pull(), Some(0.0));
        assert!(rx.try_recv().is_err());
        assert_eq!(playback.pull(), Some(0.0));
        assert_eq!(playback.pull(), None);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn capture_gate_silences_already_queued_samples() {
        let mut playback = PlaybackState::new();
        playback.enqueue(&[0.1, 0.2]);
        playback.muted = true;
        assert_eq!(playback.pull(), None);
        assert_eq!(playback.position, 0);
    }

    #[test]
    fn cues_have_quiet_envelopes_and_no_clipped_samples() {
        for cue in [
            FeedbackCue::Start,
            FeedbackCue::Stop,
            FeedbackCue::Complete,
            FeedbackCue::Error,
        ] {
            let samples = cue_samples(cue, 48_000);
            assert!(!samples.is_empty());
            assert!(samples.len() < 48_000 / 2);
            assert_eq!(samples[0], 0.0);
            assert!(samples
                .iter()
                .all(|sample| sample.is_finite() && sample.abs() <= 0.101));
            assert!(samples.last().unwrap().abs() < 0.001);
        }
    }

    #[test]
    fn embedded_feedback_sounds_decode() {
        assert!(!decode_wav(LISTEN_START_WAV)
            .expect("listen_start.wav should decode")
            .is_empty());
        assert!(!decode_wav(LISTEN_STOP_WAV)
            .expect("listen_stop.wav should decode")
            .is_empty());
    }
}
