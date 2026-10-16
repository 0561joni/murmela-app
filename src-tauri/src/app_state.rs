use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use tauri::{AppHandle, Manager};

use crate::asr::{self, LocalTranscriptionEngine, TranscriptionEngine};
use crate::audio_capture::RecordingController;
use crate::autostart;
use crate::desktop_shell::DesktopShellController;
use crate::dictation::QuickDictationController;
use crate::file_jobs::FileTranscriptionController;
use crate::model_downloads::ModelDownloadManager;
use crate::settings::HealthCheckResponse;
use crate::sound::SoundPlayer;
use crate::startup::StartupPhase;
use crate::storage;
use crate::vocabulary;

#[derive(Clone)]
pub struct AppState {
    pub app_name: String,
    pub app_version: String,
    pub temp_dir: PathBuf,
    pub models_dir: PathBuf,
    pub db_path: PathBuf,
    pub engine: Arc<LocalTranscriptionEngine>,
    pub translation: crate::translation::TranslationService,
    pub sound_player: Arc<Option<SoundPlayer>>,
    pub recording_controller: RecordingController,
    pub dictation_controller: QuickDictationController,
    pub file_transcription_controller: FileTranscriptionController,
    pub model_download_manager: ModelDownloadManager,
    pub desktop_shell: DesktopShellController,
    pub startup_notices: Arc<Mutex<Vec<String>>>,
    pub review_store: crate::review::ReviewStore,
    pub review_jobs: crate::review_jobs::ReviewJobController,
    pub review_media: crate::review_media::MediaStore,
    pub live_pair: crate::live_pair::LivePair,
    pub meetings: crate::meeting::MeetingController,
    _live_pair_pressure: Arc<crate::memory_pressure::MemoryPressureWatch>,
}

