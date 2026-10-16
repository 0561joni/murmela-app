

use murmela_lib::{
    app_state,
    asr,
    audio_capture,
    audio_files,
    autostart,
    desktop_shell,
    diarization,
    diarization_worker,
    dictation,
    file_jobs,
    insertion,
    key_gesture,
    licences,
    license,
    live_pair,
    meeting,
    model_downloads,
    model_metadata,
    network_activity,
    permissions,
    review,
    review_jobs,
    review_media,
    settings,
    shutdown,
    single_instance,
    sound,
    startup,
    storage,
    text_translation,
    transcript_commands,
    transcription_worker,
    translation,
    updates,
    vocabulary,
};

use app_state::AppState;
use asr::{
    FileTranscriptionRequest, InstalledModel, TranscriptionEngine, TranscriptionPreviewRequest,
    TranscriptionPreviewResponse,
};
use audio_capture::{InputDeviceOption, RecordingResult, RecordingStatusResponse};
use audio_files::{
    FileTranscriptionRequest as UploadedFileTranscriptionRequest, SelectedSourceFile,
};
use dictation::QuickDictationStatusResponse;
use file_jobs::{FileTranscriptionStatusEvent, StartFileTranscriptionResponse};
use model_downloads::{DownloadableModel, ModelDownloadStatus};
use serde::Serialize;
use settings::{AppSettings, HealthCheckResponse, InsertBehavior, SettingsPatch};
use startup::{StartupCoordinator, StartupPhase, StartupStatus};
use std::process::Command;
use std::time::Duration;
use storage::{TranscriptDetail, TranscriptSummary};
use tauri::{DragDropEvent, Emitter, Manager, WebviewEvent, Window, WindowEvent};
use tauri_plugin_clipboard_manager::ClipboardExt;
use transcript_commands::{TranscriptCopyVariant, TranscriptExportFormat, TranscriptExportResult};
use vocabulary::{CreateVocabularyTermInput, UpdateVocabularyTermInput, VocabularyTerm};

#[tauri::command]
fn health_check(state: tauri::State<'_, AppState>) -> Result<HealthCheckResponse, String> {
    Ok(state.health_check())
}

#[tauri::command]
fn is_app_shutting_down() -> bool {
    shutdown::is_shutting_down()
}

#[tauri::command]
fn quit_app(app: tauri::AppHandle) {
    shutdown::request_exit(&app, shutdown::ExitAction::Quit);
}

#[tauri::command]
fn restart_app(app: tauri::AppHandle) {
    shutdown::request_exit(&app, shutdown::ExitAction::Restart);
}

#[tauri::command]
fn get_startup_status(startup: tauri::State<'_, StartupCoordinator>) -> StartupStatus {
    startup.status()
}

#[tauri::command]
fn frontend_startup_complete(app: tauri::AppHandle, startup: tauri::State<'_, StartupCoordinator>) {
    if !startup.mark_frontend_ready(&app) {
        return;
    }

    let app_handle = app.clone();
    let startup = startup.inner().clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let _ = finish_startup_handoff(&app_handle, &startup);
    });
}

#[tauri::command]
fn report_startup_failure(
    app: tauri::AppHandle,
    startup: tauri::State<'_, StartupCoordinator>,
    message: String,
) {
    startup.fail(&app, message);
}

#[tauri::command]
fn complete_startup_handoff(
    app: tauri::AppHandle,
    startup: tauri::State<'_, StartupCoordinator>,
) -> Result<(), String> {
    finish_startup_handoff(&app, startup.inner())
}

fn finish_startup_handoff(
    app: &tauri::AppHandle,
    startup: &StartupCoordinator,
) -> Result<(), String> {
    if !startup.claim_handoff() {
        return Ok(());
    }

    let main = app
        .get_webview_window("main")
        .ok_or_else(|| "main window is unavailable".to_string())?;
    main.show().map_err(|error| error.to_string())?;
    let _ = main.unminimize();
    let _ = main.set_focus();
    if let Some(splash) = app.get_webview_window("splashscreen") {
        splash.close().map_err(|error| error.to_string())?;
    }
    eprintln!("[startup] splash handoff completed");
    Ok(())
}

