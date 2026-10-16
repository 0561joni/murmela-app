//! Local post-ASR translation. Source transcripts are never rewritten.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::asr::{FileTranscriptionRequest, LocalTranscriptionEngine, TranscriptResult};
use crate::audio_capture::{RecordingController, RecordingOverlayState};
use crate::desktop_shell::{DesktopShellController, DictationOverlayPayload, OverlayPhase};
use crate::review_jobs::{ProcessingPermit, ProcessingQueue};
use crate::storage;

pub(crate) const WARM_ASR_OWNER: &str = "dictation-asr";

/// A persistent `--transcribe-worker-persistent` child that keeps its model
/// loaded between dictations. Dropping it kills and reaps its process group.
pub struct WarmAsrWorker {
    child: crate::managed_process::ManagedChild,
    input: std::process::ChildStdin,
    output: mpsc::Receiver<Result<crate::transcription_worker::WorkerOutput, String>>,
}

impl WarmAsrWorker {
    fn spawn() -> Result<Self> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg(crate::transcription_worker::PERSISTENT_WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::managed_process::isolate(&mut command);
        let mut child = crate::managed_process::ManagedChild::new(command.spawn()?);
        let input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("ASR input unavailable"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("ASR output unavailable"))?;
        let (tx, output) = mpsc::channel();
        crate::transcription_worker::read_worker_output_lines(stdout, tx);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    /// Reuse a cached worker when it is still clean; otherwise start a new one.
    pub(crate) fn reuse_or_spawn(cached: Option<Self>) -> Result<Self> {
        if let Some(mut worker) = cached {
            if worker.is_reusable() {
                return Ok(worker);
            }
        }
        Self::spawn()
    }

    /// Send one request and wait for its final answer: `Some` for a
    /// transcription, `None` once a preload request reports the model ready.
    pub(crate) fn exchange(
        &mut self,
        request: &crate::transcription_worker::WorkerRequest,
        deadline: Instant,
        check: impl Fn() -> Result<()>,
    ) -> Result<Option<TranscriptResult>> {
        use crate::transcription_worker::WorkerOutput;
        self.send(request)?;
        loop {
            check()?;
            if Instant::now() > deadline {
                bail!("Dictation transcription timed out.");
            }
            match self.output.recv_timeout(Duration::from_millis(100)) {
                Ok(Ok(WorkerOutput::Result { result })) if !request.preload => {
                    return Ok(Some(result))
                }
                Ok(Ok(WorkerOutput::Ready)) if request.preload => return Ok(None),
                Ok(Ok(WorkerOutput::Result { .. } | WorkerOutput::Ready)) => {
                    bail!("Dictation transcription runtime answered out of order.")
                }
                Ok(Ok(WorkerOutput::Error { message })) => bail!("{message}"),
                Ok(Ok(WorkerOutput::Progress { .. } | WorkerOutput::Heartbeat { .. })) => {}
                Ok(Err(error)) => bail!("{error}"),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("Dictation transcription runtime exited unexpectedly.");
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Alive, and nothing left over from an earlier request.
    pub(crate) fn is_reusable(&mut self) -> bool {
        if !matches!(self.child.try_wait(), Ok(None)) {
            return false;
        }
        loop {
            match self.output.try_recv() {
                Ok(_) => return false,
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => return false,
            }
        }
    }

    fn send(&mut self, request: &crate::transcription_worker::WorkerRequest) -> Result<()> {
        serde_json::to_writer(&mut self.input, request)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        Ok(())
    }
}

fn wav_duration_seconds(path: &Path) -> Option<u64> {
    let reader = hound::WavReader::open(path).ok()?;
    let spec = reader.spec();
    let frames = reader.duration() as u64;
    (spec.sample_rate > 0).then(|| frames / spec.sample_rate as u64)
}

pub const MODEL_ID: &str = "translategemma-12b-q6-k";
/// The Gemma Terms of Use version users accept before downloading TranslateGemma
/// ("Last modified: April 1, 2026"). Bump it when Google changes the terms.
pub const GEMMA_TERMS_VERSION: &str = "gemma-terms-2026-04-01";
pub const MODEL_FILE: &str = "translategemma-12b-it.Q6_K.gguf";
pub const MODEL_REVISION: &str = "1076826a801dbc6cc8ad4ff4689a3272dcb8a378";
pub const MODEL_SHA256: &str = "c30995b3c145e6ef3b3a6fda63749186d83d9c8f16725ff1403c904b5e0ead8c";
pub const MODEL_SIZE: i64 = 9_660_827_392;
/// TranslateGemma 4B: about a third of the memory, for Macs where 12B does not fit.
pub const COMPACT_MODEL_ID: &str = "translategemma-4b-q6-k";
pub const PROMPT_VERSION: u32 = 5;
/// Line protocol of the resident translation helper (workers/translation).
pub(crate) const WORKER_PROTOCOL: u64 = 3;

/// A downloadable TranslateGemma build. All of them run on the same helper and prompt.
#[derive(Debug)]
pub struct TranslationModel {
    pub id: &'static str,
    pub name: &'static str,
    pub file: &'static str,
    pub revision: &'static str,
    pub sha256: &'static str,
    pub size: i64,
    pub url: &'static str,
}

/// Best quality first: it wins when both are installed and nothing is chosen.
pub const MODELS: [TranslationModel; 2] = [
    TranslationModel {
        id: MODEL_ID,
        name: "TranslateGemma 12B",
        file: MODEL_FILE,
        revision: MODEL_REVISION,
        sha256: MODEL_SHA256,
        size: MODEL_SIZE,
        url: "https://huggingface.co/mradermacher/translategemma-12b-it-GGUF/resolve/1076826a801dbc6cc8ad4ff4689a3272dcb8a378/translategemma-12b-it.Q6_K.gguf",
    },
    TranslationModel {
        id: COMPACT_MODEL_ID,
        name: "TranslateGemma 4B",
        file: "translategemma-4b-it.Q6_K.gguf",
        revision: "d37868f759d2f817a43d28dd80cf9f982ffa62fd",
        sha256: "9848d1c4c61020511d278e1a82d460f198d3214e19d74344ef09446cdc3cb544",
        size: 3_190_755_840,
        url: "https://huggingface.co/mradermacher/translategemma-4b-it-GGUF/resolve/d37868f759d2f817a43d28dd80cf9f982ffa62fd/translategemma-4b-it.Q6_K.gguf",
    },
];

pub fn model_spec(id: &str) -> Option<&'static TranslationModel> {
    MODELS.iter().find(|model| model.id == id)
}
/// Control-Option-L ("L for language"); also the default in schema.sql.
pub const DEFAULT_SHORTCUT: &str = "Ctrl+Alt+L";
type ModelFingerprint = (u64, SystemTime);
#[derive(Default)]
enum Verification {
    #[default]
    Unchecked,
    Checking(ModelFingerprint),
    Valid(ModelFingerprint),
    Invalid(ModelFingerprint, String),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    #[default]
    #[serde(rename = "original")]
    Original,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "de")]
    German,
    #[serde(rename = "fr")]
    French,
    #[serde(rename = "es")]
    Spanish,
    #[serde(rename = "es-AR")]
    SpanishArgentina,
    #[serde(rename = "it")]
    Italian,
    #[serde(rename = "pt-BR")]
    PortugueseBrazil,
    #[serde(rename = "nl")]
    Dutch,
    #[serde(rename = "pl")]
    Polish,
}
/// Dictation output languages in the order the language shortcut visits them
/// (the Translate screen's order).
pub const OUTPUT_LANGUAGES: [OutputMode; 9] = [
    OutputMode::English,
    OutputMode::German,
    OutputMode::French,
    OutputMode::Spanish,
    OutputMode::SpanishArgentina,
    OutputMode::Italian,
    OutputMode::PortugueseBrazil,
    OutputMode::Dutch,
    OutputMode::Polish,
];
/// Languages the shortcut visits until the user picks others.
pub const DEFAULT_CYCLE_LANGUAGES: [&str; 4] = ["en", "de", "fr", "es-AR"];
impl OutputMode {
    pub fn code(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::English => "en",
            Self::German => "de",
            Self::French => "fr",
            Self::Spanish => "es",
            Self::SpanishArgentina => "es-AR",
            Self::Italian => "it",
            Self::PortugueseBrazil => "pt-BR",
            Self::Dutch => "nl",
            Self::Polish => "pl",
        }
    }
    /// An output language (never Original) by its code.
    pub fn language(code: &str) -> Option<Self> {
        OUTPUT_LANGUAGES.into_iter().find(|mode| mode.code() == code)
    }
    /// The next stop after this one: Original, then the `cycle` languages in
    /// `OUTPUT_LANGUAGES` order. Works from a language outside `cycle` too.
    pub fn next(self, cycle: &[OutputMode]) -> Self {
        Self::step(self, cycle, false)
    }
    pub fn previous(self, cycle: &[OutputMode]) -> Self {
        Self::step(self, cycle, true)
    }
    fn step(self, cycle: &[OutputMode], back: bool) -> Self {
        let mut stops = vec![Self::Original];
        stops.extend(OUTPUT_LANGUAGES);
        if back {
            stops.reverse();
        }
        let from = stops.iter().position(|mode| *mode == self).unwrap_or(0);
        stops
            .iter()
            .cycle()
            .skip(from + 1)
            .take(stops.len())
            .copied()
            .find(|mode| *mode == Self::Original || cycle.contains(mode))
            .unwrap_or(Self::Original)
    }
    /// Whether a dictation detected only in `languages` is already in this
    /// language, so translating it would only paraphrase it.
    pub fn already_in(self, languages: &[String]) -> bool {
        let code = match self {
            Self::Original => return false,
            Self::SpanishArgentina => "es",
            Self::PortugueseBrazil => "pt",
            other => other.code(),
        };
        !languages.is_empty() && languages.iter().all(|language| language == code)
    }
}
/// The stored cycle setting as output languages; unknown codes are skipped.
pub fn cycle_languages(codes: &[String]) -> Vec<OutputMode> {
    codes.iter().filter_map(|code| OutputMode::language(code)).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationOutput {
    pub session_id: String,
    pub source_text: String,
    pub output_text: Option<String>,
    pub target_language: OutputMode,
    pub status: String,
    pub model_id: Option<String>,
    pub model_revision: Option<String>,
    pub prompt_version: Option<u32>,
    pub transcript_id: Option<String>,
    pub error_message: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub source_languages: Vec<String>,
    /// The translation failed its checks twice; dictation pastes the
    /// original text with a warning instead.
    #[serde(default)]
    pub fallback_to_original: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputState {
    pub output_mode: OutputMode,
    pub busy: bool,
    pub stage: String,
    pub status_text: String,
    pub ready: bool,
    pub error_message: Option<String>,
    pub last_output: Option<DictationOutput>,
}

#[derive(Clone)]
pub struct DictationSession {
    pub id: String,
    pub mode: OutputMode,
    pub cancelled: Arc<AtomicBool>,
    pub save_history: bool,
    pub prefer_gpu: bool,
    pub recording_id: Option<String>,
    pub recording_path: Option<String>,
}
/// A translated output language falls back to Original after this much idle
/// time, so a later dictation is not translated by accident.
const ORIGINAL_RESET_AFTER: Duration = Duration::from_secs(60);

#[derive(Default)]
struct SessionState {
    mode: OutputMode,
    active: Option<DictationSession>,
    stage: String,
    status_text: String,
    last_output: Option<DictationOutput>,
    /// Bumped on every language change and dictation end; a pending reset
    /// only applies if nothing happened since it was armed.
    reset_revision: u64,
}
impl SessionState {
    /// Restarts the countdown back to Original. Returns the revision the
    /// countdown must still match when it ends, or None if no reset is due.
    fn arm_original_reset(&mut self) -> Option<u64> {
        self.reset_revision = self.reset_revision.wrapping_add(1);
        (self.mode != OutputMode::Original && self.active.is_none()).then_some(self.reset_revision)
    }
    /// Switches back to Original if the countdown is still current and no
    /// dictation or recording is running. Returns true when the mode changed.
    fn expire_original_reset(&mut self, revision: u64, recording: bool) -> bool {
        if revision != self.reset_revision
            || self.active.is_some()
            || recording
            || self.mode == OutputMode::Original
        {
            return false;
        }
        self.mode = OutputMode::Original;
        true
    }
    /// Sets the language for the next dictation. A dictation still recording
    /// takes the new language too: it only applies once the recording ends.
    fn change_mode(&mut self, mode: OutputMode, recording: bool) -> Result<()> {
        match self.active.as_mut() {
            Some(session) if recording && self.stage == "recording" => session.mode = mode,
            None if !recording => {}
            _ => bail!("Finish or cancel the current dictation before changing its language."),
        }
        self.mode = mode;
        Ok(())
    }
    fn finish(&mut self, id: &str) -> bool {
        if !self.active.as_ref().is_some_and(|s| s.id == id) {
            return false;
        }
        if let Some(session) = self.active.take() {
            session.cancelled.store(true, Ordering::SeqCst);
        }
        self.stage = "idle".into();
        self.status_text.clear();
        true
    }
}

#[derive(Debug)]
struct WorkerFailure(String);
impl std::fmt::Display for WorkerFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Translation failed ({}). The original text is available.",
            self.0
        )
    }
}
impl std::error::Error for WorkerFailure {}