impl AppState {
    pub fn initialize(app: &AppHandle, report_phase: impl Fn(StartupPhase)) -> Result<Self> {
        let app_name = app.package_info().name.clone();
        let app_version = app.package_info().version.to_string();
        let app_data_dir = app
            .path()
            .app_data_dir()
            .context("failed to resolve app data directory")?;
        let temp_dir = app_data_dir.join("temp");
        let models_dir = app_data_dir.join("models");
        let db_path = app_data_dir.join("speech_to_text.sqlite");

        fs::create_dir_all(&app_data_dir)?;
        fs::create_dir_all(&temp_dir)?;
        fs::create_dir_all(&models_dir)?;
        crate::model_downloads::start_background_vad_download(models_dir.clone(), db_path.clone());

        report_phase(StartupPhase::Models);
        let engine_models = asr::discover_installed_models(&models_dir)?;
        let engine = Arc::new(LocalTranscriptionEngine::new(
            models_dir.clone(),
            engine_models.clone(),
        ));
        let transcription_engine: Arc<dyn TranscriptionEngine> = engine.clone();

        report_phase(StartupPhase::Audio);
        let recording_controller = RecordingController::new(temp_dir.clone());
        let desktop_shell = DesktopShellController::initialize(app)?;
        let sound_player = Arc::new(match SoundPlayer::new() {
            Ok(player) => Some(player),
            Err(err) => {
                eprintln!("[sound] disabled (init failed): {err:?}");
                None
            }
        });
        let processing_queue = crate::review_jobs::ProcessingQueue::default();
        let translation = crate::translation::TranslationService::new(
            app.clone(),
            db_path.clone(),
            models_dir.clone(),
            engine.clone(),
            recording_controller.clone(),
            desktop_shell.clone(),
            processing_queue.clone(),
        );
        // Resident live-pair helper: loaded after start-up once it is the
        // selected shortcut model, independent of the processing queue.
        let status_app = app.clone();
        let live_pair = crate::live_pair::LivePair::new(move |status| {
            let _ = tauri::Emitter::emit(&status_app, "live-pair-status", status.clone());
        });
        let pressure_pair = live_pair.clone();
        let live_pair_pressure = Arc::new(crate::memory_pressure::MemoryPressureWatch::with_handler(
            0x1 | 0x2 | 0x4,
            move |level| pressure_pair.memory_pressure(level),
        ));
        let dictation_controller = QuickDictationController::new(
            app.clone(),
            Arc::clone(&transcription_engine),
            recording_controller.clone(),
            db_path.clone(),
            desktop_shell.clone(),
            Arc::clone(&sound_player),
            translation.clone(),
            live_pair.clone(),
        );
        crate::review_media::cleanup_stale_audio(&temp_dir);
        let review_store = crate::review::ReviewStore::new(db_path.clone());
        let review_jobs = crate::review_jobs::ReviewJobController::new(
            app.clone(),
            review_store.clone(),
            models_dir.clone(),
            temp_dir.clone(),
            processing_queue.clone(),
        );
        let review_media = crate::review_media::MediaStore::new(temp_dir.clone());
        let file_transcription_controller = FileTranscriptionController::new(
            app.clone(),
            Arc::clone(&transcription_engine),
            models_dir.clone(),
            db_path.clone(),
            review_store.clone(),
            processing_queue,
        );
        let model_download_manager = ModelDownloadManager::new(
            app.clone(),
            models_dir.clone(),
            db_path.clone(),
            Arc::clone(&engine),
        );

        let meetings = crate::meeting::MeetingController::new(app_data_dir.join("meetings"));

        let state = Self {
            app_name,
            app_version,
            temp_dir,
            models_dir,
            db_path,
            engine,
            translation,
            sound_player,
            recording_controller,
            dictation_controller,
            file_transcription_controller,
            model_download_manager,
            desktop_shell,
            startup_notices: Arc::new(Mutex::new(Vec::new())),
            review_store,
            review_jobs,
            review_media,
            live_pair,
            meetings,
            _live_pair_pressure: live_pair_pressure,
        };

        report_phase(StartupPhase::Library);
        storage::initialize_database(&state.db_path)?;
        {
            let meetings_dir = state.meetings.meetings_dir().to_path_buf();
            let db_path = state.db_path.clone();
            std::thread::spawn(move || {
                if let Err(error) = crate::meeting::remove_unused_recordings(&meetings_dir, &db_path) {
                    eprintln!("[call] could not tidy old recordings: {error:#}");
                }
            });
        }
        storage::sync_installed_models(&state, &engine_models)?;
        storage::apply_preferred_model_defaults(&state, &engine_models)?;
        vocabulary::seed_builtin_terms(&state)?;
        let settings = storage::get_settings(&state)?;
        if settings.file_diarization_enabled
            && crate::model_downloads::installed_diarization_package_path(&state.models_dir)
                .is_none()
        {
            if let Err(error) = state
                .model_download_manager
                .start_download(crate::diarization::DIARIZATION_MODEL_ID)
            {
                eprintln!("[diarization-model] startup resume unavailable: {error:#}");
            }
        }

        report_phase(StartupPhase::Shortcuts);
        state
            .recording_controller
            .set_preferred_input_device(settings.preferred_input_device.clone());
        autostart::sync_launch_at_login(app, settings.launch_at_login_enabled)?;
        if let Err(error) = state.dictation_controller.sync_shortcut_registration() {
            if !settings.translation_enabled {
                return Err(error);
            }
            storage::update_settings(
                &state,
                crate::settings::SettingsPatch {
                    translation_enabled: Some(false),
                    ..Default::default()
                },
            )?;
            state.dictation_controller.sync_shortcut_registration()?;
            if let Ok(mut notices) = state.startup_notices.lock() {
                notices.push(format!("Translation shortcut could not be registered. Original dictation remains available. Choose another language shortcut in Settings: {error}"));
            }
        }
        // Last-resort safety net: auto-recovers dictation if it ever gets stuck.
        state.dictation_controller.spawn_watchdog();
        Ok(state)
    }

    pub fn health_check(&self) -> HealthCheckResponse {
        HealthCheckResponse {
            app_name: self.app_name.clone(),
            app_version: self.app_version.clone(),
            db_path: self.db_path.display().to_string(),
            temp_dir: self.temp_dir.display().to_string(),
            models_dir: self.models_dir.display().to_string(),
            startup_notices: self
                .startup_notices
                .lock()
                .map(|notices| notices.clone())
                .unwrap_or_default(),
        }
    }
}
