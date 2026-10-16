//! Live pair: Nemotron streaming preview plus a Parakeet v3 final pass, both in
//! one resident CoreML helper (`workers/fluid`). The helper belongs to the app
//! rather than the processing queue: it loads once after start-up and
//! stays loaded, so a shortcut press only resets streaming state. Capture owns the
//! complete audio; the helper acknowledges a sample cursor and never drops audio.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::audio_capture::CaptureTap;
use crate::audio_preprocess::StreamingNormalizer;
use crate::desktop_shell::{DesktopShellController, StreamingState, StreamingText};
use crate::managed_process::ManagedChild;

pub const MODEL_ID: &str = "live-pair";
pub const MODEL_NAME: &str = "Live pair · Nemotron + Parakeet";
pub const ENGINE: &str = "fluidaudio-live-pair";
/// Five minutes of 16 kHz audio, the longest live dictation.
pub const MAX_SAMPLES: usize = 300 * 16_000;
const MAX_MESSAGE: u64 = 1024 * 1024;
/// A cold CoreML compile takes about 30 s on an M3 Pro (once per macOS build).
const LOAD_TIMEOUT: Duration = Duration::from_secs(180);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const FINISH_TIMEOUT: Duration = Duration::from_secs(60);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(2);
/// Restart delays after consecutive failures; then give up and show an error.
const BACKOFF: [Duration; 3] = [
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];
/// With "Keep models loaded" off, the helper exits this long after its last use.
const IDLE_EXIT: Duration = Duration::from_secs(60);
/// After a critical memory event, reload this long after pressure is normal.
const PRESSURE_RELOAD_DELAY: Duration = Duration::from_secs(60);
const TICK: Duration = Duration::from_secs(1);

// MARK: Pinned artifacts

#[derive(Deserialize)]
struct Manifest {
    models: ManifestModels,
}
#[derive(Deserialize)]
struct ManifestModels {
    nemotron: ManifestModel,
    parakeet: ManifestModel,
}
#[derive(Deserialize)]
struct ManifestModel {
    repository: String,
    revision: String,
    directory: String,
    files: Vec<ManifestFile>,
}
#[derive(Deserialize)]
struct ManifestFile {
    path: String,
    bytes: i64,
    sha256: String,
}

/// One downloadable file, relative to `<models>/live-pair/`.
pub(crate) struct Artifact {
    pub path: String,
    pub bytes: i64,
    pub url: String,
    pub sha256: String,
}

static MANIFEST: LazyLock<Manifest> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../workers/fluid/manifest.json"))
        .expect("Pinned live-pair manifest")
});
static ARTIFACTS: LazyLock<Vec<Artifact>> = LazyLock::new(|| {
    let models = &MANIFEST.models;
    [&models.nemotron, &models.parakeet]
        .into_iter()
        .flat_map(|model| {
            model.files.iter().map(move |file| Artifact {
                path: format!("{}/{}", model.directory, file.path),
                bytes: file.bytes,
                url: format!(
                    "https://huggingface.co/{}/resolve/{}/{}?download=true",
                    model.repository, model.revision, file.path
                ),
                sha256: file.sha256.clone(),
            })
        })
        .collect()
});
static REVISION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "nemotron@{}+parakeet@{}",
        MANIFEST.models.nemotron.revision, MANIFEST.models.parakeet.revision
    )
});

pub(crate) fn artifacts() -> &'static [Artifact] {
    &ARTIFACTS
}
pub(crate) fn revision() -> &'static str {
    &REVISION
}
pub(crate) fn total_bytes() -> i64 {
    artifacts().iter().map(|artifact| artifact.bytes).sum()
}

// MARK: Setup

pub fn platform_supported() -> bool {
    crate::model_metadata::apple_silicon_supported()
}

pub fn helper_path(app: &tauri::AppHandle) -> Option<PathBuf> {
    use tauri::Manager;
    let packaged = app
        .path()
        .resource_dir()
        .ok()?
        .join("workers/fluid/murmela-fluid-worker");
    if packaged.is_file() {
        return Some(packaged);
    }
    if cfg!(debug_assertions) {
        let development =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("bundle/fluid/murmela-fluid-worker");
        if development.is_file() {
            return Some(development);
        }
    }
    None
}

/// What the resident helper should have loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    pub helper: PathBuf,
    pub nemotron: PathBuf,
    pub parakeet: PathBuf,
    pub chunk_ms: u32,
    pub keep_loaded: bool,
}
impl Config {
    /// The loaded models match, whatever the residency policy.
    fn same_models(&self, other: &Config) -> bool {
        self.helper == other.helper
            && self.nemotron == other.nemotron
            && self.parakeet == other.parakeet
            && self.chunk_ms == other.chunk_ms
    }
}

pub(crate) fn config_for(
    app: &tauri::AppHandle,
    models_dir: &Path,
    settings: &crate::settings::AppSettings,
) -> Result<Config> {
    if !platform_supported() {
        bail!("LIVE_PAIR_SETUP: Live dictation requires Apple Silicon and macOS 14 or newer.");
    }
    let helper = helper_path(app).ok_or_else(|| {
        anyhow!("LIVE_PAIR_SETUP: The signed live-pair helper is missing. Reinstall Murmela.")
    })?;
    if !crate::model_downloads::live_pair_installed(models_dir) {
        bail!("LIVE_PAIR_SETUP: Download or repair Live dictation in Settings → Engines.");
    }
    if !crate::settings::LIVE_PAIR_CHUNKS_MS.contains(&settings.live_pair_chunk_ms) {
        bail!("LIVE_PAIR_SETUP: Choose a live preview chunk of 560 or 1120 ms.");
    }
    let root = models_dir.join(MODEL_ID);
    Ok(Config {
        helper,
        nemotron: root
            .join("nemotron/latin")
            .join(format!("{}ms", settings.live_pair_chunk_ms)),
        parakeet: root.join("parakeet"),
        chunk_ms: settings.live_pair_chunk_ms,
        keep_loaded: settings.live_pair_keep_loaded,
    })
}