#[derive(Default)]
struct WorkerProtocol {
    chunk_count: Option<u64>,
    next_chunk: u64,
    complete: bool,
}
enum WorkerEvent {
    Ready,
    Chunk(u64, u64),
    Result(String),
}
impl WorkerProtocol {
    fn accept(&mut self, id: &str, value: &serde_json::Value) -> Result<WorkerEvent> {
        if value["version"] != WORKER_PROTOCOL
            || value["promptVersion"] != PROMPT_VERSION
            || value["requestId"].as_str() != Some(id)
            || self.complete
        {
            bail!("Translation protocol mismatch.");
        }
        match value["type"].as_str() {
            Some("error") => {
                Err(WorkerFailure(value["code"].as_str().unwrap_or("WORKER_FAILED").into()).into())
            }
            Some("ready") if self.chunk_count.is_none() => {
                let count = value["chunkCount"]
                    .as_u64()
                    .filter(|c| *c > 0)
                    .ok_or_else(|| anyhow!("Invalid translation chunk count."))?;
                self.chunk_count = Some(count);
                Ok(WorkerEvent::Ready)
            }
            Some("progress") if self.chunk_count.is_some() => {
                let count = self.chunk_count.unwrap();
                if value["chunkIndex"].as_u64() != Some(self.next_chunk)
                    || value["chunkCount"].as_u64() != Some(count)
                    || self.next_chunk >= count
                {
                    bail!("Invalid translation progress.");
                }
                self.next_chunk += 1;
                Ok(WorkerEvent::Chunk(self.next_chunk, count))
            }
            Some("result")
                if self.chunk_count == Some(self.next_chunk) && value["completed"] == true =>
            {
                let text = value["text"]
                    .as_str()
                    .filter(|t| !t.trim().is_empty())
                    .ok_or_else(|| anyhow!("Translation was empty."))?;
                self.complete = true;
                Ok(WorkerEvent::Result(text.into()))
            }
            _ => bail!("Invalid or incomplete translation worker response."),
        }
    }
}