#[tauri::command]
fn get_dictation_output_state(state: tauri::State<'_, AppState>) -> translation::OutputState {
    state.translation.snapshot()
}
#[tauri::command]
fn set_dictation_output_mode(
    state: tauri::State<'_, AppState>,
    mode: translation::OutputMode,
) -> Result<translation::OutputState, String> {
    state.translation.set_mode(mode).map_err(|e| e.to_string())
}
#[tauri::command]
async fn retry_streaming_dictation(state: tauri::State<'_, AppState>) -> Result<QuickDictationStatusResponse, String> {
    let controller = state.dictation_controller.clone();
    tauri::async_runtime::spawn_blocking(move || controller.retry_streaming().map_err(|e| e.to_string()))
        .await.map_err(|e| e.to_string())?
}
#[tauri::command]
async fn retry_dictation_translation(
    state: tauri::State<'_, AppState>,
    transcript_id: Option<String>,
) -> Result<translation::DictationOutput, String> {
    let work = shutdown::begin_work(true).map_err(|e| e.to_string())?;
    let service = state.translation.clone();
    let store = state.review_store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        let saved = transcript_id
            .map(|id| {
                store
                    .get(&review::ReviewRef::Saved { id })
                    .and_then(|document| {
                        document.detail.translation.ok_or_else(|| {
                            anyhow::anyhow!("No translation is available for this transcript.")
                        })
                    })
            })
            .transpose()
            .map_err(|e| e.to_string())?;
        service.retry(saved).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_settings(state: tauri::State<'_, AppState>) -> Result<AppSettings, String> {
    storage::get_settings(state.inner()).map_err(|error| error.to_string())
}

#[tauri::command]
fn update_settings(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    patch: SettingsPatch,
) -> Result<AppSettings, String> {
    let previous = storage::get_settings(state.inner()).map_err(|error| error.to_string())?;
    let sync_autostart = patch.launch_at_login_enabled.is_some();
    let sync_shortcut = patch.shortcut.is_some()
        || patch.shortcut_mode.is_some()
        || patch.translation_enabled.is_some()
        || patch.translation_cycle_shortcut.is_some()
        || patch.paste_last_shortcut.is_some();
    if state.translation.snapshot().busy && (sync_shortcut || patch.translation_model_id.is_some())
    {
        return Err(
            "Finish or cancel dictation before changing its shortcut or translation settings."
                .into(),
        );
    }
    if patch
        .translation_model_id
        .as_deref()
        .is_some_and(|id| translation::model_spec(id).is_none())
    {
        return Err("Unsupported translation model.".into());
    }
    if patch.file_diarization_enabled == Some(true) {
        license::require_pro(&state.db_path, "speaker labels").map_err(|error| error.to_string())?;
    }
    let diarization_change = patch.file_diarization_enabled;
    let history_change = patch.save_history.is_some();
    let translation_change = patch.translation_enabled;
    let translation_model_change = patch.translation_model_id.is_some();
    let settings =
        storage::update_settings(state.inner(), patch).map_err(|error| error.to_string())?;
    if history_change {
        state.dictation_controller.refresh_tray_recents();
    }
    let integration_result = (|| -> Result<(), String> {
        if sync_autostart {
            autostart::sync_launch_at_login(&app, settings.launch_at_login_enabled)
                .map_err(|error| error.to_string())?;
        }
        if sync_shortcut {
            state
                .dictation_controller
                .sync_shortcut_registration()
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    })();
    if let Err(error) = integration_result {
        // Failed OS integration must not leave a different preference in the database.
        let restored = storage::update_settings(
            state.inner(),
            SettingsPatch {
                default_mode: Some(previous.default_mode.clone()),
                shortcut: Some(previous.shortcut.clone()),
                translation_enabled: Some(previous.translation_enabled),
                translation_cycle_shortcut: Some(previous.translation_cycle_shortcut.clone()),
                translation_cycle_languages: Some(previous.translation_cycle_languages.clone()),
                translation_model_id: Some(previous.translation_model_id.clone()),
                shortcut_mode: Some(previous.shortcut_mode.clone()),
                language_mode: Some(previous.language_mode.clone()),
                fixed_language: Some(previous.fixed_language.clone()),
                preferred_input_device: Some(previous.preferred_input_device.clone()),
                insert_behavior: Some(previous.insert_behavior.clone()),
                launch_at_login_enabled: Some(previous.launch_at_login_enabled.clone()),
                gpu_enabled: Some(previous.gpu_enabled.clone()),
                shortcut_dictation_model_profile: Some(
                    previous.shortcut_dictation_model_profile.clone(),
                ),
                shortcut_dictation_selected_model_id: Some(
                    previous.shortcut_dictation_selected_model_id.clone(),
                ),
                quick_dictate_model_profile: Some(previous.quick_dictate_model_profile.clone()),
                quick_dictate_selected_model_id: Some(
                    previous.quick_dictate_selected_model_id.clone(),
                ),
                file_transcribe_model_profile: Some(previous.file_transcribe_model_profile.clone()),
                file_transcribe_selected_model_id: Some(
                    previous.file_transcribe_selected_model_id.clone(),
                ),
                appearance: Some(previous.appearance.clone()),
                motion_preference: Some(previous.motion_preference.clone()),
                save_history: Some(previous.save_history.clone()),
                sounds_enabled: Some(previous.sounds_enabled.clone()),
                volume_ducking_enabled: Some(previous.volume_ducking_enabled.clone()),
                file_diarization_enabled: Some(previous.file_diarization_enabled.clone()),
                live_pair_chunk_ms: Some(previous.live_pair_chunk_ms),
                live_pair_keep_loaded: Some(previous.live_pair_keep_loaded),
                paste_last_shortcut: Some(previous.paste_last_shortcut.clone()),
                serious_mode: Some(previous.serious_mode),
                auto_update_enabled: Some(previous.auto_update_enabled),
            },
        )
        .map_err(|rollback| format!("{error}; could not restore previous settings: {rollback}"))?;
        if sync_autostart {
            let _ = autostart::sync_launch_at_login(&app, restored.launch_at_login_enabled);
        }
        if sync_shortcut {
            let _ = state.dictation_controller.sync_shortcut_registration();
        }
        return Err(error);
    }
    if translation_change == Some(false) || translation_model_change {
        state.translation.release_translation_model();
    }
    if translation_change == Some(false)
        && state.translation.snapshot().output_mode != translation::OutputMode::Original
    {
        let _ = state
            .translation
            .set_mode(translation::OutputMode::Original);
    }
    match diarization_change {
        Some(true)
            if model_downloads::installed_diarization_package_path(&state.models_dir).is_none() =>
        {
            if let Err(error) = state
                .model_download_manager
                .start_download(diarization::DIARIZATION_MODEL_ID)
            {
                eprintln!("[diarization-model] automatic download unavailable: {error:#}");
            }
        }
        Some(false) => {
            let _ = state
                .model_download_manager
                .cancel_download(diarization::DIARIZATION_MODEL_ID);
        }
        _ => {}
    }
    state
        .recording_controller
        .set_preferred_input_device(settings.preferred_input_device.clone());
    let _ = app.emit("settings-changed", &settings);
    Ok(settings)
}

#[tauri::command]
fn list_transcripts(
    state: tauri::State<'_, AppState>,
    query: Option<String>,
) -> Result<Vec<TranscriptSummary>, String> {
    storage::list_transcripts(state.inner(), query).map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_transcript(
    state: tauri::State<'_, AppState>,
    transcript_id: String,
) -> Result<(), String> {
    storage::delete_transcript(state.inner(), &transcript_id).map_err(|error| error.to_string())?;
    state.dictation_controller.refresh_tray_recents();
    Ok(())
}

#[tauri::command]
fn delete_all_transcripts(state: tauri::State<'_, AppState>) -> Result<(), String> {
    storage::delete_all_transcripts(state.inner()).map_err(|error| error.to_string())?;
    // Deleting all history also forgets the in-memory last dictation.
    state.dictation_controller.forget_last_dictation();
    Ok(())
}

#[tauri::command]
fn get_transcript(
    state: tauri::State<'_, AppState>,
    transcript_id: String,
) -> Result<TranscriptDetail, String> {
    state
        .review_store
        .get(&review::ReviewRef::Saved { id: transcript_id })
        .map(|document| document.detail)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn get_review(
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
) -> Result<review::ReviewDocument, review::ReviewError> {
    let store = state.review_store.clone();
    tauri::async_runtime::spawn_blocking(move || store.get(&reference))
        .await
        .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
        .map_err(Into::into)
}

#[tauri::command]
async fn edit_review(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
    expected_revision: u64,
    edit: review::ReviewEdit,
) -> Result<review::ReviewDocument, review::ReviewError> {
    let work = shutdown::begin_work(false).map_err(review::ReviewError::from)?;
    let store = state.review_store.clone();
    let document = tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        store.edit(&reference, expected_revision, edit)
    })
    .await
    .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
    .map_err(review::ReviewError::from)?;
    let _ = app.emit("review-updated", &document.reference);
    Ok(document)
}

#[tauri::command]
async fn start_review_job(
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
    speaker_count: Option<i32>,
    reset: bool,
) -> Result<review_jobs::ReviewJobStatus, review::ReviewError> {
    license::require_pro(&state.db_path, "speaker labels").map_err(review::ReviewError::from)?;
    // A recorded call's speakers come from its two channels; diarizing the mix would replace them.
    let from_channels = state.review_store.machine(&reference).is_ok_and(|result| {
        result.diarization_model_id.as_deref() == Some(meeting::speakers::MEETING_DIARIZATION_MODEL_ID)
    });
    if from_channels {
        return Err(review::ReviewError::from(anyhow::anyhow!(
            "Speakers in a recorded call come from its two channels. Rename, merge or reassign them instead."
        )));
    }
    let jobs = state.review_jobs.clone();
    tauri::async_runtime::spawn_blocking(move || jobs.start(reference, speaker_count, reset, None))
        .await
        .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
        .map_err(Into::into)
}

#[tauri::command]
fn get_review_job_statuses(state: tauri::State<'_, AppState>) -> Vec<review_jobs::ReviewJobStatus> {
    state.review_jobs.statuses()
}
#[tauri::command]
fn cancel_review_job(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<(), review::ReviewError> {
    state.review_jobs.cancel(&job_id).map_err(Into::into)
}

#[tauri::command]
async fn resolve_review_audio(
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
    replacement_path: Option<String>,
    fallback: bool,
) -> Result<review_media::ReviewAudio, review::ReviewError> {
    let state = state.inner().clone();
    let work = shutdown::begin_work(false).map_err(review::ReviewError::from)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        state
            .review_media
            .resolve(&state.review_store, &reference, replacement_path, fallback)
    })
    .await
    .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
    .map_err(Into::into)
}
#[tauri::command]
fn release_review_audio(state: tauri::State<'_, AppState>, token: String) {
    state.review_media.release(&token);
}
#[tauri::command]
async fn copy_review(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
    variant: TranscriptCopyVariant,
) -> Result<(), review::ReviewError> {
    let store = state.review_store.clone();
    tauri::async_runtime::spawn_blocking(move || {
        transcript_commands::copy(&app, &store.get(&reference)?.detail, variant)
    })
    .await
    .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
    .map_err(Into::into)
}
#[tauri::command]
async fn export_review(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    state: tauri::State<'_, AppState>,
    reference: review::ReviewRef,
    format: TranscriptExportFormat,
    variant: Option<TranscriptCopyVariant>,
) -> Result<TranscriptExportResult, review::ReviewError> {
    let store = state.review_store.clone();
    let work = shutdown::begin_work(false).map_err(review::ReviewError::from)?;
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        transcript_commands::export_variant_blocking(
            &app,
            &window,
            &store.get(&reference)?.detail,
            format,
            variant,
        )
    })
    .await
    .map_err(|e| review::ReviewError::from(anyhow::anyhow!(e)))?
    .map_err(Into::into)
}
#[tauri::command]
async fn get_file_transcription_result(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<file_jobs::FileTranscriptionResponse, String> {
    let controller = state.file_transcription_controller.clone();
    tauri::async_runtime::spawn_blocking(move || controller.result(&job_id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
}
#[tauri::command]
fn dismiss_file_transcription(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<(), String> {
    if let Ok(result) = state.file_transcription_controller.result(&job_id) {
        let reference = result
            .saved_transcript
            .map(|s| review::ReviewRef::Saved { id: s.id })
            .unwrap_or(review::ReviewRef::Session { id: job_id.clone() });
        if state.review_jobs.active_for(&reference) {
            return Err("Stop the speaker retry before dismissing this result.".into());
        }
    }
    state
        .file_transcription_controller
        .dismiss(&job_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn rename_transcript(
    state: tauri::State<'_, AppState>,
    transcript_id: String,
    title: String,
) -> Result<TranscriptSummary, String> {
    state
        .review_store
        .rename_title(&transcript_id, &title)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_translator_status(state: tauri::State<'_, AppState>) -> text_translation::TranslatorStatus {
    text_translation::status(&state.translation)
}

#[tauri::command]
fn detect_text_language(text: String) -> Option<text_translation::DetectedLanguage> {
    text_translation::detect_language(&text)
}

#[tauri::command]
async fn translate_text(
    state: tauri::State<'_, AppState>,
    request: text_translation::TextTranslationRequest,
) -> Result<text_translation::TextTranslationOutcome, String> {
    let service = state.translation.clone();
    tauri::async_runtime::spawn_blocking(move || {
        text_translation::translate(&service, request).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn translation_alternatives(
    state: tauri::State<'_, AppState>,
    request: text_translation::ParagraphEditRequest,
) -> Result<Vec<text_translation::WordAlternative>, String> {
    license::require_pro(&state.db_path, "word alternatives").map_err(|error| error.to_string())?;
    let service = state.translation.clone();
    tauri::async_runtime::spawn_blocking(move || {
        text_translation::alternatives(&service, request).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn rewrite_translation(
    state: tauri::State<'_, AppState>,
    request: text_translation::ParagraphEditRequest,
) -> Result<text_translation::RewrittenParagraph, String> {
    license::require_pro(&state.db_path, "rewriting with a chosen word").map_err(|error| error.to_string())?;
    let service = state.translation.clone();
    tauri::async_runtime::spawn_blocking(move || {
        text_translation::rewrite(&service, request).map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn cancel_text_translation(request_id: Option<String>) {
    text_translation::cancel(request_id.as_deref());
}

#[tauri::command]
async fn pick_text_file(
    state: tauri::State<'_, AppState>,
    window: Window,
) -> Result<Option<text_translation::OpenedTextFile>, String> {
    license::require_pro(&state.db_path, "document translation").map_err(|error| error.to_string())?;
    text_translation::pick_text_file(&window)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn read_text_file(
    state: tauri::State<'_, AppState>,
    path: String,
) -> Result<text_translation::OpenedTextFile, String> {
    license::require_pro(&state.db_path, "document translation").map_err(|error| error.to_string())?;
    text_translation::read_text_file(std::path::Path::new(&path)).map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_translation_file(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    file_name: String,
    contents: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        text_translation::save_text_file_blocking(&app, &window, "Save translation", &file_name, &contents)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn save_text_translation(
    state: tauri::State<'_, AppState>,
    document: text_translation::SavedTextTranslation,
) -> Result<Option<text_translation::SavedTextTranslation>, String> {
    text_translation::save(&state.db_path, &document).map_err(|error| error.to_string())
}

#[tauri::command]
fn list_text_translations(
    state: tauri::State<'_, AppState>,
    query: Option<String>,
) -> Result<Vec<text_translation::TextTranslationSummary>, String> {
    text_translation::list(&state.db_path, query.as_deref().unwrap_or(""))
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_text_translation(
    state: tauri::State<'_, AppState>,
    id: String,
) -> Result<Option<text_translation::SavedTextTranslation>, String> {
    text_translation::get(&state.db_path, &id).map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_text_translation(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    text_translation::delete(&state.db_path, Some(&id)).map_err(|error| error.to_string())
}

#[tauri::command]
fn copy_text_to_clipboard(app: tauri::AppHandle, text: String) -> Result<(), String> {
    app.clipboard()
        .write_text(text)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_installed_models(state: tauri::State<'_, AppState>) -> Result<Vec<InstalledModel>, String> {
    storage::list_installed_models(state.inner()).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_live_pair_status(state: tauri::State<'_, AppState>) -> live_pair::LivePairStatus {
    state.live_pair.status()
}

#[tauri::command]
fn list_downloadable_models(state: tauri::State<'_, AppState>) -> Vec<DownloadableModel> {
    model_downloads::list_downloadable_models(Some(&state.models_dir))
}

#[tauri::command]
fn get_model_download_statuses(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<ModelDownloadStatus>, String> {
    Ok(state.model_download_manager.statuses())
}

#[tauri::command]
fn start_model_download(
    state: tauri::State<'_, AppState>,
    model_id: String,
) -> Result<ModelDownloadStatus, String> {
    state
        .model_download_manager
        .start_download(&model_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_licence_documents(app: tauri::AppHandle) -> Vec<licences::LicenceDocument> {
    licences::list_documents(&app)
}

#[tauri::command]
async fn read_licence_document(app: tauri::AppHandle, name: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || licences::read_document(&app, &name))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Keeps the key from a `murmela://activate?key=…` link until the window collects it.
fn receive_license_links(app: &tauri::AppHandle, urls: Vec<tauri::Url>) {
    let Some(key) = urls.iter().find_map(|url| license::key_from_link(url.as_str())) else {
        return;
    };
    app.state::<license::PendingLinks>().offer(key);
    let _ = app.emit("app://license-link", ());
    let window_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = desktop_shell::show_main_window(&window_app);
    });
}

/// Opens murmela.com's pricing in the default browser. Only this fixed page, never a URL from the page.
#[tauri::command]
fn open_pro_page() -> Result<(), String> {
    const PRICING: &str = "https://murmela.com/#pricing";
    open(PRICING)
}

#[tauri::command]
fn take_pending_license_link(links: tauri::State<'_, license::PendingLinks>) -> Option<String> {
    links.take()
}

#[tauri::command]
fn get_license_status(state: tauri::State<'_, AppState>) -> Result<license::LicenseStatus, String> {
    license::status(&state.db_path).map_err(|error| error.to_string())
}

#[tauri::command]
async fn activate_license(
    state: tauri::State<'_, AppState>,
    license_key: String,
) -> Result<license::LicenseStatus, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || license::activate(&db_path, &license_key))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn deactivate_license(state: tauri::State<'_, AppState>) -> Result<license::LicenseStatus, String> {
    let db_path = state.db_path.clone();
    tauri::async_runtime::spawn_blocking(move || license::deactivate(&db_path))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

/// Whether the model needs terms accepted first, and whether they were.
#[tauri::command]
fn model_terms_status(
    state: tauri::State<'_, AppState>,
    model_id: String,
) -> Result<serde_json::Value, String> {
    let Some(version) = model_downloads::required_terms_version(&model_id) else {
        return Ok(serde_json::json!({ "required": false, "accepted": true }));
    };
    let accepted = storage::model_terms_accepted(&state.db_path, &model_id, version)
        .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "required": true, "accepted": accepted, "version": version }))
}

#[tauri::command]
fn accept_model_terms(state: tauri::State<'_, AppState>, model_id: String) -> Result<(), String> {
    let version = model_downloads::required_terms_version(&model_id)
        .ok_or_else(|| format!("{model_id} has no terms to accept"))?;
    storage::accept_model_terms(&state.db_path, &model_id, version).map_err(|error| error.to_string())
}

#[tauri::command]
fn cancel_model_download(
    state: tauri::State<'_, AppState>,
    model_id: String,
) -> Result<ModelDownloadStatus, String> {
    state
        .model_download_manager
        .cancel_download(&model_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn delete_model(state: tauri::State<'_, AppState>, model_id: String) -> Result<(), String> {
    let manager = state.model_download_manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.delete_model(&model_id))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn rescan_models_folder(state: tauri::State<'_, AppState>) -> Result<Vec<InstalledModel>, String> {
    let manager = state.model_download_manager.clone();
    tauri::async_runtime::spawn_blocking(move || manager.refresh_installed())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn open_models_folder(state: tauri::State<'_, AppState>) -> Result<(), String> {
    open(&state.models_dir)
}

#[tauri::command]
fn list_input_devices() -> Result<Vec<InputDeviceOption>, String> {
    audio_capture::list_input_devices().map_err(|error| error.to_string())
}

#[tauri::command]
fn get_dictation_overlay_status(
    state: tauri::State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    serde_json::to_value(state.desktop_shell.overlay_payload()).map_err(|error| error.to_string())
}

#[tauri::command]
async fn preview_transcription(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    request: TranscriptionPreviewRequest,
) -> Result<TranscriptionPreviewResponse, String> {
    let work = shutdown::begin_work(true).map_err(|e| e.to_string())?;
    shutdown::set_manual_handoff(false);
    let app_state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        shutdown::ensure_running().map_err(|e| e.to_string())?;
        let resolved_model = resolve_model_selection(
            app_state
                .engine
                .list_models()
                .map_err(|error| error.to_string())?,
            request.selected_model_id.as_deref(),
            request.profile,
        );

        let Some(file_path) = request.file_path.clone() else {
            return Ok(TranscriptionPreviewResponse {
                dictation_output: None,
                source_kind: request.source_kind,
                resolved_model,
                result: None,
                error: Some(asr::EngineErrorPayload {
                    code: "transcription_input_missing".to_string(),
                    message: "Record audio first so the app has a normalized WAV to transcribe."
                        .to_string(),
                }),
            });
        };

        let session = if request.source_kind == asr::PreviewSourceKind::QuickDictate {
            Some(
                app_state
                    .translation
                    .manual_session(request.session_id.as_deref(), &file_path)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let _session_guard = session.as_ref().map(|s| app_state.translation.guard(s));
        let (_permit, warm_worker) = match &session {
            Some(session) => app_state.translation.acquire_for_asr(session),
            None => app_state
                .translation
                .acquire_background(
                    &uuid::Uuid::new_v4().to_string(),
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .map(|permit| (permit, None)),
        }
        .map_err(|e| e.to_string())?;
        let vocabulary_prompt = vocabulary::build_asr_prompt_from_db_path(&app_state.db_path)
            .map_err(|error| error.to_string())?;

        let engine_request = FileTranscriptionRequest {
            use_context: Some(match request.source_kind {
                asr::PreviewSourceKind::QuickDictate => {
                    model_metadata::ModelUseContext::QuickDictate
                }
                asr::PreviewSourceKind::FileUpload => {
                    model_metadata::ModelUseContext::FileTranscription
                }
            }),
            profile: request.profile,
            selected_model_id: request.selected_model_id.clone(),
            language_mode: request.language_mode,
            fixed_language: request.fixed_language.clone(),
            timestamps: request.timestamps,
            prefer_gpu: request.prefer_gpu,
            file_path: file_path.clone(),
            context_prompt: vocabulary_prompt.as_ref().map(|prompt| prompt.text.clone()),
            context_terms: vocabulary_prompt
                .as_ref()
                .map(|prompt| prompt.terms.clone())
                .unwrap_or_default(),
        };
        let result = if let Some(session) = &session {
            app_state
                .translation
                .transcribe(session, engine_request, &_permit, warm_worker)
        } else {
            app_state.engine.transcribe_file(engine_request, None)
        };

        Ok(match result {
            Ok(result) => {
                let corrected = vocabulary::correct_transcript_result(&app_state.db_path, result)
                    .map_err(|error| error.to_string())?;
                let dictation_output = session
                    .as_ref()
                    .map(|session| {
                        let duration = app_state
                            .recording_controller
                            .status()
                            .ok()
                            .and_then(|s| s.duration_ms)
                            .unwrap_or(0);
                        app_state.translation.process(session, &corrected, duration)
                    })
                    .transpose()
                    .map_err(|e| e.to_string())?;
                if let Some(session) = &session {
                    app_state
                        .translation
                        .ensure_active(session)
                        .map_err(|e| e.to_string())?;
                }
                let error = dictation_output
                    .as_ref()
                    .and_then(|o| o.error_message.as_ref())
                    .map(|message| asr::EngineErrorPayload {
                        code: "translation_failed".into(),
                        message: message.clone(),
                    });
                let _ = app_state
                    .desktop_shell
                    .set_overlay_payload(Default::default());
                if request.source_kind == asr::PreviewSourceKind::QuickDictate && error.is_none() {
                    let duration = corrected
                        .segments
                        .last()
                        .map(|segment| segment.end_ms)
                        .unwrap_or(0);
                    dictation::record_dictation_stats(&app, &app_state.db_path, &corrected.plain_text, duration);
                }
                TranscriptionPreviewResponse {
                    dictation_output,
                    source_kind: request.source_kind,
                    resolved_model,
                    result: Some(corrected),
                    error,
                }
            }
            Err(error) => TranscriptionPreviewResponse {
                dictation_output: None,
                source_kind: request.source_kind,
                resolved_model,
                result: None,
                error: Some(asr::engine_error_payload(&error)),
            },
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn pick_audio_files(window: Window) -> Result<Vec<SelectedSourceFile>, String> {
    audio_files::pick_audio_files(&window)
        .await
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn prepare_dropped_audio_files(paths: Vec<String>) -> Result<Vec<SelectedSourceFile>, String> {
    audio_files::prepare_dropped_audio_files(paths).map_err(|error| error.to_string())
}

#[tauri::command]
fn start_file_transcription(
    state: tauri::State<'_, AppState>,
    request: UploadedFileTranscriptionRequest,
) -> Result<StartFileTranscriptionResponse, String> {
    let _work = shutdown::begin_work(true).map_err(|e| e.to_string())?;
    diarization::validate_speaker_count_hint(request.speaker_count_hint).map_err(str::to_string)?;
    Ok(state.file_transcription_controller.start(request))
}

#[tauri::command]
fn get_file_transcription_statuses(
    state: tauri::State<'_, AppState>,
) -> Result<Vec<FileTranscriptionStatusEvent>, String> {
    Ok(state.file_transcription_controller.statuses())
}

#[tauri::command]
fn cancel_file_transcription(
    state: tauri::State<'_, AppState>,
    job_id: String,
) -> Result<(), String> {
    state
        .file_transcription_controller
        .cancel(&job_id)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_recording_status(
    state: tauri::State<'_, AppState>,
) -> Result<RecordingStatusResponse, String> {
    state
        .recording_controller
        .status()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn get_recording_input_level(state: tauri::State<'_, AppState>) -> Result<f32, String> {
    state
        .recording_controller
        .input_level()
        .map_err(|error| error.to_string())
}

#[tauri::command]
async fn start_recording_session(
    state: tauri::State<'_, AppState>,
    feedback: Option<bool>,
    purpose: Option<String>,
) -> Result<RecordingStatusResponse, String> {
    let work = shutdown::begin_work(true).map_err(|e| e.to_string())?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        shutdown::ensure_running().map_err(|e| e.to_string())?;
        if state
            .recording_controller
            .status()
            .map(|status| {
                matches!(
                    status.state,
                    audio_capture::RecordingOverlayState::Listening
                )
            })
            .unwrap_or(false)
        {
            return Err("A recording is already active.".to_string());
        }
        let microphone_test = purpose.as_deref() == Some("microphone_test")
            || (purpose.is_none() && feedback == Some(false));
        if purpose
            .as_deref()
            .is_some_and(|p| p != "dictation" && p != "microphone_test")
        {
            return Err("Unknown recording purpose.".into());
        }
        if microphone_test && state.translation.snapshot().busy {
            return Err("Finish the current dictation first.".into());
        }
        let session = if microphone_test {
            None
        } else {
            Some(state.translation.begin().map_err(|e| e.to_string())?)
        };
        let reservation = session.as_ref().map(|s| state.translation.guard(s));
        let enabled = storage::get_settings(&state)
            .map(|settings| settings.sounds_enabled)
            .unwrap_or(false)
            && feedback.unwrap_or(true);
        if let Some(player) = state.sound_player.as_ref().as_ref() {
            player
                .prepare_capture(enabled)
                .map_err(|error| error.to_string())?;
        }
        let result = state
            .recording_controller
            .start()
            .map_err(|error| error.to_string());
        if result.is_err() {
            if let Some(player) = state.sound_player.as_ref().as_ref() {
                player.finish_capture(false, true);
            }
        }
        if shutdown::is_shutting_down() {
            let _ = state.recording_controller.cancel();
            return Err("APP_SHUTTING_DOWN: Murmela is quitting.".into());
        }
        if let Ok(status) = &result {
            if session.is_some() {
                if let Some(id) = status.current_session_id.clone() {
                    state.translation.bind_recording(id, None);
                }
                let _ = state.desktop_shell.set_overlay_payload(
                    desktop_shell::DictationOverlayPayload {
                        phase: desktop_shell::OverlayPhase::Listening,
                        ..Default::default()
                    },
                );
                if let Some(reservation) = reservation {
                    reservation.disarm();
                }
            }
        }
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
async fn stop_recording_session(
    state: tauri::State<'_, AppState>,
) -> Result<RecordingResult, String> {
    let work = shutdown::begin_work(true).map_err(|e| e.to_string())?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let _work = work;
        shutdown::ensure_running().map_err(|e| e.to_string())?;
        let result = state
            .recording_controller
            .stop()
            .map_err(|error| error.to_string());
        shutdown::set_manual_handoff(result.is_ok());
        if let Ok(recording) = &result {
            state.translation.bind_recording(
                recording.session_id.clone(),
                Some(recording.file_path.clone()),
            );
            if let Ok(session) = state.translation.current() {
                let _ = state
                    .translation
                    .phase(&session, "handoff", "Preparing transcription…");
            }
        } else {
            state.translation.cancel();
        }
        let enabled = storage::get_settings(&state)
            .map(|settings| settings.sounds_enabled)
            .unwrap_or(false);
        if let Some(player) = state.sound_player.as_ref().as_ref() {
            player.finish_capture(enabled, result.is_err());
        }
        result
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn cancel_recording_session(
    state: tauri::State<'_, AppState>,
) -> Result<RecordingStatusResponse, String> {
    state.translation.cancel();
    shutdown::set_manual_handoff(false);
    let _ = state.desktop_shell.set_overlay_payload(Default::default());
    let result = state
        .recording_controller
        .cancel()
        .map_err(|error| error.to_string());
    if result.is_ok() {
        if let Some(player) = state.sound_player.as_ref().as_ref() {
            player.finish_capture(false, true);
        }
    }
    result
}

#[tauri::command]
fn preview_feedback_sound(
    state: tauri::State<'_, AppState>,
    cue: sound::FeedbackCue,
) -> Result<(), String> {
    if !storage::get_settings(state.inner())
        .map_err(|error| error.to_string())?
        .sounds_enabled
    {
        return Err("Enable feedback sounds first.".into());
    }
    let player = state
        .sound_player
        .as_ref()
        .as_ref()
        .ok_or("Sound output unavailable.")?;
    player.preview(cue).map_err(|error| error.to_string())
}

#[tauri::command]
fn report_manual_feedback(app: tauri::AppHandle, operation_id: String, failed: bool) {
    sound::notify(
        &app,
        if failed {
            sound::FeedbackCue::Error
        } else {
            sound::FeedbackCue::Complete
        },
        &format!("manual:{operation_id}"),
    );
}

#[tauri::command]
fn get_quick_dictate_status(
    state: tauri::State<'_, AppState>,
) -> Result<QuickDictationStatusResponse, String> {
    Ok(state.dictation_controller.status())
}

#[tauri::command]
fn reset_quick_dictation(
    state: tauri::State<'_, AppState>,
) -> Result<QuickDictationStatusResponse, String> {
    shutdown::set_manual_handoff(false);
    state
        .dictation_controller
        .force_reset()
        .map_err(|error| error.to_string())
}

/// Snapshot of everything that must be in place for shortcut dictation to work
/// end-to-end. Drives the Home readiness checklist so silent prerequisites
/// (no model, unbound shortcut, missing Accessibility) become visible.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct DictationReadiness {
    has_model: bool,
    // The saved shortcut model is installed and can run shortcut dictation.
    shortcut_model_ready: bool,
    microphone: permissions::MicrophonePermission,
    first_run_completed: bool,
    translation_ready: bool,
    shortcut_registered: bool,
    auto_paste_enabled: bool,
    // Auto-paste synthesises ⌘V, which macOS gates behind Accessibility.
    accessibility_required: bool,
    accessibility_granted: bool,
}

#[tauri::command]
fn get_dictation_readiness(
    state: tauri::State<'_, AppState>,
) -> Result<DictationReadiness, String> {
    let settings = storage::get_settings(state.inner()).map_err(|error| error.to_string())?;
    let models = state
        .engine
        .list_models()
        .map_err(|error| error.to_string())?;
    let mut status = state.dictation_controller.status();
    // The 🌐 / Right ⌥ listener needs Accessibility; once granted, start it.
    if !status.is_registered
        && key_gesture::is_key_gesture(&settings.shortcut)
        && insertion::accessibility_trusted()
    {
        if let Ok(updated) = state.dictation_controller.sync_shortcut_registration() {
            status = updated;
        }
    }
    let auto_paste = matches!(settings.insert_behavior, InsertBehavior::Paste);
    Ok(DictationReadiness {
        translation_ready: state.translation.snapshot().output_mode
            == translation::OutputMode::Original
            || state.translation.ready().is_ok(),
        has_model: !models.is_empty(),
        shortcut_model_ready: storage::selection_usable(
            &models,
            settings.shortcut_dictation_selected_model_id.as_deref(),
            model_metadata::ModelUseContext::ShortcutDictation,
        ),
        microphone: permissions::microphone_permission(),
        first_run_completed: storage::first_run_completed(state.inner())
            .map_err(|error| error.to_string())?,
        shortcut_registered: status.is_registered,
        auto_paste_enabled: auto_paste,
        accessibility_required: auto_paste,
        accessibility_granted: insertion::accessibility_trusted(),
    })
}

#[tauri::command]
fn open_accessibility_settings() {
    insertion::open_accessibility_settings();
}

#[tauri::command]
fn request_accessibility_access() {
    insertion::request_accessibility_access();
}

#[tauri::command]
fn get_update_status(updates: tauri::State<'_, updates::Updates>) -> updates::UpdateStatus {
    updates.status()
}

#[tauri::command]
async fn check_for_updates(app: tauri::AppHandle) -> Result<updates::UpdateStatus, String> {
    updates::check(&app).await.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
async fn download_update_as_free(app: tauri::AppHandle) -> Result<updates::UpdateStatus, String> {
    updates::download_as_free(&app).await.map_err(|error| format!("{error:#}"))
}

#[tauri::command]
fn install_update_and_restart(app: tauri::AppHandle) -> Result<(), String> {
    updates::install_and_restart(&app).map_err(|error| format!("{error:#}"))
}

/// What macOS does on a tap of the 🌐 key ("Press 🌐 key to").
#[tauri::command]
fn get_globe_key_action() -> key_gesture::GlobeKeyAction {
    key_gesture::globe_key_action()
}

#[tauri::command]
fn open_keyboard_settings() {
    let _ = open("x-apple.systempreferences:com.apple.Keyboard-Settings.extension");
}

#[tauri::command]
fn get_dictation_stats(state: tauri::State<'_, AppState>) -> Result<storage::DictationStats, String> {
    storage::dictation_stats(&state.db_path).map_err(|error| error.to_string())
}

#[tauri::command]
fn reset_dictation_stats(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<storage::DictationStats, String> {
    storage::reset_dictation_stats(&state.db_path).map_err(|error| error.to_string())?;
    let stats = storage::dictation_stats(&state.db_path).map_err(|error| error.to_string())?;
    let _ = app.emit(dictation::DICTATION_STATS_EVENT, &stats);
    Ok(stats)
}

#[tauri::command]
fn get_call_capture_support() -> meeting::sources::SystemAudioSupport {
    meeting::sources::system_audio_support()
}

#[tauri::command]
fn get_call_status(state: tauri::State<'_, AppState>) -> meeting::CallStatus {
    state.meetings.status()
}

#[tauri::command]
async fn start_call_recording(
    state: tauri::State<'_, AppState>,
    mode: meeting::CallMode,
    title: Option<String>,
) -> Result<meeting::CallStatus, String> {
    if permissions::microphone_permission() == permissions::MicrophonePermission::Denied {
        return Err("MICROPHONE_DENIED: Murmela may not use the microphone. Allow it in System Settings → Privacy & Security → Microphone.".into());
    }
    let preferred = storage::get_settings(state.inner())
        .map_err(|error| error.to_string())?
        .preferred_input_device;
    let meetings = state.meetings.clone();
    tauri::async_runtime::spawn_blocking(move || meetings.start(mode, title, preferred))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))
}

/// What happens to a call after it stops: it is queued like a dropped file.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallTranscriptionStarted {
    job_id: String,
    source_file: audio_files::SelectedSourceFile,
}

fn transcribe_call_recording(
    state: &AppState,
    source_file: audio_files::SelectedSourceFile,
    speaker_count_hint: Option<i32>,
) -> Result<CallTranscriptionStarted, String> {
    diarization::validate_speaker_count_hint(speaker_count_hint).map_err(str::to_string)?;
    let job_id = uuid::Uuid::new_v4().to_string();
    state
        .file_transcription_controller
        .start(UploadedFileTranscriptionRequest {
            job_id: job_id.clone(),
            source_file: source_file.clone(),
            speaker_count_hint,
        });
    Ok(CallTranscriptionStarted { job_id, source_file })
}

#[tauri::command]
async fn stop_call_recording(
    state: tauri::State<'_, AppState>,
    speaker_count_hint: Option<i32>,
) -> Result<CallTranscriptionStarted, String> {
    let meetings = state.meetings.clone();
    let finished = tauri::async_runtime::spawn_blocking(move || meetings.stop())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| format!("{error:#}"))?;
    transcribe_call_recording(state.inner(), finished.source_file, speaker_count_hint)
}

#[tauri::command]
fn discard_call_recording(state: tauri::State<'_, AppState>) -> Result<(), String> {
    state.meetings.discard().map_err(|error| format!("{error:#}"))
}

/// Recordings not transcribed yet, without those already queued or running.
#[tauri::command]
fn list_pending_call_recordings(state: tauri::State<'_, AppState>) -> Vec<meeting::PendingRecording> {
    let busy: Vec<String> = state
        .file_transcription_controller
        .statuses()
        .into_iter()
        .filter(|job| {
            !matches!(
                job.stage,
                file_jobs::FileTranscriptionJobStage::Failed
                    | file_jobs::FileTranscriptionJobStage::Canceled
            )
        })
        .map(|job| job.source_file.file_path)
        .collect();
    let meetings_dir = state.meetings.meetings_dir();
    meeting::pending_recordings(meetings_dir)
        .into_iter()
        .filter(|recording| {
            let mix = meetings_dir.join(&recording.id).join(meeting::MIX_FILE);
            !busy.iter().any(|path| std::path::Path::new(path) == mix)
        })
        .collect()
}

#[tauri::command]
fn transcribe_pending_call_recording(
    state: tauri::State<'_, AppState>,
    id: String,
    speaker_count_hint: Option<i32>,
) -> Result<CallTranscriptionStarted, String> {
    let source = meeting::pending_source(state.meetings.meetings_dir(), &id)
        .map_err(|error| format!("{error:#}"))?;
    transcribe_call_recording(state.inner(), source, speaker_count_hint)
}

#[tauri::command]
fn delete_pending_call_recording(state: tauri::State<'_, AppState>, id: String) -> Result<(), String> {
    meeting::delete_recording(state.meetings.meetings_dir(), &id).map_err(|error| format!("{error:#}"))
}

/// Shows the browser helper's folder, to load it as an unpacked extension.
#[tauri::command]
fn reveal_browser_helper(app: tauri::AppHandle) -> Result<String, String> {
    let resource = app
        .path()
        .resource_dir()
        .map(|dir| dir.join("browser-helper"))
        .ok()
        .filter(|dir| dir.join("manifest.json").is_file());
    let folder = resource
        .or_else(|| {
            cfg!(debug_assertions)
                .then(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../browser-helper"))
        })
        .filter(|dir| dir.join("manifest.json").is_file())
        .ok_or_else(|| "The browser helper is not included in this build.".to_string())?;
    open(&folder)?;
    Ok(folder.display().to_string())
}

/// The places Murmela can contact and every connection it made, for Settings → Network activity.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct NetworkActivity {
    destinations: Vec<network_activity::DestinationInfo>,
    entries: Vec<network_activity::NetworkActivityEntry>,
}

#[tauri::command]
fn get_network_activity(state: tauri::State<'_, AppState>) -> Result<NetworkActivity, String> {
    Ok(NetworkActivity {
        destinations: network_activity::destinations(),
        entries: network_activity::list(&state.db_path).map_err(|error| error.to_string())?,
    })
}

/// Saves the network activity log, written as text by the Settings card.
#[tauri::command]
async fn export_network_activity(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    file_name: String,
    contents: String,
) -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        text_translation::save_text_file_blocking(&app, &window, "Export network activity", &file_name, &contents)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
fn clear_network_activity(state: tauri::State<'_, AppState>) -> Result<(), String> {
    network_activity::clear(&state.db_path).map_err(|error| error.to_string())
}

#[tauri::command]
fn request_microphone_access() -> permissions::MicrophonePermission {
    permissions::request_microphone_permission()
}

#[tauri::command]
fn open_microphone_settings() {
    permissions::open_microphone_settings();
}

#[tauri::command]
fn complete_first_run(state: tauri::State<'_, AppState>) -> Result<(), String> {
    storage::mark_first_run_completed(state.inner()).map_err(|error| error.to_string())
}

/// Free space on the volume holding the models folder, for download planning.
#[tauri::command]
fn get_models_free_space(state: tauri::State<'_, AppState>) -> Option<u64> {
    fs2::available_space(&state.models_dir).ok()
}

#[tauri::command]
fn suspend_shortcut_capture(
    state: tauri::State<'_, AppState>,
) -> Result<QuickDictationStatusResponse, String> {
    state
        .dictation_controller
        .suspend_shortcut_registration()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn resume_shortcut_capture(
    state: tauri::State<'_, AppState>,
) -> Result<QuickDictationStatusResponse, String> {
    state
        .dictation_controller
        .resume_shortcut_registration()
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn list_vocabulary_terms(state: tauri::State<'_, AppState>) -> Result<Vec<VocabularyTerm>, String> {
    vocabulary::list_vocabulary_terms(state.inner()).map_err(|error| error.to_string())
}

#[tauri::command]
fn create_vocabulary_term(
    state: tauri::State<'_, AppState>,
    input: CreateVocabularyTermInput,
) -> Result<VocabularyTerm, String> {
    vocabulary::create_vocabulary_term(state.inner(), input).map_err(|error| error.to_string())
}

#[tauri::command]
fn update_vocabulary_term(
    state: tauri::State<'_, AppState>,
    term_id: String,
    input: UpdateVocabularyTermInput,
) -> Result<VocabularyTerm, String> {
    vocabulary::update_vocabulary_term(state.inner(), &term_id, input)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn delete_vocabulary_term(
    state: tauri::State<'_, AppState>,
    term_id: String,
) -> Result<(), String> {
    vocabulary::delete_vocabulary_term(state.inner(), &term_id).map_err(|error| error.to_string())
}

fn resolve_model_selection(
    models: Vec<InstalledModel>,
    selected_model_id: Option<&str>,
    profile: settings::ModelProfile,
) -> Option<InstalledModel> {
    if let Some(model_id) = selected_model_id {
        if let Some(model) = models.iter().find(|model| model.id == model_id) {
            return Some(model.clone());
        }
    }

    models
        .iter()
        .find(|model| model.profile == profile && model.is_default)
        .or_else(|| models.iter().find(|model| model.profile == profile))
        .cloned()
}

fn main() {
    if std::env::args().any(|arg| arg == transcription_worker::PERSISTENT_WORKER_ARG) {
        std::process::exit(transcription_worker::run_persistent_stdio_worker());
    }
    if std::env::args().any(|arg| arg == transcription_worker::WORKER_ARG) {
        std::process::exit(transcription_worker::run_stdio_worker());
    }
    if std::env::args().any(|arg| arg == diarization_worker::WORKER_ARG) {
        std::process::exit(diarization_worker::run_stdio_worker());
    }


    let context = tauri::generate_context!();

    tauri::Builder::default()
        .manage(StartupCoordinator::new())
        .manage(updates::Updates::default())
        // Keep this first: duplicate launches must exit before any app services start.
        .plugin(single_instance::init())
        .plugin(tauri_plugin_deep_link::init())
        .manage(license::PendingLinks::default())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .setup(|app| {
            shutdown::install_macos_termination_handler(app.handle())?;
            {
                use tauri_plugin_deep_link::DeepLinkExt;
                // A link that launched the app arrives here; later ones through on_open_url.
                if let Ok(Some(urls)) = app.deep_link().get_current() {
                    receive_license_links(app.handle(), urls);
                }
                let link_app = app.handle().clone();
                app.deep_link()
                    .on_open_url(move |event| receive_license_links(&link_app, event.urls()));
            }
            let startup_work = shutdown::begin_work(false)?;
            let app_handle = app.handle().clone();
            let startup = app.state::<StartupCoordinator>().inner().clone();
            tauri::async_runtime::spawn_blocking(move || {
                let _startup_work = startup_work;
                let progress_app = app_handle.clone();
                let progress_startup = startup.clone();
                match AppState::initialize(&app_handle, move |phase| {
                    progress_startup.advance(&progress_app, phase);
                }) {
                    Ok(app_state) => {

                        app_state.dictation_controller.refresh_tray_recents();
                        if app_handle.manage(app_state) {
                            updates::start(&app_handle);
                            startup.advance(&app_handle, StartupPhase::Workspace);
                        } else {
                            startup.fail(&app_handle, "Application state was already initialized.");
                        }
                    }
                    Err(error) => startup.fail(&app_handle, format!("{error:#}")),
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }

            match event {
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    // Closing the window keeps Murmela running in the menu bar.
                    let _ = window.hide();
                }
                WindowEvent::DragDrop(drag_event) => {
                    let app = window.app_handle();
                    match drag_event {
                        DragDropEvent::Enter { paths, .. } => {
                            let _ = app.emit("app://file-drag-enter", paths);
                        }
                        DragDropEvent::Over { .. } => {}
                        DragDropEvent::Drop { paths, .. } => {
                            let _ = app.emit("app://file-drop", paths);
                        }
                        DragDropEvent::Leave => {
                            let _ = app.emit("app://file-drag-leave", ());
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        })
        .on_webview_event(|webview, event| {
            if let WebviewEvent::DragDrop(drag_event) = event {
                let app = webview.app_handle();
                match drag_event {
                    DragDropEvent::Enter { paths, .. } => {
                        let _ = app.emit("app://file-drag-enter", paths);
                    }
                    DragDropEvent::Over { .. } => {}
                    DragDropEvent::Drop { paths, .. } => {
                        let _ = app.emit("app://file-drop", paths);
                    }
                    DragDropEvent::Leave => {
                        let _ = app.emit("app://file-drag-leave", ());
                    }
                    _ => {}
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            health_check,
            quit_app,
            is_app_shutting_down,
            restart_app,
            get_startup_status,
            frontend_startup_complete,
            report_startup_failure,
            complete_startup_handoff,
            get_dictation_output_state,
            set_dictation_output_mode,
            retry_dictation_translation,
            retry_streaming_dictation,
            get_settings,
            update_settings,
            list_transcripts,
            delete_transcript,
            delete_all_transcripts,
            get_transcript,
            get_review,
            edit_review,
            start_review_job,
            get_review_job_statuses,
            cancel_review_job,
            resolve_review_audio,
            release_review_audio,
            copy_review,
            export_review,
            get_file_transcription_result,
            dismiss_file_transcription,
            rename_transcript,
            get_translator_status,
            detect_text_language,
            translate_text,
            translation_alternatives,
            rewrite_translation,
            cancel_text_translation,
            pick_text_file,
            read_text_file,
            save_translation_file,
            save_text_translation,
            list_text_translations,
            get_text_translation,
            delete_text_translation,
            copy_text_to_clipboard,
            list_installed_models,
            list_downloadable_models,
            get_live_pair_status,
            get_model_download_statuses,
            start_model_download,
            model_terms_status,
            accept_model_terms,
            get_license_status,
            take_pending_license_link,
            open_pro_page,
            activate_license,
            deactivate_license,
            list_licence_documents,
            read_licence_document,
            cancel_model_download,
            list_input_devices,
            open_models_folder,
            delete_model,
            rescan_models_folder,
            get_dictation_overlay_status,
            preview_transcription,
            pick_audio_files,
            prepare_dropped_audio_files,
            start_file_transcription,
            get_file_transcription_statuses,
            cancel_file_transcription,
            get_recording_status,
            get_recording_input_level,
            start_recording_session,
            preview_feedback_sound,
            report_manual_feedback,
            stop_recording_session,
            cancel_recording_session,
            get_quick_dictate_status,
            reset_quick_dictation,
            get_dictation_readiness,
            open_accessibility_settings,
            request_accessibility_access,
            get_update_status,
            check_for_updates,
            install_update_and_restart,
            download_update_as_free,
            get_globe_key_action,
            open_keyboard_settings,
            request_microphone_access,
            open_microphone_settings,
            get_dictation_stats,
            reset_dictation_stats,
            get_network_activity,
            clear_network_activity,
            export_network_activity,
            get_call_capture_support,
            get_call_status,
            start_call_recording,
            stop_call_recording,
            discard_call_recording,
            list_pending_call_recordings,
            transcribe_pending_call_recording,
            delete_pending_call_recording,
            reveal_browser_helper,
            complete_first_run,
            get_models_free_space,
            suspend_shortcut_capture,
            resume_shortcut_capture,
            list_vocabulary_terms,
            create_vocabulary_term,
            update_vocabulary_term,
            delete_vocabulary_term
        ])
        .build(context)
        .expect("error while building tauri application")
        .run(|_app, _event| {
            if let tauri::RunEvent::ExitRequested { ref api, .. } = _event {
                if !shutdown::ready_to_exit() {
                    api.prevent_exit();
                    shutdown::request_exit(_app, shutdown::ExitAction::Quit);
                }
            }
            if let tauri::RunEvent::Exit = _event {
                updates::install_pending_on_exit(_app);
            }
            if let tauri::RunEvent::Reopen { .. } = _event {
                // A visible dictation overlay does not mean the workspace is
                // open. Always restore it when macOS requests a Dock reopen.
                if let Err(error) = desktop_shell::show_main_window(_app) {
                    eprintln!("[desktop] could not reopen Murmela from the Dock: {error:#}");
                }
            }
        });
}

/// Opens a file, folder, URL or System Settings pane with the default app.
fn open(target: impl AsRef<std::ffi::OsStr>) -> Result<(), String> {
    Command::new("open")
        .arg(target)
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}