/// Language hint: automatic, or one of the Latin-script languages that both
/// models share.
pub(crate) fn source_language(settings: &crate::settings::AppSettings) -> Result<String> {
    if settings.language_mode == crate::settings::LanguageMode::Auto {
        return Ok("auto".into());
    }
    let code = settings
        .fixed_language
        .as_deref()
        .map(|value| crate::output_format::normalize_language_code(Some(value)))
        .unwrap_or_default();
    match code.as_str() {
        "de" | "en" | "es" | "fr" | "it" | "pt" => Ok(code),
        _ => bail!("LIVE_PAIR_SETUP: Live dictation doesn't support this language. Choose Automatic or one of its languages under Settings → Dictation → Language you speak."),
    }
}

pub(crate) fn transcript(
    id: &str,
    text: String,
    duration_ms: i64,
    language: Option<String>,
    warning: Option<String>,
) -> crate::asr::TranscriptResult {
    let mut result = transcript_base(id, text, duration_ms, language);
    result.model_name = MODEL_NAME.into();
    if let Some(reason) = warning {
        result.warnings.push(crate::asr::TranscriptWarning {
            start_ms: 0,
            end_ms: duration_ms,
            reason,
            attempts: 1,
            outcome: "live_preview_text_used".into(),
        });
    }
    result
}

// MARK: Protocol

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Message {
    version: u32,
    session_id: String,
    sequence: u64,
    #[serde(rename = "type")]
    kind: String,
    code: String,
    committed_text: String,
    tentative_text: String,
    ack_sample: usize,
    stream_text: String,
    final_text: String,
    language: String,
    stream_error: String,
    final_error: String,
    load_ms: f64,
    chunk_ms: u32,
    loaded: bool,
    rss_bytes: u64,
    timings: Timings,
}
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Timings {
    audio_ms: f64,
    flush_ms: f64,
    final_ms: f64,
    /// Time the final pass waited for Parakeet's background load.
    final_wait_ms: f64,
}

fn wire_code(code: &str) -> &str {
    if !code.is_empty()
        && code.len() <= 64
        && code
            .bytes()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
    {
        code
    } else {
        "LIVE_PAIR_RUNTIME_ERROR"
    }
}

enum IoEvent {
    Message(Box<Message>),
    Failed,
}