#[derive(Clone)]
pub struct TranslationService {
    app: AppHandle,
    db_path: PathBuf,
    models_dir: PathBuf,
    engine: Arc<LocalTranscriptionEngine>,
    recording: RecordingController,
    shell: DesktopShellController,
    queue: ProcessingQueue,
    state: Arc<Mutex<SessionState>>,
    verified: Arc<Mutex<std::collections::HashMap<PathBuf, Verification>>>,
    resident: crate::resident_translator::ResidentTranslator,
    _memory_pressure: Arc<crate::memory_pressure::MemoryPressureWatch>,
    _translator_pressure: Arc<crate::memory_pressure::MemoryPressureWatch>,
}
pub struct SessionGuard {
    service: TranslationService,
    id: String,
    armed: bool,
}
impl SessionGuard {
    pub fn disarm(mut self) {
        self.armed = false;
    }
}
impl Drop for SessionGuard {
    fn drop(&mut self) {
        if self.armed {
            self.service.finish(&self.id);
        }
    }
}

impl TranslationService {
    pub fn new(
        app: AppHandle,
        db_path: PathBuf,
        models_dir: PathBuf,
        engine: Arc<LocalTranscriptionEngine>,
        recording: RecordingController,
        shell: DesktopShellController,
        queue: ProcessingQueue,
    ) -> Self {
        let resident = crate::resident_translator::ResidentTranslator::default();
        let evict = resident.clone();
        Self {
            _memory_pressure: Arc::new(crate::memory_pressure::MemoryPressureWatch::new(queue.clone())),
            // Warning or critical memory pressure releases an idle translation model.
            _translator_pressure: Arc::new(crate::memory_pressure::MemoryPressureWatch::with_handler(
                0x2 | 0x4,
                move |_| evict.evict(),
            )),
            resident,
            app,
            db_path,
            models_dir,
            engine,
            recording,
            shell,
            queue,
            state: Default::default(),
            verified: Default::default(),
        }
    }
    pub fn ready(&self) -> Result<()> {
        if !platform_supported() {
            bail!("Translation currently requires Apple Silicon.");
        }
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        if !settings.translation_enabled {
            bail!("Turn on Translate while you dictate in Settings → Languages.");
        }
        let (spec, model) = self.selected_model()?;
        if helper_path(&self.app).is_none() {
            bail!("The bundled translation runtime is missing. Rebuild or reinstall Murmela.");
        }
        self.ensure_verified(spec, &model)?;
        Ok(())
    }
    /// The model chosen in Settings, or another installed one when it is missing.
    pub(crate) fn selected_model(&self) -> Result<(&'static TranslationModel, PathBuf)> {
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        self.installed_model(Some(&settings.translation_model_id))
    }
    /// `preferred` when installed, else the best installed model.
    pub(crate) fn installed_model(
        &self,
        preferred: Option<&str>,
    ) -> Result<(&'static TranslationModel, PathBuf)> {
        let installed = |spec: &'static TranslationModel| {
            crate::model_downloads::installed_translation_model_path(&self.models_dir, spec.id)
                .map(|path| (spec, path))
        };
        preferred
            .and_then(model_spec)
            .and_then(installed)
            .or_else(|| MODELS.iter().find_map(installed))
            .ok_or_else(|| anyhow!("Download the translation model in Settings → Languages."))
    }
    /// Loads the translation model in the background while the user dictates,
    /// when translation is ready and macOS has enough free memory.
    fn preload_translation(&self, prefer_gpu: bool) {
        if self.ready().is_err() {
            return;
        }
        let (Ok((spec, model)), Some(helper)) = (self.selected_model(), helper_path(&self.app)) else {
            return;
        };
        self.resident.preload(helper, model, prefer_gpu, spec.size as u64);
    }
    /// Releases a loaded translation model, for example when translation is turned off.
    pub fn release_translation_model(&self) {
        self.resident.evict();
    }
    fn ensure_verified(&self, spec: &'static TranslationModel, path: &Path) -> Result<()> {
        let metadata = path.metadata()?;
        let fingerprint = (metadata.len(), metadata.modified()?);
        let mut verified = self
            .verified
            .lock()
            .map_err(|_| anyhow!("Model state unavailable"))?;
        match verified.get(path).unwrap_or(&Verification::Unchecked) {
            Verification::Valid(value) if *value == fingerprint => return Ok(()),
            Verification::Invalid(value, message) if *value == fingerprint => bail!("{message}"),
            Verification::Checking(value) if *value == fingerprint => {
                bail!("Verifying the translation model. Please wait…")
            }
            _ => {}
        }
        verified.insert(path.to_owned(), Verification::Checking(fingerprint));
        drop(verified);
        let service = self.clone();
        let path = path.to_owned();
        std::thread::spawn(move || {
            let result = service.verify_model(
                spec,
                &path,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(90),
            );
            if let Err(error) = result {
                if let Ok(mut state) = service.verified.lock() {
                    if matches!(state.get(&path), Some(Verification::Checking(value)) if *value == fingerprint) {
                        state.insert(path.clone(), Verification::Invalid(fingerprint, error.to_string()));
                    }
                }
            }
            service.publish();
        });
        bail!("Verifying the translation model. Please wait…")
    }
    pub fn snapshot(&self) -> OutputState {
        let error = self.ready().err().map(|e| e.to_string());
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        OutputState {
            output_mode: state.mode,
            busy: state.active.is_some(),
            stage: state.stage.clone(),
            status_text: state.status_text.clone(),
            ready: error.is_none(),
            error_message: error,
            last_output: state.last_output.clone(),
        }
    }
    fn publish(&self) {
        let _ = self.app.emit("dictation-output-status", self.snapshot());
    }
    pub fn set_mode(&self, mode: OutputMode) -> Result<OutputState> {
        self.change_mode(mode)
    }
    /// Steps to the next language the user picked for the shortcut.
    pub fn cycle(&self) -> Result<OutputState> {
        self.step_mode(false)
    }
    pub fn cycle_back(&self) -> Result<OutputState> {
        self.step_mode(true)
    }
    fn step_mode(&self, back: bool) -> Result<OutputState> {
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        let cycle = cycle_languages(&settings.translation_cycle_languages);
        let current = self
            .state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?
            .mode;
        self.change_mode(if back {
            current.previous(&cycle)
        } else {
            current.next(&cycle)
        })
    }
    fn is_recording(&self) -> bool {
        self.recording.status().is_ok_and(|s| {
            matches!(
                s.state,
                RecordingOverlayState::Listening
            )
        })
    }
    fn schedule_original_reset(&self, revision: Option<u64>) {
        let Some(revision) = revision else { return };
        let service = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(ORIGINAL_RESET_AFTER);
            service.expire_original_reset(revision);
        });
    }
    fn expire_original_reset(&self, revision: u64) {
        let recording = self.is_recording();
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if recording && state.reset_revision == revision && state.active.is_none() {
            // The microphone is busy (for example a microphone test): wait another round.
            let next = state.arm_original_reset();
            drop(state);
            self.schedule_original_reset(next);
            return;
        }
        let changed = state.expire_original_reset(revision, recording);
        drop(state);
        if changed {
            self.shell.set_output_mode(OutputMode::Original);
            self.publish();
        }
    }
    fn change_mode(&self, mode: OutputMode) -> Result<OutputState> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?;
        if mode != OutputMode::Original {
            self.ready()?;
        }
        state.change_mode(mode, self.is_recording())?;
        let listening = state.active.is_some();
        let reset = state.arm_original_reset();
        drop(state);
        self.schedule_original_reset(reset);
        if mode != OutputMode::Original {
            // Switching to a translated language means a dictation is coming.
            if let Ok(settings) = storage::get_settings_from_db_path(&self.db_path) {
                self.preload_translation(settings.gpu_enabled);
            }
        }
        if listening {
            self.shell.show_output_mode(mode)?;
        } else {
            self.shell.set_output_mode(mode);
            self.shell.flash_mode()?;
        }
        self.publish();
        Ok(self.snapshot())
    }
    pub fn begin(&self) -> Result<DictationSession> {
        crate::shutdown::ensure_running()?;
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?;
        if state.active.is_some() {
            bail!("Another dictation is still active.");
        }
        if state.mode != OutputMode::Original {
            self.ready()?;
        }
        let session = DictationSession {
            id: uuid::Uuid::new_v4().to_string(),
            mode: state.mode,
            cancelled: Arc::new(AtomicBool::new(false)),
            save_history: settings.save_history,
            prefer_gpu: settings.gpu_enabled,
            recording_id: None,
            recording_path: None,
        };
        state.active = Some(session.clone());
        state.last_output = None;
        state.stage = "recording".into();
        state.status_text = "Listening".into();
        drop(state);
        if session.mode != OutputMode::Original {
            // Load while the user speaks, so translating starts at once on release.
            self.preload_translation(session.prefer_gpu);
        }
        self.shell.set_output_mode(session.mode);
        self.publish();
        Ok(session)
    }
    pub fn current(&self) -> Result<DictationSession> {
        self.state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?
            .active
            .clone()
            .ok_or_else(|| anyhow!("Dictation was canceled."))
    }
    pub(crate) fn models_dir(&self) -> PathBuf {
        self.models_dir.clone()
    }
    pub(crate) fn processing_queue(&self) -> ProcessingQueue {
        self.queue.clone()
    }
    pub(crate) fn resident(&self) -> &crate::resident_translator::ResidentTranslator {
        &self.resident
    }
    pub(crate) fn app_handle(&self) -> &AppHandle {
        &self.app
    }
    pub(crate) fn prefer_gpu(&self) -> bool {
        storage::get_settings_from_db_path(&self.db_path).map_or(true, |settings| settings.gpu_enabled)
    }
    pub(crate) fn release_asr_resources(&self) {
        self.engine.release_resources();
    }
    pub fn bind_recording(&self, id: String, path: Option<String>) {
        if let Ok(mut state) = self.state.lock() {
            if let Some(session) = state.active.as_mut() {
                session.recording_id = Some(id);
                if path.is_some() {
                    session.recording_path = path;
                }
            }
        }
    }
    pub fn manual_session(&self, id: Option<&str>, path: &str) -> Result<DictationSession> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?;
        let session = state
            .active
            .clone()
            .ok_or_else(|| anyhow!("Dictation was canceled."))?;
        if session.recording_id.as_deref() != id || session.recording_path.as_deref() != Some(path)
        {
            bail!("Recording does not belong to the active dictation.");
        }
        if state.stage != "handoff" {
            bail!("This dictation is already being processed.");
        }
        state.stage = "waiting".into();
        drop(state);
        self.ensure_active(&session)?;
        Ok(session)
    }
    pub fn guard(&self, session: &DictationSession) -> SessionGuard {
        SessionGuard {
            service: self.clone(),
            id: session.id.clone(),
            armed: true,
        }
    }
    pub fn ensure_active(&self, session: &DictationSession) -> Result<()> {
        if session.cancelled.load(Ordering::SeqCst)
            || crate::shutdown::is_shutting_down()
            || !self.current().is_ok_and(|s| s.id == session.id)
        {
            bail!("DICTATION_CANCELED: Dictation was canceled.");
        }
        Ok(())
    }
    pub(crate) fn with_active<T>(
        &self,
        session: &DictationSession,
        action: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?;
        if session.cancelled.load(Ordering::SeqCst)
            || crate::shutdown::is_shutting_down()
            || !state.active.as_ref().is_some_and(|s| s.id == session.id)
        {
            bail!("DICTATION_CANCELED: Dictation was canceled.");
        }
        // Cancellation/new-session admission use this same mutex, so a delayed
        // write cannot cross into another session between its check and commit.
        action()
    }
    pub fn phase(&self, session: &DictationSession, stage: &str, text: &str) -> Result<()> {
        self.ensure_active(session)?;
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("Dictation state unavailable"))?;
            if !state.active.as_ref().is_some_and(|s| s.id == session.id) {
                bail!("Dictation was canceled.");
            }
            state.stage = stage.into();
            state.status_text = text.into();
            self.shell.processing_for_session(
                &session.id,
                text,
                matches!(stage, "loading" | "translating"),
            )?;
        }
        self.publish();
        Ok(())
    }
    pub fn acquire(&self, session: &DictationSession) -> Result<ProcessingPermit> {
        self.phase(session, "waiting", "Waiting for local processing…")?;
        self.queue.enqueue_priority(&session.id);
        let permit = self.queue.acquire(&session.id, &session.cancelled)?;
        self.phase(session, "transcribing", "Transcribing")?;
        Ok(permit)
    }
    pub fn acquire_background(&self, id: &str, cancelled: &AtomicBool) -> Result<ProcessingPermit> {
        self.queue.enqueue(id);
        self.queue.acquire(id, cancelled)
    }
    pub fn cancel(&self) {
        let mut reset = None;
        let active = self.state.lock().ok().and_then(|mut s| {
            let active = s.active.take();
            reset = s.arm_original_reset();
            if let Some(session) = &active {
                session.cancelled.store(true, Ordering::SeqCst);
            }
            if let Some(mut output) = s.last_output.clone().filter(|output| {
                output.status == "pending"
                    && active
                        .as_ref()
                        .is_some_and(|active| active.id == output.session_id)
            }) {
                output.status = "canceled".into();
                output.error_code = Some("dictation_canceled".into());
                output.error_message =
                    Some("Dictation was canceled. The original is available.".into());
                if output.transcript_id.is_some() {
                    let _ = storage::save_translation(&self.db_path, &output);
                }
                s.last_output = Some(output);
            }
            s.stage = "idle".into();
            s.status_text.clear();
            active
        });
        if let Some(session) = active {
            self.queue.remove(&session.id);
        }
        self.schedule_original_reset(reset);
        self.publish();
    }
    fn finish(&self, id: &str) {
        let mut overlay = None;
        let mut reset = None;
        if let Ok(mut state) = self.state.lock() {
            if state.active.as_ref().is_some_and(|s| s.id == id) {
                overlay = Some(self.shell.overlay_payload());
                state.finish(id);
                reset = state.arm_original_reset();
                self.shell.set_output_mode(state.mode);
            }
        }
        self.schedule_original_reset(reset);
        if let Some(overlay) = overlay {
            if matches!(
                overlay.phase,
                OverlayPhase::Processing | OverlayPhase::Listening
            ) {
                let _ = self
                    .shell
                    .set_overlay_if_revision(overlay.revision, Default::default());
            }
        }
        self.publish();
    }
    /// Admission for dictation ASR. Also returns the warm ASR worker left by
    /// the previous Original-mode dictation, if it is still cached. Any other
    /// workload's admission evicts that worker first (see `ProcessingQueue`).
    pub fn acquire_for_asr(
        &self,
        session: &DictationSession,
    ) -> Result<(ProcessingPermit, Option<WarmAsrWorker>)> {
        self.phase(session, "waiting", "Waiting for local processing…")?;
        self.queue.enqueue_priority(&session.id);
        let (permit, retained) = self.queue.acquire_with_idle(
            &session.id,
            &session.cancelled,
            Some(WARM_ASR_OWNER),
        )?;
        self.phase(session, "transcribing", "Transcribing")?;
        let warm = retained.and_then(|value| value.downcast::<WarmAsrWorker>().ok().map(|w| *w));
        Ok((permit, warm))
    }

    /// The parent holds the inference permit. A child process makes every ASR
    /// backend cancellable and guarantees its memory is gone before translation.
    /// In Original mode the child (with its loaded model) is cached for 60 s so
    /// back-to-back dictations skip process start and model loading.
    pub fn transcribe(
        &self,
        session: &DictationSession,
        request: FileTranscriptionRequest,
        permit: &ProcessingPermit,
        warm: Option<WarmAsrWorker>,
    ) -> Result<TranscriptResult> {
        self.ensure_active(session)?;
        self.engine.release_resources();
        let mut worker = WarmAsrWorker::reuse_or_spawn(warm)?;
        let result = self.run_asr(session, &mut worker, request)?;
        self.keep_or_release_asr(session, permit, worker);
        Ok(result)
    }

    /// Run one transcription on `worker` for an active dictation.
    pub(crate) fn run_asr(
        &self,
        session: &DictationSession,
        worker: &mut WarmAsrWorker,
        request: FileTranscriptionRequest,
    ) -> Result<TranscriptResult> {
        use crate::transcription_worker::WorkerRequest;
        let audio_seconds = wav_duration_seconds(Path::new(&request.file_path)).unwrap_or(0);
        // 90 s for start-up and model loading, plus real time for long
        // (up to five-minute) dictations on slower CPU models.
        let deadline = Instant::now() + Duration::from_secs(90 + audio_seconds);
        worker
            .exchange(
                &WorkerRequest {
                    models_dir: self.models_dir.clone(),
                    request,
                    preload: false,
                },
                deadline,
                || self.ensure_active(session),
            )?
            .ok_or_else(|| anyhow!("Dictation transcription runtime answered out of order."))
    }

    /// Load the request's model into `worker` without transcribing.
    pub(crate) fn preload_asr(
        &self,
        session: &DictationSession,
        worker: &mut WarmAsrWorker,
        request: FileTranscriptionRequest,
        check: impl Fn() -> Result<()>,
    ) -> Result<()> {
        use crate::transcription_worker::WorkerRequest;
        self.ensure_active(session)?;
        worker
            .exchange(
                &WorkerRequest {
                    models_dir: self.models_dir.clone(),
                    request,
                    preload: true,
                },
                Instant::now() + Duration::from_secs(90),
                check,
            )
            .map(|_| ())
    }

    /// In Original mode the child (with its loaded model) is cached for 60 s so
    /// back-to-back dictations skip process start and model loading. Otherwise
    /// `worker` drops here: the process group is killed and reaped before
    /// translation loads its model.
    pub(crate) fn keep_or_release_asr(
        &self,
        session: &DictationSession,
        permit: &ProcessingPermit,
        worker: WarmAsrWorker,
    ) {
        if session.mode == OutputMode::Original {
            self.queue.cache_idle(permit, WARM_ASR_OWNER, worker);
        }
    }

    /// The model a dictation request will run on (validated as a batch request).
    pub(crate) fn batch_model_for(
        &self,
        request: &FileTranscriptionRequest,
    ) -> Result<crate::asr::InstalledModel> {
        self.engine.model_for_request(request)
    }
    fn record_output(&self, session: &DictationSession, output: &DictationOutput) {
        if let Ok(mut state) = self.state.lock() {
            if state.active.as_ref().is_some_and(|s| s.id == session.id) {
                state.last_output = Some(output.clone());
            }
        }
        self.publish();
    }
    pub fn process(
        &self,
        session: &DictationSession,
        original: &TranscriptResult,
        duration_ms: i64,
    ) -> Result<DictationOutput> {
        self.ensure_active(session)?;
        let mut output = DictationOutput {
            session_id: session.id.clone(),
            source_text: original.plain_text.clone(),
            output_text: None,
            target_language: session.mode,
            status: "pending".into(),
            model_id: None,
            model_revision: None,
            prompt_version: None,
            transcript_id: None,
            error_message: None,
            error_code: None,
            source_languages: original.detected_languages.clone(),
            fallback_to_original: false,
        };
        if session.mode == OutputMode::Original
            || session.mode.already_in(&original.detected_languages)
            || original.plain_text.trim().is_empty()
        {
            output.output_text = Some(original.plain_text.clone());
            output.status = "completed".into();
            self.record_output(session, &output);
            return Ok(output);
        }
        let spec = self.selected_model().map(|(spec, _)| spec).unwrap_or(&MODELS[0]);
        output.model_id = Some(spec.id.into());
        output.model_revision = Some(spec.revision.into());
        output.prompt_version = Some(PROMPT_VERSION);
        // Keep the source available even if the history disk/database fails.
        self.record_output(session, &output);
        if session.save_history {
            let saved = self.with_active(session, || {
                storage::save_translated_dictation_source(
                    &self.db_path,
                    original,
                    duration_ms,
                    &mut output,
                )
            });
            if let Err(error) = saved {
                output.status = "failed".into();
                output.error_code = Some("history_save_failed".into());
                output.error_message = Some(format!("Could not save the original to history: {error}. The original is available to copy."));
                self.record_output(session, &output);
                return Ok(output);
            }
        }
        self.record_output(session, &output);
        self.translate_output(session, output)
    }
    fn translate_output(
        &self,
        session: &DictationSession,
        mut output: DictationOutput,
    ) -> Result<DictationOutput> {
        self.engine.release_resources();
        let target = serde_json::to_value(session.mode)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        let checked = |text: String| {
            let issues = crate::translation_check::check_translation(
                &output.source_text,
                &text,
                &target,
            );
            (text, issues)
        };
        // One stricter retry when the first result fails its checks.
        let result = self
            .run_worker(session, &output.source_text, &output.source_languages, None)
            .map(checked)
            .and_then(|(text, issues)| {
                if issues.is_empty() {
                    return Ok((text, issues));
                }
                eprintln!(
                    "[translation] check failed ({}); retrying once",
                    crate::translation_check::describe(&issues)
                );
                let instruction = crate::translation_check::retry_instruction(&issues);
                self.run_worker(
                    session,
                    &output.source_text,
                    &output.source_languages,
                    Some(&instruction),
                )
                .map(checked)
            });
        match result {
            Ok((text, issues)) if issues.is_empty() => {
                output.output_text = Some(text);
                output.status = "completed".into();
            }
            Ok((_, issues)) => {
                output.status = "failed".into();
                output.error_code = Some("translation_check_failed".into());
                output.error_message = Some(format!(
                    "The translation did not pass its checks ({}). The original was used instead.",
                    crate::translation_check::describe(&issues)
                ));
                output.fallback_to_original = true;
            }
            Err(error) => {
                let canceled = session.cancelled.load(Ordering::SeqCst);
                output.status = if canceled { "canceled" } else { "failed" }.into();
                output.error_code = Some(if canceled {
                    "dictation_canceled".into()
                } else {
                    error
                        .downcast_ref::<WorkerFailure>()
                        .map(|e| e.0.clone())
                        .unwrap_or_else(|| "translation_failed".into())
                });
                output.error_message = Some(error.to_string());
            }
        }
        if output.transcript_id.is_some() {
            if let Err(error) = self.with_active(session, || {
                storage::save_translation(&self.db_path, &output)
            }) {
                if self.ensure_active(session).is_err() {
                    output.status = "canceled".into();
                    output.output_text = None;
                    return Ok(output);
                }
                output.status = "failed".into();
                output.error_code = Some("history_save_failed".into());
                output.error_message = Some(format!("Could not save translation history: {error}. The original is available to copy."));
            }
            let _ = self.app.emit(
                "review-updated",
                crate::review::ReviewRef::Saved {
                    id: output.transcript_id.clone().unwrap(),
                },
            );
        }
        self.record_output(session, &output);
        Ok(output)
    }
    pub fn retry(&self, saved: Option<DictationOutput>) -> Result<DictationOutput> {
        self.ready()?;
        let previous = saved
            .or_else(|| self.snapshot().last_output)
            .filter(|o| o.status == "failed" || o.status == "canceled")
            .ok_or_else(|| anyhow!("No failed translation is available to retry."))?;
        let session = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("Dictation state unavailable"))?;
            if state.active.is_some() {
                bail!("Finish the current dictation first.");
            }
            let settings = storage::get_settings_from_db_path(&self.db_path)?;
            let session = DictationSession {
                id: uuid::Uuid::new_v4().to_string(),
                mode: previous.target_language,
                cancelled: Arc::new(AtomicBool::new(false)),
                save_history: settings.save_history,
                prefer_gpu: settings.gpu_enabled,
                recording_id: None,
                recording_path: None,
            };
            state.active = Some(session.clone());
            session
        };
        let _guard = self.guard(&session);
        self.shell.set_output_mode(session.mode);
        let _permit = self.acquire(&session)?;
        let mut output = previous;
        if !session.save_history {
            output.transcript_id = None;
        }
        output.session_id = session.id.clone();
        output.error_message = None;
        output.error_code = None;
        output.fallback_to_original = false;
        output.status = "pending".into();
        output.output_text = None;
        let spec = self.selected_model().map(|(spec, _)| spec).unwrap_or(&MODELS[0]);
        output.model_id = Some(spec.id.into());
        output.model_revision = Some(spec.revision.into());
        output.prompt_version = Some(PROMPT_VERSION);
        self.record_output(&session, &output);
        if output.transcript_id.is_some() {
            storage::save_translation(&self.db_path, &output)?;
        }
        let result = self.translate_output(&session, output);
        self.shell
            .set_overlay_payload(DictationOverlayPayload::default())?;
        result
    }
    /// Whether `path` passed its checksum and has not changed since.
    pub(crate) fn model_verified(&self, path: &Path) -> bool {
        let Ok(metadata) = path.metadata() else { return false };
        let Ok(modified) = metadata.modified() else { return false };
        self.verified.lock().ok().is_some_and(|v| {
            matches!(v.get(path), Some(Verification::Valid(value)) if *value == (metadata.len(), modified))
        })
    }
    pub(crate) fn verify_model(
        &self,
        spec: &TranslationModel,
        path: &Path,
        cancelled: &AtomicBool,
        deadline: Instant,
    ) -> Result<()> {
        let metadata = path.metadata()?;
        let fingerprint = (metadata.len(), metadata.modified()?);
        if self.model_verified(path) {
            return Ok(());
        }
        let mut file = std::fs::File::open(path)?;
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() {
                bail!("Dictation was canceled.");
            }
            if Instant::now() > deadline {
                bail!("Translation model verification timed out.");
            }
            let n = file.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            hash.update(&buffer[..n]);
        }
        if format!("{:x}", hash.finalize()) != spec.sha256 {
            bail!("Translation model is damaged. Download it again in Settings → Languages.");
        }
        let current = path.metadata()?;
        if (current.len(), current.modified()?) != fingerprint {
            bail!("Translation model changed during verification. Please retry.");
        }
        self.verified
            .lock()
            .map_err(|_| anyhow!("Model state unavailable"))?
            .insert(path.to_owned(), Verification::Valid(fingerprint));
        Ok(())
    }
    fn run_worker(
        &self,
        session: &DictationSession,
        text: &str,
        source_languages: &[String],
        strict_instruction: Option<&str>,
    ) -> Result<String> {
        self.phase(session, "loading", "Loading translation model…")?;
        let (spec, model) = self.selected_model()?;
        self.verify_model(spec, &model, &session.cancelled, Instant::now() + Duration::from_secs(90))?;
        let helper = helper_path(&self.app).ok_or_else(|| anyhow!("Translation runtime is missing."))?;
        let stop = || session.cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down();
        // The model preloaded during the dictation, or kept from the last one.
        let warm = self.resident.take(&stop);
        let process = match warm.filter(|process| process.serves(&model, session.prefer_gpu)) {
            Some(process) => Ok(process),
            None => crate::resident_translator::TranslatorProcess::spawn(&helper, &model, session.prefer_gpu),
        };
        let mut process = match process {
            Ok(process) => process,
            Err(error) => {
                self.resident.give_back(None);
                return Err(error);
            }
        };
        let request = serde_json::json!({"version": WORKER_PROTOCOL, "type": "translate",
            "requestId": session.id, "modelPath": model, "text": text,
            "sourceLanguages": source_languages, "targetLanguage": session.mode,
            "preferGpu": session.prefer_gpu, "strictInstruction": strict_instruction});
        let result = process.send(&request).and_then(|_| self.await_translation(session, &mut process));
        // A failed request leaves a healthy helper; a broken exchange does not.
        let healthy = match &result {
            Ok(_) => true,
            Err(error) => error.downcast_ref::<WorkerFailure>().is_some(),
        };
        self.resident.give_back(healthy.then_some(process));
        result
    }
    fn await_translation(
        &self,
        session: &DictationSession,
        process: &mut crate::resident_translator::TranslatorProcess,
    ) -> Result<String> {
        // Loading counts against the first deadline; a resident model answers at once.
        let mut deadline = Instant::now() + Duration::from_secs(150);
        let mut protocol = WorkerProtocol::default();
        loop {
            self.ensure_active(session)?;
            if Instant::now() > deadline {
                bail!("Translation timed out. The original text is available to retry.");
            }
            let Some(line) = process.next_line(Duration::from_millis(100))? else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("Invalid translation worker response."))?;
            match protocol.accept(&session.id, &value)? {
                WorkerEvent::Ready => {
                    deadline = Instant::now() + Duration::from_secs(180);
                    self.phase(session, "translating", "Translating")?;
                }
                WorkerEvent::Chunk(index, total) => {
                    deadline = Instant::now() + Duration::from_secs(180);
                    self.phase(session, "translating", &format!("Translating · {index}/{total}"))?;
                }
                WorkerEvent::Result(text) => return Ok(text),
            }
        }
    }
}

