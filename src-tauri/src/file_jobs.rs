use std::collections::{HashMap, VecDeque};
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::asr::{
    FileTranscriptionRequest as EngineFileTranscriptionRequest, InstalledModel, PreviewSourceKind,
    TranscriptResult, TranscriptionEngine,
};
use crate::audio_files::{FileTranscriptionRequest, SelectedSourceFile};
use crate::settings::AppSettings;
use crate::storage::{self, TranscriptSummary};
use crate::transcription_worker::{self, WorkerOutput, WorkerRequest};
use crate::vocabulary;
use crate::{diarization, diarization_worker, model_downloads};

const FILE_TRANSCRIPTION_STATUS_EVENT: &str = "file-transcription-status";
const WATCHDOG_TIMEOUT_MS: i64 = 120_000;
const WORKER_STALL_TIMEOUT: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileTranscriptionJobStage {
    Queued,
    Preparing,
    Transcribing,
    Diarizing,
    Saving,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFileTranscriptionResponse {
    pub job_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTranscriptionResponse {
    pub source_file: SelectedSourceFile,
    pub resolved_model: Option<InstalledModel>,
    pub result: TranscriptResult,
    pub saved_transcript: Option<TranscriptSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTranscriptionStatusEvent {
    pub job_id: String,
    pub source_file: SelectedSourceFile,
    pub stage: FileTranscriptionJobStage,
    pub progress_percent: Option<f32>,
    pub processed_ms: Option<i64>,
    pub total_ms: Option<i64>,
    pub eta_seconds: Option<i64>,
    pub status_text: String,
    pub result: Option<FileTranscriptionResponse>,
    #[serde(default)]
    pub review_ref: Option<crate::review::ReviewRef>,
    #[serde(default)]
    pub result_revision: u64,
    pub error_message: Option<String>,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone)]
struct ActiveFileTranscriptionRun {
    job_id: String,
    control: Arc<FileJobRunControl>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileJobCancellationReason {
    None = 0,
    Watchdog = 1,
    User = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchdogDisposition {
    PreserveAsr,
    FailJob,
}

#[derive(Debug)]
struct FileJobRunControl {
    cancelled: AtomicBool,
    cancellation_reason: AtomicU8,
}

impl FileJobRunControl {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            cancellation_reason: AtomicU8::new(FileJobCancellationReason::None as u8),
        }
    }

    fn request_user_cancellation(&self) {
        self.cancellation_reason
            .store(FileJobCancellationReason::User as u8, Ordering::SeqCst);
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn request_watchdog_cancellation(&self) {
        let _ = self.cancellation_reason.compare_exchange(
            FileJobCancellationReason::None as u8,
            FileJobCancellationReason::Watchdog as u8,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn stop(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn reason(&self) -> FileJobCancellationReason {
        match self.cancellation_reason.load(Ordering::SeqCst) {
            value if value == FileJobCancellationReason::Watchdog as u8 => {
                FileJobCancellationReason::Watchdog
            }
            value if value == FileJobCancellationReason::User as u8 => {
                FileJobCancellationReason::User
            }
            _ => FileJobCancellationReason::None,
        }
    }
}

#[derive(Clone)]
pub struct FileTranscriptionController {
    app: AppHandle,
    engine: Arc<dyn TranscriptionEngine>,
    models_dir: PathBuf,
    db_path: PathBuf,
    statuses: Arc<Mutex<HashMap<String, FileTranscriptionStatusEvent>>>,
    status_updates: Arc<Mutex<()>>,
    publication: Arc<Mutex<()>>,
    queued_requests: Arc<Mutex<VecDeque<FileTranscriptionRequest>>>,
    active_run: Arc<Mutex<Option<ActiveFileTranscriptionRun>>>,
    processing_queue: crate::review_jobs::ProcessingQueue,
    review_store: crate::review::ReviewStore,
    published_models: Arc<Mutex<HashMap<String, Option<InstalledModel>>>>,
    log_path: PathBuf,
    /// While a call's channels are transcribed one after the other: where each
    /// pass sits in the overall progress, and what the status says.
    progress_window: Arc<Mutex<Option<ProgressWindow>>>,
}

#[derive(Clone)]
struct ProgressWindow {
    offset: f32,
    share: f32,
    text: String,
}

impl FileTranscriptionController {
    pub fn new(
        app: AppHandle,
        engine: Arc<dyn TranscriptionEngine>,
        models_dir: PathBuf,
        db_path: PathBuf,
        review_store: crate::review::ReviewStore,
        processing_queue: crate::review_jobs::ProcessingQueue,
    ) -> Self {
        let log_path = db_path
            .parent()
            .map(|dir| dir.join("file-transcription.log"))
            .unwrap_or_else(|| PathBuf::from("file-transcription.log"));
        Self {
            app,
            engine,
            models_dir,
            db_path,
            statuses: Arc::new(Mutex::new(HashMap::new())),
            status_updates: Arc::new(Mutex::new(())),
            publication: Default::default(),
            queued_requests: Arc::new(Mutex::new(VecDeque::new())),
            active_run: Arc::new(Mutex::new(None)),
            processing_queue,
            review_store,
            published_models: Default::default(),
            log_path,
            progress_window: Default::default(),
        }
    }

    pub fn start(&self, request: FileTranscriptionRequest) -> StartFileTranscriptionResponse {
        let now = chrono::Utc::now().timestamp_millis();
        let job_id = request.job_id.clone();
        let source_file = request.source_file.clone();
        let queued_status = FileTranscriptionStatusEvent {
            job_id: job_id.clone(),
            source_file,
            stage: FileTranscriptionJobStage::Queued,
            progress_percent: None,
            processed_ms: None,
            total_ms: None,
            eta_seconds: None,
            status_text: "Queued for local transcription.".to_string(),
            result: None,
            review_ref: None,
            result_revision: 0,
            error_message: None,
            started_at_ms: now,
            updated_at_ms: now,
        };

        // Idempotent admission prevents duplicate workers for repeated IPC calls.
        {
            let mut statuses = self.statuses.lock().expect("file status mutex poisoned");
            if statuses.contains_key(&job_id) {
                return StartFileTranscriptionResponse { job_id };
            }
            statuses.insert(job_id.clone(), queued_status.clone());
        }
        self.processing_queue.enqueue(&job_id);
        self.persist_status(queued_status);
        self.log_job(&job_id, "accepted", "Queued for local transcription.");
        self.queued_requests
            .lock()
            .expect("file transcription queue mutex poisoned")
            .push_back(request);
        self.maybe_spawn_next();

        StartFileTranscriptionResponse { job_id }
    }

    pub fn statuses(&self) -> Vec<FileTranscriptionStatusEvent> {
        let mut statuses = self
            .statuses
            .lock()
            .expect("file transcription status mutex poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        statuses.sort_by(|left, right| right.started_at_ms.cmp(&left.started_at_ms));
        statuses
    }

    pub fn cancel_for_shutdown(&self) {
        self.queued_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        if let Some(run) = self
            .active_run
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            run.control.request_user_cancellation();
        }
    }

    pub fn result(&self, job_id: &str) -> Result<FileTranscriptionResponse> {
        let status = self
            .current_status(job_id)
            .ok_or_else(|| anyhow!("File job not found."))?;
        let reference = status
            .review_ref
            .ok_or_else(|| anyhow!("Text is not ready yet."))?;
        let document = self.review_store.get(&reference)?;
        let source_file = self.review_store.source(&reference)?;
        let result = crate::review::result_from_detail(&document.detail);
        Ok(FileTranscriptionResponse {
            source_file,
            result,
            resolved_model: self
                .published_models
                .lock()
                .ok()
                .and_then(|models| models.get(job_id).cloned())
                .flatten(),
            saved_transcript: matches!(reference, crate::review::ReviewRef::Saved { .. })
                .then_some(document.detail.summary),
        })
    }

    pub fn dismiss(&self, job_id: &str) -> Result<()> {
        let status = self
            .current_status(job_id)
            .ok_or_else(|| anyhow!("File job not found."))?;
        if !is_terminal(&status.stage) {
            return Err(anyhow!("Stop processing before dismissing this file."));
        }
        if let Some(reference) = status.review_ref {
            self.review_store.discard(&reference)?;
        }
        self.statuses
            .lock()
            .map_err(|_| anyhow!("File status unavailable"))?
            .remove(job_id);
        self.published_models
            .lock()
            .map_err(|_| anyhow!("File result unavailable"))?
            .remove(job_id);
        Ok(())
    }

    pub fn cancel(&self, job_id: &str) -> Result<()> {
        let _publication = self
            .publication
            .lock()
            .map_err(|_| anyhow!("File result unavailable"))?;
        if self
            .current_status(job_id)
            .is_some_and(|s| is_terminal(&s.stage))
        {
            return Ok(());
        }
        let removed_from_queue = {
            let mut queue = self
                .queued_requests
                .lock()
                .expect("file transcription queue mutex poisoned");
            let original_len = queue.len();
            queue.retain(|request| request.job_id != job_id);
            queue.len() != original_len
        };

        let active_cancelled = {
            let active = self
                .active_run
                .lock()
                .expect("file transcription active mutex poisoned");
            if let Some(run) = active.as_ref() {
                if run.job_id == job_id {
                    run.control.request_user_cancellation();
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if removed_from_queue {
            self.processing_queue.remove(job_id);
        }
        if active_cancelled
            && self
                .current_status(job_id)
                .is_some_and(|s| s.review_ref.is_some())
        {
            if let Some(mut status) = self.current_status(job_id) {
                status.status_text = "Stopping speaker processing; keeping your transcript…".into();
                status.updated_at_ms = chrono::Utc::now().timestamp_millis();
                self.persist_status(status);
            }
            return Ok(());
        }
        if removed_from_queue || active_cancelled {
            self.cancel_job(
                job_id,
                "File transcription canceled.",
                "The file transcription was canceled by the user.".to_string(),
            );
            if removed_from_queue && !active_cancelled {
                self.maybe_spawn_next();
            }
            Ok(())
        } else {
            Err(anyhow!(
                "No queued or active file transcription job matched {job_id}"
            ))
        }
    }

    fn maybe_spawn_next(&self) {
        if crate::shutdown::is_shutting_down() {
            return;
        }
        let mut active = self
            .active_run
            .lock()
            .expect("file transcription active mutex poisoned");
        if active.is_some() {
            return;
        }
        let Some(request) = self
            .queued_requests
            .lock()
            .expect("file queue mutex poisoned")
            .pop_front()
        else {
            return;
        };
        let control = Arc::new(FileJobRunControl::new());
        *active = Some(ActiveFileTranscriptionRun {
            job_id: request.job_id.clone(),
            control: control.clone(),
        });
        drop(active);

        let controller = self.clone();
        thread::spawn(move || {
            controller.run_job(request, control);
        });
    }

    fn run_job(&self, request: FileTranscriptionRequest, control: Arc<FileJobRunControl>) {
        let Ok(_work) = crate::shutdown::begin_work(true) else {
            return;
        };
        let permit = self
            .processing_queue
            .acquire(&request.job_id, &control.cancelled);
        if permit.is_err() {
            self.processing_queue.remove(&request.job_id);
            self.finish_run(&request.job_id);
            self.maybe_spawn_next();
            return;
        }
        let _processing_guard = permit.ok();
        if let Some(state) = self.app.try_state::<crate::app_state::AppState>() {
            state.engine.release_resources();
        }
        let (watchdog_finished, watchdog_wait) = mpsc::channel();
        let watchdog =
            self.spawn_watchdog(request.job_id.clone(), Arc::clone(&control), watchdog_wait);

        let result = self.process_file_job(&request, Arc::clone(&control));
        control.stop();
        let _ = watchdog_finished.send(());
        let _ = watchdog.join();

        if let Err(error) = result {
            self.fail_job(
                &request.job_id,
                "File transcription failed.",
                error.to_string(),
            );
        }

        drop(_processing_guard);
        self.finish_run(&request.job_id);
        self.maybe_spawn_next();
    }

    fn process_file_job(
        &self,
        request: &FileTranscriptionRequest,
        control: Arc<FileJobRunControl>,
    ) -> Result<()> {
        self.update_status(
            &request.job_id,
            FileTranscriptionJobStage::Preparing,
            "Preparing audio file.",
            None,
            None,
            None,
            None,
            None,
            None,
        )?;
        self.log_job(
            &request.job_id,
            "preparing",
            &request.source_file.original_name,
        );

        let preparation_started = Instant::now();
        let temp_dir = self.db_path.parent().unwrap_or(Path::new(".")).join("temp");
        let prepared =
            crate::audio_preprocess::prepare_job_audio(&request.source_file.file_path, &temp_dir)?;
        if control.is_cancelled() {
            return Err(anyhow!("File preparation canceled."));
        }
        let mut request = request.clone();
        request.source_file.duration_ms = Some(prepared.duration_ms);
        request.source_file.sha256 = Some(prepared.sha256.clone());
        if let Some(status) = self
            .statuses
            .lock()
            .expect("file status mutex poisoned")
            .get_mut(&request.job_id)
        {
            status.source_file = request.source_file.clone();
        }
        let request = &request;
        self.log_job(
            &request.job_id,
            "timing",
            &format!(
                "preparation_ms={}",
                preparation_started.elapsed().as_millis()
            ),
        );

        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        // A recorded call or meeting: its sidecar says how to transcribe it.
        let call = crate::meeting::sidecar_for(
            &self.meetings_dir(),
            Path::new(&request.source_file.file_path),
        );
        // A call's two channels already separate the speakers; only a room recording needs diarization.
        let two_channel_call = call
            .as_ref()
            .is_some_and(|(_, sidecar)| sidecar.mode == crate::meeting::CallMode::Call);
        // Speaker labels are a Pro feature; models that label speakers natively are unaffected.
        let diarization_enabled =
            settings.file_diarization_enabled && crate::license::is_pro(&self.db_path);
        let vocabulary_prompt = vocabulary::build_asr_prompt_from_db_path(&self.db_path)?;
        if let Some(prompt) = &vocabulary_prompt {
            self.log_job(
                &request.job_id,
                "dictionary-prompt",
                &format!(
                    "included={} truncated={}",
                    prompt.included_count, prompt.truncated_count
                ),
            );
        }
        let resolved_model = resolve_model_for_settings(
            self.engine.as_ref(),
            &settings,
            PreviewSourceKind::FileUpload,
        )?;
        let vibevoice = resolved_model
            .as_ref()
            .is_some_and(|model| model.engine == "vibevoice-mlx");
        if vibevoice {
            // VibeVoice needs about 14 GB; never let it compete with a loaded translation model.
            if let Some(state) = self.app.try_state::<crate::app_state::AppState>() {
                state.translation.release_translation_model();
            }
        }
        if let Some(model) = resolved_model.as_ref() {
            if !model
                .capabilities
                .supported_contexts
                .contains(&crate::model_metadata::ModelUseContext::FileTranscription)
            {
                return Err(anyhow!(
                    "MODEL_CONTEXT_UNSUPPORTED: {} cannot be used for file transcription",
                    model.model_name
                ));
            }
            if let (Some(duration), Some(limit)) = (
                request.source_file.duration_ms,
                model.capabilities.maximum_audio_duration_ms,
            ) {
                if duration > limit {
                    return Err(anyhow!(
                        "MODEL_AUDIO_TOO_LONG: {} supports audio up to {} minutes",
                        model.model_name,
                        limit / 60_000
                    ));
                }
            }
        }

        self.update_status(
            &request.job_id,
            FileTranscriptionJobStage::Transcribing,
            &format!(
                "Transcribing {} locally...",
                request.source_file.original_name
            ),
            Some(0.0),
            Some(0),
            request.source_file.duration_ms,
            None,
            None,
            None,
        )?;
        self.log_job(
            &request.job_id,
            "transcribing",
            &request.source_file.original_name,
        );

        let started_at = Instant::now();
        let engine_request = EngineFileTranscriptionRequest {
            use_context: Some(crate::model_metadata::ModelUseContext::FileTranscription),
            profile: settings.file_transcribe_model_profile,
            selected_model_id: settings.file_transcribe_selected_model_id.clone(),
            language_mode: settings.language_mode,
            fixed_language: settings.fixed_language.clone(),
            timestamps: true,
            prefer_gpu: settings.gpu_enabled,
            // VibeVoice reads the original file; the others get the prepared WAV.
            file_path: if vibevoice {
                request.source_file.file_path.clone()
            } else {
                prepared.path.to_string_lossy().into_owned()
            },
            context_prompt: vocabulary_prompt.as_ref().map(|prompt| prompt.text.clone()),
            context_terms: vocabulary_prompt
                .as_ref()
                .map(|prompt| prompt.terms.clone())
                .unwrap_or_default(),
        };
        let transcript = match &call {
            Some((dir, sidecar)) if sidecar.mode == crate::meeting::CallMode::Call => self
                .transcribe_call(
                    request,
                    dir,
                    sidecar,
                    engine_request,
                    vibevoice,
                    &temp_dir,
                    Arc::clone(&control),
                    started_at,
                )?,
            _ => self.run_transcription_worker(
                request,
                engine_request,
                Arc::clone(&control),
                started_at,
            )?,
        };

        if control.is_cancelled() {
            return Err(anyhow!(
                "File transcription watchdog expired before completion."
            ));
        }

        let asr_duration = started_at.elapsed();
        self.log_job(
            &request.job_id,
            "timing",
            &format!("asr_ms={}", asr_duration.as_millis()),
        );
        let mut corrected = vocabulary::correct_transcript_result(&self.db_path, transcript)?;
        if !corrected.warnings.is_empty() {
            self.log_job(
                &request.job_id,
                "recovery",
                &format!(
                    "quality={:?} recovered_regions={} warnings={}",
                    corrected.quality_status,
                    corrected.recovered_region_count,
                    corrected.warnings.len()
                ),
            );
        }
        let standalone = !two_channel_call && should_run_post_process_diarization(
            diarization_enabled,
            resolved_model.as_ref(),
            request.source_file.duration_ms,
            request.speaker_count_hint,
        );
        if standalone {
            corrected.diarization_status = diarization::DiarizationStatus::Running;
            corrected.diarization_speaker_count_hint = request.speaker_count_hint;
        }
        let initial_publication = self
            .publication
            .lock()
            .map_err(|_| anyhow!("File result unavailable"))?;
        if control.is_cancelled() {
            return Err(anyhow!("Transcription canceled before saving."));
        }
        let initial_saving = Instant::now();
        let saved_transcript = if settings.save_history {
            Some(storage::save_file_transcription_as(
                &self.db_path,
                &request.source_file,
                &corrected,
                if call.is_some() {
                    storage::SourceType::Meeting
                } else {
                    storage::SourceType::FileUpload
                },
            )?)
        } else {
            None
        };
        self.update_status(
            &request.job_id,
            if standalone {
                FileTranscriptionJobStage::Diarizing
            } else {
                FileTranscriptionJobStage::Saving
            },
            if standalone {
                "Text ready · Identifying speakers"
            } else {
                "Text ready"
            },
            None,
            request.source_file.duration_ms,
            request.source_file.duration_ms,
            None,
            Some(FileTranscriptionResponse {
                source_file: request.source_file.clone(),
                resolved_model: resolved_model.clone(),
                result: corrected.clone(),
                saved_transcript: saved_transcript.clone(),
            }),
            None,
        )?;
        self.log_job(
            &request.job_id,
            "timing",
            &format!("initial_saving_ms={}", initial_saving.elapsed().as_millis()),
        );
        drop(initial_publication);
        let speaker_started = Instant::now();
        if !two_channel_call && should_run_post_process_diarization(
            diarization_enabled,
            resolved_model.as_ref(),
            request.source_file.duration_ms,
            request.speaker_count_hint,
        ) {
            self.update_status(
                &request.job_id,
                FileTranscriptionJobStage::Diarizing,
                "Text ready · Identifying speakers",
                None,
                request.source_file.duration_ms,
                request.source_file.duration_ms,
                None,
                None,
                None,
            )?;
            self.log_job(
                &request.job_id,
                "diarizing",
                &request.source_file.original_name,
            );
            if let Some(package_path) =
                model_downloads::installed_diarization_package_path(&self.models_dir)
            {
                let worker_request = diarization_worker::WorkerRequest {
                    job_id: request.job_id.clone(),
                    audio_path: prepared.path.clone(),
                    package_path,
                    exact_speaker_count: request.speaker_count_hint,
                    spec_version: diarization::DIARIZATION_MODEL_SPEC_V2.manifest_version,
                };
                match diarization_worker::run_subprocess_worker(
                    &worker_request,
                    Some(&control.cancelled),
                    || {
                        self.refresh_diarization_activity(&request.job_id);
                    },
                ) {
                    Ok(turns) => {
                        let diagnostics = diarization::aggregate_diagnostics(&turns);
                        if let Some(warning) =
                            diarization::overclustering_warning(&turns, request.speaker_count_hint)
                        {
                            self.log_job(&request.job_id, "diarization_fallback", &warning);
                            diarization::mark_failure(&mut corrected, warning);
                        } else {
                            let reconciliation = Instant::now();
                            diarization::apply_turns_to_transcript(
                                &mut corrected,
                                turns,
                                request.speaker_count_hint,
                            );
                            self.log_job(
                                &request.job_id,
                                "timing",
                                &format!(
                                    "reconciliation_ms={}",
                                    reconciliation.elapsed().as_millis()
                                ),
                            );
                            let assigned = corrected
                                .segments
                                .iter()
                                .filter(|segment| {
                                    matches!(
                                        segment.speaker_attribution,
                                        crate::speaker_reconciliation::SpeakerAttribution::Assigned
                                    )
                                })
                                .count();
                            let likely = corrected
                                .segments
                                .iter()
                                .filter(|segment| {
                                    matches!(
                                        segment.speaker_attribution,
                                        crate::speaker_reconciliation::SpeakerAttribution::Likely
                                    )
                                })
                                .count();
                            let uncertain = corrected.segments.iter().filter(|segment| matches!(segment.speaker_attribution, crate::speaker_reconciliation::SpeakerAttribution::Uncertain | crate::speaker_reconciliation::SpeakerAttribution::Overlap)).count();
                            self.log_job(
                                &request.job_id,
                                "diarization_completed",
                                &format!("threshold={:.2} hint={:?} clusters={} turns={} short_clusters={} assigned={} likely={} uncertain_or_overlap={}", diarization::DIARIZATION_MODEL_SPEC_V2.clustering_threshold, request.speaker_count_hint, diagnostics.cluster_count, diagnostics.turn_count, diagnostics.short_cluster_count, assigned, likely, uncertain),
                            );
                        }
                    }
                    Err(error) if error.is::<diarization_worker::DiarizationWorkerCanceled>() => {
                        if preserve_asr_after_diarization_cancellation(control.reason()) {
                            self.log_job(
                                &request.job_id,
                                "diarization_fallback",
                                "worker stopped reporting activity",
                            );
                            if control.reason() == FileJobCancellationReason::User {
                                corrected.diarization_status =
                                    diarization::DiarizationStatus::Canceled;
                                corrected.diarization_warning = Some(
                                    "Speaker identification stopped. Your transcript was kept."
                                        .into(),
                                );
                            } else {
                                diarization::mark_failure(&mut corrected,"Speaker identification stopped responding. Your transcript was kept.");
                            }
                        } else {
                            return Err(error);
                        }
                    }
                    Err(error) => {
                        self.log_job(&request.job_id, "diarization_fallback", &error.to_string());
                        diarization::mark_failure(
                            &mut corrected,
                            "Speaker identification could not finish. The transcript was saved without speaker labels.",
                        );
                    }
                }
            } else {
                self.log_job(
                    &request.job_id,
                    "diarization_fallback",
                    "verified model package unavailable",
                );
                diarization::mark_failure(
                    &mut corrected,
                    "Speaker identification is enabled, but its model is still installing or unavailable. The transcript was saved without speaker labels.",
                );
            }
        }
        self.log_job(
            &request.job_id,
            "timing",
            &format!(
                "speakers_total_ms={}",
                speaker_started.elapsed().as_millis()
            ),
        );
        let wall_duration_ms = asr_duration.as_millis() as i64;

        let _final_publication = self
            .publication
            .lock()
            .map_err(|_| anyhow!("File result unavailable"))?;
        if standalone && control.reason() == FileJobCancellationReason::User {
            if let Some(reference) = self
                .current_status(&request.job_id)
                .and_then(|s| s.review_ref)
            {
                corrected = self.review_store.machine(&reference)?;
                corrected.diarization_status = diarization::DiarizationStatus::Canceled;
                corrected.diarization_warning =
                    Some("Speaker identification stopped. Your transcript was kept.".into());
            }
        }
        let completion_message = match corrected.quality_status {
            crate::asr::TranscriptQualityStatus::Clean => "Transcription completed.",
            crate::asr::TranscriptQualityStatus::Recovered => {
                "Transcription completed after decoder recovery."
            }
            crate::asr::TranscriptQualityStatus::Partial => {
                "Transcription completed with a section that needs review."
            }
        };
        self.update_status(
            &request.job_id,
            FileTranscriptionJobStage::Saving,
            "Saving transcript to local history...",
            Some(100.0),
            request.source_file.duration_ms,
            request.source_file.duration_ms,
            Some(0),
            None,
            None,
        )?;
        self.log_job(
            &request.job_id,
            "saving",
            &request.source_file.original_name,
        );

        if let Some(model_id) =
            resolve_model_id_for_job(&self.db_path, PreviewSourceKind::FileUpload)
                .ok()
                .flatten()
        {
            if let Some(audio_duration_ms) = request.source_file.duration_ms {
                let audio_ms = audio_duration_ms.max(1) as f64;
                let wall_ms = wall_duration_ms.max(1) as f64;
                let _ = storage::record_file_transcription_performance(
                    &self.db_path,
                    &model_id,
                    audio_ms / wall_ms,
                );
            }
        }

        let response = FileTranscriptionResponse {
            source_file: request.source_file.clone(),
            resolved_model,
            result: corrected,
            saved_transcript,
        };

        self.update_status(
            &request.job_id,
            FileTranscriptionJobStage::Completed,
            completion_message,
            Some(100.0),
            request.source_file.duration_ms,
            request.source_file.duration_ms,
            Some(0),
            Some(response),
            None,
        )?;
        self.log_job(
            &request.job_id,
            "completed",
            &request.source_file.original_name,
        );
        if let Some((dir, _)) = &call {
            // The mix stays for playback; the separate channels are no longer needed.
            crate::meeting::release_channels(dir);
        }
        Ok(())
    }

    fn meetings_dir(&self) -> PathBuf {
        self.db_path
            .parent()
            .unwrap_or(Path::new("."))
            .join("meetings")
    }

    /// Transcribes a call's microphone and call-app channels separately, tells the
    /// other participants apart (Pro), and merges both into one transcript with speakers.
    #[allow(clippy::too_many_arguments)]
    fn transcribe_call(
        &self,
        request: &FileTranscriptionRequest,
        dir: &Path,
        sidecar: &crate::meeting::CallSidecar,
        engine_request: EngineFileTranscriptionRequest,
        vibevoice: bool,
        temp_dir: &Path,
        control: Arc<FileJobRunControl>,
        started_at: Instant,
    ) -> Result<TranscriptResult> {
        let local_path = sidecar.local_path(dir);
        let remote_path = sidecar.remote_path(dir);
        let mut channels = vec![("your side", local_path)];
        if let Some(remote) = remote_path {
            channels.push(("the other side", remote));
        }
        let share = 1.0 / channels.len() as f32;
        let mut results = Vec::new();
        let mut remote_audio = None;
        for (index, (label, path)) in channels.into_iter().enumerate() {
            if !path.is_file() {
                return Err(anyhow!(
                    "CALL_CHANNEL_MISSING: The recording of {label} is missing from {}.",
                    dir.display()
                ));
            }
            let prepared =
                crate::audio_preprocess::prepare_job_audio(&path.to_string_lossy(), temp_dir)?;
            if let Ok(mut window) = self.progress_window.lock() {
                *window = Some(ProgressWindow {
                    offset: index as f32 * share * 100.0,
                    share,
                    text: format!("Transcribing {label}..."),
                });
            }
            let mut channel_request = engine_request.clone();
            channel_request.file_path = if vibevoice {
                path.to_string_lossy().into_owned()
            } else {
                prepared.path.to_string_lossy().into_owned()
            };
            let result = self.run_transcription_worker(
                request,
                channel_request,
                Arc::clone(&control),
                started_at,
            );
            if let Ok(mut window) = self.progress_window.lock() {
                *window = None;
            }
            results.push(result?);
            if index == 1 {
                remote_audio = Some(prepared);
            }
        }
        let mut results = results.into_iter();
        let local = results.next();
        let remote = results.next();

        let separate = crate::license::is_pro(&self.db_path);
        let mut remote_turns = None;
        let remote_has_speech = remote.as_ref().is_some_and(|result| !result.segments.is_empty());
        if separate && remote_has_speech {
            if let (Some(audio), Some(package_path)) = (
                remote_audio.as_ref(),
                model_downloads::installed_diarization_package_path(&self.models_dir),
            ) {
                self.update_status(
                    &request.job_id,
                    FileTranscriptionJobStage::Diarizing,
                    "Text ready · Telling the other participants apart",
                    None,
                    request.source_file.duration_ms,
                    request.source_file.duration_ms,
                    None,
                    None,
                    None,
                )?;
                let worker_request = diarization_worker::WorkerRequest {
                    job_id: request.job_id.clone(),
                    audio_path: audio.path.clone(),
                    package_path,
                    // The hint counts everyone; the call app's channel holds all but this Mac's user.
                    exact_speaker_count: request
                        .speaker_count_hint
                        .map(|count| count - 1)
                        .filter(|count| *count >= 1),
                    spec_version: diarization::DIARIZATION_MODEL_SPEC_V2.manifest_version,
                };
                match diarization_worker::run_subprocess_worker(
                    &worker_request,
                    Some(&control.cancelled),
                    || self.refresh_diarization_activity(&request.job_id),
                ) {
                    Ok(turns) => {
                        if let Some(warning) =
                            diarization::overclustering_warning(&turns, worker_request.exact_speaker_count)
                        {
                            self.log_job(&request.job_id, "call_diarization_fallback", &warning);
                        } else {
                            remote_turns = Some(turns);
                        }
                    }
                    Err(error) => {
                        self.log_job(&request.job_id, "call_diarization_fallback", &error.to_string());
                    }
                }
            }
        }
        let spans = crate::meeting::speakers::spans_from_events(&sidecar.events, sidecar.duration_ms);
        self.log_job(
            &request.job_id,
            "call",
            &format!(
                "channels={} browser_events={} spans={} acoustic_turns={}",
                if remote.is_some() { 2 } else { 1 },
                sidecar.events.len(),
                spans.len(),
                remote_turns.as_ref().map_or(0, Vec::len)
            ),
        );
        let model_name = local
            .as_ref()
            .or(remote.as_ref())
            .map(|result| result.model_name.clone())
            .unwrap_or_default();
        Ok(crate::meeting::speakers::assemble(
            crate::meeting::speakers::AssembleInput {
                job_id: request.job_id.clone(),
                model_name,
                local,
                remote,
                remote_turns,
                spans,
                local_name: sidecar.self_name.clone(),
                separate_remote_speakers: separate,
            },
        ))
    }

    fn run_transcription_worker(
        &self,
        request: &FileTranscriptionRequest,
        engine_request: EngineFileTranscriptionRequest,
        control: Arc<FileJobRunControl>,
        started_at: Instant,
    ) -> Result<TranscriptResult> {
        let executable = std::env::current_exe()
            .map_err(|error| anyhow!("failed to resolve app executable: {error}"))?;
        let worker_request = WorkerRequest {
            models_dir: self.models_dir.clone(),
            request: engine_request,
            preload: false,
        };
        let request_json = serde_json::to_vec(&worker_request)?;

        let mut command = Command::new(executable);
        crate::managed_process::isolate(&mut command);
        let child = command
            .arg(transcription_worker::WORKER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| anyhow!("failed to start transcription worker: {error}"))?;

        let mut child = crate::managed_process::ManagedChild::new(child);
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&request_json)?;
        }

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("transcription worker stdout was unavailable"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("transcription worker stderr was unavailable"))?;

        let (output_tx, output_rx) = mpsc::channel();
        let output_reader = transcription_worker::read_worker_output_lines(stdout, output_tx);
        let stderr_reader = thread::spawn(move || {
            let mut stderr = stderr;
            let mut text = String::new();
            let _ = std::io::Read::read_to_string(&mut stderr, &mut text);
            text
        });

        let mut last_progress = -1;
        let mut last_decoder_activity = Instant::now();

        loop {
            if control.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                let _ = output_reader.join();
                let stderr_text = stderr_reader.join().unwrap_or_default();
                let detail = stderr_text.trim();
                return Err(anyhow!(
                    "File transcription watchdog expired before completion.{}",
                    if detail.is_empty() {
                        "".to_string()
                    } else {
                        format!(" Worker output: {detail}")
                    }
                ));
            }

            match output_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(Ok(WorkerOutput::Progress { progress_percent })) => {
                    if progress_percent > last_progress {
                        last_progress = progress_percent;
                        last_decoder_activity = Instant::now();
                    }
                    self.update_worker_progress(request, progress_percent, started_at)?;
                }
                Ok(Ok(WorkerOutput::Heartbeat { progress_percent })) => {
                    if progress_percent > last_progress {
                        last_progress = progress_percent;
                        last_decoder_activity = Instant::now();
                    }
                    if last_decoder_activity.elapsed() > WORKER_STALL_TIMEOUT {
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = output_reader.join();
                        let _ = stderr_reader.join();
                        return Err(anyhow!(
                            "File transcription worker stopped advancing for ten minutes."
                        ));
                    }
                    self.update_worker_progress(request, progress_percent.max(0), started_at)?;
                }
                // Only persistent workers answer preload requests.
                Ok(Ok(WorkerOutput::Ready)) => {}
                Ok(Ok(WorkerOutput::Result { result })) => {
                    let _ = child.wait();
                    let _ = output_reader.join();
                    let _ = stderr_reader.join();
                    return Ok(result);
                }
                Ok(Ok(WorkerOutput::Error { message })) => {
                    let _ = child.wait();
                    let _ = output_reader.join();
                    let _ = stderr_reader.join();
                    return Err(anyhow!(message));
                }
                Ok(Err(error)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = output_reader.join();
                    let _ = stderr_reader.join();
                    return Err(anyhow!(
                        "transcription worker emitted invalid output: {error}"
                    ));
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(status) = child.try_wait()? {
                        let _ = output_reader.join();
                        let stderr_text = stderr_reader.join().unwrap_or_default();
                        return Err(anyhow!(
                            "transcription worker exited before returning a result (status: {status}). {}",
                            stderr_text.trim()
                        ));
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if let Some(status) = child.try_wait()? {
                        let stderr_text = stderr_reader.join().unwrap_or_default();
                        return Err(anyhow!(
                            "transcription worker output closed before returning a result (status: {status}). {}",
                            stderr_text.trim()
                        ));
                    }
                }
            }
        }
    }

    fn update_worker_progress(
        &self,
        request: &FileTranscriptionRequest,
        raw_progress_percent: i32,
        started_at: Instant,
    ) -> Result<()> {
        let window = self.progress_window.lock().ok().and_then(|window| window.clone());
        let raw = (raw_progress_percent as f32).clamp(0.0, 100.0);
        let pct = match &window {
            Some(window) => window.offset + raw * window.share,
            None => raw,
        };
        let processed_ms = request
            .source_file
            .duration_ms
            .map(|total_ms| (total_ms as f64 * pct as f64 / 100.0) as i64);
        let eta_seconds = if pct > 0.0 {
            let elapsed_secs = started_at.elapsed().as_secs_f64();
            let remaining_pct = 100.0 - pct as f64;
            Some((elapsed_secs * remaining_pct / pct as f64).ceil() as i64)
        } else {
            None
        };

        let text = window.map(|window| window.text).unwrap_or_else(|| {
            format!("Transcribing {} locally...", request.source_file.original_name)
        });
        self.update_status(
            &request.job_id,
            FileTranscriptionJobStage::Transcribing,
            &text,
            Some(pct),
            processed_ms,
            request.source_file.duration_ms,
            eta_seconds,
            None,
            None,
        )
    }

    fn spawn_watchdog(
        &self,
        job_id: String,
        control: Arc<FileJobRunControl>,
        finished: mpsc::Receiver<()>,
    ) -> thread::JoinHandle<()> {
        let controller = self.clone();
        thread::spawn(move || loop {
            if control.is_cancelled() {
                break;
            }
            if finished.recv_timeout(Duration::from_millis(1500))
                != Err(mpsc::RecvTimeoutError::Timeout)
            {
                break;
            }
            if control.is_cancelled() {
                break;
            }
            let Some(status) = controller.current_status(&job_id) else {
                break;
            };
            if is_terminal(&status.stage) {
                break;
            }
            if watchdog_expired(&status, chrono::Utc::now().timestamp_millis()) {
                control.request_watchdog_cancellation();
                if watchdog_disposition(&status.stage) == WatchdogDisposition::PreserveAsr {
                    controller.log_job(
                        &job_id,
                        "diarization_watchdog",
                        "worker stopped reporting activity; preserving ASR result",
                    );
                } else {
                    controller.fail_job(
                        &job_id,
                        "File transcription timed out.",
                        "The file job stopped reporting progress and was cancelled.".to_string(),
                    );
                    controller.log_job(&job_id, "failed", "watchdog timeout");
                }
                break;
            }
        })
    }

    fn refresh_diarization_activity(&self, job_id: &str) {
        let _status_update = self
            .status_updates
            .lock()
            .expect("file transcription status update mutex poisoned");
        let refreshed = {
            let mut statuses = self
                .statuses
                .lock()
                .expect("file transcription status mutex poisoned");
            statuses
                .get_mut(job_id)
                .and_then(|status| refresh_diarization_status(status, chrono::Utc::now().timestamp_millis()))
        };

        if let Some(status) = refreshed {
            self.emit_status(status);
        }
    }

    fn update_status(
        &self,
        job_id: &str,
        stage: FileTranscriptionJobStage,
        status_text: &str,
        progress_percent: Option<f32>,
        processed_ms: Option<i64>,
        total_ms: Option<i64>,
        eta_seconds: Option<i64>,
        result: Option<FileTranscriptionResponse>,
        error_message: Option<String>,
    ) -> Result<()> {
        let _status_update = self
            .status_updates
            .lock()
            .map_err(|_| anyhow!("File status unavailable"))?;
        let current = self
            .current_status(job_id)
            .ok_or_else(|| anyhow!("Missing file transcription status for job {}", job_id))?;
        if is_terminal(&current.stage) {
            return Ok(());
        }
        let mut review_ref = current.review_ref.clone();
        let mut result_revision = current.result_revision;
        if let Some(response) = result {
            let document = if let Some(reference) = &review_ref {
                self.review_store
                    .replace_machine(reference, response.result, false)?
            } else {
                let reference = if let Some(saved) = response.saved_transcript {
                    crate::review::ReviewRef::Saved { id: saved.id }
                } else {
                    self.review_store.create_session(
                        job_id,
                        response.source_file.clone(),
                        response.result,
                    )?
                };
                let document = self.review_store.get(&reference)?;
                review_ref = Some(reference);
                document
            };
            result_revision = document.revision;
            self.published_models
                .lock()
                .expect("published model mutex poisoned")
                .insert(job_id.into(), response.resolved_model);
            let _ = self.app.emit("review-updated", &document.reference);
        }
        let next = FileTranscriptionStatusEvent {
            job_id: current.job_id.clone(),
            source_file: current.source_file.clone(),
            stage,
            progress_percent,
            processed_ms,
            total_ms,
            eta_seconds,
            status_text: status_text.to_string(),
            result: None,
            review_ref,
            result_revision,
            error_message,
            started_at_ms: current.started_at_ms,
            updated_at_ms: chrono::Utc::now().timestamp_millis().max(current.updated_at_ms + 1),
        };
        self.statuses
            .lock()
            .map_err(|_| anyhow!("File status unavailable"))?
            .insert(job_id.into(), next.clone());
        self.emit_status(next);
        Ok(())
    }

    fn fail_job(&self, job_id: &str, status_text: &str, error_message: String) {
        if let Some(current) = self.current_status(job_id) {
            if is_terminal(&current.stage) {
                return;
            }
        }

        let _ = self.update_status(
            job_id,
            FileTranscriptionJobStage::Failed,
            status_text,
            None,
            None,
            None,
            None,
            None,
            Some(error_message.clone()),
        );
        self.log_job(job_id, "failed", &error_message);
    }

    fn cancel_job(&self, job_id: &str, status_text: &str, error_message: String) {
        if self
            .current_status(job_id)
            .is_some_and(|status| is_terminal(&status.stage))
        {
            return;
        }
        let _ = self.update_status(
            job_id,
            FileTranscriptionJobStage::Canceled,
            status_text,
            None,
            None,
            None,
            None,
            None,
            Some(error_message.clone()),
        );
        self.log_job(job_id, "canceled", &error_message);
    }

    fn persist_status(&self, mut status: FileTranscriptionStatusEvent) {
        let _status_update = self
            .status_updates
            .lock()
            .expect("file transcription status update mutex poisoned");
        {
            let mut statuses = self
                .statuses
                .lock()
                .expect("file transcription status mutex poisoned");
            if let Some(current) = statuses.get(&status.job_id) {
                if is_terminal(&current.stage) {
                    return;
                }
                if status.updated_at_ms < current.updated_at_ms {
                    return;
                }
                status.updated_at_ms = status.updated_at_ms.max(current.updated_at_ms + 1);
            }
            statuses.insert(status.job_id.clone(), status.clone());
        }

        self.emit_status(status);
    }

    fn emit_status(&self, status: FileTranscriptionStatusEvent) {
        match status.stage {
            FileTranscriptionJobStage::Completed => crate::sound::notify(
                &self.app,
                crate::sound::FeedbackCue::Complete,
                &format!("file:{}:complete", status.job_id),
            ),
            FileTranscriptionJobStage::Failed => crate::sound::notify(
                &self.app,
                crate::sound::FeedbackCue::Error,
                &format!("file:{}:error", status.job_id),
            ),
            _ => {}
        }
        if let Err(error) = self
            .app
            .emit(FILE_TRANSCRIPTION_STATUS_EVENT, status.clone())
        {
            self.log_job(&status.job_id, "emit_error", &error.to_string());
        }
    }

    fn current_status(&self, job_id: &str) -> Option<FileTranscriptionStatusEvent> {
        self.statuses
            .lock()
            .expect("file transcription status mutex poisoned")
            .get(job_id)
            .cloned()
    }

    fn finish_run(&self, job_id: &str) {
        {
            let mut active = self
                .active_run
                .lock()
                .expect("file transcription active mutex poisoned");
            if let Some(run) = active.as_ref() {
                if run.job_id == job_id {
                    run.control.stop();
                }
            }
            *active = None;
        }
    }

    fn log_job(&self, job_id: &str, stage: &str, message: &str) {
        let line = format!("[{}] {} {} {}\n", chrono::Utc::now().timestamp_millis(), job_id, stage, message);
        eprintln!("[file-job] {}", line.trim_end());
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }
}

fn resolve_model_for_settings(
    engine: &dyn TranscriptionEngine,
    settings: &AppSettings,
    source_kind: PreviewSourceKind,
) -> Result<Option<InstalledModel>> {
    let models = engine.list_models()?;
    let (selected_model_id, profile) = match source_kind {
        PreviewSourceKind::QuickDictate => (
            settings.quick_dictate_selected_model_id.as_deref(),
            settings.quick_dictate_model_profile,
        ),
        PreviewSourceKind::FileUpload => (
            settings.file_transcribe_selected_model_id.as_deref(),
            settings.file_transcribe_model_profile,
        ),
    };

    Ok(if let Some(model_id) = selected_model_id {
        models.iter().find(|model| model.id == model_id).cloned()
    } else {
        models
            .iter()
            .find(|model| model.profile == profile && model.is_default)
            .or_else(|| models.iter().find(|model| model.profile == profile))
            .cloned()
    })
}

/// Short recordings (under two minutes) skip automatic speaker identification
/// unless the user asked for a specific speaker count. "Identify again" in the
/// review workspace still runs it on demand.
const AUTOMATIC_DIARIZATION_MIN_DURATION_MS: i64 = 120_000;

fn should_run_post_process_diarization(
    enabled: bool,
    model: Option<&InstalledModel>,
    duration_ms: Option<i64>,
    speaker_count_hint: Option<i32>,
) -> bool {
    let long_enough = speaker_count_hint.is_some()
        || duration_ms.is_none_or(|duration| duration >= AUTOMATIC_DIARIZATION_MIN_DURATION_MS);
    enabled && long_enough && !model.is_some_and(|model| model.capabilities.native_diarization)
}

fn resolve_model_id_for_job(
    db_path: &Path,
    source_kind: PreviewSourceKind,
) -> Result<Option<String>> {
    let settings = storage::get_settings_from_db_path(db_path)?;
    Ok(match source_kind {
        PreviewSourceKind::QuickDictate => settings.quick_dictate_selected_model_id,
        PreviewSourceKind::FileUpload => settings.file_transcribe_selected_model_id,
    })
}

fn refresh_diarization_status(
    status: &mut FileTranscriptionStatusEvent,
    refreshed_at_ms: i64,
) -> Option<FileTranscriptionStatusEvent> {
    if !matches!(status.stage, FileTranscriptionJobStage::Diarizing) {
        return None;
    }
    status.updated_at_ms = refreshed_at_ms;
    Some(status.clone())
}

fn watchdog_expired(status: &FileTranscriptionStatusEvent, current_time_ms: i64) -> bool {
    current_time_ms - status.updated_at_ms > WATCHDOG_TIMEOUT_MS
}

fn watchdog_disposition(stage: &FileTranscriptionJobStage) -> WatchdogDisposition {
    if matches!(stage, FileTranscriptionJobStage::Diarizing) {
        WatchdogDisposition::PreserveAsr
    } else {
        WatchdogDisposition::FailJob
    }
}

fn preserve_asr_after_diarization_cancellation(reason: FileJobCancellationReason) -> bool {
    matches!(
        reason,
        FileJobCancellationReason::Watchdog | FileJobCancellationReason::User
    )
}

fn is_terminal(stage: &FileTranscriptionJobStage) -> bool {
    matches!(
        stage,
        FileTranscriptionJobStage::Completed
            | FileTranscriptionJobStage::Failed
            | FileTranscriptionJobStage::Canceled
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(
        stage: FileTranscriptionJobStage,
        updated_at_ms: i64,
    ) -> FileTranscriptionStatusEvent {
        FileTranscriptionStatusEvent {
            job_id: "job-1".to_string(),
            source_file: SelectedSourceFile {
                file_path: "/tmp/audio.m4a".to_string(),
                original_name: "audio.m4a".to_string(),
                mime_type: "audio/mp4".to_string(),
                size_bytes: 42,
                duration_ms: Some(5_897_877),
                sha256: None,
            },
            stage,
            progress_percent: None,
            processed_ms: Some(5_897_877),
            total_ms: Some(5_897_877),
            eta_seconds: None,
            status_text: "Identifying speakers locally...".to_string(),
            result: None,
            review_ref: None,
            result_revision: 0,
            error_message: None,
            started_at_ms: 0,
            updated_at_ms,
        }
    }

    #[test]
    fn diarization_activity_refreshes_only_the_timestamp() {
        let mut current = status(FileTranscriptionJobStage::Diarizing, 10);
        let refreshed = refresh_diarization_status(&mut current, 2_000).expect("refreshed");

        assert_eq!(refreshed.updated_at_ms, 2_000);
        assert_eq!(refreshed.status_text, "Identifying speakers locally...");
        assert_eq!(refreshed.progress_percent, None);
        assert_eq!(refreshed.processed_ms, Some(5_897_877));
        assert_eq!(refreshed.total_ms, Some(5_897_877));
        assert!(matches!(
            refreshed.stage,
            FileTranscriptionJobStage::Diarizing
        ));
    }

    #[test]
    fn native_diarization_bypasses_standalone_post_process() {
        let mut model = InstalledModel {
            id: crate::model_metadata::MOSS_MODEL_ID.into(),
            engine: "moss-transcribe-cpp".into(),
            model_name: crate::model_metadata::MOSS_MODEL_NAME.into(),
            variant: "0.9B F16".into(),
            local_path: "/models/moss".into(),
            size_bytes: crate::model_metadata::MOSS_MODEL_SIZE,
            is_default: false,
            profile: crate::settings::ModelProfile::Accurate,
            capabilities: crate::model_metadata::ModelCapabilities::moss(),
        };
        assert!(!should_run_post_process_diarization(true, Some(&model), Some(600_000), None));
        model.capabilities = crate::model_metadata::ModelCapabilities::standard_asr();
        assert!(should_run_post_process_diarization(true, Some(&model), Some(600_000), None));
        assert!(!should_run_post_process_diarization(false, Some(&model), Some(600_000), None));
    }

    #[test]
    fn short_files_skip_automatic_speaker_identification_unless_a_count_is_given() {
        let model = InstalledModel {
            id: "ggml-large-v3-turbo-bin".into(),
            engine: "whisper.cpp".into(),
            model_name: "Whisper Turbo".into(),
            variant: "large-v3-turbo".into(),
            local_path: "/tmp/model.bin".into(),
            size_bytes: 1,
            is_default: false,
            profile: crate::settings::ModelProfile::Accurate,
            capabilities: crate::model_metadata::ModelCapabilities::standard_asr(),
        };
        assert!(!should_run_post_process_diarization(true, Some(&model), Some(60_000), None));
        assert!(should_run_post_process_diarization(true, Some(&model), Some(60_000), Some(2)));
        assert!(should_run_post_process_diarization(true, Some(&model), Some(120_000), None));
        assert!(should_run_post_process_diarization(true, Some(&model), None, None));
    }

    #[test]
    fn late_heartbeat_does_not_revive_a_terminal_job() {
        for stage in [
            FileTranscriptionJobStage::Canceled,
            FileTranscriptionJobStage::Failed,
            FileTranscriptionJobStage::Completed,
        ] {
            let mut current = status(stage, 10);
            assert!(refresh_diarization_status(&mut current, 2_000).is_none());
            assert_eq!(current.updated_at_ms, 10);
        }
    }

    #[test]
    fn repeated_diarization_heartbeats_keep_a_long_job_fresh() {
        let mut current = status(FileTranscriptionJobStage::Diarizing, 0);
        for heartbeat_ms in (2_000..=300_000).step_by(2_000) {
            let _ = refresh_diarization_status(&mut current, heartbeat_ms);
            assert!(!watchdog_expired(&current, heartbeat_ms + 1_500));
        }
    }

    #[test]
    fn watchdog_preserves_asr_only_during_diarization() {
        assert_eq!(
            watchdog_disposition(&FileTranscriptionJobStage::Diarizing),
            WatchdogDisposition::PreserveAsr
        );
        assert_eq!(
            watchdog_disposition(&FileTranscriptionJobStage::Transcribing),
            WatchdogDisposition::FailJob
        );
    }

    #[test]
    fn user_cancellation_takes_precedence_over_watchdog_cancellation() {
        let control = FileJobRunControl::new();
        control.request_watchdog_cancellation();
        control.request_user_cancellation();

        assert!(control.is_cancelled());
        assert_eq!(control.reason(), FileJobCancellationReason::User);
        assert!(preserve_asr_after_diarization_cancellation(
            control.reason()
        ));
    }

    #[test]
    fn watchdog_cancellation_preserves_the_completed_asr_result() {
        let control = FileJobRunControl::new();
        control.request_watchdog_cancellation();

        assert!(preserve_asr_after_diarization_cancellation(
            control.reason()
        ));
    }
}
