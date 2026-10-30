use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::audio_chunks::{
    plan_audio_chunks, plan_audio_chunks_with_splits, split_chunk_near_middle, AudioChunk,
};
use crate::audio_preprocess;
use crate::diarization::{DiarizationStatus, DiarizationTurn, TranscriptSpeaker};
use crate::model_metadata::ModelCapabilities;
use crate::qwen_asr::QwenAsrEngine;
use crate::settings::{LanguageMode, ModelProfile};
use crate::transcript_stitching::stitch_segments;
use crate::transcription_policy::{
    CONTROLLED_PROMPT_MAX_CHARS, CONTROLLED_PROMPT_RESET_SILENCE_MS, DIRECT_FILE_MAX_MS,
    MIN_SPLIT_RETRY_MS,
};
use crate::transcription_quality::repetition_reason;
use crate::transcription_progress::{set_step, Step};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledModel {
    pub id: String,
    pub engine: String,
    pub model_name: String,
    pub variant: String,
    pub local_path: String,
    pub size_bytes: i64,
    pub is_default: bool,
    pub profile: ModelProfile,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub language_code: String,
    pub segment_order: i32,
    pub confidence: Option<f32>,
    #[serde(default)]
    pub speaker_id: Option<String>,
    #[serde(default)]
    pub speaker_ids: Option<Vec<String>>,
    #[serde(default)]
    pub speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution,
    #[serde(default)]
    pub speaker_confidence: Option<f32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResult {
    pub job_id: String,
    pub model_name: String,
    pub full_text: String,
    pub plain_text: String,
    pub timestamped_text: String,
    pub detected_languages: Vec<String>,
    pub segments: Vec<TranscriptSegment>,
    pub quality_status: TranscriptQualityStatus,
    pub recovered_region_count: i32,
    pub warnings: Vec<TranscriptWarning>,
    #[serde(default)]
    pub diarization_status: DiarizationStatus,
    #[serde(default)]
    pub diarization_model_id: Option<String>,
    #[serde(default)]
    pub diarization_source: crate::diarization::DiarizationSource,
    #[serde(default)]
    pub diarization_warning: Option<String>,
    #[serde(default)]
    pub diarization_policy_version: Option<u32>,
    #[serde(default)]
    pub diarization_clustering_threshold: Option<f32>,
    #[serde(default)]
    pub diarization_speaker_count_hint: Option<i32>,
    #[serde(default)]
    pub speakers: Vec<TranscriptSpeaker>,
    #[serde(default)]
    pub diarization_turns: Vec<DiarizationTurn>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptQualityStatus {
    Clean,
    Recovered,
    Partial,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptWarning {
    pub start_ms: i64,
    pub end_ms: i64,
    pub reason: String,
    pub attempts: i32,
    pub outcome: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineErrorPayload {
    pub code: String,
    pub message: String,
}

pub fn engine_error_payload(error: &anyhow::Error) -> EngineErrorPayload {
    let message = error.to_string();
    let code = message
        .split_once(':')
        .map(|(candidate, _)| candidate)
        .filter(|candidate| {
            !candidate.is_empty()
                && candidate
                    .chars()
                    .all(|character| character.is_ascii_uppercase() || character == '_')
        })
        .map(|candidate| candidate.to_ascii_lowercase())
        .unwrap_or_else(|| "transcription_failed".to_string());
    EngineErrorPayload { code, message }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSourceKind {
    QuickDictate,
    FileUpload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionPreviewRequest {
    #[serde(default)]
    pub session_id: Option<String>,
    pub source_kind: PreviewSourceKind,
    pub profile: ModelProfile,
    pub selected_model_id: Option<String>,
    pub language_mode: LanguageMode,
    pub fixed_language: Option<String>,
    pub timestamps: bool,
    pub prefer_gpu: bool,
    pub file_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionPreviewResponse {
    #[serde(default)]
    pub dictation_output: Option<crate::translation::DictationOutput>,
    pub source_kind: PreviewSourceKind,
    pub resolved_model: Option<InstalledModel>,
    pub result: Option<TranscriptResult>,
    pub error: Option<EngineErrorPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTranscriptionRequest {
    #[serde(default)]
    pub use_context: Option<crate::model_metadata::ModelUseContext>,
    pub profile: ModelProfile,
    pub selected_model_id: Option<String>,
    pub language_mode: LanguageMode,
    pub fixed_language: Option<String>,
    pub timestamps: bool,
    pub prefer_gpu: bool,
    pub file_path: String,
    #[serde(default)]
    pub context_prompt: Option<String>,
    #[serde(default)]
    pub context_terms: Vec<String>,
}

pub trait TranscriptionEngine: Send + Sync {
    fn list_models(&self) -> Result<Vec<InstalledModel>>;
    fn refresh_from_disk(&self) -> Result<Vec<InstalledModel>>;
    fn transcribe_file(
        &self,
        request: FileTranscriptionRequest,
        progress: Option<Arc<AtomicI32>>,
    ) -> Result<TranscriptResult>;
}

#[derive(Debug)]
pub struct LocalTranscriptionEngine {
    models_dir: PathBuf,
    models: Mutex<Vec<InstalledModel>>,
    whisper: SharedWhisperEngine,
    qwen: QwenAsrEngine,
}

impl LocalTranscriptionEngine {
    pub fn new(models_dir: PathBuf, models: Vec<InstalledModel>) -> Self {
        let whisper_models = models
            .iter()
            .filter(|model| model.engine == "whisper.cpp")
            .cloned()
            .collect();
        Self {
            models_dir: models_dir.clone(),
            models: Mutex::new(models),
            whisper: SharedWhisperEngine::new(models_dir.clone(), whisper_models),
            qwen: QwenAsrEngine::new(models_dir),
        }
    }

    pub fn release_resources(&self) {
        self.whisper.invalidate_context_cache();
        self.qwen.invalidate_context_cache();
    }

    fn resolve_model(
        &self,
        selected_model_id: Option<&str>,
        profile: ModelProfile,
    ) -> Result<InstalledModel> {
        let models = self.list_models()?;
        if let Some(model_id) = selected_model_id {
            if let Some(model) = models.iter().find(|model| model.id == model_id) {
                return Ok(model.clone());
            }
            if model_id == crate::qwen_asr::QWEN_MODEL_ID {
                return Err(anyhow!(
                    "MODEL_INCOMPLETE: Qwen3-ASR is not fully installed; resume or restart its model download"
                ));
            }
        }
        models
            .iter()
            .find(|model| model.profile == profile && model.is_default && !model.capabilities.streaming_transcription)
            .or_else(|| models.iter().find(|model| model.profile == profile && !model.capabilities.streaming_transcription))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "MODEL_MISSING: no local model is installed for profile {}",
                    to_profile_name(profile)
                )
            })
    }
}

impl LocalTranscriptionEngine {
    /// The model a batch request will run on, with the same validation as
    /// `transcribe_file`.
    pub(crate) fn model_for_request(
        &self,
        request: &FileTranscriptionRequest,
    ) -> Result<InstalledModel> {
        if request.selected_model_id.as_deref() == Some(crate::live_pair::MODEL_ID) {
            return Err(anyhow!("MODEL_CONTEXT_UNSUPPORTED: Live dictation only works with the dictation shortcut. Choose another engine for the record button or files."));
        }
        let model = self.resolve_model(request.selected_model_id.as_deref(), request.profile)?;
        if model.capabilities.streaming_transcription {
            return Err(anyhow!("MODEL_CONTEXT_UNSUPPORTED: Choose a model that supports this transcription workflow."));
        }
        if let Some(use_context) = request.use_context {
            if !model.capabilities.supported_contexts.contains(&use_context) {
                return Err(anyhow!(
                    "MODEL_CONTEXT_UNSUPPORTED: {} is not available for {:?}",
                    model.model_name,
                    use_context
                ));
            }
        }
        Ok(model)
    }

    /// Load the request's model into this process's cache without decoding, so
    /// the next `transcribe_file` with the same model starts immediately. Native
    /// worker engines start a fresh process per transcription and have nothing
    /// to keep warm here.
    pub fn preload(&self, request: &FileTranscriptionRequest) -> Result<()> {
        let _work = crate::shutdown::begin_work(true)?;
        let model = self.model_for_request(request)?;
        match model.engine.as_str() {
            "whisper.cpp" => {
                self.qwen.invalidate_context_cache();
                self.whisper.preload(&model, request.prefer_gpu)
            }
            "qwen3_asr_c" => {
                self.whisper.invalidate_context_cache();
                self.qwen.preload(&model)
            }
            _ => Ok(()),
        }
    }
}

/// Engines whose model stays loaded inside the persistent transcription worker.
pub(crate) fn engine_supports_preload(engine: &str) -> bool {
    matches!(engine, "whisper.cpp" | "qwen3_asr_c")
}

impl TranscriptionEngine for LocalTranscriptionEngine {
    fn list_models(&self) -> Result<Vec<InstalledModel>> {
        Ok(self
            .models
            .lock()
            .map_err(|_| anyhow!("model registry poisoned"))?
            .clone())
    }

    fn refresh_from_disk(&self) -> Result<Vec<InstalledModel>> {
        let refreshed = discover_installed_models(&self.models_dir)?;
        *self
            .models
            .lock()
            .map_err(|_| anyhow!("model registry poisoned"))? = refreshed.clone();
        let _ = self.whisper.refresh_from_disk()?;
        self.qwen.invalidate_context_cache();
        Ok(refreshed)
    }

    fn transcribe_file(
        &self,
        mut request: FileTranscriptionRequest,
        progress: Option<Arc<AtomicI32>>,
    ) -> Result<TranscriptResult> {
        let _work = crate::shutdown::begin_work(true)?;
        let model = self.model_for_request(&request)?;
        request.selected_model_id = Some(model.id.clone());
        match model.engine.as_str() {
            "whisper.cpp" => {
                self.qwen.invalidate_context_cache();
                self.whisper.transcribe_file(request, progress)
            }
            "qwen3_asr_c" => {
                self.whisper.invalidate_context_cache();
                let prepared =
                    audio_preprocess::decode_audio_file(Path::new(&request.file_path))
                        .with_context(|| format!("failed to prepare {}", request.file_path))?;
                let vad_model_path =
                    crate::model_downloads::installed_vad_model_path(&self.models_dir);
                self.qwen.transcribe(
                    &model,
                    &prepared,
                    &request,
                    progress,
                    vad_model_path.as_deref(),
                )
            }
            "moss-transcribe-cpp" | "vibevoice-mlx" => {
                self.whisper.invalidate_context_cache();
                self.qwen.invalidate_context_cache();
                crate::native_asr::transcribe_with_native_worker(
                    &model,
                    &request,
                    progress,
                    &self.models_dir,
                )
            }
            engine => Err(anyhow!(
                "MODEL_ENGINE_UNSUPPORTED: unsupported engine '{engine}'"
            )),
        }
    }
}

/// A loaded Whisper context kept alive between transcriptions so we don't pay
/// the (large) model-load + GPU-allocation cost on every dictation.
struct CachedContext {
    model_path: String,
    use_gpu: bool,
    context: Arc<WhisperContext>,
}

#[derive(Debug)]
pub struct SharedWhisperEngine {
    models_dir: PathBuf,
    models: Mutex<Vec<InstalledModel>>,
    // Single-slot cache: reused while the same (model, gpu) is requested,
    // rebuilt on change. See `obtain_context`.
    context_cache: Mutex<Option<CachedContext>>,
}

impl std::fmt::Debug for CachedContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CachedContext")
            .field("model_path", &self.model_path)
            .field("use_gpu", &self.use_gpu)
            .finish()
    }
}

impl SharedWhisperEngine {
    pub fn new(models_dir: PathBuf, models: Vec<InstalledModel>) -> Self {
        Self {
            models_dir,
            models: Mutex::new(models),
            context_cache: Mutex::new(None),
        }
    }

    /// Return a loaded context for `(model, use_gpu)`, reusing the cached one
    /// when the key matches. The cache holds a single slot; switching models or
    /// GPU mode drops the previous context first to free its memory.
    fn obtain_context(&self, model: &InstalledModel, use_gpu: bool) -> Result<Arc<WhisperContext>> {
        let mut cache = self
            .context_cache
            .lock()
            .map_err(|_| anyhow!("whisper context cache poisoned"))?;
        if let Some(cached) = cache.as_ref() {
            if cached.model_path == model.local_path && cached.use_gpu == use_gpu {
                return Ok(Arc::clone(&cached.context));
            }
        }
        // Drop any previous context before allocating the new one.
        *cache = None;
        let context = Arc::new(create_whisper_context(model, use_gpu)?);
        *cache = Some(CachedContext {
            model_path: model.local_path.clone(),
            use_gpu,
            context: Arc::clone(&context),
        });
        Ok(context)
    }

    /// Load the context `transcribe_file` will ask for first (GPU when
    /// preferred and available, CPU otherwise).
    pub(crate) fn preload(&self, model: &InstalledModel, prefer_gpu: bool) -> Result<()> {
        if prefer_gpu && self.obtain_context(model, true).is_ok() {
            return Ok(());
        }
        self.obtain_context(model, false).map(|_| ())
    }

    pub(crate) fn invalidate_context_cache(&self) {
        if let Ok(mut cache) = self.context_cache.lock() {
            *cache = None;
        }
    }

    fn resolve_model(
        &self,
        selected_model_id: Option<&str>,
        profile: ModelProfile,
    ) -> Result<InstalledModel> {
        let models = self.list_models()?;
        if let Some(model_id) = selected_model_id {
            if let Some(model) = models.iter().find(|model| model.id == model_id) {
                return Ok(model.clone());
            }
        }

        models
            .iter()
            .find(|model| model.profile == profile && model.is_default)
            .or_else(|| models.iter().find(|model| model.profile == profile))
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "MODEL_MISSING: no whisper.cpp model is installed for profile {}",
                    to_profile_name(profile)
                )
            })
    }
}