/// One helper process. Dropping it kills and reaps its process group.
pub(crate) struct Process {
    helper: PathBuf,
    child: ManagedChild,
    input: mpsc::SyncSender<Vec<u8>>,
    output: mpsc::Receiver<IoEvent>,
}
impl Process {
    fn launch(helper: &Path) -> Result<Self> {
        let mut command = Command::new(helper);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::managed_process::isolate(&mut command);
        let mut child = ManagedChild::new(
            command
                .spawn()
                .map_err(|_| anyhow!("LIVE_PAIR_SETUP: Could not start the live-pair helper."))?,
        );
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Live-pair input unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Live-pair output unavailable"))?;
        let (input, writes) = mpsc::sync_channel::<Vec<u8>>(4);
        let (events, output) = mpsc::sync_channel(8);
        let failed = events.clone();
        std::thread::spawn(move || {
            while let Ok(bytes) = writes.recv() {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    let _ = failed.send(IoEvent::Failed);
                    break;
                }
            }
        });
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                let event = match reader.by_ref().take(MAX_MESSAGE + 1).read_line(&mut line) {
                    Ok(0) => IoEvent::Failed,
                    Ok(_) if line.len() as u64 <= MAX_MESSAGE && line.ends_with('\n') => {
                        match serde_json::from_str::<Message>(&line) {
                            Ok(message) => IoEvent::Message(Box::new(message)),
                            Err(_) => IoEvent::Failed,
                        }
                    }
                    _ => IoEvent::Failed,
                };
                let failed = matches!(event, IoEvent::Failed);
                if events.send(event).is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            helper: helper.to_path_buf(),
            child,
            input,
            output,
        })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Sends one request and waits for its response. Responses to other,
    /// abandoned sessions (or to earlier requests of this one) are stale and
    /// skipped; anything else out of order is a protocol failure.
    fn exchange(
        &mut self,
        kind: &str,
        session: &str,
        sequence: u64,
        fields: Value,
        timeout: Duration,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Message> {
        check()?;
        let mut request = fields;
        request["version"] = json!(1);
        request["type"] = json!(kind);
        request["sessionId"] = json!(session);
        request["sequence"] = json!(sequence);
        let mut bytes = serde_json::to_vec(&request)?;
        bytes.push(b'\n');
        self.input
            .try_send(bytes)
            .map_err(|_| anyhow!("LIVE_PAIR_PROTOCOL: Helper input is blocked."))?;
        let deadline = Instant::now() + timeout;
        loop {
            check()?;
            if Instant::now() >= deadline {
                bail!("LIVE_PAIR_TIMEOUT: The live-pair helper stopped responding.");
            }
            match self.output.recv_timeout(Duration::from_millis(10)) {
                Ok(IoEvent::Message(message)) => {
                    if message.version != 1 {
                        bail!("LIVE_PAIR_PROTOCOL: Unsupported helper response.");
                    }
                    if message.session_id != session || message.sequence <= sequence {
                        continue;
                    }
                    if message.sequence != sequence + 1 {
                        bail!("LIVE_PAIR_PROTOCOL: Unordered helper response.");
                    }
                    if message.kind == "error" {
                        bail!(
                            "{}: Live dictation could not be completed.",
                            wire_code(&message.code)
                        );
                    }
                    return Ok(*message);
                }
                Ok(IoEvent::Failed) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("LIVE_PAIR_WORKER_FAILED: The live-pair helper ended unexpectedly.")
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }
}

/// Helper failures that leave the process in an unknown state.
fn is_fatal(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    [
        "LIVE_PAIR_PROTOCOL",
        "LIVE_PAIR_TIMEOUT",
        "LIVE_PAIR_WORKER_FAILED",
        "FLUID_PROTOCOL",
        "FLUID_RUNTIME_ERROR",
    ]
    .iter()
    .any(|code| text.starts_with(code))
}

fn canceled(error: &anyhow::Error) -> bool {
    error.to_string().starts_with("DICTATION_CANCELED")
}

// MARK: Resident helper

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LivePairState {
    /// Not selected, not installed, or "Keep models loaded" is off and idle.
    Off,
    Preparing,
    Ready,
    /// Unloaded after critical memory pressure; reloads on demand.
    Unloaded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LivePairStatus {
    pub state: LivePairState,
    pub message: Option<String>,
    pub load_ms: Option<u64>,
    pub rss_bytes: Option<u64>,
}
impl LivePairStatus {
    fn new(state: LivePairState, message: Option<String>) -> Self {
        Self {
            state,
            message,
            load_ms: None,
            rss_bytes: None,
        }
    }
}

#[derive(Default)]
struct Slot {
    process: Option<Process>,
    /// The configuration whose models are loaded in `process`.
    loaded: Option<Config>,
    in_session: bool,
}

struct Control {
    config: Option<Config>,
    pressure_unloaded: bool,
    pressure_cleared: Option<Instant>,
    failures: usize,
    retry_at: Option<Instant>,
    gave_up: bool,
    last_used: Instant,
    shutdown: bool,
}

struct Inner {
    slot: Mutex<Slot>,
    control: Mutex<Control>,
    wake: Condvar,
    status: Mutex<LivePairStatus>,
    notify: Box<dyn Fn(&LivePairStatus) + Send + Sync>,
    control_ids: AtomicU64,
}

/// The resident helper and its supervisor. Cloning shares one helper.
#[derive(Clone)]
pub struct LivePair {
    inner: Arc<Inner>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

impl LivePair {
    pub fn new(notify: impl Fn(&LivePairStatus) + Send + Sync + 'static) -> Self {
        let pair = Self::unsupervised(notify);
        let weak = Arc::downgrade(&pair.inner);
        std::thread::spawn(move || supervise(weak));
        pair
    }

    /// Without the background supervisor; tests drive `tick` themselves.
    fn unsupervised(notify: impl Fn(&LivePairStatus) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                slot: Default::default(),
                control: Mutex::new(Control {
                    config: None,
                    pressure_unloaded: false,
                    pressure_cleared: None,
                    failures: 0,
                    retry_at: None,
                    gave_up: false,
                    last_used: Instant::now(),
                    shutdown: false,
                }),
                wake: Condvar::new(),
                status: Mutex::new(LivePairStatus::new(LivePairState::Off, None)),
                notify: Box::new(notify),
                control_ids: AtomicU64::new(0),
            }),
        }
    }

    pub fn status(&self) -> LivePairStatus {
        lock(&self.inner.status).clone()
    }

    /// Selects what should be resident (`None`: nothing). A changed model set
    /// reloads; a failed helper gets a fresh set of retries.
    pub(crate) fn configure(&self, config: Option<Config>) {
        let mut control = lock(&self.inner.control);
        if control.config != config {
            control.config = config;
            control.failures = 0;
            control.retry_at = None;
            control.gave_up = false;
        }
        drop(control);
        self.inner.wake.notify_all();
    }

    /// The setup problem to show while the live pair is selected but unusable.
    pub(crate) fn report_setup_error(&self, message: String) {
        self.configure(None);
        set_status(
            &self.inner,
            LivePairStatus::new(LivePairState::Error, Some(message)),
        );
    }

    /// Memory-pressure level from the dispatch source (1 normal, 2 warning,
    /// 4 critical). Warning keeps the models; critical unloads them.
    pub(crate) fn memory_pressure(&self, level: usize) {
        let mut control = lock(&self.inner.control);
        if level & 0x4 != 0 {
            control.pressure_unloaded = true;
            control.pressure_cleared = None;
        } else if level & 0x1 != 0 && control.pressure_unloaded {
            control.pressure_cleared = Some(Instant::now());
        }
        drop(control);
        self.inner.wake.notify_all();
    }

    pub fn shutdown(&self) {
        lock(&self.inner.control).shutdown = true;
        self.inner.wake.notify_all();
        let mut slot = lock(&self.inner.slot);
        slot.process = None;
        slot.loaded = None;
    }

    /// Claims the helper for one dictation, loading the models first if they
    /// are not resident (a lazy reload after memory pressure or a crash).
    fn begin(&self, config: &Config, check: &dyn Fn() -> Result<()>) -> Result<Lease> {
        {
            let mut control = lock(&self.inner.control);
            if control.config.as_ref() != Some(config) {
                control.config = Some(config.clone());
            }
            // A shortcut press is an explicit retry and an explicit reload.
            control.gave_up = false;
            control.failures = 0;
            control.pressure_unloaded = false;
            control.pressure_cleared = None;
            control.last_used = Instant::now();
        }
        let mut slot = lock(&self.inner.slot);
        check()?;
        if slot.in_session {
            bail!("LIVE_PAIR_BUSY: Another live dictation is still finishing.");
        }
        ensure_loaded(&self.inner, &mut slot, config, check)?;
        slot.in_session = true;
        Ok(Lease {
            inner: self.inner.clone(),
            session_done: false,
        })
    }
}

fn set_status(inner: &Inner, status: LivePairStatus) {
    let mut current = lock(&inner.status);
    if *current != status {
        *current = status.clone();
        drop(current);
        (inner.notify)(&status);
    }
}

fn control_session(inner: &Inner) -> String {
    format!("ctl-{}", inner.control_ids.fetch_add(1, Ordering::SeqCst))
}

/// Starts the helper and loads + warms both models unless they are resident.
fn ensure_loaded(
    inner: &Inner,
    slot: &mut Slot,
    config: &Config,
    check: &dyn Fn() -> Result<()>,
) -> Result<()> {
    // A dead helper, or one built from another path, is replaced.
    if slot
        .process
        .as_mut()
        .is_some_and(|process| !process.alive() || process.helper != config.helper)
    {
        slot.process = None;
        slot.loaded = None;
    }
    if slot.process.is_some()
        && slot
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.same_models(config))
    {
        return Ok(());
    }
    set_status(
        inner,
        LivePairStatus::new(
            LivePairState::Preparing,
            Some("Loading the live-pair models".into()),
        ),
    );
    let result = (|| -> Result<Message> {
        if slot.process.is_none() {
            slot.process = Some(Process::launch(&config.helper)?);
        }
        let process = slot.process.as_mut().expect("process");
        slot.loaded = None;
        let ready = process.exchange(
            "load",
            &control_session(inner),
            0,
            json!({"nemotronPath": config.nemotron, "parakeetPath": config.parakeet}),
            LOAD_TIMEOUT,
            check,
        )?;
        if ready.kind != "ready" || ready.chunk_ms != config.chunk_ms {
            bail!("LIVE_PAIR_SETUP: The live-pair models do not match the selected chunk size.");
        }
        let warm = process.exchange(
            "warmup",
            &control_session(inner),
            0,
            json!({}),
            LOAD_TIMEOUT,
            check,
        )?;
        if warm.kind != "warm" {
            bail!("LIVE_PAIR_PROTOCOL: Unexpected warm-up response.");
        }
        Ok(ready)
    })();
    match result {
        Ok(ready) => {
            slot.loaded = Some(config.clone());
            eprintln!(
                "[live-pair] ready load_ms={:.0} rss_bytes={}",
                ready.load_ms, ready.rss_bytes
            );
            set_status(
                inner,
                LivePairStatus {
                    state: LivePairState::Ready,
                    message: None,
                    load_ms: Some(ready.load_ms as u64),
                    rss_bytes: Some(ready.rss_bytes),
                },
            );
            Ok(())
        }
        Err(error) => {
            // An interrupted or failed load leaves no half-loaded helper.
            slot.process = None;
            slot.loaded = None;
            Err(error)
        }
    }
}