/// The translation worker runs on Apple silicon only.
pub fn platform_supported() -> bool {
    cfg!(target_arch = "aarch64")
}
pub fn helper_path(app: &AppHandle) -> Option<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(resources) = app.path().resource_dir() {
        paths.push(resources.join("workers/murmela-translation-worker"));
    }
    #[cfg(debug_assertions)]
    paths.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/translation-runtime/build/murmela-translation-worker"),
    );
    paths.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_cycle_and_wire_codes_are_stable() {
        use OutputMode::*;
        let picked = cycle_languages(&["fr".into(), "en".into(), "xx".into(), "es-AR".into()]);
        assert_eq!(picked, [French, English, SpanishArgentina], "unknown codes are skipped");
        // The shortcut visits the picked languages in the fixed order, then Original.
        assert_eq!(Original.next(&picked), English);
        assert_eq!(English.next(&picked), French);
        assert_eq!(French.next(&picked), SpanishArgentina);
        assert_eq!(SpanishArgentina.next(&picked), Original);
        assert_eq!(Original.previous(&picked), SpanishArgentina);
        assert_eq!(English.previous(&picked), Original);
        // From a language chosen on the Dictate screen but not in the cycle.
        assert_eq!(German.next(&picked), French);
        assert_eq!(German.previous(&picked), English);
        assert_eq!(Original.next(&[]), Original);
        for mode in OUTPUT_LANGUAGES {
            assert_eq!(OutputMode::language(mode.code()), Some(mode));
            assert_eq!(serde_json::to_string(&mode).unwrap(), format!("\"{}\"", mode.code()));
        }
        assert_eq!(OutputMode::language("original"), None);
        assert!(serde_json::from_str::<OutputMode>("\"es-ES\"").is_err());
    }
    #[test]
    fn dictation_already_in_the_target_language_is_not_translated() {
        let langs = |codes: &[&str]| codes.iter().map(|c| c.to_string()).collect::<Vec<_>>();
        assert!(OutputMode::German.already_in(&langs(&["de"])));
        assert!(OutputMode::English.already_in(&langs(&["en", "en"])));
        assert!(!OutputMode::English.already_in(&langs(&["de", "en"])), "mixed speech is translated");
        assert!(!OutputMode::German.already_in(&[]), "unknown language is translated");
        assert!(OutputMode::SpanishArgentina.already_in(&langs(&["es"])));
        assert!(OutputMode::PortugueseBrazil.already_in(&langs(&["pt"])));
        assert!(!OutputMode::Original.already_in(&langs(&["de"])));
    }
    fn session(id: &str) -> DictationSession {
        DictationSession {
            id: id.into(),
            mode: OutputMode::French,
            cancelled: Arc::new(AtomicBool::new(false)),
            save_history: false,
            prefer_gpu: true,
            recording_id: None,
            recording_path: None,
        }
    }
    #[test]
    fn language_is_locked_through_manual_handoff_waiting_and_inference() {
        let mut state = SessionState {
            active: Some(session("first")),
            ..Default::default()
        };
        for phase in [
            "recording",
            "handoff",
            "waiting",
            "transcribing",
            "loading",
            "translating",
        ] {
            state.stage = phase.into();
            assert!(
                state
                    .change_mode(OutputMode::SpanishArgentina, false)
                    .is_err(),
                "{phase}"
            );
            assert_eq!(state.mode, OutputMode::Original);
        }
        // While it is still recording, the dictation takes the new language.
        state.stage = "recording".into();
        state.change_mode(OutputMode::German, true).unwrap();
        assert_eq!(state.mode, OutputMode::German);
        assert_eq!(state.active.as_ref().unwrap().mode, OutputMode::German);
        assert!(state.finish("first"));
        assert!(state.change_mode(OutputMode::French, true).is_err()); // microphone test
        state.change_mode(OutputMode::French, false).unwrap();
    }
    #[test]
    fn translated_language_returns_to_original_only_after_an_undisturbed_countdown() {
        let mut state = SessionState::default();
        assert_eq!(state.arm_original_reset(), None); // already Original
        state.change_mode(OutputMode::French, false).unwrap();
        let first = state.arm_original_reset().unwrap();
        let second = state.arm_original_reset().unwrap(); // a later dictation restarts it
        assert!(!state.expire_original_reset(first, false));
        assert_eq!(state.mode, OutputMode::French);
        assert!(!state.expire_original_reset(second, true)); // microphone busy
        state.active = Some(session("running"));
        assert!(!state.expire_original_reset(second, false)); // dictation running
        assert_eq!(state.arm_original_reset(), None);
        state.active = None;
        let third = state.arm_original_reset().unwrap();
        assert!(state.expire_original_reset(third, false));
        assert_eq!(state.mode, OutputMode::Original);
    }
    #[test]
    fn late_session_cleanup_cannot_finish_a_new_dictation() {
        let mut state = SessionState {
            active: Some(session("new")),
            stage: "recording".into(),
            ..Default::default()
        };
        assert!(!state.finish("old"));
        assert_eq!(state.active.as_ref().unwrap().id, "new");
        assert_eq!(state.stage, "recording");
    }
    fn record(kind: &str, extra: serde_json::Value) -> serde_json::Value {
        let mut value = serde_json::json!({"version": WORKER_PROTOCOL, "promptVersion": PROMPT_VERSION, "requestId": "test", "type": kind});
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    }
    #[test]
    fn protocol_requires_every_chunk_and_one_complete_result() {
        let mut protocol = WorkerProtocol::default();
        protocol
            .accept(
                "test",
                &record("ready", serde_json::json!({"chunkCount": 2})),
            )
            .unwrap();
        let result = record(
            "result",
            serde_json::json!({"completed": true, "text": "Bonjour"}),
        );
        assert!(protocol.accept("test", &result).is_err());
        for index in 0..2 {
            protocol
                .accept(
                    "test",
                    &record(
                        "progress",
                        serde_json::json!({"chunkIndex": index, "chunkCount": 2}),
                    ),
                )
                .unwrap();
        }
        assert!(protocol
            .accept(
                "test",
                &record(
                    "result",
                    serde_json::json!({"completed": false, "text": "Bonjour"})
                )
            )
            .is_err());
        assert!(
            matches!(protocol.accept("test", &result).unwrap(), WorkerEvent::Result(text) if text == "Bonjour")
        );
        assert!(protocol.accept("test", &result).is_err());
    }
    #[test]
    fn protocol_rejects_foreign_jobs_duplicate_progress_and_truncated_output() {
        let mut protocol = WorkerProtocol::default();
        let ready = record("ready", serde_json::json!({"chunkCount": 1}));
        assert!(protocol.accept("other", &ready).is_err());
        protocol.accept("test", &ready).unwrap();
        let progress = record(
            "progress",
            serde_json::json!({"chunkIndex": 0, "chunkCount": 1}),
        );
        protocol.accept("test", &progress).unwrap();
        assert!(protocol.accept("test", &progress).is_err());
        let error = protocol
            .accept(
                "test",
                &record("error", serde_json::json!({"code": "OUTPUT_TRUNCATED"})),
            )
            .err()
            .unwrap();
        assert_eq!(
            error.downcast_ref::<WorkerFailure>().unwrap().0,
            "OUTPUT_TRUNCATED"
        );
    }
}