impl TranscriptionEngine for SharedWhisperEngine {
    fn list_models(&self) -> Result<Vec<InstalledModel>> {
        Ok(self
            .models
            .lock()
            .map_err(|_| anyhow!("model registry poisoned"))?
            .clone())
    }

    fn refresh_from_disk(&self) -> Result<Vec<InstalledModel>> {
        let refreshed = discover_whisper_models(&self.models_dir)?;
        *self
            .models
            .lock()
            .map_err(|_| anyhow!("model registry poisoned"))? = refreshed.clone();
        // A model file may have been replaced/removed; drop the cached context
        // so the next transcription reloads from disk.
        self.invalidate_context_cache();
        Ok(refreshed)
    }

    fn transcribe_file(
        &self,
        request: FileTranscriptionRequest,
        progress: Option<Arc<AtomicI32>>,
    ) -> Result<TranscriptResult> {
        let model = self.resolve_model(request.selected_model_id.as_deref(), request.profile)?;
        let prepared = audio_preprocess::decode_audio_file(Path::new(&request.file_path))
            .with_context(|| format!("failed to prepare {}", request.file_path))?;
        let use_gpu = request.prefer_gpu;
        let vad_model_path = crate::model_downloads::installed_vad_model_path(&self.models_dir);

        let (context, gpu_active) = if use_gpu {
            match self.obtain_context(&model, true) {
                Ok(ctx) => (ctx, true),
                Err(_) => (self.obtain_context(&model, false)?, false),
            }
        } else {
            (self.obtain_context(&model, false)?, false)
        };

        let transcript = run_whisper(
            context.as_ref(),
            &model,
            &prepared,
            &request,
            &progress,
            vad_model_path.as_deref(),
        )
        .or_else(|error| {
            if gpu_active && !crate::shutdown::is_shutting_down() {
                let cpu_context = self
                    .obtain_context(&model, false)
                    .with_context(|| format!("{}; CPU context creation also failed", error))?;
                run_whisper(
                    cpu_context.as_ref(),
                    &model,
                    &prepared,
                    &request,
                    &progress,
                    vad_model_path.as_deref(),
                )
                .with_context(|| format!("{}; CPU fallback also failed", error))
            } else {
                Err(error)
            }
        })?;
        Ok(transcript)
    }
}