/// Wall-clock time advancing much faster than monotonic time means the Mac slept.
struct WakeDetector {
    wall: SystemTime,
    mono: Instant,
}
impl WakeDetector {
    fn new() -> Self {
        Self {
            wall: SystemTime::now(),
            mono: Instant::now(),
        }
    }
    fn woke(&mut self) -> bool {
        let wall = SystemTime::now()
            .duration_since(self.wall)
            .unwrap_or_default();
        let mono = self.mono.elapsed();
        *self = Self::new();
        wall > mono + Duration::from_secs(5)
    }
}

fn supervise(weak: std::sync::Weak<Inner>) {
    let mut wake = WakeDetector::new();
    loop {
        let Some(inner) = weak.upgrade() else { return };
        {
            let control = lock(&inner.control);
            if control.shutdown {
                return;
            }
            drop(
                inner
                    .wake
                    .wait_timeout(control, TICK)
                    .unwrap_or_else(|error| error.into_inner()),
            );
        }
        if crate::shutdown::is_shutting_down() {
            let mut slot = lock(&inner.slot);
            slot.process = None;
            slot.loaded = None;
            return;
        }
        tick(&inner, wake.woke(), Instant::now());
    }
}

/// One supervisor pass. Never touches a helper that a dictation holds.
fn tick(inner: &Inner, woke: bool, now: Instant) {
    let (config, pressure_unloaded, pressure_cleared, retry_at, gave_up, last_used) = {
        let control = lock(&inner.control);
        if control.shutdown {
            return;
        }
        (
            control.config.clone(),
            control.pressure_unloaded,
            control.pressure_cleared,
            control.retry_at,
            control.gave_up,
            control.last_used,
        )
    };
    let mut slot = lock(&inner.slot);
    if slot.in_session {
        return;
    }
    let crashed = slot.process.as_mut().is_some_and(|process| !process.alive());
    if crashed {
        eprintln!("[live-pair] helper exited; restarting");
        slot.process = None;
        slot.loaded = None;
    }
    let Some(config) = config else {
        if slot.process.take().is_some() {
            slot.loaded = None;
        }
        let status = lock(&inner.status).state;
        if status != LivePairState::Error {
            set_status(inner, LivePairStatus::new(LivePairState::Off, None));
        }
        return;
    };
    if pressure_unloaded {
        if pressure_cleared.is_some_and(|at| now >= at + PRESSURE_RELOAD_DELAY) {
            let mut control = lock(&inner.control);
            control.pressure_unloaded = false;
            control.pressure_cleared = None;
            drop(control);
            inner.wake.notify_all();
            return;
        }
        if slot.loaded.is_some() {
            let session = control_session(inner);
            let unloaded = slot
                .process
                .as_mut()
                .map(|process| {
                    process.exchange("unload", &session, 0, json!({}), REQUEST_TIMEOUT, &|| {
                        Ok(())
                    })
                })
                .transpose();
            if unloaded.is_err() {
                slot.process = None;
            }
            slot.loaded = None;
            eprintln!("[live-pair] unloaded after critical memory pressure");
        }
        set_status(
            inner,
            LivePairStatus::new(
                LivePairState::Unloaded,
                Some("Unloaded under memory pressure. Reloads on the next dictation.".into()),
            ),
        );
        return;
    }
    if !config.keep_loaded {
        if slot.process.is_some() && now >= last_used + IDLE_EXIT {
            slot.process = None;
            slot.loaded = None;
        }
        if slot.process.is_none() {
            set_status(
                inner,
                LivePairStatus::new(
                    LivePairState::Off,
                    Some("Loads when you press the shortcut.".into()),
                ),
            );
        }
        return;
    }
    let resident = slot.process.is_some()
        && slot
            .loaded
            .as_ref()
            .is_some_and(|loaded| loaded.same_models(&config));
    if resident {
        if woke {
            let session = control_session(inner);
            let alive = slot.process.as_mut().is_some_and(|process| {
                process
                    .exchange("ping", &session, 0, json!({}), REQUEST_TIMEOUT, &|| Ok(()))
                    .is_ok_and(|pong| pong.loaded)
            });
            if !alive {
                eprintln!("[live-pair] helper unhealthy after wake; restarting");
                slot.process = None;
                slot.loaded = None;
                inner.wake.notify_all();
            }
        }
        return;
    }
    if gave_up || retry_at.is_some_and(|at| now < at) {
        return;
    }
    let result = ensure_loaded(inner, &mut slot, &config, &|| {
        if crate::shutdown::is_shutting_down() {
            bail!("DICTATION_CANCELED: Shutting down.");
        }
        Ok(())
    });
    drop(slot);
    let mut control = lock(&inner.control);
    if control.config.as_ref() != Some(&config) {
        return;
    }
    match result {
        Ok(()) => {
            control.failures = 0;
            control.retry_at = None;
        }
        Err(error) => {
            eprintln!("[live-pair] load failed: {error}");
            control.failures += 1;
            if let Some(delay) = BACKOFF.get(control.failures - 1) {
                control.retry_at = Some(now + *delay);
                drop(control);
                set_status(
                    inner,
                    LivePairStatus::new(
                        LivePairState::Preparing,
                        Some("Restarting the live-pair helper".into()),
                    ),
                );
            } else {
                control.gave_up = true;
                drop(control);
                set_status(
                    inner,
                    LivePairStatus::new(LivePairState::Error, Some(error.to_string())),
                );
            }
        }
    }
}

