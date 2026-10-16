use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use anyhow::{anyhow, Context, Result};
use reqwest::blocking::{Client, Response};
use reqwest::header::RANGE;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager};

use crate::asr::{
    discover_installed_models, InstalledModel, LocalTranscriptionEngine, TranscriptionEngine,
};
use crate::diarization::DIARIZATION_MODEL_ID;
use crate::network_activity::{self, Destination, Outcome};
use crate::model_metadata::{
    capabilities_for_model, MOSS_MODEL_DIR, MOSS_MODEL_ID, MOSS_MODEL_NAME, MOSS_MODEL_REVISION,
    MOSS_MODEL_SIZE, VIBEVOICE_MODEL_DIR, VIBEVOICE_MODEL_ID, VIBEVOICE_MODEL_NAME,
    VIBEVOICE_MODEL_REVISION, VIBEVOICE_MODEL_SIZE,
};
#[cfg(test)]
use crate::qwen_asr::QWEN_REQUIRED_ARTIFACTS;
use crate::qwen_asr::{
    QWEN_MODEL_DIR, QWEN_MODEL_ID, QWEN_MODEL_NAME, QWEN_MODEL_REVISION, QWEN_TOTAL_SIZE,
};
use crate::settings::ModelProfile;
use crate::storage;

const MODEL_DOWNLOAD_EVENT: &str = "model-download-status";
pub const VAD_MODEL_NAME: &str = "ggml-silero-v6.2.0.bin";
pub const DIARIZATION_MODEL_DIR: &str = "sherpa-diarization-pyannote3-eres2net-voxceleb-v2";
/// Shared completion marker for every directory-based model package.
/// Keep the historical filename so existing Qwen installations remain valid.
pub const MODEL_COMPLETE_FILE: &str = ".blabber-model.json";
pub const DIARIZATION_ARTIFACTS_REVIEWED: bool = true;
pub const DIARIZATION_TOTAL_SIZE: i64 = 32_478_041;
pub const DIARIZATION_REVISION: &str = "segmentation@340b52f1f5cd12d45a30fa284691417eaad2ff92+embedding@8be2a75c9ed7a590538b268e46fbb65e1aa9d208";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelAvailability {
    Available,
    UnsupportedPlatform,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelCapability {
    Asr,
    Vad,
    Diarization,
    Translation,
}

/// Where a listed model comes from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelOrigin {
    /// Offered for download by Murmela.
    Catalog,
    /// Added to the models folder by hand; not verified by Murmela.
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadableModel {
    pub id: String,
    pub engine: String,
    pub model_name: String,
    pub description: String,
    pub size_bytes: i64,
    pub profile: ModelProfile,
    pub availability: ModelAvailability,
    pub requirements: Option<String>,
    pub availability_reason: Option<String>,
    pub installed: bool,
    pub artifact_count: u32,
    pub capability: ModelCapability,
    pub capabilities: crate::model_metadata::ModelCapabilities,
    pub license_url: Option<String>,
    pub origin: ModelOrigin,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelDownloadState {
    Idle,
    Downloading,
    Completed,
    Canceled,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelDownloadStatus {
    pub model_id: String,
    pub model_name: String,
    pub state: ModelDownloadState,
    pub downloaded_bytes: i64,
    pub total_bytes: Option<i64>,
    pub progress_percent: Option<f32>,
    pub error_message: Option<String>,
    pub current_artifact: Option<String>,
    pub artifact_index: Option<u32>,
    pub artifact_count: u32,
}

#[derive(Clone)]
pub struct ModelDownloadManager {
    app: AppHandle,
    models_dir: PathBuf,
    db_path: PathBuf,
    engine: Arc<LocalTranscriptionEngine>,
    statuses: Arc<Mutex<HashMap<String, ModelDownloadStatus>>>,
    active_download: Arc<Mutex<Option<String>>>,
    cancellation_flags: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl ModelDownloadManager {
    pub fn new(
        app: AppHandle,
        models_dir: PathBuf,
        db_path: PathBuf,
        engine: Arc<LocalTranscriptionEngine>,
    ) -> Self {
        let statuses = downloadable_specs()
            .into_iter()
            .map(|spec| (spec.id.to_string(), idle_status(&spec)))
            .collect();
        Self {
            app,
            models_dir,
            db_path,
            engine,
            statuses: Arc::new(Mutex::new(statuses)),
            active_download: Arc::new(Mutex::new(None)),
            cancellation_flags: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn statuses(&self) -> Vec<ModelDownloadStatus> {
        let mut values = self
            .statuses
            .lock()
            .map(|statuses| statuses.values().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        values.sort_by(|left, right| left.model_name.cmp(&right.model_name));
        values
    }

    pub fn cancel_for_shutdown(&self) {
        if let Ok(flags) = self.cancellation_flags.lock() {
            for cancelled in flags.values() {
                cancelled.store(true, Ordering::SeqCst);
            }
        }
    }

    pub fn start_download(&self, model_id: &str) -> Result<ModelDownloadStatus> {
        let work = crate::shutdown::begin_work(false)?;
        let spec = downloadable_specs()
            .into_iter()
            .find(|entry| entry.id == model_id)
            .ok_or_else(|| anyhow!("Unknown model download: {model_id}"))?;
        if let Some(version) = required_terms_version(spec.id) {
            if !crate::storage::model_terms_accepted(&self.db_path, spec.id, version)? {
                return Err(anyhow!(
                    "MODEL_TERMS_REQUIRED: Accept the Gemma Terms of Use before downloading {}.",
                    spec.model_name
                ));
            }
        }
        match spec.availability() {
            ModelAvailability::Available => {}
            ModelAvailability::UnsupportedPlatform => {
                return Err(anyhow!(
                    "MODEL_UNSUPPORTED_PLATFORM: {} is not available on this Mac",
                    spec.model_name
                ))
            }
        }

        ensure_disk_space(&spec, &self.models_dir)?;

        let mut active = self
            .active_download
            .lock()
            .map_err(|_| anyhow!("download state is unavailable"))?;
        if let Some(current) = &*active {
            if current == model_id {
                return self
                    .statuses
                    .lock()
                    .ok()
                    .and_then(|statuses| statuses.get(model_id).cloned())
                    .ok_or_else(|| anyhow!("Model download state is unavailable."));
            }
            return Err(anyhow!("Another model download is already in progress."));
        }
        *active = Some(model_id.to_string());
        drop(active);

        let cancelled = Arc::new(AtomicBool::new(false));
        self.cancellation_flags
            .lock()
            .map_err(|_| anyhow!("download cancellation state is unavailable"))?
            .insert(model_id.to_string(), Arc::clone(&cancelled));

        let initial = downloading_status(&spec, 0, None, None);
        self.update_status(initial.clone());

        let app = self.app.clone();
        let models_dir = self.models_dir.clone();
        let db_path = self.db_path.clone();
        let engine = Arc::clone(&self.engine);
        let statuses = Arc::clone(&self.statuses);
        let active_download = Arc::clone(&self.active_download);
        let cancellation_flags = Arc::clone(&self.cancellation_flags);
        thread::spawn(move || {
            let _work = work;
            let result = download_model(
                &spec,
                &models_dir,
                &db_path,
                &cancelled,
                |downloaded, artifact, index| {
                    persist_status(
                        &app,
                        &statuses,
                        downloading_status(&spec, downloaded, Some(artifact), Some(index)),
                    );
                },
            );

            let status = match result {
                Ok(()) => {
                    if spec.capability == ModelCapability::Asr {
                        match engine.refresh_from_disk() {
                            Ok(models) => {
                                let _ =
                                    storage::sync_installed_models_for_db_path(&db_path, &models);
                                let _ = storage::apply_preferred_model_defaults_for_db_path(
                                    &db_path, &models,
                                );
                            }
                            Err(_) => {
                                if let Ok(models) = discover_installed_models(&models_dir) {
                                    let _ = storage::sync_installed_models_for_db_path(
                                        &db_path, &models,
                                    );
                                }
                            }
                        }
                    }
                    if spec.id == crate::live_pair::MODEL_ID {
                        if let Some(state) = app.try_state::<crate::app_state::AppState>() {
                            state.dictation_controller.sync_live_pair();
                        }
                    }
                    if spec.capability == ModelCapability::Translation {
                        if let Some(state) = app.try_state::<crate::app_state::AppState>() {
                            // The new model becomes the choice unless another installed one already is.
                            let selected_installed = storage::get_settings_from_db_path(&db_path)
                                .ok()
                                .is_some_and(|settings| {
                                    settings.translation_model_id != spec.id
                                        && installed_translation_model_path(
                                            &models_dir,
                                            &settings.translation_model_id,
                                        )
                                        .is_some()
                                });
                            let activation = storage::update_settings_for_db_path(
                                &db_path,
                                crate::settings::SettingsPatch {
                                    translation_enabled: Some(true),
                                    translation_model_id: (!selected_installed)
                                        .then(|| spec.id.to_string()),
                                    ..Default::default()
                                },
                            )
                            .and_then(|_| state.dictation_controller.sync_shortcut_registration());
                            if let Err(error) = activation {
                                let _ = storage::update_settings_for_db_path(
                                    &db_path,
                                    crate::settings::SettingsPatch {
                                        translation_enabled: Some(false),
                                        ..Default::default()
                                    },
                                );
                                let _ = app.emit("translation-setup-error", error.to_string());
                            }
                        }
                    }
                    ModelDownloadStatus {
                        model_id: spec.id.to_string(),
                        model_name: spec.model_name.to_string(),
                        state: ModelDownloadState::Completed,
                        downloaded_bytes: spec.size_bytes,
                        total_bytes: Some(spec.size_bytes),
                        progress_percent: Some(100.0),
                        error_message: None,
                        current_artifact: None,
                        artifact_index: Some(spec.artifacts.len() as u32),
                        artifact_count: spec.artifacts.len() as u32,
                    }
                }
                Err(error) if is_cancelled_error(&error) => {
                    let downloaded_bytes = statuses
                        .lock()
                        .ok()
                        .and_then(|values| {
                            values.get(spec.id).map(|status| status.downloaded_bytes)
                        })
                        .unwrap_or(0);
                    ModelDownloadStatus {
                        model_id: spec.id.to_string(),
                        model_name: spec.model_name.to_string(),
                        state: ModelDownloadState::Canceled,
                        downloaded_bytes,
                        total_bytes: Some(spec.size_bytes),
                        progress_percent: Some(
                            (downloaded_bytes as f64 / spec.size_bytes.max(1) as f64 * 100.0)
                                as f32,
                        ),
                        error_message: None,
                        current_artifact: None,
                        artifact_index: None,
                        artifact_count: spec.artifacts.len() as u32,
                    }
                }
                Err(error) => ModelDownloadStatus {
                    model_id: spec.id.to_string(),
                    model_name: spec.model_name.to_string(),
                    state: ModelDownloadState::Failed,
                    downloaded_bytes: 0,
                    total_bytes: Some(spec.size_bytes),
                    progress_percent: None,
                    error_message: Some(error.to_string()),
                    current_artifact: None,
                    artifact_index: None,
                    artifact_count: spec.artifacts.len() as u32,
                },
            };
            persist_status(&app, &statuses, status);
            if let Ok(mut current) = active_download.lock() {
                *current = None;
            }
            if let Ok(mut flags) = cancellation_flags.lock() {
                flags.remove(spec.id);
            }
        });
        Ok(initial)
    }

    /// Deletes an installed model. Loaded copies are released first, and
    /// selections that used it fall back to another installed model.
    pub fn delete_model(&self, model_id: &str) -> Result<()> {
        let _work = crate::shutdown::begin_work(false)?;
        if self
            .active_download
            .lock()
            .map_err(|_| anyhow!("download state is unavailable"))?
            .as_deref()
            == Some(model_id)
        {
            return Err(anyhow!("MODEL_BUSY: Cancel the download before deleting this model."));
        }
        let (target, spec) = deletion_target(&self.models_dir, model_id)?;
        let state = self.app.try_state::<crate::app_state::AppState>();
        if let Some(state) = &state {
            let queue = state.translation.processing_queue();
            if queue.busy() {
                return Err(anyhow!(
                    "MODEL_BUSY: A dictation or transcription is running. Delete the model when it has finished."
                ));
            }
            if model_id == crate::live_pair::MODEL_ID {
                state.live_pair.configure(None);
            }
            queue.evict_idle();
        }
        self.engine.release_resources();

        let removed = match &target {
            DeletionTarget::File(path) => {
                let _ = fs::remove_file(path.with_file_name(format!(
                    "{}.part",
                    path.file_name().unwrap_or_default().to_string_lossy()
                )));
                fs::remove_file(path)
            }
            DeletionTarget::Directory(path) => {
                if let Some(name) = path.file_name() {
                    let _ = fs::remove_dir_all(
                        self.models_dir.join(format!(".{}.part", name.to_string_lossy())),
                    );
                }
                fs::remove_dir_all(path)
            }
        };
        match removed {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(anyhow!("MODEL_DELETE_FAILED: {error}")),
        }

        self.refresh_installed()?;
        let mut patch = crate::settings::SettingsPatch::default();
        if spec.as_ref().is_some_and(|spec| spec.capability == ModelCapability::Translation) {
            // Another installed translation model takes over; without one, translation turns off.
            match crate::translation::MODELS.iter().find(|model| {
                installed_translation_model_path(&self.models_dir, model.id).is_some()
            }) {
                Some(other) => patch.translation_model_id = Some(other.id.to_string()),
                None => patch.translation_enabled = Some(false),
            }
            if let Some(state) = &state {
                state.translation.release_translation_model();
            }
        }
        if spec.as_ref().is_some_and(|spec| spec.capability == ModelCapability::Diarization) {
            patch.file_diarization_enabled = Some(false);
        }
        if patch.translation_enabled.is_some()
            || patch.translation_model_id.is_some()
            || patch.file_diarization_enabled.is_some()
        {
            storage::update_settings_for_db_path(&self.db_path, patch)?;
        }
        if let Some(state) = &state {
            let _ = state.dictation_controller.sync_shortcut_registration();
        }
        if let Some(spec) = spec {
            persist_status(&self.app, &self.statuses, idle_status(&spec));
        }
        Ok(())
    }

    /// Re-reads the models folder, so models added or removed by hand are
    /// picked up without a restart.
    pub fn refresh_installed(&self) -> Result<Vec<InstalledModel>> {
        let models = self
            .engine
            .refresh_from_disk()
            .or_else(|_| discover_installed_models(&self.models_dir))?;
        storage::sync_installed_models_for_db_path(&self.db_path, &models)?;
        storage::apply_preferred_model_defaults_for_db_path(&self.db_path, &models)?;
        Ok(models)
    }

    pub fn cancel_download(&self, model_id: &str) -> Result<ModelDownloadStatus> {
        let flag = self
            .cancellation_flags
            .lock()
            .map_err(|_| anyhow!("download cancellation state is unavailable"))?
            .get(model_id)
            .cloned()
            .ok_or_else(|| anyhow!("No active download exists for {model_id}."))?;
        flag.store(true, Ordering::Relaxed);
        self.statuses
            .lock()
            .map_err(|_| anyhow!("download state is unavailable"))?
            .get(model_id)
            .cloned()
            .ok_or_else(|| anyhow!("Model download state is unavailable."))
    }

    fn update_status(&self, status: ModelDownloadStatus) {
        persist_status(&self.app, &self.statuses, status);
    }
}

fn idle_status(spec: &DownloadableModelSpec) -> ModelDownloadStatus {
    ModelDownloadStatus {
        model_id: spec.id.to_string(),
        model_name: spec.model_name.to_string(),
        state: ModelDownloadState::Idle,
        downloaded_bytes: 0,
        total_bytes: Some(spec.size_bytes),
        progress_percent: Some(0.0),
        error_message: None,
        current_artifact: None,
        artifact_index: None,
        artifact_count: spec.artifacts.len() as u32,
    }
}

fn downloading_status(
    spec: &DownloadableModelSpec,
    downloaded_bytes: i64,
    artifact: Option<&str>,
    artifact_index: Option<u32>,
) -> ModelDownloadStatus {
    ModelDownloadStatus {
        model_id: spec.id.to_string(),
        model_name: spec.model_name.to_string(),
        state: ModelDownloadState::Downloading,
        downloaded_bytes,
        total_bytes: Some(spec.size_bytes),
        progress_percent: Some(
            (downloaded_bytes as f64 / spec.size_bytes.max(1) as f64 * 100.0) as f32,
        ),
        error_message: None,
        current_artifact: artifact.map(ToString::to_string),
        artifact_index,
        artifact_count: spec.artifacts.len() as u32,
    }
}

fn persist_status(
    app: &AppHandle,
    statuses: &Arc<Mutex<HashMap<String, ModelDownloadStatus>>>,
    status: ModelDownloadStatus,
) {
    let changed = if let Ok(mut map) = statuses.lock() {
        let previous = map.insert(status.model_id.clone(), status.clone());
        previous.is_some_and(|previous| {
            previous.state == ModelDownloadState::Downloading && previous.state != status.state
        })
    } else {
        false
    };
    if changed {
        let key = format!(
            "download:{}:{}",
            status.model_id,
            chrono::Utc::now().timestamp_millis()
        );
        match status.state {
            ModelDownloadState::Completed => {
                crate::sound::notify(app, crate::sound::FeedbackCue::Complete, &key)
            }
            ModelDownloadState::Failed => {
                crate::sound::notify(app, crate::sound::FeedbackCue::Error, &key)
            }
            _ => {}
        }
    }
    let _ = app.emit(MODEL_DOWNLOAD_EVENT, status);
}

/// Downloads a model from Hugging Face, logged as one entry in the network activity log.
fn download_model(
    spec: &DownloadableModelSpec,
    models_dir: &Path,
    db_path: &Path,
    cancelled: &AtomicBool,
    on_progress: impl FnMut(i64, &str, u32),
) -> Result<()> {
    let mut connection = network_activity::Connection::begin(
        db_path,
        Destination::HuggingFace,
        format!("Download {}", download_purpose(spec)),
    );
    let result = download_model_files(spec, models_dir, cancelled, &mut connection, on_progress);
    connection.finish(match &result {
        Ok(()) => Outcome::Completed,
        Err(error) if is_cancelled_error(error) => Outcome::Canceled,
        Err(_) => Outcome::Failed,
    });
    result
}

fn download_purpose(spec: &DownloadableModelSpec) -> String {
    if spec.capability == ModelCapability::Vad {
        "the voice-activity model (automatic, once)".to_string()
    } else {
        spec.model_name.to_string()
    }
}

fn download_model_files(
    spec: &DownloadableModelSpec,
    models_dir: &Path,
    cancelled: &AtomicBool,
    connection: &mut network_activity::Connection,
    mut on_progress: impl FnMut(i64, &str, u32),
) -> Result<()> {
    ensure_not_cancelled(cancelled)?;
    fs::create_dir_all(models_dir)?;
    match spec.layout {
        InstallLayout::File { file_name } => {
            let artifact = spec.artifacts.first().expect("file model artifact");
            let final_path = models_dir.join(file_name);
            if final_path.is_file() && verify_file(&final_path, artifact)? {
                return Ok(());
            }
            let temp_path = models_dir.join(format!("{file_name}.part"));
            download_artifact(artifact, &temp_path, 0, cancelled, connection, &mut on_progress, 1)?;
            fs::rename(temp_path, final_path)?;
        }
        InstallLayout::Directory { directory_name } => {
            let installed_valid = model_is_installed(spec, models_dir)
                && (spec.capability != ModelCapability::Translation
                    || spec.artifacts.iter().all(|artifact| {
                        verify_file(
                            &models_dir.join(directory_name).join(artifact.path),
                            artifact,
                        )
                        .unwrap_or(false)
                    }));
            if installed_valid {
                return Ok(());
            }
            let final_dir = models_dir.join(directory_name);
            let staging_dir = models_dir.join(format!(".{directory_name}.part"));
            fs::create_dir_all(&staging_dir)?;
            let mut completed_bytes = 0_i64;
            for (index, artifact) in spec.artifacts.iter().enumerate() {
                ensure_not_cancelled(cancelled)?;
                let final_artifact = staging_dir.join(artifact.path);
                if final_artifact.is_file() && verify_file(&final_artifact, artifact)? {
                    completed_bytes += artifact.size_bytes;
                    on_progress(completed_bytes, artifact.path, index as u32 + 1);
                    continue;
                }
                if final_artifact.exists() {
                    fs::remove_file(&final_artifact)?;
                }
                let partial = staging_dir.join(format!("{}.part", artifact.path));
                // Directory packages may nest files (CoreML bundles).
                if let Some(parent) = partial.parent() {
                    fs::create_dir_all(parent)?;
                }
                download_artifact(
                    artifact,
                    &partial,
                    completed_bytes,
                    cancelled,
                    connection,
                    &mut on_progress,
                    index as u32 + 1,
                )?;
                fs::rename(partial, final_artifact)?;
                completed_bytes += artifact.size_bytes;
            }
            write_completion_manifest(spec, &staging_dir)?;
            if final_dir.exists() {
                fs::remove_dir_all(&final_dir)?;
            }
            fs::rename(staging_dir, final_dir)?;
        }
    }
    Ok(())
}

fn download_artifact(
    artifact: &ModelArtifactSpec,
    partial_path: &Path,
    completed_before: i64,
    cancelled: &AtomicBool,
    connection: &mut network_activity::Connection,
    on_progress: &mut impl FnMut(i64, &str, u32),
    artifact_index: u32,
) -> Result<()> {
    ensure_not_cancelled(cancelled)?;
    let client = Client::builder().build()?;
    let mut existing = partial_path
        .metadata()
        .map(|value| value.len())
        .unwrap_or(0);
    if existing > artifact.size_bytes as u64 {
        fs::remove_file(partial_path)?;
        existing = 0;
    }

    let mut response = send_download_request(connection, &client, artifact.url, existing)?;
    if existing > 0 && response.status() != StatusCode::PARTIAL_CONTENT {
        fs::remove_file(partial_path)?;
        existing = 0;
        response = send_download_request(connection, &client, artifact.url, 0)?;
    }
    response = response
        .error_for_status()
        .with_context(|| format!("download failed for {}", artifact.path))?;

    let mut hasher = Sha256::new();
    if existing > 0 {
        let mut current = File::open(partial_path)?;
        let mut buffer = [0_u8; 256 * 1024];
        loop {
            let read = current.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .read(true)
        .open(partial_path)?;
    if existing == 0 {
        file.set_len(0)?;
    }
    file.seek(SeekFrom::Start(existing))?;

    let mut downloaded = existing as i64;
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        ensure_not_cancelled(cancelled)?;
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        connection.add_bytes(read as u64);
        downloaded += read as i64;
        on_progress(completed_before + downloaded, artifact.path, artifact_index);
    }
    file.flush()?;
    if downloaded != artifact.size_bytes {
        return Err(anyhow!(
            "Size mismatch for {}: expected {} bytes but received {}.",
            artifact.path,
            artifact.size_bytes,
            downloaded
        ));
    }
    let computed_hash = format!("{:x}", hasher.finalize());
    if computed_hash != artifact.sha256 {
        let _ = fs::remove_file(partial_path);
        return Err(anyhow!(
            "Checksum mismatch for {}. The partial download was removed; retry the model download.",
            artifact.path
        ));
    }
    Ok(())
}

fn ensure_not_cancelled(cancelled: &AtomicBool) -> Result<()> {
    if cancelled.load(Ordering::Relaxed) || crate::shutdown::is_shutting_down() {
        Err(anyhow!("MODEL_DOWNLOAD_CANCELED: download canceled"))
    } else {
        Ok(())
    }
}

fn is_cancelled_error(error: &anyhow::Error) -> bool {
    error.to_string().starts_with("MODEL_DOWNLOAD_CANCELED:")
}

fn send_download_request(
    connection: &mut network_activity::Connection,
    client: &Client,
    url: &str,
    start: u64,
) -> Result<Response> {
    let mut request = client.get(url);
    if start > 0 {
        request = request.header(RANGE, format!("bytes={start}-"));
    }
    connection
        .send(client, request)
        .with_context(|| format!("failed to download {url}"))
}

fn verify_file(path: &Path, artifact: &ModelArtifactSpec) -> Result<bool> {
    if path.metadata().map(|value| value.len() as i64).ok() != Some(artifact.size_bytes) {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()) == artifact.sha256)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletionManifest {
    model_id: String,
    revision: String,
    artifacts: Vec<CompletionArtifact>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletionArtifact {
    path: String,
    size_bytes: i64,
    sha256: String,
}

fn write_completion_manifest(spec: &DownloadableModelSpec, directory: &Path) -> Result<()> {
    let manifest = CompletionManifest {
        model_id: spec.id.to_string(),
        revision: spec.revision.unwrap_or("").to_string(),
        artifacts: spec
            .artifacts
            .iter()
            .map(|artifact| CompletionArtifact {
                path: artifact.path.to_string(),
                size_bytes: artifact.size_bytes,
                sha256: artifact.sha256.to_string(),
            })
            .collect(),
    };
    let partial = directory.join(format!("{MODEL_COMPLETE_FILE}.part"));
    let final_path = directory.join(MODEL_COMPLETE_FILE);
    let mut file = File::create(&partial)?;
    serde_json::to_writer_pretty(&mut file, &manifest)?;
    file.write_all(b"\n")?;
    file.flush()?;
    fs::rename(partial, final_path)?;
    Ok(())
}

pub fn start_background_vad_download(models_dir: PathBuf, db_path: PathBuf) {
    let final_path = models_dir.join(VAD_MODEL_NAME);
    if final_path.exists() {
        return;
    }
    let Ok(work) = crate::shutdown::begin_work(false) else {
        return;
    };
    thread::spawn(move || {
        let _work = work;
        let spec = vad_model_spec();
        if let Err(error) = download_model(
            &spec,
            &models_dir,
            &db_path,
            &AtomicBool::new(false),
            |_, _, _| {},
        ) {
            eprintln!(
                "[vad-model] download unavailable; using silence-based chunk boundaries: {error:#}"
            );
        }
    });
}

pub fn installed_vad_model_path(models_dir: &Path) -> Option<PathBuf> {
    let path = models_dir.join(VAD_MODEL_NAME);
    path.is_file().then_some(path)
}

#[derive(Clone, Copy)]
struct ModelArtifactSpec {
    path: &'static str,
    size_bytes: i64,
    url: &'static str,
    sha256: &'static str,
}

#[derive(Clone, Copy)]
enum InstallLayout {
    File { file_name: &'static str },
    Directory { directory_name: &'static str },
}

#[derive(Clone)]
struct DownloadableModelSpec {
    id: &'static str,
    engine: &'static str,
    model_name: &'static str,
    description: &'static str,
    requirements: Option<&'static str>,
    size_bytes: i64,
    profile: ModelProfile,
    layout: InstallLayout,
    revision: Option<&'static str>,
    artifacts: &'static [ModelArtifactSpec],
    capability: ModelCapability,
}

impl DownloadableModelSpec {

    fn availability(&self) -> ModelAvailability {
        if self.id == VIBEVOICE_MODEL_ID {
            return if crate::model_metadata::vibevoice_unavailable_reason().is_none()
                && crate::native_asr::worker_available(VIBEVOICE_MODEL_ID)
            {
                ModelAvailability::Available
            } else {
                ModelAvailability::UnsupportedPlatform
            };
        }
        if self.id == crate::live_pair::MODEL_ID {
            return if crate::live_pair::platform_supported() {
                ModelAvailability::Available
            } else {
                ModelAvailability::UnsupportedPlatform
            };
        }
        if self.capability == ModelCapability::Translation
            && !crate::translation::platform_supported()
        {
            ModelAvailability::UnsupportedPlatform
        } else {
            ModelAvailability::Available
        }
    }
}

macro_rules! single_file_artifact {
    ($name:literal, $size:literal, $url:literal, $sha:literal) => {
        &[ModelArtifactSpec {
            path: $name,
            size_bytes: $size,
            url: $url,
            sha256: $sha,
        }]
    };
}

const QWEN_ARTIFACTS: &[ModelArtifactSpec] = &[
    ModelArtifactSpec { path: "config.json", size_bytes: 6_194, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/config.json?download=true", sha256: "2e74a751548b8ad7d7526d29365ad8144c345d8b412b1152d25dc6698452712f" },
    ModelArtifactSpec { path: "generation_config.json", size_bytes: 142, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/generation_config.json?download=true", sha256: "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c" },
    ModelArtifactSpec { path: "model.safetensors.index.json", size_bytes: 64_821, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/model.safetensors.index.json?download=true", sha256: "f994739fe38e5210b9e3e8ce6c6307315e2ceac3cb630e7b7414d69dce520f60" },
    ModelArtifactSpec { path: "model-00001-of-00002.safetensors", size_bytes: 4_220_320_824, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/model-00001-of-00002.safetensors?download=true", sha256: "a4cd1f1a04d90b757dc7f7dd26254e69a013b19e80efe590a83c6a3bde8608d6" },
    ModelArtifactSpec { path: "model-00002-of-00002.safetensors", size_bytes: 478_200_688, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/model-00002-of-00002.safetensors?download=true", sha256: "6e0b9d9e09e2e0238e7ef3cc8a484ab387e91b90f1900bedf88bc92d7929ccfc" },
    ModelArtifactSpec { path: "vocab.json", size_bytes: 2_776_833, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/vocab.json?download=true", sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910" },
    ModelArtifactSpec { path: "merges.txt", size_bytes: 1_671_853, url: "https://huggingface.co/Qwen/Qwen3-ASR-1.7B/resolve/b188e100bd85038c06d2812d24a39776eba774ca/merges.txt?download=true", sha256: "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5" },
];

const DIARIZATION_ARTIFACTS: &[ModelArtifactSpec] = &[
    ModelArtifactSpec {
        path: "segmentation.onnx",
        size_bytes: 5_992_778,
        url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/340b52f1f5cd12d45a30fa284691417eaad2ff92/model.onnx?download=true",
        sha256: "fed22097bca974bad329a930b60865703766ff89f05fa09060bf6fd44e92e319",
    },
    ModelArtifactSpec {
        path: "embedding.onnx",
        size_bytes: 26_485_263,
        url: "https://huggingface.co/csukuangfj/speaker-embedding-models/resolve/8be2a75c9ed7a590538b268e46fbb65e1aa9d208/3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx?download=true",
        sha256: "c59158379255ad66e161679cca6af8d52d51e389e3224ab7d7a7baae295c2db5",
    },
];

const MOSS_ARTIFACTS: &[ModelArtifactSpec] = &[ModelArtifactSpec {
    path: "moss-transcribe-f16.gguf",
    size_bytes: MOSS_MODEL_SIZE,
    url: "https://huggingface.co/mudler/moss-transcribe.cpp-gguf/resolve/54e4bbd17da3f84adf1c1bcf7791b9b9266f741e/moss-transcribe-f16.gguf?download=true",
    sha256: "88a9518ccd9c7d04a3ebc2c49174f47cebe528096f949760942bc31f7ebe8046",
}];

const VIBEVOICE_ARTIFACTS: &[ModelArtifactSpec] = &[
    ModelArtifactSpec { path: "config.json", size_bytes: 4_372, url: "https://huggingface.co/mlx-community/VibeVoice-ASR-8bit/resolve/725c72e54d6ef875472c27fbc50fab470a960940/config.json?download=true", sha256: "f4418d57174253f52174c74d6dc3b53ae452d8234b2e007231bea53f2437f16a" },
    ModelArtifactSpec { path: "model.safetensors.index.json", size_bytes: 130_385, url: "https://huggingface.co/mlx-community/VibeVoice-ASR-8bit/resolve/725c72e54d6ef875472c27fbc50fab470a960940/model.safetensors.index.json?download=true", sha256: "8f282316181bcbd6bb4d7d57ce3e3c5601de35d9003b6bfe1da3a0a22a814a55" },
    ModelArtifactSpec { path: "model-00001-of-00002.safetensors", size_bytes: 5_331_193_271, url: "https://huggingface.co/mlx-community/VibeVoice-ASR-8bit/resolve/725c72e54d6ef875472c27fbc50fab470a960940/model-00001-of-00002.safetensors?download=true", sha256: "ce6e064d50295cb0100f33af8c69c9d2a3d647a8d375f764851e940180308650" },
    ModelArtifactSpec { path: "model-00002-of-00002.safetensors", size_bytes: 4_190_296_379, url: "https://huggingface.co/mlx-community/VibeVoice-ASR-8bit/resolve/725c72e54d6ef875472c27fbc50fab470a960940/model-00002-of-00002.safetensors?download=true", sha256: "53750f68f0fca138e70d8ed5eb38c29a02e3b44c3e530142a7b0cb3453bf455a" },
];

/// Both live-pair models, pinned per file in `workers/fluid/manifest.json`.
static LIVE_PAIR_ARTIFACTS: std::sync::LazyLock<Vec<ModelArtifactSpec>> =
    std::sync::LazyLock::new(|| {
        crate::live_pair::artifacts()
            .iter()
            .map(|artifact| ModelArtifactSpec {
                path: artifact.path.as_str(),
                size_bytes: artifact.bytes,
                url: artifact.url.as_str(),
                sha256: artifact.sha256.as_str(),
            })
            .collect()
    });

fn live_pair_spec() -> DownloadableModelSpec {
    DownloadableModelSpec {
        id: crate::live_pair::MODEL_ID,
        engine: crate::live_pair::ENGINE,
        model_name: crate::live_pair::MODEL_NAME,
        description: "Experimental shortcut dictation with live text while you speak (Nemotron 3.5 streaming) and a final pass over the whole recording on release (Parakeet v3), pasted once. Both models stay loaded on the Neural Engine. German, English and other Western European languages; up to five minutes.",
        requirements: Some("Apple Silicon · macOS 14+ · about 1.4 GB of memory while loaded · includes the 560 ms and 1120 ms preview variants"),
        size_bytes: crate::live_pair::total_bytes(),
        profile: ModelProfile::Fast,
        layout: InstallLayout::Directory { directory_name: crate::live_pair::MODEL_ID },
        revision: Some(crate::live_pair::revision()),
        artifacts: &LIVE_PAIR_ARTIFACTS,
        capability: ModelCapability::Asr,
    }
}

const TRANSLATION_ARTIFACTS: [&[ModelArtifactSpec]; 2] = {
    use crate::translation::MODELS;
    [
        &[ModelArtifactSpec { path: MODELS[0].file, size_bytes: MODELS[0].size, url: MODELS[0].url, sha256: MODELS[0].sha256 }],
        &[ModelArtifactSpec { path: MODELS[1].file, size_bytes: MODELS[1].size, url: MODELS[1].url, sha256: MODELS[1].sha256 }],
    ]
};

/// One TranslateGemma build from `translation::MODELS`.
fn translation_spec(
    index: usize,
    description: &'static str,
    requirements: &'static str,
    profile: ModelProfile,
) -> DownloadableModelSpec {
    let model = &crate::translation::MODELS[index];
    DownloadableModelSpec {
        id: model.id,
        engine: "llama.cpp-translation",
        model_name: model.name,
        description,
        requirements: Some(requirements),
        size_bytes: model.size,
        profile,
        layout: InstallLayout::Directory { directory_name: model.id },
        revision: Some(model.revision),
        artifacts: TRANSLATION_ARTIFACTS[index],
        capability: ModelCapability::Translation,
    }
}

/// Installed per the completion manifest. Like other packages, a warm check
/// compares sizes only; SHA-256 was verified once, after download.
pub fn live_pair_installed(models_dir: &Path) -> bool {
    model_is_installed(&live_pair_spec(), models_dir)
}

fn downloadable_specs() -> Vec<DownloadableModelSpec> {
    vec![
        live_pair_spec(),
        translation_spec(
            0,
            "Best-quality offline translation for the Translate screen and for dictation.",
            "Apple Silicon · 16 GB RAM recommended · about 11 GB of free memory while translating",
            ModelProfile::Accurate,
        ),
        translation_spec(
            1,
            "Compact offline translation. Faster and needs a third of the memory, with lower quality than 12B.",
            "Apple Silicon · works on 8–16 GB Macs · about 5 GB of free memory while translating",
            ModelProfile::Fast,
        ),
        whisper_turbo_spec(),
        DownloadableModelSpec {
            id: QWEN_MODEL_ID,
            engine: "qwen3_asr_c",
            model_name: QWEN_MODEL_NAME,
            description: "High-quality multilingual and code-switch transcription with dictionary-aware spelling prompts. CPU-only.",
            requirements: Some("16 GB RAM recommended · CPU-only"),
            size_bytes: QWEN_TOTAL_SIZE,
            profile: ModelProfile::Accurate,
            layout: InstallLayout::Directory { directory_name: QWEN_MODEL_DIR },
            revision: Some(QWEN_MODEL_REVISION),
            artifacts: QWEN_ARTIFACTS,
            capability: ModelCapability::Asr,
        },
        DownloadableModelSpec {
            id: MOSS_MODEL_ID,
            engine: "moss-transcribe-cpp",
            model_name: MOSS_MODEL_NAME,
            description: "Long-form multilingual transcription with built-in speaker identification, timestamps, and vocabulary hotwords.",
            requirements: Some("CPU-only · up to 90 minutes · speaker identification built in"),
            size_bytes: MOSS_MODEL_SIZE,
            profile: ModelProfile::Accurate,
            layout: InstallLayout::Directory { directory_name: MOSS_MODEL_DIR },
            revision: Some(MOSS_MODEL_REVISION),
            artifacts: MOSS_ARTIFACTS,
            capability: ModelCapability::Asr,
        },
        DownloadableModelSpec {
            id: VIBEVOICE_MODEL_ID,
            engine: "vibevoice-mlx",
            model_name: VIBEVOICE_MODEL_NAME,
            description: "The most accurate engine for files: long recordings up to 60 minutes with speaker labels, timestamps, mixed languages and your vocabulary. Experimental: Microsoft publishes it for research and development.",
            requirements: Some("Apple Silicon · macOS 14+ · at least 18 GB of memory (about 14 GB while transcribing) · file transcription only"),
            size_bytes: VIBEVOICE_MODEL_SIZE,
            profile: ModelProfile::Accurate,
            layout: InstallLayout::Directory { directory_name: VIBEVOICE_MODEL_DIR },
            revision: Some(VIBEVOICE_MODEL_REVISION),
            artifacts: VIBEVOICE_ARTIFACTS,
            capability: ModelCapability::Asr,
        },
        DownloadableModelSpec {
            id: DIARIZATION_MODEL_ID,
            engine: "sherpa-onnx",
            model_name: "Offline speaker diarization",
            description: "Local speaker separation using pyannote segmentation and VoxCeleb ERes2Net embeddings.",
            requirements: Some("CPU-only · approximately 32 MB download"),
            size_bytes: DIARIZATION_TOTAL_SIZE,
            profile: ModelProfile::Balanced,
            layout: InstallLayout::Directory { directory_name: DIARIZATION_MODEL_DIR },
            revision: Some(DIARIZATION_REVISION),
            artifacts: DIARIZATION_ARTIFACTS,
            capability: ModelCapability::Diarization,
        },
    ]
}

/// Whisper Large v3 Turbo, quantized: the one whisper.cpp download Murmela offers.
fn whisper_turbo_spec() -> DownloadableModelSpec {
    DownloadableModelSpec {
        id: "ggml-large-v3-turbo-q5_0-bin",
        engine: "whisper.cpp",
        model_name: "ggml-large-v3-turbo-q5_0.bin",
        description: "Quantized turbo model with lower memory use and a strong quality-speed tradeoff.",
        requirements: None,
        size_bytes: 574_041_195,
        profile: ModelProfile::Accurate,
        layout: InstallLayout::File { file_name: "ggml-large-v3-turbo-q5_0.bin" },
        revision: None,
        artifacts: single_file_artifact!("ggml-large-v3-turbo-q5_0.bin", 574_041_195, "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin?download=true", "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2"),
        capability: ModelCapability::Asr,
    }
}

fn vad_model_spec() -> DownloadableModelSpec {
    DownloadableModelSpec {
        id: "internal-silero-v6-2-0",
        engine: "whisper.cpp",
        model_name: VAD_MODEL_NAME,
        description: "Internal voice-activity model for safe long-recording boundaries.",
        requirements: None,
        size_bytes: 885_098,
        profile: ModelProfile::Fast,
        layout: InstallLayout::File {
            file_name: VAD_MODEL_NAME,
        },
        revision: None,
        artifacts: single_file_artifact!(
            "ggml-silero-v6.2.0.bin",
            885_098,
            "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin",
            "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987"
        ),
        capability: ModelCapability::Vad,
    }
}

/// Terms a user must accept in the app before this model is downloaded.
pub fn required_terms_version(id: &str) -> Option<&'static str> {
    crate::translation::model_spec(id).map(|_| crate::translation::GEMMA_TERMS_VERSION)
}

/// Where each model's licence can be read; the texts also ship with the app
/// (THIRD_PARTY_NOTICES.md and `licenses/`).
fn model_license_url(id: &str) -> Option<&'static str> {
    match id {
        // Nemotron: OpenMDW-1.1; Parakeet: CC-BY-4.0.
        crate::live_pair::MODEL_ID => Some("https://openmdw.ai/license/1-1/"),
        crate::translation::MODEL_ID | crate::translation::COMPACT_MODEL_ID => {
            Some("https://ai.google.dev/gemma/terms")
        }
        VIBEVOICE_MODEL_ID => Some("https://github.com/microsoft/VibeVoice/blob/main/LICENSE"),
        QWEN_MODEL_ID | MOSS_MODEL_ID | DIARIZATION_MODEL_ID => {
            Some("https://www.apache.org/licenses/LICENSE-2.0")
        }
        id if id.starts_with("ggml-") => Some("https://github.com/openai/whisper/blob/main/LICENSE"),
        _ => None,
    }
}

pub fn list_downloadable_models(models_dir: Option<&Path>) -> Vec<DownloadableModel> {
    let specs = downloadable_specs();
    let mut models: Vec<DownloadableModel> = specs
        .iter()
        .map(|spec| (spec, models_dir.is_some_and(|dir| model_is_installed(spec, dir))))
        .map(|(spec, installed)| DownloadableModel {
            id: spec.id.to_string(),
            engine: spec.engine.to_string(),
            model_name: spec.model_name.to_string(),
            description: spec.description.to_string(),
            size_bytes: spec.size_bytes,
            profile: spec.profile,
            availability: spec.availability(),
            requirements: spec.requirements.map(ToString::to_string),
            availability_reason: match spec.availability() {
                ModelAvailability::UnsupportedPlatform => Some(
                    if spec.id == VIBEVOICE_MODEL_ID {
                        crate::model_metadata::vibevoice_unavailable_reason()
                            .unwrap_or("The bundled VibeVoice worker is missing from this build.")
                    } else {
                        "This runtime is not supported on the current platform."
                    }
                    .to_string(),
                ),
                ModelAvailability::Available => None,
            },
            installed,
            artifact_count: spec.artifacts.len() as u32,
            capability: spec.capability,
            capabilities: capabilities_for_model(spec.id, spec.engine),
            license_url: model_license_url(spec.id).map(Into::into),
            origin: ModelOrigin::Catalog,
        })
        .collect();
    if let Some(dir) = models_dir {
        models.extend(custom_models(dir, &specs).into_iter().map(|model| DownloadableModel {
            description: "Added to the models folder by hand. Murmela has not verified this model; use it at your own risk.".into(),
            size_bytes: model.size_bytes,
            profile: model.profile,
            availability: ModelAvailability::Available,
            requirements: None,
            availability_reason: None,
            installed: true,
            artifact_count: 1,
            capability: ModelCapability::Asr,
            capabilities: model.capabilities.clone(),
            license_url: None,
            origin: ModelOrigin::Custom,
            id: model.id,
            engine: model.engine,
            model_name: model.model_name,
        }));
    }
    models
}

/// whisper.cpp models in the folder that Murmela never offered.
fn custom_models(models_dir: &Path, specs: &[DownloadableModelSpec]) -> Vec<InstalledModel> {
    crate::asr::discover_whisper_models(models_dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|model| {
            !specs.iter().any(|spec| {
                matches!(spec.layout, InstallLayout::File { file_name } if file_name == model.model_name)
            })
        })
        .collect()
}

enum DeletionTarget {
    File(PathBuf),
    Directory(PathBuf),
}

/// The files that make up an installed model, and its catalog entry if any.
fn deletion_target(
    models_dir: &Path,
    model_id: &str,
) -> Result<(DeletionTarget, Option<DownloadableModelSpec>)> {
    if let Some(spec) = downloadable_specs().into_iter().find(|spec| spec.id == model_id) {
        let target = match spec.layout {
            InstallLayout::File { file_name } => DeletionTarget::File(models_dir.join(file_name)),
            InstallLayout::Directory { directory_name } => {
                DeletionTarget::Directory(models_dir.join(directory_name))
            }
        };
        return Ok((target, Some(spec)));
    }
    let specs = downloadable_specs();
    let model = custom_models(models_dir, &specs)
        .into_iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| anyhow!("MODEL_MISSING: {model_id} is not installed."))?;
    let path = PathBuf::from(&model.local_path);
    // Only files directly in the models folder, never the internal VAD model.
    if path.parent() != Some(models_dir)
        || path.file_name().and_then(|name| name.to_str()) == Some(VAD_MODEL_NAME)
    {
        return Err(anyhow!("MODEL_PROTECTED: {} cannot be deleted from Murmela.", model.model_name));
    }
    Ok((DeletionTarget::File(path), None))
}

/// Headroom kept free beyond the model itself, so a download never fills the disk.
const DOWNLOAD_DISK_HEADROOM_BYTES: u64 = 1_000_000_000;

/// Refuses a download up front when the models volume cannot hold the rest of
/// it, instead of failing partway with a full disk.
fn ensure_disk_space(spec: &DownloadableModelSpec, models_dir: &Path) -> Result<()> {
    if model_is_installed(spec, models_dir) {
        return Ok(());
    }
    let Ok(available) = fs2::available_space(models_dir) else {
        return Ok(());
    };
    let remaining = (spec.size_bytes.max(0) as u64)
        .saturating_sub(partial_download_bytes(spec, models_dir));
    let needed = remaining + DOWNLOAD_DISK_HEADROOM_BYTES;
    if available < needed {
        return Err(anyhow!(
            "DISK_SPACE_LOW: {} needs about {} of free disk space, but only {} is available. Free up space and try again.",
            spec.model_name,
            format_gigabytes(needed),
            format_gigabytes(available),
        ));
    }
    Ok(())
}

/// Bytes a previous, interrupted download already left on disk.
fn partial_download_bytes(spec: &DownloadableModelSpec, models_dir: &Path) -> u64 {
    match spec.layout {
        InstallLayout::File { file_name } => fs::metadata(models_dir.join(format!("{file_name}.part")))
            .map(|metadata| metadata.len())
            .unwrap_or(0),
        InstallLayout::Directory { directory_name } => {
            directory_size(&models_dir.join(format!(".{directory_name}.part")))
        }
    }
}

fn directory_size(path: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .filter_map(|entry| entry.ok())
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => directory_size(&entry.path()),
            Ok(_) => entry.metadata().map(|metadata| metadata.len()).unwrap_or(0),
            Err(_) => 0,
        })
        .sum()
}

fn format_gigabytes(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
}

fn model_is_installed(spec: &DownloadableModelSpec, models_dir: &Path) -> bool {
    match spec.layout {
        InstallLayout::File { file_name } => models_dir.join(file_name).is_file(),
        InstallLayout::Directory { directory_name } => {
            let directory = models_dir.join(directory_name);
            let manifest = fs::read_to_string(directory.join(MODEL_COMPLETE_FILE))
                .ok()
                .and_then(|contents| serde_json::from_str::<CompletionManifest>(&contents).ok());
            let Some(manifest) = manifest else {
                return false;
            };
            manifest.model_id == spec.id
                && manifest.revision == spec.revision.unwrap_or("")
                && manifest.artifacts.len() == spec.artifacts.len()
                && spec.artifacts.iter().all(|artifact| {
                    manifest.artifacts.iter().any(|installed| {
                        installed.path == artifact.path
                            && installed.size_bytes == artifact.size_bytes
                            && installed.sha256 == artifact.sha256
                    }) && directory
                        .join(artifact.path)
                        .metadata()
                        .map(|metadata| {
                            metadata.is_file() && metadata.len() as i64 == artifact.size_bytes
                        })
                        .unwrap_or(false)
                })
        }
    }
}

pub fn installed_translation_model_path(models_dir: &Path, model_id: &str) -> Option<PathBuf> {
    let model = crate::translation::model_spec(model_id)?;
    let spec = downloadable_specs().into_iter().find(|s| s.id == model.id)?;
    model_is_installed(&spec, models_dir).then(|| models_dir.join(model.id).join(model.file))
}

pub fn installed_diarization_package_path(models_dir: &Path) -> Option<PathBuf> {
    if !DIARIZATION_ARTIFACTS_REVIEWED {
        return None;
    }
    let spec = downloadable_specs()
        .into_iter()
        .find(|spec| spec.id == DIARIZATION_MODEL_ID)?;
    model_is_installed(&spec, models_dir).then(|| models_dir.join(DIARIZATION_MODEL_DIR))
}

pub fn discover_native_asr_models(models_dir: &Path) -> Vec<InstalledModel> {
    downloadable_specs()
        .into_iter()
        .filter(|spec| {
            matches!(
                spec.id,
                MOSS_MODEL_ID | VIBEVOICE_MODEL_ID | crate::live_pair::MODEL_ID
            )
        })
        .filter(|spec| spec.availability() == ModelAvailability::Available)
        .filter(|spec| model_is_installed(spec, models_dir))
        .filter_map(|spec| {
            let InstallLayout::Directory { directory_name } = spec.layout else {
                return None;
            };
            Some(InstalledModel {
                id: spec.id.to_string(),
                engine: spec.engine.to_string(),
                model_name: spec.model_name.to_string(),
                variant: if spec.id == crate::live_pair::MODEL_ID {
                    "CoreML · streaming + final pass"
                } else if spec.id == VIBEVOICE_MODEL_ID {
                    "8-bit MLX"
                } else {
                    "0.9B F16"
                }
                .to_string(),
                local_path: models_dir.join(directory_name).display().to_string(),
                size_bytes: spec.size_bytes,
                is_default: false,
                profile: spec.profile,
                capabilities: capabilities_for_model(spec.id, spec.engine),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HASH: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn test_directory_spec(
        id: &'static str,
        directory_name: &'static str,
        url: &'static str,
    ) -> DownloadableModelSpec {
        let artifacts = Box::leak(
            vec![ModelArtifactSpec {
                path: "model.bin",
                size_bytes: 5,
                url,
                sha256: TEST_HASH,
            }]
            .into_boxed_slice(),
        );
        DownloadableModelSpec {
            id,
            engine: "test",
            model_name: "Test directory model",
            description: "Test fixture",
            requirements: None,
            size_bytes: 5,
            profile: ModelProfile::Fast,
            layout: InstallLayout::Directory { directory_name },
            revision: Some("test-revision"),
            artifacts,
            capability: ModelCapability::Diarization,
        }
    }

    #[test]
    fn qwen_manifest_is_pinned_and_complete() {
        let spec = downloadable_specs()
            .into_iter()
            .find(|spec| spec.id == QWEN_MODEL_ID)
            .expect("Qwen model");
        assert_eq!(spec.revision, Some(QWEN_MODEL_REVISION));
        assert_eq!(spec.artifacts.len(), 7);
        assert_eq!(
            spec.artifacts
                .iter()
                .map(|artifact| artifact.size_bytes)
                .sum::<i64>(),
            QWEN_TOTAL_SIZE
        );
        assert!(spec
            .artifacts
            .iter()
            .all(|artifact| artifact.sha256.len() == 64));
        for (path, size, sha256) in QWEN_REQUIRED_ARTIFACTS {
            let artifact = spec
                .artifacts
                .iter()
                .find(|artifact| artifact.path == *path)
                .expect("pinned artifact");
            assert_eq!(artifact.size_bytes, *size);
            assert_eq!(artifact.sha256, *sha256);
        }
    }

    #[test]
    fn native_asr_manifests_are_immutable_and_complete() {
        let specs = downloadable_specs();
        let moss = specs
            .iter()
            .find(|spec| spec.id == MOSS_MODEL_ID)
            .expect("MOSS");
        assert_eq!(moss.revision, Some(MOSS_MODEL_REVISION));
        assert_eq!(moss.artifacts.len(), 1);
        assert_eq!(moss.artifacts[0].size_bytes, MOSS_MODEL_SIZE);
        assert_eq!(
            moss.artifacts[0].sha256,
            "88a9518ccd9c7d04a3ebc2c49174f47cebe528096f949760942bc31f7ebe8046"
        );

        let vibe = specs
            .iter()
            .find(|spec| spec.id == VIBEVOICE_MODEL_ID)
            .expect("VibeVoice");
        assert_eq!(vibe.revision, Some(VIBEVOICE_MODEL_REVISION));
        assert_eq!(vibe.artifacts.len(), 4);
        assert_eq!(
            vibe.artifacts.iter().map(|artifact| artifact.size_bytes).sum::<i64>(),
            VIBEVOICE_MODEL_SIZE
        );
        assert!(vibe.artifacts.iter().all(|artifact| artifact.sha256.len() == 64));
        assert!(specs.iter().all(|spec| !spec.id.starts_with("ggml-tiny")));
    }

    #[test]
    fn diarization_manifest_is_reviewed_pinned_and_complete() {
        let spec = downloadable_specs()
            .into_iter()
            .find(|spec| spec.id == DIARIZATION_MODEL_ID)
            .expect("diarization model card");
        assert_eq!(spec.capability, ModelCapability::Diarization);
        assert_eq!(spec.availability(), ModelAvailability::Available);
        assert_eq!(spec.revision, Some(DIARIZATION_REVISION));
        assert_eq!(spec.artifacts.len(), 2);
        assert_eq!(
            spec.artifacts
                .iter()
                .map(|artifact| artifact.size_bytes)
                .sum::<i64>(),
            DIARIZATION_TOTAL_SIZE
        );
        assert_eq!(
            spec.artifacts
                .iter()
                .map(|artifact| artifact.path)
                .collect::<Vec<_>>(),
            ["segmentation.onnx", "embedding.onnx"]
        );
        assert_eq!(spec.id, "sherpa-diarization-pyannote3-eres2net-voxceleb-v2");
        assert_eq!(spec.size_bytes, 32_478_041);
        assert_eq!(spec.artifacts[0].size_bytes, 5_992_778);
        assert_eq!(
            spec.artifacts[0].sha256,
            "fed22097bca974bad329a930b60865703766ff89f05fa09060bf6fd44e92e319"
        );
        assert_eq!(spec.artifacts[1].size_bytes, 26_485_263);
        assert_eq!(
            spec.artifacts[1].sha256,
            "c59158379255ad66e161679cca6af8d52d51e389e3224ab7d7a7baae295c2db5"
        );
        assert!(spec.artifacts[1]
            .url
            .contains("3dspeaker_speech_eres2net_sv_en_voxceleb_16k.onnx"));
        assert!(spec.artifacts.iter().all(|artifact| {
            artifact.sha256.len() == 64
                && artifact
                    .sha256
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
                && artifact.url.contains("/resolve/")
                && !artifact.url.contains("/resolve/main/")
        }));
        assert!(DIARIZATION_ARTIFACTS_REVIEWED);
    }

    #[test]
    fn artifact_verification_rejects_size_and_hash_mismatches() {
        let path = std::env::temp_dir().join(format!(
            "murmela-artifact-verification-{}",
            uuid::Uuid::new_v4()
        ));
        fs::write(&path, b"hello").expect("fixture");
        let valid = ModelArtifactSpec {
            path: "fixture",
            size_bytes: 5,
            url: "https://example.invalid/fixture",
            sha256: "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        };
        assert!(verify_file(&path, &valid).expect("valid hash"));
        let wrong_size = ModelArtifactSpec {
            size_bytes: 6,
            ..valid
        };
        assert!(!verify_file(&path, &wrong_size).expect("wrong size"));
        let wrong_hash = ModelArtifactSpec {
            sha256: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            ..valid
        };
        assert!(!verify_file(&path, &wrong_hash).expect("wrong hash"));
        fs::remove_file(path).expect("cleanup");
    }

    #[test]
    fn directory_installation_requires_matching_manifest_and_artifacts() {
        let models_dir = std::env::temp_dir().join(format!(
            "murmela-directory-validation-{}",
            uuid::Uuid::new_v4()
        ));
        let spec = test_directory_spec(
            "test-directory-model",
            "test-directory-model",
            "https://example.invalid/model.bin",
        );
        let package_dir = models_dir.join("test-directory-model");
        fs::create_dir_all(&package_dir).expect("package directory");
        fs::write(package_dir.join("model.bin"), b"hello").expect("artifact");

        assert!(!model_is_installed(&spec, &models_dir));
        write_completion_manifest(&spec, &package_dir).expect("completion manifest");
        assert!(model_is_installed(&spec, &models_dir));

        let manifest_path = package_dir.join(MODEL_COMPLETE_FILE);
        let mut manifest: CompletionManifest =
            serde_json::from_slice(&fs::read(&manifest_path).expect("manifest bytes"))
                .expect("manifest");
        manifest.revision = "stale-revision".to_string();
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("stale manifest"),
        )
        .expect("write stale manifest");
        assert!(!model_is_installed(&spec, &models_dir));

        write_completion_manifest(&spec, &package_dir).expect("restore manifest");
        let mut manifest: CompletionManifest =
            serde_json::from_slice(&fs::read(&manifest_path).expect("restored manifest bytes"))
                .expect("restored manifest");
        manifest.artifacts[0].sha256 =
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        fs::write(
            &manifest_path,
            serde_json::to_vec(&manifest).expect("wrong-hash manifest"),
        )
        .expect("write wrong-hash manifest");
        assert!(!model_is_installed(&spec, &models_dir));

        write_completion_manifest(&spec, &package_dir).expect("restore manifest again");
        fs::write(package_dir.join("model.bin"), b"tiny").expect("wrong-size artifact");
        assert!(!model_is_installed(&spec, &models_dir));

        fs::remove_dir_all(models_dir).expect("cleanup");
    }

    /// Downloads refuse any other host, so a model hosted elsewhere would never install.
    #[test]
    fn every_model_downloads_from_hugging_face() {
        for spec in downloadable_specs().iter().chain([&vad_model_spec()]) {
            for artifact in spec.artifacts {
                assert!(
                    Destination::HuggingFace.allows(artifact.url),
                    "{} downloads {} from outside huggingface.co",
                    spec.id,
                    artifact.url
                );
            }
        }
    }

    #[test]
    fn cancellation_keeps_a_resumable_partial_artifact() {
        let models_dir =
            std::env::temp_dir().join(format!("murmela-directory-cancel-{}", uuid::Uuid::new_v4()));
        let spec = test_directory_spec(
            "cancel-test-package",
            "cancel-test-package",
            "https://example.invalid/model.bin",
        );
        let partial_path = models_dir
            .join(".cancel-test-package.part")
            .join("model.bin.part");
        fs::create_dir_all(partial_path.parent().expect("partial parent"))
            .expect("partial directory");
        fs::write(&partial_path, b"he").expect("partial artifact");

        let db_path = models_dir.join("network.sqlite");
        let error = download_model(&spec, &models_dir, &db_path, &AtomicBool::new(true), |_, _, _| {})
            .expect_err("canceled download");
        assert!(is_cancelled_error(&error));
        assert_eq!(fs::read(&partial_path).expect("preserved partial"), b"he");
        assert!(!models_dir.join("cancel-test-package").exists());

        fs::remove_dir_all(models_dir).expect("cleanup");
    }

    #[test]
    fn unrelated_directory_package_does_not_block_resumed_atomic_install() {
        let models_dir = std::env::temp_dir().join(format!(
            "murmela-directory-download-{}",
            uuid::Uuid::new_v4()
        ));
        let unrelated = test_directory_spec(
            "qwen-test-package",
            QWEN_MODEL_DIR,
            "https://example.invalid/qwen.bin",
        );
        let unrelated_dir = models_dir.join(QWEN_MODEL_DIR);
        fs::create_dir_all(&unrelated_dir).expect("unrelated package directory");
        fs::write(unrelated_dir.join("model.bin"), b"hello").expect("unrelated artifact");
        write_completion_manifest(&unrelated, &unrelated_dir).expect("unrelated manifest");
        assert!(model_is_installed(&unrelated, &models_dir));

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("test server");
        let address = listener.local_addr().expect("server address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("download connection");
            let mut request = [0_u8; 2048];
            let bytes_read = stream.read(&mut request).expect("request");
            let request = String::from_utf8_lossy(&request[..bytes_read]);
            assert!(request.contains("Range: bytes=2-") || request.contains("range: bytes=2-"));
            stream
                .write_all(
                    b"HTTP/1.1 206 Partial Content\r\nContent-Length: 3\r\nContent-Range: bytes 2-4/5\r\nConnection: close\r\n\r\nllo",
                )
                .expect("response");
        });
        let url: &'static str = Box::leak(format!("http://{address}/model.bin").into_boxed_str());
        let requested =
            test_directory_spec("diarization-test-package", "diarization-test-package", url);
        let final_dir = models_dir.join("diarization-test-package");
        fs::create_dir_all(&final_dir).expect("stale final directory");
        fs::write(final_dir.join("model.bin"), b"stale").expect("stale artifact");
        let staging_dir = models_dir.join(".diarization-test-package.part");
        fs::create_dir_all(&staging_dir).expect("staging directory");
        fs::write(staging_dir.join("model.bin.part"), b"he").expect("partial artifact");

        let db_path = models_dir.join("network.sqlite");
        download_model(
            &requested,
            &models_dir,
            &db_path,
            &AtomicBool::new(false),
            |_, _, _| {},
        )
        .expect("directory download");
        server.join().expect("test server completed");

        // The download is logged once, with the bytes that actually arrived.
        let logged = network_activity::list(&db_path).expect("network log");
        assert_eq!(logged.len(), 1);
        assert_eq!(logged[0].destination, Destination::HuggingFace);
        assert_eq!(logged[0].purpose, format!("Download {}", requested.model_name));
        assert_eq!(logged[0].bytes, Some(3));
        assert_eq!(logged[0].outcome, Outcome::Completed);

        assert_eq!(
            fs::read(final_dir.join("model.bin")).expect("installed artifact"),
            b"hello"
        );
        assert!(model_is_installed(&requested, &models_dir));
        fs::remove_dir_all(models_dir).expect("cleanup");
    }

    fn whisper_file(models_dir: &Path, name: &str) {
        // Discovery ignores files under 1 MB; a sparse file is enough.
        File::create(models_dir.join(name))
            .and_then(|file| file.set_len(2 * 1024 * 1024))
            .expect("model file");
    }

    #[test]
    fn whisper_files_added_by_hand_are_listed_as_custom() {
        let models_dir =
            std::env::temp_dir().join(format!("murmela-model-list-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&models_dir).expect("models dir");
        let ids = |models: &[DownloadableModel]| {
            models.iter().map(|m| (m.id.clone(), m.origin)).collect::<Vec<_>>()
        };

        let listed = ids(&list_downloadable_models(Some(&models_dir)));
        assert!(listed.contains(&("ggml-large-v3-turbo-q5_0-bin".into(), ModelOrigin::Catalog)));

        whisper_file(&models_dir, "ggml-medium.bin");
        whisper_file(&models_dir, "ggml-large-v3.bin");
        whisper_file(&models_dir, "ggml-tiny.bin");
        whisper_file(&models_dir, VAD_MODEL_NAME);
        let listed = list_downloadable_models(Some(&models_dir));
        let origins = ids(&listed);
        assert!(origins.contains(&("ggml-medium-bin".into(), ModelOrigin::Custom)));
        assert!(origins.contains(&("ggml-large-v3-bin".into(), ModelOrigin::Custom)));
        assert!(origins.contains(&("ggml-tiny-bin".into(), ModelOrigin::Custom)));
        assert!(!origins.iter().any(|(id, _)| id.contains("silero")));
        fs::remove_dir_all(models_dir).expect("cleanup");
    }

    #[test]
    fn deletion_targets_stay_inside_the_models_folder() {
        let models_dir =
            std::env::temp_dir().join(format!("murmela-model-delete-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&models_dir).expect("models dir");
        whisper_file(&models_dir, "ggml-large-v3.bin");

        let (target, spec) = deletion_target(&models_dir, "ggml-large-v3-bin").expect("custom model");
        assert!(spec.is_none());
        assert!(matches!(target, DeletionTarget::File(path) if path == models_dir.join("ggml-large-v3.bin")));
        let (target, spec) = deletion_target(&models_dir, QWEN_MODEL_ID).expect("catalog package");
        assert_eq!(spec.map(|s| s.id), Some(QWEN_MODEL_ID));
        assert!(matches!(target, DeletionTarget::Directory(path) if path == models_dir.join(QWEN_MODEL_DIR)));
        assert!(deletion_target(&models_dir, "ggml-missing-bin").is_err());
        assert!(deletion_target(&models_dir, "../../etc").is_err());
        fs::remove_dir_all(models_dir).expect("cleanup");
    }

    #[test]
    fn cancellation_is_typed_and_detectable() {
        let cancelled = AtomicBool::new(true);
        let error = ensure_not_cancelled(&cancelled).expect_err("cancellation error");
        assert!(is_cancelled_error(&error));
    }
}
