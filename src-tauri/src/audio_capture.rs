use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};
use serde::Serialize;
use uuid::Uuid;

use crate::audio_preprocess::{
    normalize_audio, write_wav, PreparedAudio, TARGET_CHANNELS, TARGET_SAMPLE_RATE_HZ,
};

const MIN_CAPTURE_PEAK: f32 = 0.003;
const MIN_CAPTURE_RMS: f32 = 0.0006;
/// A microphone opened ahead of a shortcut press closes again after this.
const ARMED_LIFETIME: Duration = Duration::from_secs(3);
/// Audio from before the press that a recording keeps from an armed microphone.
const MAX_PREROLL: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDeviceOption {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordingOverlayState {
    Idle,
    Listening,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingStatusResponse {
    pub state: RecordingOverlayState,
    pub current_session_id: Option<String>,
    pub active_input_device: Option<String>,
    pub last_recording_path: Option<String>,
    pub last_error_message: Option<String>,
    pub duration_ms: Option<i64>,
    pub sample_rate_hz: Option<u32>,
    pub channels: Option<u16>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordingResult {
    pub session_id: String,
    pub file_path: String,
    pub duration_ms: i64,
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub sample_count: usize,
}

struct ActiveRecordingSession {
    session_id: String,
    device_name: Option<String>,
    input_sample_rate_hz: Option<u32>,
    input_channels: Option<u16>,
    accumulated_samples: Vec<f32>,
    last_error_message: Option<String>,
    current_segment: Option<ActiveRecordingSegment>,
}

struct ActiveRecordingSegment {
    device_name: String,
    input_sample_rate_hz: u32,
    input_channels: u16,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_limit: Arc<AtomicUsize>,
    input_level: Arc<Mutex<f32>>,
    error_message: Arc<Mutex<Option<String>>>,
    stream: Stream,
}

/// A cursor-based view of a single capture segment. Normalization and inference
/// run outside the microphone callback; slow consumers never discard packets.
#[derive(Clone)]
pub(crate) struct CaptureTap {
    pub session_id: String,
    pub rate: u32,
    pub channels: u16,
    samples: Arc<Mutex<Vec<f32>>>,
    error: Arc<Mutex<Option<String>>>,
}
impl CaptureTap {
    pub(crate) fn from_recording(session_id: String, audio: PreparedAudio) -> Self {
        Self {
            session_id,
            rate: audio.sample_rate_hz,
            channels: audio.channels,
            samples: Arc::new(Mutex::new(audio.samples)),
            error: Default::default(),
        }
    }
    pub fn read_since(&self, start: usize, maximum: usize) -> Result<Vec<f32>> {
        if self
            .error
            .lock()
            .map_err(|_| anyhow!("Capture state unavailable"))?
            .is_some()
        {
            return Err(anyhow!("CAPTURE_FAILED: The microphone stopped. Stop recording to recover the captured audio."));
        }
        let samples = self
            .samples
            .lock()
            .map_err(|_| anyhow!("Capture buffer unavailable"))?;
        if start > samples.len() {
            return Err(anyhow!("CAPTURE_ORDER: Capture moved backwards."));
        }
        Ok(samples[start..samples.len().min(start.saturating_add(maximum))].to_vec())
    }
    pub fn sample_count(&self) -> usize {
        self.samples.lock().map(|s| s.len()).unwrap_or(0)
    }
}

struct RecordingWorkerState {
    temp_dir: PathBuf,
    preferred_input_device: Arc<Mutex<Option<String>>>,
    input_cache: InputCache,
    active: Option<ActiveRecordingSession>,
    /// Opened when the shortcut key went down, before the press is known to be
    /// a dictation. `start` adopts it, keeping the audio since the key went down.
    armed: Option<ArmedSegment>,
    last_status: RecordingStatusResponse,
}

struct ArmedSegment {
    segment: ActiveRecordingSegment,
    preferred: Option<String>,
    since: Instant,
}

impl RecordingWorkerState {
    fn new(
        temp_dir: PathBuf,
        preferred_input_device: Arc<Mutex<Option<String>>>,
        input_cache: InputCache,
    ) -> Self {
        Self {
            temp_dir,
            preferred_input_device,
            input_cache,
            active: None,
            armed: None,
            last_status: RecordingStatusResponse {
                state: RecordingOverlayState::Idle,
                current_session_id: None,
                active_input_device: None,
                last_recording_path: None,
                last_error_message: None,
                duration_ms: None,
                sample_rate_hz: None,
                channels: None,
            },
        }
    }

    fn status(&self) -> RecordingStatusResponse {
        let mut status = self.last_status.clone();
        if let Some(active) = &self.active {
            status.state = RecordingOverlayState::Listening;
            status.current_session_id = Some(active.session_id.clone());
            status.active_input_device = active.device_name.clone();
            status.duration_ms = Some(Self::current_duration_ms(active));
            status.sample_rate_hz = active.input_sample_rate_hz;
            status.channels = active.input_channels;
            status.last_error_message = active.last_error_message.clone();
        }
        status
    }

    fn start(&mut self) -> Result<RecordingStatusResponse> {
        // Self-heal: if a session is still marked active, it is stale — almost
        // always the residue of a previous `start`/`stop` whose response the
        // controller already timed out on. Tear it down and start fresh rather
        // than refusing forever with "a recording session is already active",
        // which is what permanently wedged dictation after intensive use.
        if let Some(existing) = self.active.take() {
            if let Some(segment) = existing.current_segment {
                drop(segment.stream);
            }
            eprintln!(
                "[audio] discarded a stale active recording session before starting a new one"
            );
        }
        let session_id = Uuid::new_v4().to_string();
        let preferred = preferred_input_device_name(&self.preferred_input_device);
        let segment = match self.armed.take() {
            Some(armed)
                if armed.preferred == preferred
                    && armed.since.elapsed() < ARMED_LIFETIME
                    && armed.segment.healthy() =>
            {
                armed.segment.keep_recent(MAX_PREROLL);
                armed.segment
            }
            _ => open_input_segment(preferred, &self.input_cache)?,
        };
        let device_name = segment.device_name.clone();
        let input_sample_rate_hz = segment.input_sample_rate_hz;
        let input_channels = segment.input_channels;
        self.active = Some(ActiveRecordingSession {
            session_id: session_id.clone(),
            device_name: Some(device_name.clone()),
            input_sample_rate_hz: Some(input_sample_rate_hz),
            input_channels: Some(input_channels),
            accumulated_samples: Vec::new(),
            last_error_message: None,
            current_segment: Some(segment),
        });
        self.last_status = RecordingStatusResponse {
            state: RecordingOverlayState::Listening,
            current_session_id: Some(session_id),
            active_input_device: Some(device_name),
            last_recording_path: self.last_status.last_recording_path.clone(),
            last_error_message: None,
            duration_ms: Some(0),
            sample_rate_hz: Some(input_sample_rate_hz),
            channels: Some(input_channels),
        };
        Ok(self.status())
    }

    /// Opens the microphone ahead of a likely dictation. Failures are left for
    /// `start` to report.
    fn arm(&mut self) {
        if self.active.is_some() {
            return;
        }
        let preferred = preferred_input_device_name(&self.preferred_input_device);
        if let Some(armed) = self.armed.as_mut().filter(|armed| armed.preferred == preferred) {
            // Pressed again: the pre-roll starts over.
            armed.segment.keep_recent(Duration::ZERO);
            armed.since = Instant::now();
            return;
        }
        self.armed = open_input_segment(preferred.clone(), &self.input_cache)
            .inspect_err(|error| eprintln!("[audio] could not open the microphone early: {error:#}"))
            .ok()
            .map(|segment| ArmedSegment {
                segment,
                preferred,
                since: Instant::now(),
            });
    }

    fn expire_armed(&mut self) {
        if self
            .armed
            .as_ref()
            .is_some_and(|armed| armed.since.elapsed() >= ARMED_LIFETIME)
        {
            self.armed = None;
        }
    }

    fn stop(&mut self) -> Result<RecordingResult> {
        let active = self
            .active
            .take()
            .ok_or_else(|| anyhow!("No recording session is currently active"))?;
        let mut active = active;
        finalize_current_segment(&mut active)?;
        if active.accumulated_samples.is_empty() {
            let message = capture_failure_message(&active, active.last_error_message.clone());
            self.last_status = RecordingStatusResponse {
                state: RecordingOverlayState::Error,
                current_session_id: None,
                active_input_device: active.device_name.clone(),
                last_recording_path: self.last_status.last_recording_path.clone(),
                last_error_message: Some(message.clone()),
                duration_ms: Some(Self::current_duration_ms(&active)),
                sample_rate_hz: active.input_sample_rate_hz,
                channels: active.input_channels,
            };
            return Err(anyhow!(message));
        }
        fs::create_dir_all(&self.temp_dir)?;
        let device_name = active
            .device_name
            .clone()
            .unwrap_or_else(|| "the selected device".to_string());
        let prepared = PreparedAudio {
            sample_rate_hz: TARGET_SAMPLE_RATE_HZ,
            channels: TARGET_CHANNELS,
            samples: active.accumulated_samples,
        };
        let duration_ms = ((prepared.samples.len() as f64 / prepared.sample_rate_hz as f64)
            * 1000.0)
            .round() as i64;
        let (peak, rms) = signal_metrics(&prepared.samples);
        if peak < MIN_CAPTURE_PEAK && rms < MIN_CAPTURE_RMS {
            let message = format!(
                "No microphone signal was detected from {}. Check the selected input device, System Settings → Privacy & Security → Microphone, or the hardware mute switch.",
                device_name
            );
            self.last_status = RecordingStatusResponse {
                state: RecordingOverlayState::Error,
                current_session_id: None,
                active_input_device: active.device_name.clone(),
                last_recording_path: self.last_status.last_recording_path.clone(),
                last_error_message: Some(message.clone()),
                duration_ms: Some(duration_ms),
                sample_rate_hz: Some(prepared.sample_rate_hz),
                channels: Some(TARGET_CHANNELS),
            };
            return Err(anyhow!(message));
        }
        let file_path = self.temp_dir.join(format!("{}.wav", active.session_id));
        write_wav(&file_path, &prepared)?;
        cleanup_older_recordings(&self.temp_dir, &file_path)?;
        let result = RecordingResult {
            session_id: active.session_id.clone(),
            file_path: file_path.display().to_string(),
            duration_ms,
            sample_rate_hz: prepared.sample_rate_hz,
            channels: TARGET_CHANNELS,
            sample_count: prepared.samples.len(),
        };
        self.last_status = RecordingStatusResponse {
            state: RecordingOverlayState::Success,
            current_session_id: Some(active.session_id),
            active_input_device: active.device_name,
            last_recording_path: Some(result.file_path.clone()),
            last_error_message: None,
            duration_ms: Some(duration_ms),
            sample_rate_hz: Some(prepared.sample_rate_hz),
            channels: Some(TARGET_CHANNELS),
        };
        Ok(result)
    }

    fn cancel(&mut self) -> Result<RecordingStatusResponse> {
        let active = self
            .active
            .take()
            .ok_or_else(|| anyhow!("No recording session is currently active"))?;
        if let Some(segment) = active.current_segment {
            drop(segment.stream);
        }
        self.last_status = RecordingStatusResponse {
            state: RecordingOverlayState::Idle,
            current_session_id: None,
            active_input_device: active.device_name,
            last_recording_path: self.last_status.last_recording_path.clone(),
            last_error_message: None,
            duration_ms: None,
            sample_rate_hz: None,
            channels: None,
        };
        Ok(self.last_status.clone())
    }

    fn current_duration_ms(active: &ActiveRecordingSession) -> i64 {
        let mut sample_count = active.accumulated_samples.len();
        if let Some(segment) = &active.current_segment {
            if let Ok(buffer) = segment.samples.lock() {
                let frame_count = if segment.input_channels <= 1 {
                    buffer.len()
                } else {
                    buffer.len() / segment.input_channels as usize
                };
                let normalized_len = if segment.input_sample_rate_hz == 0 {
                    0
                } else if segment.input_sample_rate_hz == TARGET_SAMPLE_RATE_HZ {
                    frame_count
                } else {
                    ((frame_count as f64) * TARGET_SAMPLE_RATE_HZ as f64
                        / segment.input_sample_rate_hz as f64)
                        .round() as usize
                };
                sample_count += normalized_len;
            }
        }
        if sample_count == 0 {
            0
        } else {
            (((sample_count as f64 / TARGET_SAMPLE_RATE_HZ as f64) * 1000.0).round() as i64).max(1)
        }
    }
}

fn cleanup_older_recordings(temp_dir: &PathBuf, keep_path: &PathBuf) -> Result<()> {
    for entry in fs::read_dir(temp_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == *keep_path {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("wav") {
            continue;
        }
        if path.is_file() {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

enum WorkerCommand {
    Status(Sender<RecordingStatusResponse>),
    InputLevel(Sender<f32>),
    /// `true` also caps the capture at the five-minute live limit.
    LiveTap(bool, Sender<Result<CaptureTap, String>>),
    Start(Sender<Result<RecordingStatusResponse, String>>),
    Stop(Sender<Result<RecordingResult, String>>),
    StopOwned(String, Sender<Result<RecordingResult, String>>),
    Cancel(Sender<Result<RecordingStatusResponse, String>>),
    Arm,
    Disarm,
}

/// Outcome of dispatching a command to the recording worker thread.
enum SendOutcome<R> {
    /// The worker replied within the timeout.
    Ok(R),
    /// The worker did not reply in time — it is presumed wedged.
    Timeout,
    /// The worker channel is gone, or the controller lock was poisoned.
    Failed(anyhow::Error),
}

#[derive(Clone)]
pub struct RecordingController {
    // Wrapped so a wedged worker thread (e.g. a CoreAudio stream-setup call
    // that never returns) can be abandoned and replaced — see `recover`.
    sender: Arc<Mutex<Sender<WorkerCommand>>>,
    temp_dir: PathBuf,
    preferred_input_device: Arc<Mutex<Option<String>>>,
    input_cache: InputCache,
}

/// Spawn a fresh worker thread and return the channel that drives it.
fn spawn_worker(
    temp_dir: PathBuf,
    preferred_input_device: Arc<Mutex<Option<String>>>,
    input_cache: InputCache,
) -> Sender<WorkerCommand> {
    let (sender, receiver) = mpsc::channel::<WorkerCommand>();
    thread::spawn(move || {
        let mut worker = RecordingWorkerState::new(temp_dir, preferred_input_device, input_cache);
        process_worker_commands(&mut worker, receiver);
    });
    sender
}

impl RecordingController {
    pub fn new(temp_dir: PathBuf) -> Self {
        let preferred_input_device = Arc::new(Mutex::new(None));
        let input_cache = InputCache::default();
        let sender = spawn_worker(
            temp_dir.clone(),
            Arc::clone(&preferred_input_device),
            input_cache.clone(),
        );
        Self {
            sender: Arc::new(Mutex::new(sender)),
            temp_dir,
            preferred_input_device,
            input_cache,
        }
    }

    pub fn set_preferred_input_device(&self, device_name: Option<String>) {
        let changed = self
            .preferred_input_device
            .lock()
            .map(|mut preferred| {
                let changed = *preferred != device_name;
                *preferred = device_name.clone();
                changed
            })
            .unwrap_or(false);
        // Also runs at startup, so the first dictation skips CoreAudio's device scan.
        if changed || self.input_cache.get(&device_name).is_none() {
            self.input_cache.refresh_in_background(device_name);
        }
    }

    /// Opens the microphone now because a dictation is likely to start (the
    /// shortcut key went down). Never blocks: the event tap calls it.
    pub fn arm(&self) {
        if let Ok(sender) = self.sender.lock() {
            let _ = sender.send(WorkerCommand::Arm);
        }
    }

    /// The press was not a dictation: close an armed microphone.
    pub fn disarm(&self) {
        if let Ok(sender) = self.sender.lock() {
            let _ = sender.send(WorkerCommand::Disarm);
        }
    }

    /// Abandon the current (possibly wedged) worker thread and start a clean
    /// one. The old thread is left to die on its own; it only owns its own
    /// audio stream, so leaking it briefly is acceptable and far better than
    /// leaving recording permanently dead.
    pub fn recover(&self) {
        let fresh = spawn_worker(
            self.temp_dir.clone(),
            Arc::clone(&self.preferred_input_device),
            self.input_cache.clone(),
        );
        if let Ok(mut sender) = self.sender.lock() {
            *sender = fresh;
            eprintln!("[audio] recording worker was unresponsive — respawned a fresh worker");
        }
    }

    /// Send a command to the worker and wait for its reply, distinguishing a
    /// timeout (worker wedged) from a hard channel failure.
    fn dispatch<R: Send + 'static>(
        &self,
        timeout: Duration,
        build: impl FnOnce(Sender<R>) -> WorkerCommand,
    ) -> SendOutcome<R> {
        let sender = match self.sender.lock() {
            Ok(sender) => sender.clone(),
            Err(_) => return SendOutcome::Failed(anyhow!("recording worker is not available")),
        };
        let (response_tx, response_rx) = mpsc::channel();
        if sender.send(build(response_tx)).is_err() {
            return SendOutcome::Failed(anyhow!("recording worker is not available"));
        }
        match response_rx.recv_timeout(timeout) {
            Ok(value) => SendOutcome::Ok(value),
            Err(_) => SendOutcome::Timeout,
        }
    }

    pub fn status(&self) -> Result<RecordingStatusResponse> {
        match self.dispatch(Duration::from_secs(2), WorkerCommand::Status) {
            SendOutcome::Ok(value) => Ok(value),
            SendOutcome::Failed(error) => Err(error),
            SendOutcome::Timeout => Err(anyhow!("recording worker timed out")),
        }
    }

    pub fn start(&self) -> Result<RecordingStatusResponse> {
        // On timeout the worker is wedged; respawn it and retry once so the
        // next dictation always gets a clean worker instead of failing forever.
        let result = match self.dispatch(Duration::from_secs(5), WorkerCommand::Start) {
            SendOutcome::Ok(value) => value,
            SendOutcome::Failed(error) => return Err(error),
            SendOutcome::Timeout => {
                self.recover();
                match self.dispatch(Duration::from_secs(5), WorkerCommand::Start) {
                    SendOutcome::Ok(value) => value,
                    SendOutcome::Failed(error) => return Err(error),
                    SendOutcome::Timeout => return Err(anyhow!("recording worker timed out")),
                }
            }
        };
        result.map_err(anyhow::Error::msg)
    }

    pub fn input_level(&self) -> Result<f32> {
        match self.dispatch(Duration::from_millis(250), WorkerCommand::InputLevel) {
            SendOutcome::Ok(value) => Ok(value),
            SendOutcome::Failed(error) => Err(error),
            SendOutcome::Timeout => Err(anyhow!("recording worker timed out")),
        }
    }

    pub(crate) fn live_tap(&self) -> Result<CaptureTap> {
        self.tap(true)
    }

    /// Read-only view of the active capture for early dictation ASR. Unlike
    /// `live_tap` it leaves the capture's sample limit unchanged.
    pub(crate) fn observe_tap(&self) -> Result<CaptureTap> {
        self.tap(false)
    }

    fn tap(&self, limit: bool) -> Result<CaptureTap> {
        match self.dispatch(Duration::from_secs(2), |tx| {
            WorkerCommand::LiveTap(limit, tx)
        }) {
            SendOutcome::Ok(value) => value.map_err(anyhow::Error::msg),
            SendOutcome::Failed(error) => Err(error),
            SendOutcome::Timeout => Err(anyhow!("Capture setup timed out")),
        }
    }

    pub fn stop(&self) -> Result<RecordingResult> {
        let result = match self.dispatch(Duration::from_secs(10), WorkerCommand::Stop) {
            SendOutcome::Ok(value) => value,
            SendOutcome::Failed(error) => return Err(error),
            SendOutcome::Timeout => {
                // The worker hung mid-stop; this capture is lost, but recover so
                // future dictations work. The retry runs against a clean worker.
                self.recover();
                match self.dispatch(Duration::from_secs(10), WorkerCommand::Stop) {
                    SendOutcome::Ok(value) => value,
                    SendOutcome::Failed(error) => return Err(error),
                    SendOutcome::Timeout => return Err(anyhow!("recording worker timed out")),
                }
            }
        };
        result.map_err(anyhow::Error::msg)
    }
    pub(crate) fn stop_owned(&self, session_id: &str) -> Result<RecordingResult> {
        match self.dispatch(Duration::from_secs(10), |tx| {
            WorkerCommand::StopOwned(session_id.into(), tx)
        }) {
            SendOutcome::Ok(value) => value.map_err(anyhow::Error::msg),
            SendOutcome::Failed(error) => Err(error),
            SendOutcome::Timeout => Err(anyhow!(
                "The recording did not stop in time. Reset dictation before trying again."
            )),
        }
    }

    pub fn cancel(&self) -> Result<RecordingStatusResponse> {
        // Used by the watchdog and the manual reset path, so it must also
        // recover a wedged worker rather than simply timing out.
        let result = match self.dispatch(Duration::from_secs(5), WorkerCommand::Cancel) {
            SendOutcome::Ok(value) => value,
            SendOutcome::Failed(error) => return Err(error),
            SendOutcome::Timeout => {
                self.recover();
                match self.dispatch(Duration::from_secs(5), WorkerCommand::Cancel) {
                    SendOutcome::Ok(value) => value,
                    SendOutcome::Failed(error) => return Err(error),
                    SendOutcome::Timeout => return Err(anyhow!("recording worker timed out")),
                }
            }
        };
        result.map_err(anyhow::Error::msg)
    }
}

fn process_worker_commands(worker: &mut RecordingWorkerState, receiver: Receiver<WorkerCommand>) {
    loop {
        let command = match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                worker.expire_armed();
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match command {
            WorkerCommand::Arm => worker.arm(),
            WorkerCommand::Disarm => worker.armed = None,
            WorkerCommand::Status(response_tx) => {
                let _ = response_tx.send(worker.status());
            }
            WorkerCommand::InputLevel(response_tx) => {
                let _ = response_tx.send(worker.current_input_level());
            }
            WorkerCommand::LiveTap(limit, response_tx) => {
                let result = (|| -> Result<CaptureTap> {
                    let active = worker
                        .active
                        .as_ref()
                        .ok_or_else(|| anyhow!("No active recording"))?;
                    if !active.accumulated_samples.is_empty() {
                        return Err(anyhow!("Live capture cannot resume a paused recording"));
                    }
                    let segment = active
                        .current_segment
                        .as_ref()
                        .ok_or_else(|| anyhow!("No capture segment"))?;
                    if limit {
                        // The helper's five minutes include the live pair's lead-in.
                        let millis = 300_000 - crate::live_pair::LEAD_SILENCE_MS;
                        segment.sample_limit.store(
                            segment.input_sample_rate_hz as usize
                                * segment.input_channels as usize
                                * millis
                                / 1000,
                            Ordering::SeqCst,
                        );
                    }
                    Ok(CaptureTap {
                        session_id: active.session_id.clone(),
                        rate: segment.input_sample_rate_hz,
                        channels: segment.input_channels,
                        samples: segment.samples.clone(),
                        error: segment.error_message.clone(),
                    })
                })();
                let _ = response_tx.send(result.map_err(|error| error.to_string()));
            }
            WorkerCommand::Start(response_tx) => {
                let _ = response_tx.send(worker.start().map_err(|error| error.to_string()));
            }
            WorkerCommand::Stop(response_tx) => {
                let _ = response_tx.send(worker.stop().map_err(|error| error.to_string()));
            }
            WorkerCommand::StopOwned(id, response_tx) => {
                let result = if worker
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id == id)
                {
                    worker.stop().map_err(|error| error.to_string())
                } else {
                    Err("DICTATION_CANCELED: Capture belongs to another session.".into())
                };
                let _ = response_tx.send(result);
            }
            WorkerCommand::Cancel(response_tx) => {
                let _ = response_tx.send(worker.cancel().map_err(|error| error.to_string()));
            }
        }
    }
}

/// The resolved microphone and its format. Looking them up costs about 175 ms
/// (a device scan for a named microphone, then the format query), so presses
/// reuse the last result and a background refresh keeps it current.
#[derive(Clone)]
struct ResolvedInput {
    preferred: Option<String>,
    device: cpal::Device,
    name: String,
    config: cpal::SupportedStreamConfig,
}

#[derive(Clone, Default)]
struct InputCache(Arc<Mutex<Option<ResolvedInput>>>);

impl InputCache {
    fn get(&self, preferred: &Option<String>) -> Option<ResolvedInput> {
        self.0
            .lock()
            .ok()?
            .clone()
            .filter(|cached| cached.preferred == *preferred)
    }

    fn store(&self, resolved: Option<ResolvedInput>) {
        if let Ok(mut cache) = self.0.lock() {
            *cache = resolved;
        }
    }

    fn refresh_in_background(&self, preferred: Option<String>) {
        let cache = self.clone();
        let _ = thread::Builder::new()
            .name("murmela-input-scan".into())
            .spawn(move || match resolve_input(preferred) {
                Ok(resolved) => cache.store(Some(resolved)),
                Err(error) => {
                    cache.store(None);
                    eprintln!("[audio] microphone lookup failed: {error:#}");
                }
            });
    }
}

fn resolve_input(preferred: Option<String>) -> Result<ResolvedInput> {
    let host = cpal::default_host();
    let device = select_input_device(&host, preferred.as_deref())?;
    let config = device
        .default_input_config()
        .context("Failed to resolve default microphone config")?;
    Ok(ResolvedInput {
        preferred,
        name: describe_device(&device),
        device,
        config,
    })
}

/// Opens the microphone, from the cache when it still names the device that
/// would be chosen now; on any doubt or failure it resolves afresh.
fn open_input_segment(preferred: Option<String>, cache: &InputCache) -> Result<ActiveRecordingSegment> {
    let cached = cache.get(&preferred).filter(|cached| {
        // The system default can change at any time (headset plugged in);
        // asking for it is cheap, unlike scanning for a named device.
        preferred.is_some()
            || cpal::default_host()
                .default_input_device()
                .is_some_and(|device| describe_device(&device) == cached.name)
    });
    if let Some(cached) = cached {
        match create_input_segment(&cached) {
            Ok(segment) => {
                // Picks up format changes and replugged devices for the next press.
                cache.refresh_in_background(preferred);
                return Ok(segment);
            }
            Err(error) => eprintln!("[audio] cached microphone failed, looking it up again: {error:#}"),
        }
    }
    let resolved = resolve_input(preferred)?;
    let segment = create_input_segment(&resolved);
    cache.store(segment.is_ok().then_some(resolved));
    segment
}

fn create_input_segment(input: &ResolvedInput) -> Result<ActiveRecordingSegment> {
    let input_sample_rate_hz = input.config.sample_rate();
    let input_channels = input.config.channels();
    let stream_config: StreamConfig = input.config.clone().into();
    let samples = Arc::new(Mutex::new(Vec::new()));
    let sample_limit = Arc::new(AtomicUsize::new(usize::MAX));
    let input_level = Arc::new(Mutex::new(0.0));
    let error_message = Arc::new(Mutex::new(None));
    let stream = build_input_stream(
        &input.device,
        input.config.sample_format(),
        &stream_config,
        Arc::clone(&samples),
        sample_limit.clone(),
        Arc::clone(&input_level),
        Arc::clone(&error_message),
    )?;
    stream.play().context("Failed to start microphone stream")?;
    Ok(ActiveRecordingSegment {
        device_name: input.name.clone(),
        input_sample_rate_hz,
        input_channels,
        samples,
        sample_limit,
        input_level,
        error_message,
        stream,
    })
}

impl ActiveRecordingSegment {
    fn healthy(&self) -> bool {
        self.error_message.lock().map(|error| error.is_none()).unwrap_or(false)
    }

    /// Drops all but the most recent `keep` of captured audio.
    fn keep_recent(&self, keep: Duration) {
        if let Ok(mut samples) = self.samples.lock() {
            let excess =
                excess_samples(samples.len(), self.input_sample_rate_hz, self.input_channels, keep);
            samples.drain(..excess);
        }
    }
}

/// How many interleaved samples to drop from the front of `len` so that at
/// most `keep` remains, in whole frames.
fn excess_samples(len: usize, rate: u32, channels: u16, keep: Duration) -> usize {
    let frame = channels.max(1) as usize;
    let keep = (keep.as_secs_f64() * rate as f64) as usize * frame;
    len.saturating_sub(keep).div_ceil(frame) * frame
}

pub fn list_input_devices() -> Result<Vec<InputDeviceOption>> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .map(|device| describe_device(&device));
    let mut devices = host
        .input_devices()
        .context("failed to enumerate input devices")?
        .filter_map(|device| {
            let name = describe_device(&device);
            if name.trim().is_empty() {
                return None;
            }
            Some(InputDeviceOption {
                id: name.clone(),
                name: name.clone(),
                is_default: default_name.as_deref() == Some(name.as_str()),
            })
        })
        .collect::<Vec<_>>();
    devices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then(left.name.cmp(&right.name))
    });
    devices.dedup_by(|left, right| left.id == right.id);
    Ok(devices)
}

pub(crate) fn select_input_device(
    host: &cpal::Host,
    preferred_device_name: Option<&str>,
) -> Result<cpal::Device> {
    if let Some(preferred_name) = preferred_device_name {
        let mut devices = host
            .input_devices()
            .context("failed to enumerate input devices")?;
        if let Some(device) = devices.find(|device| describe_device(device) == preferred_name) {
            return Ok(device);
        }
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("No default microphone is available"))
}

pub(crate) fn describe_device(device: &cpal::Device) -> String {
    device
        .description()
        .map(|description| description.name().to_string())
        .unwrap_or_else(|_| "Unknown microphone".to_string())
}

fn preferred_input_device_name(
    preferred_input_device: &Arc<Mutex<Option<String>>>,
) -> Option<String> {
    preferred_input_device
        .lock()
        .ok()
        .and_then(|value| value.clone())
}

fn finalize_current_segment(session: &mut ActiveRecordingSession) -> Result<()> {
    let Some(segment) = session.current_segment.take() else {
        return Ok(());
    };

    // Give the input callback thread a moment to flush the last buffered frames
    // before we tear the stream down and read the capture buffer.
    thread::sleep(Duration::from_millis(70));
    drop(segment.stream);
    thread::sleep(Duration::from_millis(30));
    let captured_samples = segment
        .samples
        .lock()
        .map_err(|_| anyhow!("Recording buffer lock was poisoned"))?
        .clone();
    session.device_name = Some(segment.device_name.clone());
    session.input_sample_rate_hz = Some(segment.input_sample_rate_hz);
    session.input_channels = Some(segment.input_channels);
    session.last_error_message = segment
        .error_message
        .lock()
        .ok()
        .and_then(|value| value.clone());

    if captured_samples.is_empty() {
        return Ok(());
    }

    let prepared = normalize_audio(
        &captured_samples,
        segment.input_sample_rate_hz,
        segment.input_channels,
    );
    session
        .accumulated_samples
        .extend_from_slice(&prepared.samples);
    Ok(())
}

fn build_input_stream(
    device: &cpal::Device,
    sample_format: SampleFormat,
    config: &StreamConfig,
    samples: Arc<Mutex<Vec<f32>>>,
    sample_limit: Arc<AtomicUsize>,
    input_level: Arc<Mutex<f32>>,
    error_message: Arc<Mutex<Option<String>>>,
) -> Result<Stream> {
    let stream = match sample_format {
        SampleFormat::F32 => device.build_input_stream(
            config,
            {
                let samples = Arc::clone(&samples);
                let input_level = Arc::clone(&input_level);
                let sample_limit = sample_limit.clone();
                move |data: &[f32], _| append_samples(data, &samples, &input_level, &sample_limit)
            },
            {
                let error_message = Arc::clone(&error_message);
                move |error| store_error(error, &error_message)
            },
            None,
        )?,
        SampleFormat::I16 => device.build_input_stream(
            config,
            {
                let samples = Arc::clone(&samples);
                let input_level = Arc::clone(&input_level);
                let sample_limit = sample_limit.clone();
                move |data: &[i16], _| {
                    let converted = data
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect::<Vec<_>>();
                    append_samples(&converted, &samples, &input_level, &sample_limit);
                }
            },
            {
                let error_message = Arc::clone(&error_message);
                move |error| store_error(error, &error_message)
            },
            None,
        )?,
        SampleFormat::U16 => device.build_input_stream(
            config,
            {
                let samples = Arc::clone(&samples);
                let input_level = Arc::clone(&input_level);
                let sample_limit = sample_limit.clone();
                move |data: &[u16], _| {
                    let converted = data
                        .iter()
                        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect::<Vec<_>>();
                    append_samples(&converted, &samples, &input_level, &sample_limit);
                }
            },
            {
                let error_message = Arc::clone(&error_message);
                move |error| store_error(error, &error_message)
            },
            None,
        )?,
        _ => return Err(anyhow!("Unsupported microphone sample format")),
    };
    Ok(stream)
}

fn append_samples(
    input: &[f32],
    samples: &Arc<Mutex<Vec<f32>>>,
    input_level: &Arc<Mutex<f32>>,
    limit: &AtomicUsize,
) {
    if let Ok(mut buffer) = samples.lock() {
        let count = input
            .len()
            .min(limit.load(Ordering::Relaxed).saturating_sub(buffer.len()));
        buffer.extend_from_slice(&input[..count]);
    }
    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f32;
    for sample in input {
        let magnitude = sample.abs().min(1.0);
        peak = peak.max(magnitude);
        sum_squares += magnitude * magnitude;
    }
    let rms = if input.is_empty() {
        0.0
    } else {
        (sum_squares / input.len() as f32).sqrt().min(1.0)
    };
    let boosted = (rms * 5.4).max(peak * 1.9).clamp(0.0, 1.0);
    let envelope = if boosted <= 0.003 {
        0.0
    } else {
        boosted.powf(0.72).clamp(0.0, 1.0)
    };
    if let Ok(mut level) = input_level.lock() {
        let current = *level;
        *level = if envelope >= current {
            current * 0.14 + envelope * 0.86
        } else {
            current * 0.68 + envelope * 0.32
        };
    }
}

fn store_error(error: cpal::StreamError, error_message: &Arc<Mutex<Option<String>>>) {
    if let Ok(mut last_error) = error_message.lock() {
        *last_error = Some(error.to_string());
    }
}

fn signal_metrics(samples: &[f32]) -> (f32, f32) {
    let mut peak = 0.0_f32;
    let mut sum_squares = 0.0_f32;
    for sample in samples {
        let magnitude = sample.abs();
        peak = peak.max(magnitude);
        sum_squares += magnitude * magnitude;
    }
    let rms = if samples.is_empty() {
        0.0
    } else {
        (sum_squares / samples.len() as f32).sqrt()
    };
    (peak, rms)
}

fn capture_failure_message(
    active: &ActiveRecordingSession,
    fallback_message: Option<String>,
) -> String {
    if let Some(message) = fallback_message.filter(|value| !value.trim().is_empty()) {
        return message;
    }

    format!(
        "No usable microphone audio was captured from {}. Check the selected input device, System Settings → Privacy & Security → Microphone, or the hardware mute switch.",
        active
            .device_name
            .as_deref()
            .unwrap_or("the selected device")
    )
}

impl RecordingWorkerState {
    fn current_input_level(&self) -> f32 {
        self.active
            .as_ref()
            .and_then(|active| active.current_segment.as_ref())
            .and_then(|segment| segment.input_level.lock().ok().map(|value| *value))
            .unwrap_or(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_tap_preserves_packet_tail_and_enforces_explicit_capture_limit() {
        let samples = Arc::new(Mutex::new(Vec::new()));
        let level = Default::default();
        let limit = AtomicUsize::new(7);
        let tap = CaptureTap {
            session_id: "capture".into(),
            rate: 48000,
            channels: 2,
            samples: samples.clone(),
            error: Default::default(),
        };
        append_samples(&[0.1, 0.2, 0.3], &samples, &level, &limit);
        let first = tap.read_since(0, 2).unwrap();
        append_samples(&[0.4, 0.5, 0.6, 0.7, 0.8], &samples, &level, &limit);
        append_samples(&[0.9], &samples, &level, &limit);
        let tail = tap.read_since(first.len(), 99).unwrap();
        assert_eq!(
            [first, tail].concat(),
            vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]
        );
        assert_eq!(tap.sample_count(), 7);
        assert!(tap.read_since(7, 10).unwrap().is_empty());
        assert!(tap.read_since(8, 10).is_err());
        *tap.error.lock().unwrap() = Some("Device disconnected".into());
        assert!(tap.read_since(0, 1).is_err());
    }

    #[test]
    fn preroll_keeps_whole_recent_frames() {
        let keep = Duration::from_millis(10);
        // 48 kHz stereo: 10 ms is 480 frames, 960 samples.
        assert_eq!(excess_samples(2000, 48_000, 2, keep), 1040);
        assert_eq!(excess_samples(2001, 48_000, 2, keep), 1042);
        assert_eq!(excess_samples(900, 48_000, 2, keep), 0);
        assert_eq!(excess_samples(900, 48_000, 2, Duration::ZERO), 900);
    }

    // These cover the controller<->worker dispatch and the respawn machinery
    // without touching audio hardware (status() never opens a stream), which is
    // exactly the layer the post-intensive-use freeze lived in.

    #[test]
    fn status_roundtrips_through_worker() {
        let controller = RecordingController::new(std::env::temp_dir().join("murmela-test-a"));
        let status = controller.status().expect("worker should answer status");
        assert!(matches!(status.state, RecordingOverlayState::Idle));
    }

    #[test]
    fn recover_yields_a_responsive_worker() {
        let controller = RecordingController::new(std::env::temp_dir().join("murmela-test-b"));
        // Simulate a wedged worker by abandoning it and spawning a fresh one;
        // the controller must keep answering afterwards (the old freeze left it
        // permanently unresponsive instead).
        controller.recover();
        let status = controller
            .status()
            .expect("recovered worker should answer status");
        assert!(matches!(status.state, RecordingOverlayState::Idle));
        // A second recovery must remain healthy too.
        controller.recover();
        assert!(controller.status().is_ok());
    }
}