/// Exclusive use of the loaded helper for one dictation.
struct Lease {
    inner: Arc<Inner>,
    session_done: bool,
}
impl Lease {
    fn exchange(
        &mut self,
        kind: &str,
        session: &str,
        sequence: u64,
        fields: Value,
        timeout: Duration,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Message> {
        let mut slot = lock(&self.inner.slot);
        let process = slot
            .process
            .as_mut()
            .ok_or_else(|| anyhow!("LIVE_PAIR_WORKER_FAILED: The live-pair helper is gone."))?;
        let result = process.exchange(kind, session, sequence, fields, timeout, check);
        if let Err(error) = &result {
            if is_fatal(error) {
                slot.process = None;
                slot.loaded = None;
            }
        }
        result
    }

    /// Tells the helper to drop an unfinished session. `last` is the last
    /// request sequence sent; a reply still owed for it is skipped as stale. A
    /// helper that cannot confirm promptly is replaced rather than trusted with
    /// the next session.
    fn cancel(&mut self, session: &str, last: u64) {
        if self.session_done {
            return;
        }
        self.session_done = true;
        let mut slot = lock(&self.inner.slot);
        if let Some(process) = slot.process.as_mut() {
            if process
                .exchange("cancel", session, last + 1, json!({}), CANCEL_TIMEOUT, &|| {
                    Ok(())
                })
                .is_err()
            {
                slot.process = None;
                slot.loaded = None;
            }
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        lock(&self.inner.slot).in_session = false;
        lock(&self.inner.control).last_used = Instant::now();
        self.inner.wake.notify_all();
    }
}

// MARK: Live session

#[derive(Default)]
struct AudioState {
    samples: Vec<f32>,
    finished: bool,
    stop_requested: Option<Instant>,
    error: Option<String>,
    processed: usize,
    maximum_lag: usize,
}

pub(crate) struct Completion {
    pub text: String,
    pub language: Option<String>,
    /// Set when the final pass failed and the live preview text was used.
    pub warning: Option<String>,
    _work: crate::shutdown::WorkGuard,
}

#[derive(Clone)]
pub(crate) struct LiveSession {
    id: String,
    recording_id: String,
    state: Arc<Mutex<AudioState>>,
    result: Arc<Mutex<mpsc::Receiver<Result<Completion>>>>,
    cancelled: Arc<AtomicBool>,
    armed: Arc<AtomicBool>,
}

fn active(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() {
        bail!("DICTATION_CANCELED: Dictation was canceled.");
    }
    Ok(())
}

impl LiveSession {
    pub fn activate(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }
    fn check(&self) -> Result<()> {
        active(&self.cancelled)?;
        let state = lock(&self.state);
        if let Some(error) = &state.error {
            bail!("{error}");
        }
        if state
            .stop_requested
            .is_some_and(|stop| stop.elapsed() > Duration::from_secs(90))
        {
            bail!("LIVE_PAIR_FINALIZE_TIMEOUT: Processing exceeded 90 seconds after stopping. The recording can be recovered.");
        }
        Ok(())
    }
    /// Called only once capture has stopped and its last callback was drained.
    pub fn finish(&self, recording_id: &str, expected_samples: usize) -> Result<Completion> {
        if recording_id != self.recording_id {
            bail!("LIVE_PAIR_CAPTURE_ORDER: Recording belongs to another session.");
        }
        lock(&self.state).stop_requested = Some(Instant::now());
        loop {
            self.check()?;
            match lock(&self.result).recv_timeout(Duration::from_millis(20)) {
                Ok(result) => {
                    let completion = result?;
                    let state = lock(&self.state);
                    if !state.finished
                        || state.samples.len() != expected_samples
                        || state.processed != expected_samples
                    {
                        bail!("LIVE_PAIR_CAPTURE_ORDER: The final recording and streamed audio disagree. Nothing was pasted.");
                    }
                    return Ok(completion);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => bail!(
                    "LIVE_PAIR_WORKER_FAILED: No final transcript. The recording can be recovered."
                ),
            }
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }
}

fn phase(finished: bool, lag_ms: u64) -> StreamingState {
    if finished {
        StreamingState::Finalizing
    } else if lag_ms > 2000 {
        StreamingState::CatchingUp
    } else {
        StreamingState::Listening
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn start(
    id: String,
    tap: CaptureTap,
    cancelled: Arc<AtomicBool>,
    pair: LivePair,
    config: Config,
    shell: DesktopShellController,
    language: String,
    stop_capture: Arc<dyn Fn() + Send + Sync>,
) -> Result<LiveSession> {
    let work = crate::shutdown::begin_work(true)?;
    let (tx, rx) = mpsc::channel();
    let session = LiveSession {
        id: id.clone(),
        recording_id: tap.session_id.clone(),
        state: Default::default(),
        result: Arc::new(Mutex::new(rx)),
        cancelled,
        armed: Default::default(),
    };
    spawn_capture(&session, tap, shell.clone(), stop_capture.clone());
    let run = session.clone();
    std::thread::spawn(move || {
        let mut sequence = 0;
        let mut lease = None;
        let id = run.id.clone();
        let overlay = shell.clone();
        let ui = move |state: StreamingState, text: Option<StreamingText>, lag: u64| {
            let _ = overlay.update_stream(&id, state, text, lag);
        };
        let outcome = stream(&run, &pair, &config, &ui, &language, &mut lease, &mut sequence)
            .map(|(text, language, warning)| Completion {
                text,
                language,
                warning,
                _work: work,
            });
        if let Some(lease) = lease.as_mut() {
            lease.cancel(&run.id, sequence);
        }
        drop(lease);
        let failed = outcome.as_ref().is_err_and(|error| !canceled(error));
        let _ = tx.send(outcome);
        if failed && !run.cancelled.load(Ordering::SeqCst) {
            let _ = shell.update_stream(&run.id, StreamingState::Failed, None, 0);
            stop_capture();
        }
    });
    Ok(session)
}

/// Feeds the helper whole chunks as capture produces them, then finalizes.
fn stream(
    run: &LiveSession,
    pair: &LivePair,
    config: &Config,
    ui: &dyn Fn(StreamingState, Option<StreamingText>, u64),
    language: &str,
    lease: &mut Option<Lease>,
    sequence: &mut u64,
) -> Result<(String, Option<String>, Option<String>)> {
    while !run.armed.load(Ordering::SeqCst) {
        active(&run.cancelled)?;
        std::thread::sleep(Duration::from_millis(1));
    }
    let check = || run.check();
    let began = Instant::now();
    if pair.status().state != LivePairState::Ready {
        ui(StreamingState::Loading, None, 0);
    }
    let lease = lease.insert(pair.begin(config, &check)?);
    let load_ms = began.elapsed().as_millis();
    lease.exchange(
        "start",
        &run.id,
        0,
        json!({"language": language, "chunkMs": config.chunk_ms}),
        REQUEST_TIMEOUT,
        &check,
    )?;
    let chunk = config.chunk_ms as usize * 16;
    let mut cursor = 0;
    let mut committed = String::new();
    ui(StreamingState::Listening, None, 0);
    loop {
        run.check()?;
        let state = lock(&run.state);
        let available = state.samples.len() - cursor;
        let finished = state.finished;
        if available == 0 && finished {
            break;
        }
        if available < chunk && !finished {
            drop(state);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        let count = available.min(chunk);
        let samples: Vec<f32> = state.samples[cursor..cursor + count]
            .iter()
            .map(|sample| sample.clamp(-1.0, 1.0))
            .collect();
        drop(state);
        *sequence += 1;
        let progress = lease.exchange(
            "audio",
            &run.id,
            *sequence,
            json!({"startSample": cursor, "samples": samples}),
            REQUEST_TIMEOUT,
            &check,
        )?;
        cursor += count;
        // The preview may only grow; rewriting it is reserved for the final text.
        if progress.kind != "progress"
            || progress.ack_sample != cursor
            || !progress.committed_text.starts_with(&committed)
        {
            bail!("LIVE_PAIR_PROTOCOL: Inconsistent live preview. Nothing was pasted.");
        }
        committed = progress.committed_text;
        let mut state = lock(&run.state);
        state.processed = cursor;
        let lag = ((state.samples.len() - cursor) * 1000 / 16000) as u64;
        let finished = state.finished;
        drop(state);
        ui(
            phase(finished, lag),
            Some(StreamingText {
                committed: committed.clone(),
                tentative: progress.tentative_text,
            }),
            lag,
        );
    }
    ui(StreamingState::Finalizing, None, 0);
    *sequence += 1;
    let done = lease.exchange(
        "finish",
        &run.id,
        *sequence,
        json!({"expectedSamples": cursor}),
        FINISH_TIMEOUT,
        &check,
    )?;
    lease.session_done = true;
    run.check()?;
    if done.kind != "final" || done.ack_sample != cursor {
        bail!("LIVE_PAIR_PROTOCOL: Incomplete final response. Nothing was pasted.");
    }
    let (text, warning) = choose_text(&done);
    let state = lock(&run.state);
    let finalization = state
        .stop_requested
        .map(|stop| stop.elapsed().as_millis())
        .unwrap_or(0);
    eprintln!(
        "[live-pair] complete audio_ms={:.0} release_to_text_ms={finalization} flush_ms={:.0} final_ms={:.0} final_wait_ms={:.0} load_ms={load_ms} peak_lag_ms={} stream_error={} final_error={}",
        done.timings.audio_ms,
        done.timings.flush_ms,
        done.timings.final_ms,
        done.timings.final_wait_ms,
        state.maximum_lag * 1000 / 16000,
        if done.stream_error.is_empty() { "-" } else { wire_code(&done.stream_error) },
        if done.final_error.is_empty() { "-" } else { wire_code(&done.final_error) },
    );
    let language = (!done.language.is_empty()).then(|| done.language.clone());
    Ok((text, language, warning))
}

/// Parakeet's text is pasted. If the final pass failed, the live preview text
/// stands in, and history records why.
fn choose_text(done: &Message) -> (String, Option<String>) {
    if done.final_error.is_empty() {
        (done.final_text.trim().to_string(), None)
    } else {
        (
            done.stream_text.trim().to_string(),
            Some(format!(
                "{}: The final pass failed, so the live preview text was used.",
                wire_code(&done.final_error)
            )),
        )
    }
}

// MARK: Headless (benchmarks)

/// Load timings of a headless helper.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlessLoad {
    /// Process start → both models loaded (wall clock).
    pub load_ms: u64,
    /// Model load as reported by the helper (includes a first CoreML compile).
    pub helper_load_ms: f64,
    pub warmup_ms: u64,
    pub rss_bytes: u64,
}

/// One helper process driven directly from 16 kHz samples, with the same
/// protocol and chunking as live dictation but without the supervisor.
pub struct HeadlessLivePair {
    process: Process,
    chunk_ms: u32,
    sessions: u64,
}
impl HeadlessLivePair {
    pub fn load(
        helper: &Path,
        nemotron: &Path,
        parakeet: &Path,
        chunk_ms: u32,
    ) -> Result<(Self, HeadlessLoad)> {
        let began = Instant::now();
        let mut process = Process::launch(helper)?;
        let none = || Ok(());
        let ready = process.exchange(
            "load",
            "ctl-load",
            0,
            json!({"nemotronPath": nemotron, "parakeetPath": parakeet}),
            LOAD_TIMEOUT,
            &none,
        )?;
        if ready.kind != "ready" || ready.chunk_ms != chunk_ms {
            bail!("LIVE_PAIR_SETUP: The live-pair models do not match the selected chunk size.");
        }
        let load_ms = began.elapsed().as_millis() as u64;
        let warming = Instant::now();
        let warm = process.exchange("warmup", "ctl-warm", 0, json!({}), LOAD_TIMEOUT, &none)?;
        if warm.kind != "warm" {
            bail!("LIVE_PAIR_PROTOCOL: Unexpected warm-up response.");
        }
        let load = HeadlessLoad {
            load_ms,
            helper_load_ms: ready.load_ms,
            warmup_ms: warming.elapsed().as_millis() as u64,
            rss_bytes: warm.rss_bytes.max(ready.rss_bytes),
        };
        Ok((
            Self {
                process,
                chunk_ms,
                sessions: 0,
            },
            load,
        ))
    }

    /// `language` is "auto" or a code accepted by `source_language`.
    pub fn transcribe(
        &mut self,
        samples: &[f32],
        language: &str,
        paced: bool,
    ) -> Result<HeadlessRun> {
        if samples.len() > MAX_SAMPLES {
            bail!("LIVE_PAIR_LIMIT: Live dictation accepts at most five minutes of audio.");
        }
        self.sessions += 1;
        let id = format!("bench-{}", self.sessions);
        let none = || Ok(());
        let feed = HeadlessFeed::new(samples, paced);
        let started = self.process.exchange(
            "start",
            &id,
            0,
            json!({"language": language, "chunkMs": self.chunk_ms}),
            REQUEST_TIMEOUT,
            &none,
        )?;
        let mut peak_rss = started.rss_bytes;
        let chunk = self.chunk_ms as usize * 16;
        let mut sequence = 0;
        let mut cursor = 0;
        let mut committed = String::new();
        let mut first_text_ms = None;
        loop {
            let count = feed.next_chunk(cursor, chunk);
            if count == 0 {
                break;
            }
            sequence += 1;
            let progress = self.process.exchange(
                "audio",
                &id,
                sequence,
                json!({"startSample": cursor, "samples": feed.chunk(cursor, count)}),
                REQUEST_TIMEOUT,
                &none,
            )?;
            cursor += count;
            if progress.kind != "progress"
                || progress.ack_sample != cursor
                || !progress.committed_text.starts_with(&committed)
            {
                bail!("LIVE_PAIR_PROTOCOL: Inconsistent live preview.");
            }
            peak_rss = peak_rss.max(progress.rss_bytes);
            if first_text_ms.is_none()
                && !(progress.committed_text.trim().is_empty()
                    && progress.tentative_text.trim().is_empty())
            {
                first_text_ms = Some(feed.started().elapsed().as_millis() as u64);
            }
            committed = progress.committed_text;
        }
        sequence += 1;
        let done = self.process.exchange(
            "finish",
            &id,
            sequence,
            json!({"expectedSamples": cursor}),
            FINISH_TIMEOUT,
            &none,
        )?;
        let finished = Instant::now();
        if done.kind != "final" || done.ack_sample != cursor {
            bail!("LIVE_PAIR_PROTOCOL: Incomplete final response.");
        }
        let (text, warning) = choose_text(&done);
        Ok(HeadlessRun {
            text,
            stream_text: Some(done.stream_text.trim().to_string()),
            language: (!done.language.is_empty()).then(|| done.language.clone()),
            first_text_ms,
            release_to_text_ms: paced
                .then(|| finished.saturating_duration_since(feed.release()).as_millis() as u64),
            total_ms: finished.duration_since(feed.started()).as_millis() as u64,
            peak_rss_bytes: Some(peak_rss.max(done.rss_bytes)),
            warning,
        })
    }

}

/// Normalizes captured audio to 16 kHz mono as it arrives.
fn spawn_capture(
    session: &LiveSession,
    tap: CaptureTap,
    shell: DesktopShellController,
    stop: Arc<dyn Fn() + Send + Sync>,
) {
    let capture = session.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut normalizer = StreamingNormalizer::new(tap.rate, tap.channels)?;
            let mut cursor = 0;
            let mut published = 0;
            let mut last_capture = Instant::now();
            let mut limit_requested = false;
            loop {
                active(&capture.cancelled)?;
                let part = tap.read_since(cursor, 32_768)?;
                if !part.is_empty() {
                    cursor += part.len();
                    last_capture = Instant::now();
                    normalizer.push(&part)?;
                }
                let mut state = lock(&capture.state);
                let stable = normalizer.stable_samples();
                state.samples.extend_from_slice(&stable[published..]);
                published = stable.len();
                state.maximum_lag = state.maximum_lag.max(state.samples.len() - state.processed);
                if state.stop_requested.is_some() && cursor == tap.sample_count() {
                    let final_audio = normalizer.finish()?;
                    state
                        .samples
                        .extend_from_slice(&final_audio.samples[published..]);
                    state.finished = true;
                    break;
                }
                let limit = cursor >= tap.rate as usize * tap.channels as usize * 300;
                drop(state);
                if limit && !limit_requested {
                    limit_requested = true;
                    shell.stream_duration_limit(&capture.id);
                    stop();
                }
                if !limit && last_capture.elapsed() > Duration::from_secs(5) {
                    bail!("LIVE_PAIR_CAPTURE_STALLED: The microphone stopped delivering audio. The captured recording can be recovered.");
                }
                if cursor == tap.sample_count() {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            if !capture.cancelled.load(Ordering::SeqCst) {
                lock(&capture.state).error = Some(error.to_string());
                let _ = shell.update_stream(&capture.id, StreamingState::Failed, None, 0);
                stop();
            }
        }
    });
}

#[cfg(test)]
mod tests;

fn transcript_base(
    id: &str,
    text: String,
    duration_ms: i64,
    language: Option<String>,
) -> crate::asr::TranscriptResult {
    use crate::asr::{TranscriptQualityStatus, TranscriptResult, TranscriptSegment};
    let language = language.map(|value| crate::output_format::normalize_language_code(Some(&value)));
    let timestamped_text = crate::output_format::timestamped_line(
        0,
        duration_ms,
        language.as_deref().unwrap_or("und"),
        &text,
    );
    TranscriptResult {
        job_id: id.into(),
        model_name: MODEL_NAME.into(),
        full_text: text.clone(),
        plain_text: text.clone(),
        timestamped_text,
        detected_languages: language.iter().cloned().collect(),
        segments: vec![TranscriptSegment {
            id: format!("{id}:0"),
            start_ms: 0,
            end_ms: duration_ms,
            text,
            language_code: language.unwrap_or_else(|| "und".into()),
            segment_order: 0,
            confidence: None,
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: Default::default(),
            speaker_confidence: None,
        }],
        quality_status: TranscriptQualityStatus::Clean,
        recovered_region_count: 0,
        warnings: vec![],
        diarization_status: Default::default(),
        diarization_model_id: None,
        diarization_source: Default::default(),
        diarization_warning: None,
        diarization_policy_version: None,
        diarization_clustering_threshold: None,
        diarization_speaker_count_hint: None,
        speakers: vec![],
        diarization_turns: vec![],
    }
}

/// One streaming transcription without capture or UI, timed like a dictation.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlessRun {
    pub text: String,
    /// Preview text at release, when the engine's final text differs from it.
    pub stream_text: Option<String>,
    pub language: Option<String>,
    /// Feed start (shortcut press) → first non-empty preview.
    pub first_text_ms: Option<u64>,
    /// All audio captured (shortcut release) → final text. Paced runs only.
    pub release_to_text_ms: Option<u64>,
    /// Feed start → final text.
    pub total_ms: u64,
    pub peak_rss_bytes: Option<u64>,
    pub warning: Option<String>,
}

/// Offers recorded audio the way capture does: in real time when paced, all at
/// once otherwise.
pub struct HeadlessFeed<'a> {
    samples: &'a [f32],
    started: Instant,
    paced: bool,
}
impl<'a> HeadlessFeed<'a> {
    pub fn new(samples: &'a [f32], paced: bool) -> Self {
        Self {
            samples,
            started: Instant::now(),
            paced,
        }
    }
    pub fn started(&self) -> Instant {
        self.started
    }
    fn captured_at(&self, samples: usize) -> Instant {
        self.started + Duration::from_micros(samples as u64 * 1_000_000 / 16_000)
    }
    /// When the last sample is captured.
    pub fn release(&self) -> Instant {
        if self.paced {
            self.captured_at(self.samples.len())
        } else {
            self.started
        }
    }
    /// Waits until a whole chunk after `cursor` is captured, or capture ended,
    /// and returns how many samples to send (0 once everything was sent).
    pub fn next_chunk(&self, cursor: usize, chunk: usize) -> usize {
        let remaining = self.samples.len().saturating_sub(cursor);
        let count = remaining.min(chunk);
        if self.paced {
            let ready = self.captured_at(cursor + count);
            let now = Instant::now();
            if ready > now {
                std::thread::sleep(ready - now);
            }
        }
        count
    }
    pub fn chunk(&self, cursor: usize, count: usize) -> Vec<f32> {
        self.samples[cursor..cursor + count]
            .iter()
            .map(|sample| sample.clamp(-1.0, 1.0))
            .collect()
    }
}