pub fn discover_whisper_models(models_dir: &Path) -> Result<Vec<InstalledModel>> {
    fs::create_dir_all(models_dir)?;
    let mut models = Vec::new();
    for entry in fs::read_dir(models_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("bin") {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.len() < 1024 * 1024 {
            continue;
        }

        let model_name = entry.file_name().to_string_lossy().to_string();
        // Any other whisper.cpp file added by hand is listed as a custom model,
        // including Tiny after its one-time retirement (use at your own risk).
        if model_name.to_ascii_lowercase().contains("silero")
            || model_name.to_ascii_lowercase().contains("vad")
        {
            continue;
        }
        let profile = profile_for_model_name(&model_name);
        models.push(InstalledModel {
            id: model_id_from_name(&model_name),
            engine: "whisper.cpp".to_string(),
            variant: to_profile_name(profile).to_string(),
            model_name,
            local_path: path.display().to_string(),
            size_bytes: metadata.len() as i64,
            is_default: is_default_model(&path),
            profile,
            capabilities: ModelCapabilities::standard_asr(),
        });
    }

    models.sort_by(|left, right| left.model_name.cmp(&right.model_name));
    Ok(models)
}

pub fn discover_installed_models(models_dir: &Path) -> Result<Vec<InstalledModel>> {
    let mut models = discover_whisper_models(models_dir)?;
    if let Some(qwen) = crate::qwen_asr::discover_model(models_dir)? {
        models.push(qwen);
    }
    models.extend(crate::model_downloads::discover_native_asr_models(
        models_dir,
    ));
    models.sort_by(|left, right| left.model_name.cmp(&right.model_name));
    Ok(models)
}

fn create_whisper_context(model: &InstalledModel, use_gpu: bool) -> Result<WhisperContext> {
    let context_params = WhisperContextParameters {
        use_gpu,
        ..WhisperContextParameters::default()
    };
    WhisperContext::new_with_params(&model.local_path, context_params)
        .with_context(|| format!("failed to load model {}", model.model_name))
}

/// Whisper for the short stretches another engine could not transcribe (where
/// VibeVoice kept looping). The smallest installed Whisper model that is
/// still accurate is used: Large v3 Turbo when it is there.
pub(crate) struct WhisperFallback {
    model: InstalledModel,
    context: WhisperContext,
    vad_model_path: Option<PathBuf>,
}

impl WhisperFallback {
    pub(crate) fn load(models_dir: &Path, prefer_gpu: bool) -> Result<Self> {
        let models = discover_whisper_models(models_dir)?;
        let model = models
            .iter()
            .find(|model| model.model_name.contains("turbo"))
            .or_else(|| models.iter().find(|model| model.is_default))
            .or_else(|| models.first())
            .cloned()
            .ok_or_else(|| anyhow!("MODEL_MISSING: no Whisper model is installed"))?;
        let context = if prefer_gpu {
            create_whisper_context(&model, true).or_else(|_| create_whisper_context(&model, false))?
        } else {
            create_whisper_context(&model, false)?
        };
        Ok(Self {
            model,
            context,
            vad_model_path: crate::model_downloads::installed_vad_model_path(models_dir),
        })
    }

    /// The segments heard in `audio`, times relative to its start; none when
    /// Whisper hears no speech.
    pub(crate) fn transcribe(
        &self,
        audio: &audio_preprocess::PreparedAudio,
        request: &FileTranscriptionRequest,
    ) -> Result<Vec<TranscriptSegment>> {
        let mut request = request.clone();
        request.selected_model_id = Some(self.model.id.clone());
        request.timestamps = true;
        match run_whisper(
            &self.context,
            &self.model,
            audio,
            &request,
            &None,
            self.vad_model_path.as_deref(),
        ) {
            Ok(result) => Ok(result.segments),
            Err(error) if error.to_string().starts_with("TRANSCRIPTION_EMPTY") => Ok(Vec::new()),
            Err(error) => Err(error),
        }
    }
}

#[derive(Debug, Clone)]
struct DecodeOptions {
    temperature: f32,
    initial_prompt: Option<String>,
    repetition_watchdog: bool,
}

impl DecodeOptions {
    fn direct(initial_prompt: Option<String>) -> Self {
        Self {
            temperature: 0.0,
            initial_prompt,
            repetition_watchdog: false,
        }
    }

    fn resilient(temperature: f32, initial_prompt: Option<String>) -> Self {
        Self {
            temperature,
            initial_prompt,
            repetition_watchdog: true,
        }
    }
}

#[derive(Clone)]
struct ProgressReporter {
    progress: Arc<AtomicI32>,
    start_percent: f32,
    end_percent: f32,
}

impl ProgressReporter {
    fn full(progress: Arc<AtomicI32>) -> Self {
        Self {
            progress,
            start_percent: 0.0,
            end_percent: 100.0,
        }
    }

    fn for_range(progress: Arc<AtomicI32>, start_percent: f32, end_percent: f32) -> Self {
        Self {
            progress,
            start_percent,
            end_percent,
        }
    }

    fn report(&self, local_percent: i32) {
        let local = (local_percent as f32).clamp(0.0, 100.0) / 100.0;
        let mapped = self.start_percent + (self.end_percent - self.start_percent) * local;
        self.progress
            .fetch_max(mapped.round() as i32, Ordering::Relaxed);
    }
}

fn run_resilient_whisper(
    context: &WhisperContext,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    progress: &Option<Arc<AtomicI32>>,
    vad_model_path: Option<&Path>,
) -> Result<TranscriptResult> {
    crate::shutdown::ensure_running()?;
    let mut decoder_state = context
        .create_state()
        .context("failed to create whisper state for resilient transcription")?;
    set_step(Step::FindingPauses);
    let preferred_splits = vad_model_path
        .and_then(|path| detect_vad_splits(path, prepared).ok())
        .unwrap_or_default();
    let chunks = if preferred_splits.is_empty() {
        plan_audio_chunks(&prepared.samples, prepared.sample_rate_hz)
    } else {
        plan_audio_chunks_with_splits(
            &prepared.samples,
            prepared.sample_rate_hz,
            &preferred_splits,
        )
    };
    set_step(Step::Transcribing);
    if chunks.is_empty() {
        return Err(anyhow!(
            "TRANSCRIPTION_EMPTY: prepared audio had no samples"
        ));
    }

    let job_id = Uuid::new_v4().to_string();
    let total_samples = prepared.samples.len().max(1) as f32;
    let mut accepted_segments = Vec::new();
    let mut warnings = Vec::new();
    let mut prompt_allowed = true;
    let dictionary_prompt = whisper_dictionary_prompt(&request.context_terms);

    for chunk in chunks {
        let start_percent = chunk.start_sample as f32 / total_samples * 100.0;
        let end_percent = chunk.end_sample as f32 / total_samples * 100.0;
        let reporter = progress
            .clone()
            .map(|progress| ProgressReporter::for_range(progress, start_percent, end_percent));
        let continuity = if prompt_allowed && matches!(request.language_mode, LanguageMode::Fixed) {
            controlled_prompt(&accepted_segments, chunk.start_ms(prepared.sample_rate_hz))
        } else {
            None
        };
        let prompt = combine_whisper_prompts(dictionary_prompt.as_deref(), continuity);

        match decode_audio_chunk(
            context,
            &mut decoder_state,
            model,
            prepared,
            request,
            chunk,
            reporter.as_ref(),
            DecodeOptions::resilient(0.0, prompt),
        ) {
            Ok(mut result) => {
                shift_segments(
                    &mut result.segments,
                    chunk.start_ms(prepared.sample_rate_hz),
                    chunk.end_ms(prepared.sample_rate_hz),
                );
                accepted_segments.extend(result.segments);
                prompt_allowed = true;
            }
            Err(primary_error) if is_recoverable_decode_error(&primary_error) => {
                match decode_audio_chunk(
                    context,
                    &mut decoder_state,
                    model,
                    prepared,
                    request,
                    chunk,
                    reporter.as_ref(),
                    DecodeOptions::resilient(0.2, None),
                ) {
                    Ok(mut result) => {
                        shift_segments(
                            &mut result.segments,
                            chunk.start_ms(prepared.sample_rate_hz),
                            chunk.end_ms(prepared.sample_rate_hz),
                        );
                        accepted_segments.extend(result.segments);
                        warnings.push(TranscriptWarning {
                            start_ms: chunk.start_ms(prepared.sample_rate_hz),
                            end_ms: chunk.end_ms(prepared.sample_rate_hz),
                            reason: clean_decode_error(&primary_error),
                            attempts: 2,
                            outcome: "recovered".to_string(),
                        });
                        prompt_allowed = false;
                    }
                    Err(retry_error) if is_recoverable_decode_error(&retry_error) => {
                        let split_recovered = recover_split_chunk(
                            context,
                            &mut decoder_state,
                            model,
                            prepared,
                            request,
                            chunk,
                            reporter.as_ref(),
                            &mut accepted_segments,
                            &mut warnings,
                            &retry_error,
                        )?;
                        if !split_recovered {
                            add_gap_segment(
                                &job_id,
                                chunk,
                                prepared.sample_rate_hz,
                                &mut accepted_segments,
                            );
                            warnings.push(TranscriptWarning {
                                start_ms: chunk.start_ms(prepared.sample_rate_hz),
                                end_ms: chunk.end_ms(prepared.sample_rate_hz),
                                reason: clean_decode_error(&retry_error),
                                attempts: 3,
                                outcome: "skipped".to_string(),
                            });
                        }
                        prompt_allowed = false;
                    }
                    Err(error) => return Err(error),
                }
            }
            Err(error) => return Err(error),
        }
    }

    if let Some(progress) = progress {
        progress.store(100, Ordering::Relaxed);
    }

    let segments = stitch_segments(accepted_segments);
    if segments.is_empty() {
        return Err(anyhow!("TRANSCRIPTION_EMPTY: whisper produced no segments"));
    }

    Ok(build_transcript_result(job_id, model, segments, warnings))
}

fn decode_audio_chunk(
    _context: &WhisperContext,
    decoder_state: &mut WhisperState,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    chunk: AudioChunk,
    reporter: Option<&ProgressReporter>,
    options: DecodeOptions,
) -> Result<TranscriptResult> {
    crate::shutdown::ensure_running()?;
    let chunk_audio = audio_preprocess::PreparedAudio {
        sample_rate_hz: prepared.sample_rate_hz,
        channels: prepared.channels,
        samples: prepared.samples[chunk.start_sample..chunk.end_sample].to_vec(),
    };

    // Auto language: whisper.cpp detects the language of this (<30 s) chunk
    // and transcribes it in the same call, so no separate detection pass or
    // extra decoder state is needed.
    run_whisper_with_state(
        decoder_state,
        model,
        &chunk_audio,
        request,
        None,
        reporter,
        options,
    )
}

pub(crate) fn detect_vad_splits(
    vad_model_path: &Path,
    prepared: &audio_preprocess::PreparedAudio,
) -> Result<Vec<usize>> {
    let path = vad_model_path
        .to_str()
        .ok_or_else(|| anyhow!("VAD model path is not valid UTF-8"))?;
    let mut context = WhisperVadContext::new(path, WhisperVadContextParams::default())
        .context("failed to load VAD model")?;
    let mut params = WhisperVadParams::default();
    params.set_min_speech_duration(150);
    params.set_min_silence_duration(300);
    params.set_max_speech_duration(28.0);
    params.set_speech_pad(120);
    let segments = context
        .segments_from_samples(params, &prepared.samples)
        .context("failed to analyze speech boundaries")?;
    let mut boundaries = Vec::new();
    for segment in segments {
        for timestamp_cs in [segment.start, segment.end] {
            let sample =
                (timestamp_cs.max(0.0) as f64 / 100.0 * prepared.sample_rate_hz as f64) as usize;
            if sample > 0 && sample < prepared.samples.len() {
                boundaries.push(sample);
            }
        }
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    Ok(boundaries)
}

#[allow(clippy::too_many_arguments)]
fn recover_split_chunk(
    context: &WhisperContext,
    decoder_state: &mut WhisperState,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    chunk: AudioChunk,
    reporter: Option<&ProgressReporter>,
    accepted_segments: &mut Vec<TranscriptSegment>,
    warnings: &mut Vec<TranscriptWarning>,
    source_error: &anyhow::Error,
) -> Result<bool> {
    if chunk.duration_ms(prepared.sample_rate_hz) < MIN_SPLIT_RETRY_MS * 2 {
        return Ok(false);
    }
    let Some((left, right)) =
        split_chunk_near_middle(&prepared.samples, chunk, prepared.sample_rate_hz)
    else {
        return Ok(false);
    };

    let mut recovered_any = false;
    let mut skipped_any = false;
    for piece in [left, right] {
        match decode_audio_chunk(
            context,
            decoder_state,
            model,
            prepared,
            request,
            piece,
            reporter,
            DecodeOptions::resilient(0.2, None),
        ) {
            Ok(mut result) => {
                shift_segments(
                    &mut result.segments,
                    piece.start_ms(prepared.sample_rate_hz),
                    piece.end_ms(prepared.sample_rate_hz),
                );
                accepted_segments.extend(result.segments);
                recovered_any = true;
            }
            Err(error) if is_recoverable_decode_error(&error) => {
                add_gap_segment(
                    "recovery",
                    piece,
                    prepared.sample_rate_hz,
                    accepted_segments,
                );
                warnings.push(TranscriptWarning {
                    start_ms: piece.start_ms(prepared.sample_rate_hz),
                    end_ms: piece.end_ms(prepared.sample_rate_hz),
                    reason: clean_decode_error(&error),
                    attempts: 3,
                    outcome: "skipped".to_string(),
                });
                skipped_any = true;
            }
            Err(error) => return Err(error),
        }
    }

    if recovered_any {
        warnings.push(TranscriptWarning {
            start_ms: chunk.start_ms(prepared.sample_rate_hz),
            end_ms: chunk.end_ms(prepared.sample_rate_hz),
            reason: clean_decode_error(source_error),
            attempts: 3,
            outcome: if skipped_any {
                "partially_recovered".to_string()
            } else {
                "recovered".to_string()
            },
        });
    }
    Ok(recovered_any || skipped_any)
}

fn shift_segments(segments: &mut [TranscriptSegment], offset_ms: i64, chunk_end_ms: i64) {
    for segment in segments {
        segment.start_ms = (segment.start_ms + offset_ms).clamp(offset_ms, chunk_end_ms);
        segment.end_ms = (segment.end_ms + offset_ms).clamp(segment.start_ms, chunk_end_ms);
    }
}

fn add_gap_segment(
    job_id: &str,
    chunk: AudioChunk,
    sample_rate_hz: u32,
    segments: &mut Vec<TranscriptSegment>,
) {
    let start_ms = chunk.start_ms(sample_rate_hz);
    let end_ms = chunk.end_ms(sample_rate_hz);
    segments.push(TranscriptSegment {
        id: format!("{job_id}:gap:{start_ms}"),
        start_ms,
        end_ms,
        text: format!(
            "[Unclear audio {}–{}]",
            crate::output_format::clock_ms(start_ms),
            crate::output_format::clock_ms(end_ms)
        ),
        language_code: "und".to_string(),
        segment_order: 0,
        confidence: None,
        speaker_id: None,
        speaker_ids: None,
        speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
        speaker_confidence: None,
    });
}

fn controlled_prompt(segments: &[TranscriptSegment], chunk_start_ms: i64) -> Option<String> {
    let last = segments.last()?;
    if chunk_start_ms - last.end_ms > CONTROLLED_PROMPT_RESET_SILENCE_MS {
        return None;
    }

    let combined = segments
        .iter()
        .rev()
        .take(2)
        .rev()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let chars = combined.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(CONTROLLED_PROMPT_MAX_CHARS);
    let prompt = chars[start..].iter().collect::<String>().trim().to_string();
    (!prompt.is_empty()).then_some(prompt)
}

/// whisper.cpp keeps at most `n_max_text_ctx` (64) prompt tokens, taken from
/// the end. Keep the dictionary short and put the highest-priority terms last
/// so they survive truncation.
const WHISPER_DICTIONARY_PROMPT_MAX_CHARS: usize = 150;
const WHISPER_COMBINED_PROMPT_MAX_CHARS: usize = 200;

pub(crate) fn whisper_dictionary_prompt(terms: &[String]) -> Option<String> {
    let mut included: Vec<&str> = Vec::new();
    let mut length = 0usize;
    for term in terms {
        let term = term.trim();
        if term.is_empty() {
            continue;
        }
        let added = term.chars().count() + if included.is_empty() { 0 } else { 2 };
        if length + added + 1 > WHISPER_DICTIONARY_PROMPT_MAX_CHARS {
            continue;
        }
        length += added;
        included.push(term);
    }
    if included.is_empty() {
        return None;
    }
    included.reverse();
    Some(format!("{}.", included.join(", ")))
}

fn combine_whisper_prompts(dictionary: Option<&str>, continuity: Option<String>) -> Option<String> {
    match (dictionary, continuity) {
        (None, continuity) => continuity,
        (Some(dictionary), None) => Some(dictionary.to_string()),
        (Some(dictionary), Some(continuity)) => {
            let budget = WHISPER_COMBINED_PROMPT_MAX_CHARS
                .saturating_sub(dictionary.chars().count() + 1);
            let chars = continuity.chars().collect::<Vec<_>>();
            let tail = chars[chars.len().saturating_sub(budget)..]
                .iter()
                .collect::<String>();
            let tail = tail.trim();
            if tail.is_empty() {
                Some(dictionary.to_string())
            } else {
                Some(format!("{dictionary} {tail}"))
            }
        }
    }
}

fn is_recoverable_decode_error(error: &anyhow::Error) -> bool {
    error.to_string().contains("DECODER_REPETITION")
}

fn clean_decode_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    message
        .strip_prefix("DECODER_REPETITION: ")
        .unwrap_or(&message)
        .to_string()
}

pub(crate) fn build_transcript_result(
    job_id: String,
    model: &InstalledModel,
    segments: Vec<TranscriptSegment>,
    warnings: Vec<TranscriptWarning>,
) -> TranscriptResult {
    build_transcript_result_named(job_id, &model.model_name, segments, warnings)
}

pub(crate) fn build_transcript_result_named(
    job_id: String,
    model_name: &str,
    mut segments: Vec<TranscriptSegment>,
    warnings: Vec<TranscriptWarning>,
) -> TranscriptResult {
    for (index, segment) in segments.iter_mut().enumerate() {
        segment.id = format!("{job_id}:{index}");
        segment.segment_order = index as i32;
    }
    let plain_text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let timestamped_text = segments
        .iter()
        .map(|segment| {
            format!(
                "[{} - {}] {}: {}",
                crate::output_format::clock_ms(segment.start_ms),
                crate::output_format::clock_ms(segment.end_ms),
                segment.language_code,
                segment.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut detected_languages = Vec::new();
    for segment in &segments {
        if segment.language_code != "und" && !detected_languages.contains(&segment.language_code) {
            detected_languages.push(segment.language_code.clone());
        }
    }
    let partial = warnings
        .iter()
        .any(|warning| warning.outcome == "skipped" || warning.outcome == "partially_recovered");
    let quality_status = if partial {
        TranscriptQualityStatus::Partial
    } else if warnings.is_empty() {
        TranscriptQualityStatus::Clean
    } else {
        TranscriptQualityStatus::Recovered
    };
    let recovered_region_count = warnings
        .iter()
        .filter(|warning| warning.outcome != "skipped")
        .count() as i32;

    TranscriptResult {
        job_id,
        model_name: model_name.to_string(),
        full_text: plain_text.clone(),
        plain_text,
        timestamped_text,
        detected_languages,
        segments,
        quality_status,
        recovered_region_count,
        warnings,
        diarization_status: DiarizationStatus::NotRequested,
        diarization_model_id: None,
        diarization_source: crate::diarization::DiarizationSource::None,
        diarization_warning: None,
        diarization_policy_version: None,
        diarization_clustering_threshold: None,
        diarization_speaker_count_hint: None,
        speakers: Vec::new(),
        diarization_turns: Vec::new(),
    }
}

fn run_whisper(
    context: &WhisperContext,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    progress: &Option<Arc<AtomicI32>>,
    vad_model_path: Option<&Path>,
) -> Result<TranscriptResult> {
    crate::shutdown::ensure_running()?;
    let duration_ms =
        (prepared.samples.len() as u128 * 1000 / prepared.sample_rate_hz.max(1) as u128) as i64;
    if duration_ms > DIRECT_FILE_MAX_MS && request.timestamps {
        return run_resilient_whisper(context, model, prepared, request, progress, vad_model_path);
    }
    set_step(Step::Transcribing);

    let reporter = progress.clone().map(ProgressReporter::full);
    let dictionary_prompt = whisper_dictionary_prompt(&request.context_terms);
    let first_attempt = run_whisper_once(
        context,
        model,
        prepared,
        request,
        None,
        reporter.as_ref(),
        DecodeOptions::direct(dictionary_prompt.clone()),
    )?;
    if !first_attempt.segments.is_empty() {
        if request.timestamps
            && repetition_reason(
                first_attempt
                    .segments
                    .iter()
                    .map(|segment| (segment.start_ms, segment.end_ms, segment.text.as_str())),
            )
            .is_some()
        {
            return run_resilient_whisper(
                context,
                model,
                prepared,
                request,
                progress,
                vad_model_path,
            );
        }
        return Ok(first_attempt);
    }

    let detected_language = first_attempt
        .detected_languages
        .first()
        .cloned()
        .filter(|language| language != "und" && language != "unknown");

    if matches!(request.language_mode, LanguageMode::Auto) {
        if let Some(language) = detected_language.clone() {
            let retry = run_whisper_once(
                context,
                model,
                prepared,
                request,
                Some(language),
                reporter.as_ref(),
                DecodeOptions::direct(dictionary_prompt.clone()),
            )?;
            if !retry.segments.is_empty() {
                if request.timestamps
                    && repetition_reason(
                        retry.segments.iter().map(|segment| {
                            (segment.start_ms, segment.end_ms, segment.text.as_str())
                        }),
                    )
                    .is_some()
                {
                    return run_resilient_whisper(
                        context,
                        model,
                        prepared,
                        request,
                        progress,
                        vad_model_path,
                    );
                }
                return Ok(retry);
            }
        }
    }

    if !request.timestamps {
        let mut timestamp_retry_request = request.clone();
        timestamp_retry_request.timestamps = true;
        let retry = run_whisper_once(
            context,
            model,
            prepared,
            &timestamp_retry_request,
            detected_language.clone(),
            reporter.as_ref(),
            DecodeOptions::direct(dictionary_prompt.clone()),
        )?;
        if !retry.segments.is_empty() {
            return Ok(retry);
        }
    }

    Err(anyhow!("TRANSCRIPTION_EMPTY: whisper produced no segments"))
}

fn run_whisper_once(
    context: &WhisperContext,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    forced_language: Option<String>,
    progress: Option<&ProgressReporter>,
    options: DecodeOptions,
) -> Result<TranscriptResult> {
    crate::shutdown::ensure_running()?;
    let mut state = context
        .create_state()
        .context("failed to create whisper state")?;
    run_whisper_with_state(
        &mut state,
        model,
        prepared,
        request,
        forced_language,
        progress,
        options,
    )
}

// whisper-rs 0.16.0's set_abort_callback_safe stores a Box<dyn FnMut() -> bool>
// but casts that allocation back to F in its trampoline. For F = fn() -> bool,
// it reads the trait object's heap data pointer as a code pointer and crashes
// on the first encoder callback. Use the C ABI directly, without user data.
extern "C" fn whisper_shutdown_abort(_user_data: *mut std::ffi::c_void) -> bool {
    crate::shutdown::is_shutting_down()
}

fn install_shutdown_abort_callback(params: &mut FullParams<'_, '_>) {
    // SAFETY: this is a process-lifetime C function with the exact ggml abort
    // signature. It reads only the thread-safe shutdown flag, never dereferences
    // user data or touches Whisper state, and has no captured allocation to free.
    unsafe {
        params.set_abort_callback(Some(whisper_shutdown_abort));
        params.set_abort_callback_user_data(std::ptr::null_mut());
    }
}

fn run_whisper_with_state(
    state: &mut WhisperState,
    model: &InstalledModel,
    prepared: &audio_preprocess::PreparedAudio,
    request: &FileTranscriptionRequest,
    forced_language: Option<String>,
    progress: Option<&ProgressReporter>,
    options: DecodeOptions,
) -> Result<TranscriptResult> {
    crate::shutdown::ensure_running()?;
    let is_dictation = matches!(
        request.use_context,
        Some(
            crate::model_metadata::ModelUseContext::ShortcutDictation
                | crate::model_metadata::ModelUseContext::QuickDictate
        )
    );
    // Dictation favours latency: greedy decoding (with whisper's temperature
    // fallback) instead of a five-way beam search. Files keep beam search.
    let mut params = FullParams::new(if is_dictation {
        SamplingStrategy::Greedy { best_of: 1 }
    } else {
        SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        }
    });
    let threads = std::thread::available_parallelism()
        .map(|value| value.get().min(8) as i32)
        .unwrap_or(4);
    install_shutdown_abort_callback(&mut params);
    params.set_n_threads(threads);
    params.set_translate(false);
    params.set_no_context(true);
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_timestamps(!request.timestamps);

    // Anti-hallucination / repetition-loop settings
    params.set_temperature(options.temperature);
    params.set_temperature_inc(0.2); // retry with increasing temperature on fallback
    params.set_entropy_thold(2.4); // trigger fallback when token entropy is low (repetitive)
    if options.repetition_watchdog {
        params.set_logprob_thold(-1.0);
        params.set_no_speech_thold(0.6);
    }
    params.set_suppress_blank(true);
    params.set_suppress_nst(true); // suppress non-speech tokens like "[Music]", "Subtitles by..."
    params.set_n_max_text_ctx(64); // limit past context to prevent loop propagation

    if let Some(progress_reporter) = progress.cloned() {
        params.set_progress_callback_safe(move |pct: i32| {
            progress_reporter.report(pct);
        });
    }

    if let Some(prompt) = options.initial_prompt.as_deref() {
        params.set_initial_prompt(prompt);
    }

    match request.language_mode {
        LanguageMode::Auto => {
            if let Some(language) = forced_language.as_deref() {
                params.set_language(Some(language));
                params.set_detect_language(false);
            } else {
                // `detect_language(true)` would make whisper.cpp stop after
                // detection. "auto" detects and then transcribes in one pass.
                params.set_language(Some("auto"));
                params.set_detect_language(false);
            }
        }
        LanguageMode::Fixed => {
            params.set_language(request.fixed_language.as_deref());
            params.set_detect_language(false);
        }
    }

    state
        .full(params, &prepared.samples)
        .context("failed to run whisper transcription")?;

    let detected_language = forced_language
        .or_else(|| {
            request
                .fixed_language
                .clone()
                .filter(|_| matches!(request.language_mode, LanguageMode::Fixed))
        })
        .or_else(|| {
            whisper_rs::get_lang_str(state.full_lang_id_from_state()).map(ToString::to_string)
        })
        .map(|language| crate::output_format::normalize_language_code(Some(&language)))
        .unwrap_or_else(|| "und".to_string());

    let job_id = Uuid::new_v4().to_string();
    let mut segments = Vec::new();

    for (index, segment) in state.as_iter().enumerate() {
        let text = segment.to_string().trim().to_string();
        if text.is_empty() {
            continue;
        }
        let start_ms = timestamp_units_to_ms(segment.start_timestamp());
        let end_ms = timestamp_units_to_ms(segment.end_timestamp());
        let speech_confidence = (1.0 - segment.no_speech_probability()).clamp(0.0, 1.0);

        // Skip segments where Whisper is 80%+ confident there is no speech —
        // these are the most likely source of hallucinated / repeated text.
        if speech_confidence < 0.2 {
            continue;
        }

        segments.push(TranscriptSegment {
            id: format!("{job_id}:{index}"),
            start_ms,
            end_ms,
            text,
            language_code: detected_language.clone(),
            segment_order: index as i32,
            confidence: Some(speech_confidence),
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
            speaker_confidence: None,
        });
    }

    if options.repetition_watchdog {
        if let Some(reason) = repetition_reason(
            segments
                .iter()
                .map(|segment| (segment.start_ms, segment.end_ms, segment.text.as_str())),
        ) {
            return Err(anyhow!("DECODER_REPETITION: {reason}"));
        }
    }

    let plain_text = segments
        .iter()
        .map(|s| s.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let timestamped_text = segments
        .iter()
        .map(|s| {
            format!(
                "[{} - {}] {}: {}",
                crate::output_format::clock_ms(s.start_ms),
                crate::output_format::clock_ms(s.end_ms),
                s.language_code,
                s.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let detected_languages = if detected_language == "und" {
        Vec::new()
    } else {
        vec![detected_language]
    };

    Ok(TranscriptResult {
        job_id,
        model_name: model.model_name.clone(),
        full_text: plain_text.clone(),
        plain_text,
        timestamped_text,
        detected_languages,
        segments,
        quality_status: TranscriptQualityStatus::Clean,
        recovered_region_count: 0,
        warnings: Vec::new(),
        diarization_status: DiarizationStatus::NotRequested,
        diarization_model_id: None,
        diarization_source: crate::diarization::DiarizationSource::None,
        diarization_warning: None,
        diarization_policy_version: None,
        diarization_clustering_threshold: None,
        diarization_speaker_count_hint: None,
        speakers: Vec::new(),
        diarization_turns: Vec::new(),
    })
}

fn model_id_from_name(name: &str) -> String {
    name.to_ascii_lowercase().replace('.', "-")
}

fn profile_for_model_name(name: &str) -> ModelProfile {
    let normalized = name.to_ascii_lowercase();
    if normalized.contains("tiny") {
        ModelProfile::Fast
    } else if normalized.contains("small") {
        ModelProfile::Balanced
    } else {
        ModelProfile::Accurate
    }
}

fn is_default_model(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default();
    matches!(
        name,
        "ggml-small.bin" | "ggml-medium.bin" | "ggml-large-v3-turbo-q5_0.bin"
    )
}

fn to_profile_name(profile: ModelProfile) -> &'static str {
    match profile {
        ModelProfile::Fast => "fast",
        ModelProfile::Balanced => "balanced",
        ModelProfile::Accurate => "accurate",
    }
}

fn timestamp_units_to_ms(value: i64) -> i64 {
    value * 10
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whisper_dictionary_prompt_is_short_and_keeps_priority_terms_last() {
        let terms: Vec<String> = (0..100).map(|i| format!("Fachbegriff{i}")).collect();
        let prompt = whisper_dictionary_prompt(&terms).unwrap();
        assert!(prompt.chars().count() <= WHISPER_DICTIONARY_PROMPT_MAX_CHARS);
        assert!(prompt.ends_with("Fachbegriff0."));
        assert!(whisper_dictionary_prompt(&[]).is_none());
        assert_eq!(
            whisper_dictionary_prompt(&["Nordwind".into(), "Casa Verde".into()]).as_deref(),
            Some("Casa Verde, Nordwind.")
        );
    }

    #[test]
    fn combined_whisper_prompt_keeps_dictionary_and_recent_context() {
        let combined =
            combine_whisper_prompts(Some("Nordwind."), Some("x".repeat(400))).unwrap();
        assert!(combined.starts_with("Nordwind. "));
        assert!(combined.chars().count() <= WHISPER_COMBINED_PROMPT_MAX_CHARS);
        assert_eq!(combine_whisper_prompts(None, None), None);
    }

    /// Runs in a dedicated process so C++ static destructors are exercised.
    /// Set MURMELA_WHISPER_SMOKE_MODEL to an installed Whisper .bin model.
    #[test]
    #[ignore = "requires MURMELA_WHISPER_SMOKE_MODEL and a macOS Metal device"]
    fn metal_cache_release_exits_cleanly() {
        let path = std::env::var("MURMELA_WHISPER_SMOKE_MODEL").expect("model path");
        let dir = Path::new(&path).parent().unwrap();
        let models = discover_whisper_models(dir).unwrap();
        let model = models
            .iter()
            .find(|m| m.local_path == path)
            .unwrap()
            .clone();
        let engine = LocalTranscriptionEngine::new(dir.into(), models);
        let context = engine
            .whisper
            .obtain_context(&model, true)
            .expect("Metal model load");
        assert_eq!(Arc::strong_count(&context), 2);
        drop(context);
        engine.release_resources();
        assert!(engine.whisper.context_cache.lock().unwrap().is_none());
        // Tauri keeps managed state alive until process termination. Mimic
        // that lifetime; the cache must already be empty before libc exit.
        std::mem::forget(engine);
    }

    /// Exercise actual decoder callbacks, not just loading/freeing Metal models.
    /// Run this ignored test alone: its final step permanently begins shutdown.
    #[test]
    #[ignore = "requires MURMELA_WHISPER_SMOKE_MODEL, MURMELA_WHISPER_SMOKE_AUDIO and macOS Metal"]
    fn native_abort_callback_decodes_and_cancels_cleanly() {
        let model_path = std::env::var("MURMELA_WHISPER_SMOKE_MODEL").expect("model path");
        let audio_path = std::env::var("MURMELA_WHISPER_SMOKE_AUDIO").expect("audio path");
        let prepared = audio_preprocess::decode_audio_file(Path::new(&audio_path)).unwrap();
        assert!(
            prepared.samples.len() > 16_000,
            "provide at least one second of nonempty test speech"
        );
        let dir = Path::new(&model_path).parent().unwrap();
        let models = discover_whisper_models(dir).unwrap();
        let model = models
            .iter()
            .find(|m| m.local_path == model_path)
            .unwrap()
            .clone();
        let engine = LocalTranscriptionEngine::new(dir.into(), models);
        // Cold GPU context, reused GPU context, and CPU path all use the same
        // production registration code as the first shortcut transcription.
        for prefer_gpu in [true, true, false] {
            let result = engine
                .transcribe_file(
                    FileTranscriptionRequest {
                        use_context: Some(
                            crate::model_metadata::ModelUseContext::ShortcutDictation,
                        ),
                        profile: model.profile,
                        selected_model_id: Some(model.id.clone()),
                        language_mode: LanguageMode::Fixed,
                        fixed_language: Some("en".into()),
                        timestamps: false,
                        prefer_gpu,
                        file_path: audio_path.clone(),
                        context_prompt: None,
                        context_terms: Vec::new(),
                    },
                    None,
                )
                .expect("native speech decoding must complete without a bad callback jump");
            assert!(
                !result.plain_text.trim().is_empty(),
                "speech must produce a transcript"
            );
            eprintln!("[callback-smoke] decoded speech successfully (prefer_gpu={prefer_gpu})");
        }

        let context = engine.whisper.obtain_context(&model, true).unwrap();
        let mut state = context.create_state().unwrap();
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(4);
        params.set_language(Some("en"));
        params.set_print_progress(false);
        params.set_print_realtime(false);
        install_shutdown_abort_callback(&mut params);
        crate::shutdown::begin_shutdown_for_decoder_test();
        // Model a quit accepted just after the application's pre-decode check.
        // Call the backend so the C callback itself must observe cancellation.
        assert!(
            state.full(params, &prepared.samples).is_err(),
            "native decoding must abort during shutdown"
        );
        eprintln!("[callback-smoke] native cancellation returned an error without crashing");
        drop(state);
        drop(context);
        engine.release_resources();
        assert!(engine.whisper.context_cache.lock().unwrap().is_none());
        std::mem::forget(engine);
    }

    fn model() -> InstalledModel {
        InstalledModel {
            id: "test-model".to_string(),
            engine: "whisper.cpp".to_string(),
            model_name: "ggml-large-v3-turbo-q5_0.bin".to_string(),
            variant: "accurate".to_string(),
            local_path: "/tmp/test-model.bin".to_string(),
            size_bytes: 1,
            is_default: true,
            profile: ModelProfile::Accurate,
            capabilities: ModelCapabilities::standard_asr(),
        }
    }

    fn segment(language: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: "temporary".to_string(),
            start_ms: 1_000,
            end_ms: 2_000,
            text: "A valid sentence.".to_string(),
            language_code: language.to_string(),
            segment_order: 12,
            confidence: Some(0.9),
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
            speaker_confidence: None,
        }
    }

    #[test]
    fn result_metadata_distinguishes_recovery_from_partial_output() {
        let recovered = build_transcript_result(
            "job".to_string(),
            &model(),
            vec![segment("fr")],
            vec![TranscriptWarning {
                start_ms: 0,
                end_ms: 5_000,
                reason: "repetition".to_string(),
                attempts: 2,
                outcome: "recovered".to_string(),
            }],
        );
        assert_eq!(recovered.quality_status, TranscriptQualityStatus::Recovered);
        assert_eq!(recovered.recovered_region_count, 1);
        assert_eq!(recovered.detected_languages, vec!["fr"]);
        assert_eq!(recovered.segments[0].segment_order, 0);

        let partial = build_transcript_result(
            "job".to_string(),
            &model(),
            vec![segment("und")],
            vec![TranscriptWarning {
                start_ms: 0,
                end_ms: 5_000,
                reason: "repetition".to_string(),
                attempts: 3,
                outcome: "skipped".to_string(),
            }],
        );
        assert_eq!(partial.quality_status, TranscriptQualityStatus::Partial);
        assert!(partial.detected_languages.is_empty());
    }

    #[test]
    fn local_engine_routes_explicit_qwen_without_changing_whisper_default() {
        let whisper = model();
        let qwen = InstalledModel {
            id: crate::qwen_asr::QWEN_MODEL_ID.to_string(),
            engine: "qwen3_asr_c".to_string(),
            model_name: crate::qwen_asr::QWEN_MODEL_NAME.to_string(),
            variant: "1.7B BF16".to_string(),
            local_path: "/tmp/qwen-model".to_string(),
            size_bytes: crate::qwen_asr::QWEN_TOTAL_SIZE,
            is_default: false,
            profile: ModelProfile::Accurate,
            capabilities: ModelCapabilities::standard_asr(),
        };
        let engine = LocalTranscriptionEngine::new(
            PathBuf::from("/tmp/models"),
            vec![whisper.clone(), qwen.clone()],
        );
        assert_eq!(
            engine
                .resolve_model(Some(crate::qwen_asr::QWEN_MODEL_ID), ModelProfile::Accurate)
                .expect("explicit Qwen")
                .engine,
            "qwen3_asr_c"
        );
        assert_eq!(
            engine
                .resolve_model(None, ModelProfile::Accurate)
                .expect("default model")
                .id,
            whisper.id
        );
    }

    #[test]
    fn typed_engine_errors_preserve_machine_readable_codes() {
        let error = anyhow!("MODEL_BUSY: another Qwen transcription is running");
        let payload = engine_error_payload(&error);
        assert_eq!(payload.code, "model_busy");
        assert_eq!(payload.message, error.to_string());
    }

    #[test]
    fn explicit_uninstalled_qwen_does_not_silently_fall_back_to_whisper() {
        let engine = LocalTranscriptionEngine::new(PathBuf::from("/tmp/models"), vec![model()]);
        let error = engine
            .resolve_model(Some(crate::qwen_asr::QWEN_MODEL_ID), ModelProfile::Accurate)
            .expect_err("incomplete Qwen must fail");
        assert!(error.to_string().starts_with("MODEL_INCOMPLETE:"));
    }
}
