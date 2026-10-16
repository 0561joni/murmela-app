use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::app_state::AppState;
use crate::asr::{
    InstalledModel, TranscriptQualityStatus, TranscriptResult, TranscriptSegment, TranscriptWarning,
};
use crate::audio_files::SelectedSourceFile;
use crate::model_metadata::ModelUseContext;
use crate::diarization::{
    DiarizationSource, DiarizationStatus, DiarizationTurn, TranscriptSpeaker,
};
use crate::settings::{
    AppSettings, Appearance, DefaultMode, InsertBehavior, LanguageMode, ModelProfile,
    MotionPreference, SettingsPatch, ShortcutMode,
};
use crate::speaker_reconciliation::SpeakerAttribution;

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    QuickDictate,
    FileUpload,
    /// A recorded call or in-person meeting.
    Meeting,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptStatus {
    Queued,
    Recording,
    Processing,
    Completed,
    Failed,
    Canceled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSummary {
    pub id: String,
    pub created_at: String,
    pub source_type: SourceType,
    pub title: String,
    pub plain_text: String,
    pub status: TranscriptStatus,
    pub detected_languages: Vec<String>,
    pub duration_ms: Option<i64>,
    pub model_name: Option<String>,
    pub quality_status: TranscriptQualityStatus,
    pub recovered_region_count: i32,
    pub diarization_status: DiarizationStatus,
    pub speaker_count: Option<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptDetail {
    #[serde(default)]
    pub translation: Option<crate::translation::DictationOutput>,
    #[serde(flatten)]
    pub summary: TranscriptSummary,
    pub full_text: String,
    pub timestamped_text: String,
    pub transcription_warnings: Vec<TranscriptWarning>,
    pub diarization_model_id: Option<String>,
    pub diarization_source: DiarizationSource,
    pub diarization_warning: Option<String>,
    pub diarization_policy_version: Option<u32>,
    pub diarization_clustering_threshold: Option<f32>,
    pub diarization_speaker_count_hint: Option<i32>,
    pub segments: Vec<TranscriptSegment>,
    pub speakers: Vec<TranscriptSpeaker>,
    pub diarization_turns: Vec<DiarizationTurn>,
    #[serde(default)]
    pub manual_segment_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct FileTranscriptionPerformance {
    pub avg_audio_ms_per_wall_ms: f64,
    pub sample_count: i64,
}

pub fn initialize_database(db_path: &Path) -> Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let connection = open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    crate::review::ensure_schema(&connection)?;
    crate::license::ensure_schema(&connection)?;
    crate::network_activity::ensure_schema(&connection)?;
    crate::text_translation::ensure_schema(&connection)?;
    recover_interrupted_translations(&connection)?;
    seed_default_settings(&connection)?;
    Ok(())
}

/// Creates the tables in `schema.sql` that don't exist yet.
pub(crate) fn ensure_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(SCHEMA)
        .context("failed to create the database tables")
}

/// True when the user accepted exactly this version of the model's terms.
pub fn model_terms_accepted(db_path: &Path, model_id: &str, version: &str) -> Result<bool> {
    let connection = open_connection_by_path(db_path)?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM model_terms_acceptances WHERE model_id=?1 AND terms_version=?2)",
        params![model_id, version],
        |row| row.get::<_, bool>(0),
    )?)
}

pub fn accept_model_terms(db_path: &Path, model_id: &str, version: &str) -> Result<()> {
    let connection = open_connection_by_path(db_path)?;
    connection.execute(
        "INSERT OR REPLACE INTO model_terms_acceptances (model_id, terms_version, accepted_at) VALUES (?1, ?2, ?3)",
        params![model_id, version, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

const FIRST_RUN_SETUP_KEY: &str = "first_run_setup_v1";

/// Whether the user finished or skipped the first-run setup. Stored as an
/// app milestone so it never resets with settings.
pub fn first_run_completed(state: &AppState) -> Result<bool> {
    let connection = open_connection(state)?;
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM app_migrations WHERE migration_key=?1)",
        [FIRST_RUN_SETUP_KEY],
        |row| row.get::<_, bool>(0),
    )?)
}

pub fn mark_first_run_completed(state: &AppState) -> Result<()> {
    let connection = open_connection(state)?;
    connection.execute(
        "INSERT OR IGNORE INTO app_migrations (migration_key, completed_at) VALUES (?1, ?2)",
        params![FIRST_RUN_SETUP_KEY, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

pub fn sync_installed_models(state: &AppState, models: &[InstalledModel]) -> Result<()> {
    sync_installed_models_for_db_path(&state.db_path, models)
}

pub fn sync_installed_models_for_db_path(db_path: &Path, models: &[InstalledModel]) -> Result<()> {
    let mut connection = open_connection_by_path(db_path)?;
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM installed_models", [])?;
    for model in models {
        transaction.execute(
            "INSERT INTO installed_models (id, engine, model_name, variant, local_path, size_bytes, is_default, profile)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                &model.id,
                &model.engine,
                &model.model_name,
                &model.variant,
                &model.local_path,
                model.size_bytes,
                model.is_default,
                to_model_profile(model.profile),
            ],
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub fn apply_preferred_model_defaults(state: &AppState, models: &[InstalledModel]) -> Result<()> {
    apply_preferred_model_defaults_for_db_path(&state.db_path, models)
}

pub fn apply_preferred_model_defaults_for_db_path(
    db_path: &Path,
    models: &[InstalledModel],
) -> Result<()> {
    let current = get_settings_from_db_path(db_path)?;
    let mut patch = SettingsPatch::default();
    let mut should_update = false;

    if !selection_usable(
        models,
        current.shortcut_dictation_selected_model_id.as_deref(),
        ModelUseContext::ShortcutDictation,
    ) {
        if let Some(model) = default_model_for(models, ModelUseContext::ShortcutDictation) {
            patch.shortcut_dictation_model_profile = Some(model.profile);
            patch.shortcut_dictation_selected_model_id = Some(Some(model.id.clone()));
            should_update = true;
        }
    }

    if !selection_usable(
        models,
        current.quick_dictate_selected_model_id.as_deref(),
        ModelUseContext::QuickDictate,
    ) {
        if let Some(model) = default_model_for(models, ModelUseContext::QuickDictate) {
            patch.quick_dictate_model_profile = Some(model.profile);
            patch.quick_dictate_selected_model_id = Some(Some(model.id.clone()));
            should_update = true;
        }
    }

    if !selection_usable(
        models,
        current.file_transcribe_selected_model_id.as_deref(),
        ModelUseContext::FileTranscription,
    ) {
        if let Some(model) = default_model_for(models, ModelUseContext::FileTranscription) {
            patch.file_transcribe_model_profile = Some(model.profile);
            patch.file_transcribe_selected_model_id = Some(Some(model.id.clone()));
            should_update = true;
        }
    }

    if should_update {
        let _ = update_settings_for_db_path(db_path, patch)?;
    }

    Ok(())
}

pub fn update_settings_for_db_path(db_path: &Path, patch: SettingsPatch) -> Result<AppSettings> {
    let current = get_settings_from_db_path(db_path)?;
    let next = AppSettings {
        default_mode: patch.default_mode.unwrap_or(current.default_mode),
        shortcut: patch.shortcut.unwrap_or(current.shortcut),
        translation_enabled: patch
            .translation_enabled
            .unwrap_or(current.translation_enabled),
        translation_cycle_shortcut: patch
            .translation_cycle_shortcut
            .unwrap_or(current.translation_cycle_shortcut),
        translation_model_id: patch
            .translation_model_id
            .unwrap_or(current.translation_model_id),
        shortcut_mode: patch.shortcut_mode.unwrap_or(current.shortcut_mode),
        language_mode: patch.language_mode.unwrap_or(current.language_mode),
        fixed_language: patch.fixed_language.unwrap_or(current.fixed_language),
        preferred_input_device: patch
            .preferred_input_device
            .unwrap_or(current.preferred_input_device),
        insert_behavior: patch.insert_behavior.unwrap_or(current.insert_behavior),
        launch_at_login_enabled: patch
            .launch_at_login_enabled
            .unwrap_or(current.launch_at_login_enabled),
        gpu_enabled: patch.gpu_enabled.unwrap_or(current.gpu_enabled),
        shortcut_dictation_model_profile: patch
            .shortcut_dictation_model_profile
            .unwrap_or(current.shortcut_dictation_model_profile),
        shortcut_dictation_selected_model_id: patch
            .shortcut_dictation_selected_model_id
            .unwrap_or(current.shortcut_dictation_selected_model_id),
        quick_dictate_model_profile: patch
            .quick_dictate_model_profile
            .unwrap_or(current.quick_dictate_model_profile),
        quick_dictate_selected_model_id: patch
            .quick_dictate_selected_model_id
            .unwrap_or(current.quick_dictate_selected_model_id),
        file_transcribe_model_profile: patch
            .file_transcribe_model_profile
            .unwrap_or(current.file_transcribe_model_profile),
        file_transcribe_selected_model_id: patch
            .file_transcribe_selected_model_id
            .unwrap_or(current.file_transcribe_selected_model_id),
        appearance: patch.appearance.unwrap_or(current.appearance),
        motion_preference: patch.motion_preference.unwrap_or(current.motion_preference),
        save_history: patch.save_history.unwrap_or(current.save_history),
        sounds_enabled: patch.sounds_enabled.unwrap_or(current.sounds_enabled),
        volume_ducking_enabled: patch
            .volume_ducking_enabled
            .unwrap_or(current.volume_ducking_enabled),
        file_diarization_enabled: patch
            .file_diarization_enabled
            .unwrap_or(current.file_diarization_enabled),
        live_pair_chunk_ms: patch
            .live_pair_chunk_ms
            .unwrap_or(current.live_pair_chunk_ms),
        live_pair_keep_loaded: patch
            .live_pair_keep_loaded
            .unwrap_or(current.live_pair_keep_loaded),
        paste_last_shortcut: patch
            .paste_last_shortcut
            .unwrap_or(current.paste_last_shortcut),
        serious_mode: patch.serious_mode.unwrap_or(current.serious_mode),
        auto_update_enabled: patch
            .auto_update_enabled
            .unwrap_or(current.auto_update_enabled),
    };
    if !crate::settings::LIVE_PAIR_CHUNKS_MS.contains(&next.live_pair_chunk_ms) {
        bail!("Choose a live preview chunk of 560 or 1120 ms.");
    }
    let connection = open_connection_by_path(db_path)?;
    connection.execute(
        "UPDATE settings SET default_mode = ?1, shortcut = ?2, shortcut_mode = ?3, language_mode = ?4, fixed_language = ?5, preferred_input_device = ?6, insert_behavior = ?7, launch_at_login_enabled = ?8, metal_enabled = ?9, shortcut_dictation_model_profile = ?10, shortcut_dictation_selected_model_id = ?11, quick_dictate_model_profile = ?12, quick_dictate_selected_model_id = ?13, file_transcribe_model_profile = ?14, file_transcribe_selected_model_id = ?15, save_history = ?16, sounds_enabled = ?17, volume_ducking_enabled = ?18, file_diarization_enabled = ?19, appearance = ?20, motion_preference = ?21, translation_enabled = ?22, translation_cycle_shortcut = ?23, translation_model_id = ?24, live_pair_chunk_ms = ?25, live_pair_keep_loaded = ?26, paste_last_shortcut = ?27, serious_mode = ?28, auto_update_enabled = ?29 WHERE id = 1",
        params![
            to_default_mode(next.default_mode),
            next.shortcut,
            to_shortcut_mode(next.shortcut_mode),
            to_language_mode(next.language_mode),
            next.fixed_language,
            next.preferred_input_device,
            to_insert_behavior(next.insert_behavior),
            next.launch_at_login_enabled,
            next.gpu_enabled,
            to_model_profile(next.shortcut_dictation_model_profile),
            next.shortcut_dictation_selected_model_id,
            to_model_profile(next.quick_dictate_model_profile),
            next.quick_dictate_selected_model_id,
            to_model_profile(next.file_transcribe_model_profile),
            next.file_transcribe_selected_model_id,
            next.save_history,
            next.sounds_enabled,
            next.volume_ducking_enabled,
            next.file_diarization_enabled,
            match next.appearance { Appearance::System => "system", Appearance::Light => "light", Appearance::Dark => "dark" },
            match next.motion_preference { MotionPreference::System => "system", MotionPreference::Reduced => "reduced" },
            next.translation_enabled, next.translation_cycle_shortcut, next.translation_model_id,
            next.live_pair_chunk_ms, next.live_pair_keep_loaded, next.paste_last_shortcut,
            next.serious_mode,
            next.auto_update_enabled,
        ],
    )?;
    get_settings_from_db_path(db_path)
}

pub fn list_installed_models(state: &AppState) -> Result<Vec<InstalledModel>> {
    list_installed_models_for_db_path(&state.db_path)
}

pub(crate) fn list_installed_models_for_db_path(db_path: &Path) -> Result<Vec<InstalledModel>> {
    let connection = open_connection_by_path(db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, engine, model_name, variant, local_path, size_bytes, is_default, profile
         FROM installed_models ORDER BY model_name ASC",
    )?;
    let rows = statement.query_map([], map_installed_model_row)?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read installed models")
}

pub fn get_settings(state: &AppState) -> Result<AppSettings> {
    let connection = open_connection(state)?;
    let settings = query_settings(&connection)?;
    Ok(settings)
}

pub fn get_settings_from_db_path(db_path: &Path) -> Result<AppSettings> {
    let connection = open_connection_by_path(db_path)?;
    let settings = query_settings(&connection)?;
    Ok(settings)
}

pub fn save_quick_dictation_transcript(
    db_path: &Path,
    result: &TranscriptResult,
    duration_ms: i64,
) -> Result<TranscriptSummary> {
    let connection = open_connection_by_path(db_path)?;
    let transaction = connection.unchecked_transaction()?;
    let transcript_id = Uuid::new_v4().to_string();
    insert_transcript(
        &transaction,
        &transcript_id,
        SourceType::QuickDictate,
        build_transcript_title(&result.plain_text),
        result,
        Some(duration_ms),
    )?;
    transaction.commit()?;
    fetch_transcript_summary(&connection, &transcript_id)
}

/// Commit the source and its pending translation together, before inference.
pub fn save_translated_dictation_source(
    db_path: &Path,
    result: &TranscriptResult,
    duration_ms: i64,
    output: &mut crate::translation::DictationOutput,
) -> Result<()> {
    let connection = open_connection_by_path(db_path)?;
    let transaction = connection.unchecked_transaction()?;
    let id = Uuid::new_v4().to_string();
    insert_transcript(
        &transaction,
        &id,
        SourceType::QuickDictate,
        build_transcript_title(&result.plain_text),
        result,
        Some(duration_ms),
    )?;
    let mut saved = output.clone();
    saved.transcript_id = Some(id.clone());
    transaction.execute("INSERT INTO transcript_translations (transcript_id, output_text, payload) VALUES (?1, NULL, ?2)",
        params![id, serde_json::to_string(&saved)?])?;
    transaction.commit()?;
    *output = saved;
    Ok(())
}

#[cfg(test)]
pub fn save_file_transcription(
    db_path: &Path,
    source_file: &SelectedSourceFile,
    result: &TranscriptResult,
) -> Result<TranscriptSummary> {
    save_file_transcription_as(db_path, source_file, result, SourceType::FileUpload)
}

/// Saves a transcript of an audio file: an uploaded file or a recorded meeting.
pub fn save_file_transcription_as(
    db_path: &Path,
    source_file: &SelectedSourceFile,
    result: &TranscriptResult,
    source_type: SourceType,
) -> Result<TranscriptSummary> {
    let connection = open_connection_by_path(db_path)?;
    let transaction = connection.unchecked_transaction()?;
    let transcript_id = Uuid::new_v4().to_string();

    insert_transcript(
        &transaction,
        &transcript_id,
        source_type,
        crate::output_format::file_display_title(&source_file.original_name),
        result,
        source_file.duration_ms,
    )?;

    transaction.execute(
        "INSERT INTO source_files (id, transcript_id, original_name, mime_type, local_path, duration_ms, size_bytes, sha256)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            Uuid::new_v4().to_string(),
            &transcript_id,
            &source_file.original_name,
            &source_file.mime_type,
            &source_file.file_path,
            source_file.duration_ms,
            source_file.size_bytes,
            source_file.sha256.as_deref(),
        ],
    )?;

    transaction.commit()?;
    fetch_transcript_summary(&connection, &transcript_id)
}

pub fn get_file_transcription_performance(
    db_path: &Path,
    model_id: &str,
) -> Result<Option<FileTranscriptionPerformance>> {
    let connection = open_connection_by_path(db_path)?;
    let mut statement = connection.prepare(
        "SELECT model_id, avg_audio_ms_per_wall_ms, sample_count
         FROM file_transcription_model_performance
         WHERE model_id = ?1",
    )?;
    let mut rows = statement.query([model_id])?;
    if let Some(row) = rows.next()? {
        return Ok(Some(FileTranscriptionPerformance {
            avg_audio_ms_per_wall_ms: row.get("avg_audio_ms_per_wall_ms")?,
            sample_count: row.get("sample_count")?,
        }));
    }
    Ok(None)
}

pub fn record_file_transcription_performance(
    db_path: &Path,
    model_id: &str,
    run_avg_audio_ms_per_wall_ms: f64,
) -> Result<()> {
    let connection = open_connection_by_path(db_path)?;
    let now = Utc::now().to_rfc3339();
    if let Some(existing) = get_file_transcription_performance(db_path, model_id)? {
        let next_count = existing.sample_count + 1;
        let next_avg = ((existing.avg_audio_ms_per_wall_ms * existing.sample_count as f64)
            + run_avg_audio_ms_per_wall_ms)
            / next_count as f64;
        connection.execute(
            "UPDATE file_transcription_model_performance
             SET avg_audio_ms_per_wall_ms = ?2, sample_count = ?3, updated_at = ?4
             WHERE model_id = ?1",
            params![model_id, next_avg, next_count, now],
        )?;
    } else {
        connection.execute(
            "INSERT INTO file_transcription_model_performance (
                model_id,
                avg_audio_ms_per_wall_ms,
                sample_count,
                updated_at
            ) VALUES (?1, ?2, 1, ?3)",
            params![model_id, run_avg_audio_ms_per_wall_ms, now],
        )?;
    }
    Ok(())
}

pub fn update_settings(state: &AppState, patch: SettingsPatch) -> Result<AppSettings> {
    update_settings_for_db_path(&state.db_path, patch)
}

pub fn list_transcripts(state: &AppState, query: Option<String>) -> Result<Vec<TranscriptSummary>> {
    let connection = open_connection(state)?;
    let normalized_query = query
        .as_ref()
        .map(|value| format!("%{}%", value.trim().to_lowercase()));
    let mut statement = if normalized_query.is_some() {
        connection.prepare(
            "SELECT id, created_at, source_type, title, plain_text, status, detected_languages, duration_ms, model_name, quality_status, recovered_region_count, diarization_status, speaker_count FROM transcripts WHERE lower(title) LIKE ?1 OR lower(plain_text) LIKE ?1 OR id IN (SELECT transcript_id FROM transcript_translations WHERE lower(output_text) LIKE ?1) ORDER BY datetime(created_at) DESC",
        )?
    } else {
        connection.prepare(
            "SELECT id, created_at, source_type, title, plain_text, status, detected_languages, duration_ms, model_name, quality_status, recovered_region_count, diarization_status, speaker_count FROM transcripts ORDER BY datetime(created_at) DESC",
        )?
    };
    let rows = if let Some(value) = normalized_query {
        statement.query_map([value], map_transcript_summary_row)?
    } else {
        statement.query_map([], map_transcript_summary_row)?
    };
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read transcript list")
}

/// The newest shortcut and in-app dictations as (id, text), newest first.
pub fn recent_quick_dictations(db_path: &Path, limit: usize) -> Result<Vec<(String, String)>> {
    let connection = open_connection_by_path(db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, plain_text FROM transcripts
         WHERE source_type = 'quick_dictate' AND trim(plain_text) <> ''
         ORDER BY datetime(created_at) DESC LIMIT ?1",
    )?;
    let rows = statement.query_map([limit as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

pub fn delete_transcript(state: &AppState, transcript_id: &str) -> Result<()> {
    let connection = open_connection(state)?;
    connection.execute(
        "DELETE FROM transcript_segments WHERE transcript_id = ?1",
        [transcript_id],
    )?;
    connection.execute(
        "DELETE FROM source_files WHERE transcript_id = ?1",
        [transcript_id],
    )?;
    connection.execute("DELETE FROM transcripts WHERE id = ?1", [transcript_id])?;
    Ok(())
}

pub fn delete_all_transcripts(state: &AppState) -> Result<()> {
    let connection = open_connection(state)?;
    connection.execute("DELETE FROM transcript_segments", [])?;
    connection.execute("DELETE FROM source_files", [])?;
    connection.execute("DELETE FROM transcripts", [])?;
    crate::text_translation::ensure_schema(&connection)?;
    connection.execute("DELETE FROM text_translations", [])?;
    Ok(())
}

pub(crate) fn replace_diarization_in_transaction(
    transaction: &rusqlite::Transaction<'_>,
    transcript_id: &str,
    result: &TranscriptResult,
    replacement_source: Option<&SelectedSourceFile>,
) -> Result<()> {
    transaction.execute(
        "UPDATE transcripts SET diarization_status=?2, diarization_model_id=?3, diarization_warning=?4, diarization_policy_version=?5, speaker_count=?6, diarization_clustering_threshold=?7, diarization_speaker_count_hint=?8, diarization_source=?9 WHERE id=?1",
        params![transcript_id, to_diarization_status(result.diarization_status), &result.diarization_model_id,
            &result.diarization_warning, result.diarization_policy_version,
            crate::review::active_speaker_count(result),
            result.diarization_clustering_threshold, result.diarization_speaker_count_hint,
            to_diarization_source(result.diarization_source)],
    )?;
    let mut segment_update=transaction.prepare("UPDATE transcript_segments SET speaker_id=?2, speaker_ids_json=?3, speaker_attribution=?4, speaker_confidence=?5 WHERE id=?1 AND transcript_id=?6")?;
    for segment in &result.segments {
        segment_update.execute(params![
            &segment.id,
            &segment.speaker_id,
            segment
                .speaker_ids
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            to_speaker_attribution(segment.speaker_attribution),
            segment.speaker_confidence,
            transcript_id
        ])?;
    }
    transaction.execute(
        "DELETE FROM transcript_speakers WHERE transcript_id=?1",
        [transcript_id],
    )?;
    transaction.execute(
        "DELETE FROM diarization_turns WHERE transcript_id=?1",
        [transcript_id],
    )?;
    for speaker in &result.speakers {
        transaction.execute("INSERT INTO transcript_speakers (transcript_id,speaker_id,display_name,speaker_order) VALUES (?1,?2,?3,?4)",
            params![transcript_id, &speaker.speaker_id, &speaker.display_name, speaker.speaker_order])?;
    }
    for turn in &result.diarization_turns {
        transaction.execute("INSERT INTO diarization_turns (id,transcript_id,start_ms,end_ms,speaker_ids_json,confidence,is_overlap,is_uncertain,turn_order) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![format!("{transcript_id}:{}", turn.id.strip_prefix(&format!("{transcript_id}:")).unwrap_or(&turn.id)), transcript_id, turn.start_ms, turn.end_ms,
                serde_json::to_string(&turn.speaker_ids)?, turn.confidence, turn.is_overlap, turn.is_uncertain, turn.turn_order])?;
    }
    if let Some(source) = replacement_source {
        transaction.execute("UPDATE source_files SET local_path=?2, original_name=?3, mime_type=?4, size_bytes=?5, duration_ms=?6, sha256=?7 WHERE transcript_id=?1",
            params![transcript_id, &source.file_path, &source.original_name, &source.mime_type, source.size_bytes, source.duration_ms, &source.sha256])?;
    }
    Ok(())
}

fn open_connection(state: &AppState) -> Result<Connection> {
    open_connection_by_path(&state.db_path)
}

pub(crate) fn open_connection_by_path(db_path: &Path) -> Result<Connection> {
    let connection = Connection::open(db_path)
        .with_context(|| format!("failed to open database at {}", db_path.display()))?;
    connection.execute_batch("PRAGMA foreign_keys = ON;")?;
    Ok(connection)
}

fn query_settings(connection: &Connection) -> Result<AppSettings> {
    let settings = connection.query_row(
        "SELECT default_mode, shortcut, shortcut_mode, language_mode, fixed_language, preferred_input_device, insert_behavior, launch_at_login_enabled, metal_enabled, shortcut_dictation_model_profile, shortcut_dictation_selected_model_id, quick_dictate_model_profile, quick_dictate_selected_model_id, file_transcribe_model_profile, file_transcribe_selected_model_id, save_history, sounds_enabled, volume_ducking_enabled, file_diarization_enabled, appearance, motion_preference, translation_enabled, translation_cycle_shortcut, translation_model_id, live_pair_chunk_ms, live_pair_keep_loaded, paste_last_shortcut, serious_mode, auto_update_enabled FROM settings WHERE id = 1",
        [],
        map_settings_row,
    )?;
    Ok(settings)
}

/// A process interrupted during translation must expose a recoverable original.
fn recover_interrupted_translations(connection: &Connection) -> Result<()> {
    connection.execute("UPDATE transcript_translations SET payload = json_set(payload, '$.status', 'failed', '$.errorMessage', 'Translation was interrupted. The original text was preserved.', '$.errorCode', 'translation_interrupted') WHERE json_extract(payload, '$.status') = 'pending'", [])?;
    Ok(())
}

pub fn save_translation(
    db_path: &Path,
    output: &crate::translation::DictationOutput,
) -> Result<()> {
    let Some(id) = output.transcript_id.as_deref() else {
        return Ok(());
    };
    let connection = open_connection_by_path(db_path)?;
    // Deleting history while a translation runs must not recreate that entry.
    connection.execute("INSERT INTO transcript_translations (transcript_id, output_text, payload)
        SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM transcripts WHERE id=?1)
        ON CONFLICT(transcript_id) DO UPDATE SET output_text=excluded.output_text, payload=excluded.payload",
        params![id, output.output_text, serde_json::to_string(output)?])?;
    Ok(())
}

fn load_translation(
    connection: &Connection,
    id: &str,
) -> Result<Option<crate::translation::DictationOutput>> {
    let raw: Option<String> = connection
        .query_row(
            "SELECT payload FROM transcript_translations WHERE transcript_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    raw.map(|s| serde_json::from_str(&s).map_err(anyhow::Error::from))
        .transpose()
}

/// Totals behind the Murmelmeter.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationStats {
    pub today_words: i64,
    pub today_dictations: i64,
    pub total_words: i64,
    pub total_dictations: i64,
    pub total_duration_ms: i64,
    /// Consecutive days with a dictation, ending today or yesterday.
    pub streak_days: i64,
}

pub fn count_words(text: &str) -> i64 {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .count() as i64
}

fn local_day(offset_days: i64) -> String {
    (chrono::Local::now().date_naive() - chrono::Duration::days(offset_days))
        .format("%Y-%m-%d")
        .to_string()
}

/// Adds one finished dictation to today's totals and returns the new stats.
pub fn record_dictation_stats(db_path: &Path, text: &str, duration_ms: i64) -> Result<DictationStats> {
    record_dictation_stats_on(db_path, &local_day(0), text, duration_ms)?;
    dictation_stats(db_path)
}

fn record_dictation_stats_on(db_path: &Path, day: &str, text: &str, duration_ms: i64) -> Result<()> {
    let words = count_words(text);
    if words == 0 {
        return Ok(());
    }
    let connection = open_connection_by_path(db_path)?;
    connection.execute(
        "INSERT INTO dictation_stats (day, words, dictations, duration_ms) VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(day) DO UPDATE SET words = words + ?2, dictations = dictations + 1,
         duration_ms = duration_ms + ?3",
        params![day, words, duration_ms.max(0)],
    )?;
    Ok(())
}

pub fn dictation_stats(db_path: &Path) -> Result<DictationStats> {
    dictation_stats_for_today(db_path, &local_day(0), &local_day(1))
}

fn dictation_stats_for_today(db_path: &Path, today: &str, yesterday: &str) -> Result<DictationStats> {
    let connection = open_connection_by_path(db_path)?;
    let mut statement = connection.prepare(
        "SELECT day, words, dictations, duration_ms FROM dictation_stats ORDER BY day DESC",
    )?;
    let days = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut stats = DictationStats::default();
    for (day, words, dictations, duration_ms) in &days {
        stats.total_words += words;
        stats.total_dictations += dictations;
        stats.total_duration_ms += duration_ms;
        if day == today {
            stats.today_words = *words;
            stats.today_dictations = *dictations;
        }
    }
    // Walk back one calendar day at a time from today (or yesterday).
    let mut expected = if days.first().is_some_and(|(day, ..)| day == today) {
        chrono::NaiveDate::parse_from_str(today, "%Y-%m-%d").ok()
    } else if days.first().is_some_and(|(day, ..)| day == yesterday) {
        chrono::NaiveDate::parse_from_str(yesterday, "%Y-%m-%d").ok()
    } else {
        None
    };
    for (day, ..) in &days {
        let Some(want) = expected else { break };
        if chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d").ok() != Some(want) {
            break;
        }
        stats.streak_days += 1;
        expected = want.pred_opt();
    }
    Ok(stats)
}

pub fn reset_dictation_stats(db_path: &Path) -> Result<()> {
    let connection = open_connection_by_path(db_path)?;
    connection.execute("DELETE FROM dictation_stats", [])?;
    Ok(())
}


fn seed_default_settings(connection: &Connection) -> Result<()> {
    connection.execute(
        "INSERT INTO settings (
            id,
            default_mode,
            shortcut,
            shortcut_mode,
            language_mode,
            fixed_language,
            preferred_input_device,
            insert_behavior,
            launch_at_login_enabled,
            metal_enabled,
            shortcut_dictation_model_profile,
            shortcut_dictation_selected_model_id,
            quick_dictate_model_profile,
            quick_dictate_selected_model_id,
            file_transcribe_model_profile,
            file_transcribe_selected_model_id,
            save_history,
            sounds_enabled,
            volume_ducking_enabled
        )
        SELECT
            1,
            'quick_dictate',
            ?1,
            'push_to_talk',
            'auto',
            NULL,
            NULL,
            'paste',
            0,
            1,
            'balanced',
            NULL,
            'balanced',
            NULL,
            'balanced',
            NULL,
            1,
            1,
            1
        WHERE NOT EXISTS (SELECT 1 FROM settings WHERE id = 1)",
        [crate::key_gesture::FN_KEY],
    )?;

    Ok(())
}

fn fetch_transcript_summary(
    connection: &Connection,
    transcript_id: &str,
) -> Result<TranscriptSummary> {
    let transcript = connection.query_row(
        "SELECT id, created_at, source_type, title, plain_text, status, detected_languages, duration_ms, model_name, quality_status, recovered_region_count, diarization_status, speaker_count FROM transcripts WHERE id = ?1",
        [transcript_id],
        map_transcript_summary_row,
    )?;
    Ok(transcript)
}

pub(crate) fn fetch_transcript_detail(
    connection: &Connection,
    transcript_id: &str,
) -> Result<TranscriptDetail> {
    let summary = fetch_transcript_summary(connection, transcript_id)?;
    let (full_text, timestamped_text, warnings_raw, diarization_model_id, diarization_source, diarization_warning, policy, clustering_threshold, speaker_count_hint): (
        String,
        String,
        String,
        Option<String>,
        String,
        Option<String>,
        Option<u32>,
        Option<f32>,
        Option<i32>,
    ) = connection.query_row(
        "SELECT full_text, timestamped_text, transcription_warnings, diarization_model_id, diarization_source, diarization_warning, diarization_policy_version, diarization_clustering_threshold, diarization_speaker_count_hint FROM transcripts WHERE id = ?1",
        [transcript_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
    )?;

    let mut segment_statement = connection.prepare(
        "SELECT id, start_ms, end_ms, text, COALESCE(language_code, 'und') AS language_code, segment_order, confidence, speaker_id, speaker_ids_json, speaker_attribution, speaker_confidence
         FROM transcript_segments WHERE transcript_id = ?1 ORDER BY segment_order ASC",
    )?;
    let segments = segment_statement
        .query_map([transcript_id], |row| {
            let speaker_ids_raw: Option<String> = row.get("speaker_ids_json")?;
            Ok(TranscriptSegment {
                id: row.get("id")?,
                start_ms: row.get("start_ms")?,
                end_ms: row.get("end_ms")?,
                text: row.get("text")?,
                language_code: row.get("language_code")?,
                segment_order: row.get("segment_order")?,
                confidence: row.get("confidence")?,
                speaker_id: row.get("speaker_id")?,
                speaker_ids: speaker_ids_raw.and_then(|value| serde_json::from_str(&value).ok()),
                speaker_attribution: parse_speaker_attribution(row.get("speaker_attribution")?)?,
                speaker_confidence: row.get("speaker_confidence")?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut speaker_statement = connection.prepare(
        "SELECT speaker_id, display_name, speaker_order FROM transcript_speakers WHERE transcript_id = ?1 ORDER BY speaker_order ASC",
    )?;
    let speakers = speaker_statement
        .query_map([transcript_id], |row| {
            Ok(TranscriptSpeaker {
                speaker_id: row.get("speaker_id")?,
                display_name: row.get("display_name")?,
                speaker_order: row.get("speaker_order")?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let mut turns_statement = connection.prepare(
        "SELECT id, start_ms, end_ms, speaker_ids_json, confidence, is_overlap, is_uncertain, turn_order
         FROM diarization_turns WHERE transcript_id = ?1 ORDER BY turn_order ASC",
    )?;
    let diarization_turns = turns_statement
        .query_map([transcript_id], |row| {
            let speaker_ids_raw: String = row.get("speaker_ids_json")?;
            Ok(DiarizationTurn {
                id: row.get("id")?,
                start_ms: row.get("start_ms")?,
                end_ms: row.get("end_ms")?,
                speaker_ids: serde_json::from_str(&speaker_ids_raw).unwrap_or_default(),
                confidence: row.get("confidence")?,
                is_overlap: row.get("is_overlap")?,
                is_uncertain: row.get("is_uncertain")?,
                turn_order: row.get("turn_order")?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(TranscriptDetail {
        translation: load_translation(connection, transcript_id)?,
        manual_segment_ids: Vec::new(),
        summary,
        full_text,
        timestamped_text,
        transcription_warnings: serde_json::from_str(&warnings_raw).unwrap_or_default(),
        diarization_model_id,
        diarization_source: parse_diarization_source(diarization_source)?,
        diarization_warning,
        diarization_policy_version: policy,
        diarization_clustering_threshold: clustering_threshold,
        diarization_speaker_count_hint: speaker_count_hint,
        segments,
        speakers,
        diarization_turns,
    })
}

fn insert_transcript(
    transaction: &rusqlite::Transaction<'_>,
    transcript_id: &str,
    source_type: SourceType,
    title: String,
    result: &TranscriptResult,
    duration_ms: Option<i64>,
) -> Result<()> {
    let created_at = Utc::now().to_rfc3339();
    let languages = serde_json::to_string(&result.detected_languages)?;
    let warnings = serde_json::to_string(&result.warnings)?;
    transaction.execute(
        "INSERT INTO transcripts (id, created_at, source_type, title, full_text, plain_text, timestamped_text, detected_languages, duration_ms, status, model_name, quality_status, recovered_region_count, transcription_warnings, diarization_status, diarization_model_id, diarization_source, diarization_warning, diarization_policy_version, speaker_count, diarization_clustering_threshold, diarization_speaker_count_hint)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        params![
            transcript_id,
            &created_at,
            to_source_type(source_type),
            &title,
            &result.full_text,
            &result.plain_text,
            &result.timestamped_text,
            languages,
            duration_ms,
            "completed",
            &result.model_name,
            to_transcript_quality_status(result.quality_status),
            result.recovered_region_count,
            warnings,
            to_diarization_status(result.diarization_status),
            &result.diarization_model_id,
            to_diarization_source(result.diarization_source),
            &result.diarization_warning,
            result.diarization_policy_version,
            crate::review::active_speaker_count(result),
            result.diarization_clustering_threshold,
            result.diarization_speaker_count_hint,
        ],
    )?;

    for segment in &result.segments {
        transaction.execute(
            "INSERT INTO transcript_segments (id, transcript_id, start_ms, end_ms, text, language_code, speaker_label, confidence, segment_order, speaker_id, speaker_ids_json, speaker_attribution, speaker_confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                &segment.id,
                transcript_id,
                segment.start_ms,
                segment.end_ms,
                &segment.text,
                &segment.language_code,
                segment.confidence,
                segment.segment_order,
                &segment.speaker_id,
                segment.speaker_ids.as_ref().map(serde_json::to_string).transpose()?,
                to_speaker_attribution(segment.speaker_attribution),
                segment.speaker_confidence,
            ],
        )?;
    }

    for speaker in &result.speakers {
        transaction.execute(
            "INSERT INTO transcript_speakers (transcript_id, speaker_id, display_name, speaker_order) VALUES (?1, ?2, ?3, ?4)",
            params![transcript_id, &speaker.speaker_id, &speaker.display_name, speaker.speaker_order],
        )?;
    }
    for turn in &result.diarization_turns {
        transaction.execute(
            "INSERT INTO diarization_turns (id, transcript_id, start_ms, end_ms, speaker_ids_json, confidence, is_overlap, is_uncertain, turn_order)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                format!("{transcript_id}:{}", turn.id), transcript_id, turn.start_ms, turn.end_ms,
                serde_json::to_string(&turn.speaker_ids)?, turn.confidence, turn.is_overlap,
                turn.is_uncertain, turn.turn_order
            ],
        )?;
    }

    Ok(())
}

fn build_transcript_title(text: &str) -> String {
    if text.trim().is_empty() {
        return crate::output_format::dictation_fallback_title(chrono::Local::now());
    }
    crate::output_format::truncate_title(text, 72)
}

fn map_installed_model_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<InstalledModel> {
    let variant: String = row.get("variant")?;
    let profile = parse_model_profile(row.get("profile")?)?;
    Ok(InstalledModel {
        id: row.get("id")?,
        engine: row.get("engine")?,
        model_name: row.get("model_name")?,
        variant,
        local_path: row.get("local_path")?,
        size_bytes: row.get("size_bytes")?,
        is_default: row.get("is_default")?,
        profile,
        capabilities: crate::model_metadata::capabilities_for_model(
            &row.get::<_, String>("id")?,
            &row.get::<_, String>("engine")?,
        ),
    })
}

fn map_settings_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AppSettings> {
    Ok(AppSettings {
        default_mode: parse_default_mode(row.get("default_mode")?)?,
        shortcut: row.get("shortcut")?,
        translation_enabled: row.get("translation_enabled")?,
        translation_cycle_shortcut: row.get("translation_cycle_shortcut")?,
        translation_model_id: row.get("translation_model_id")?,
        shortcut_mode: parse_shortcut_mode(row.get("shortcut_mode")?)?,
        language_mode: parse_language_mode(row.get("language_mode")?)?,
        fixed_language: row.get("fixed_language")?,
        preferred_input_device: row.get("preferred_input_device")?,
        insert_behavior: parse_insert_behavior(row.get("insert_behavior")?)?,
        launch_at_login_enabled: row.get("launch_at_login_enabled")?,
        gpu_enabled: row.get("metal_enabled")?,
        shortcut_dictation_model_profile: parse_model_profile(
            row.get("shortcut_dictation_model_profile")?,
        )?,
        shortcut_dictation_selected_model_id: row.get("shortcut_dictation_selected_model_id")?,
        quick_dictate_model_profile: parse_model_profile(row.get("quick_dictate_model_profile")?)?,
        quick_dictate_selected_model_id: row.get("quick_dictate_selected_model_id")?,
        file_transcribe_model_profile: parse_model_profile(
            row.get("file_transcribe_model_profile")?,
        )?,
        file_transcribe_selected_model_id: row.get("file_transcribe_selected_model_id")?,
        appearance: match row.get::<_, String>("appearance")?.as_str() {
            "light" => Appearance::Light,
            "dark" => Appearance::Dark,
            _ => Appearance::System,
        },
        motion_preference: match row.get::<_, String>("motion_preference")?.as_str() {
            "reduced" => MotionPreference::Reduced,
            _ => MotionPreference::System,
        },
        save_history: row.get("save_history")?,
        sounds_enabled: row.get("sounds_enabled")?,
        volume_ducking_enabled: row.get("volume_ducking_enabled")?,
        file_diarization_enabled: row.get("file_diarization_enabled")?,
        live_pair_chunk_ms: row.get("live_pair_chunk_ms")?,
        live_pair_keep_loaded: row.get("live_pair_keep_loaded")?,
        paste_last_shortcut: row.get("paste_last_shortcut")?,
        serious_mode: row.get("serious_mode")?,
        auto_update_enabled: row.get("auto_update_enabled")?,
    })
}

fn find_model_by_name(
    models: &[InstalledModel],
    preferred_names: &[&str],
) -> Option<InstalledModel> {
    preferred_names.iter().find_map(|name| {
        models
            .iter()
            .find(|model| model.model_name == *name)
            .cloned()
    })
}

fn find_model_by_id(models: &[InstalledModel], model_id: &str) -> Option<InstalledModel> {
    models.iter().find(|model| model.id == model_id).cloned()
}

fn supports_context(model: &InstalledModel, context: ModelUseContext) -> bool {
    model.capabilities.supported_contexts.contains(&context)
}

/// True when `selection` names an installed model that can run `context`.
pub fn selection_usable(
    models: &[InstalledModel],
    selection: Option<&str>,
    context: ModelUseContext,
) -> bool {
    selection
        .and_then(|model_id| find_model_by_id(models, model_id))
        .is_some_and(|model| supports_context(&model, context))
}

/// The model Murmela picks for `context` when the saved choice is missing or
/// unusable: a preferred file name, then a batch model of the fallback
/// profile, then any batch model that supports the workflow. Live-only models
/// are opt-in and only chosen when nothing else can run shortcut dictation.
fn default_model_for(models: &[InstalledModel], context: ModelUseContext) -> Option<InstalledModel> {
    let (preferences, profile): (&[&str], _) = match context {
        ModelUseContext::ShortcutDictation => (&["ggml-medium.bin"], ModelProfile::Balanced),
        ModelUseContext::QuickDictate => (
            &["ggml-large-v3-turbo-q5_0.bin", "ggml-large-v3-turbo.bin"],
            ModelProfile::Accurate,
        ),
        // Most accurate first (internal benchmark): VibeVoice, then
        // Qwen3-ASR and MOSS, then Whisper.
        ModelUseContext::FileTranscription => (
            &[
                crate::model_metadata::VIBEVOICE_MODEL_NAME,
                crate::qwen_asr::QWEN_MODEL_NAME,
                crate::model_metadata::MOSS_MODEL_NAME,
                "ggml-large-v3-turbo-q5_0.bin",
                "ggml-large-v3-turbo.bin",
            ],
            ModelProfile::Accurate,
        ),
    };
    let eligible: Vec<InstalledModel> = models
        .iter()
        .filter(|model| supports_context(model, context))
        .cloned()
        .collect();
    let batch = |model: &&InstalledModel| !model.capabilities.streaming_transcription;
    find_model_by_name(&eligible, preferences)
        .or_else(|| {
            eligible
                .iter()
                .filter(batch)
                .find(|model| model.profile == profile && model.is_default)
                .or_else(|| eligible.iter().filter(batch).find(|model| model.profile == profile))
                .or_else(|| eligible.iter().find(batch))
                .or_else(|| eligible.first())
                .cloned()
        })
}

fn map_transcript_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TranscriptSummary> {
    let languages_raw: String = row.get("detected_languages")?;
    let detected_languages = serde_json::from_str(&languages_raw).unwrap_or_default();
    let source_type = parse_source_type(row.get("source_type")?)?;
    let title: String = row.get("title")?;
    // Older imports stored the file name with its audio extension.
    let title = if matches!(source_type, SourceType::FileUpload | SourceType::Meeting) {
        crate::output_format::file_display_title(&title)
    } else {
        title
    };
    Ok(TranscriptSummary {
        id: row.get("id")?,
        created_at: row.get("created_at")?,
        source_type,
        title,
        plain_text: row.get("plain_text")?,
        status: parse_transcript_status(row.get("status")?)?,
        detected_languages,
        duration_ms: row.get("duration_ms")?,
        model_name: row.get("model_name")?,
        quality_status: parse_transcript_quality_status(row.get("quality_status")?)?,
        recovered_region_count: row.get("recovered_region_count")?,
        diarization_status: parse_diarization_status(row.get("diarization_status")?)?,
        speaker_count: row.get("speaker_count")?,
    })
}

fn parse_diarization_status(value: String) -> rusqlite::Result<DiarizationStatus> {
    match value.as_str() {
        "not_requested" => Ok(DiarizationStatus::NotRequested),
        "pending" => Ok(DiarizationStatus::Pending),
        "running" => Ok(DiarizationStatus::Running),
        "completed" => Ok(DiarizationStatus::Completed),
        "completed_with_uncertainty" => Ok(DiarizationStatus::CompletedWithUncertainty),
        "failed" => Ok(DiarizationStatus::Failed),
        "canceled" => Ok(DiarizationStatus::Canceled),
        "not_enough_speech" => Ok(DiarizationStatus::NotEnoughSpeech),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_diarization_status(value: DiarizationStatus) -> &'static str {
    match value {
        DiarizationStatus::NotRequested => "not_requested",
        DiarizationStatus::Pending => "pending",
        DiarizationStatus::Running => "running",
        DiarizationStatus::Completed => "completed",
        DiarizationStatus::CompletedWithUncertainty => "completed_with_uncertainty",
        DiarizationStatus::Failed => "failed",
        DiarizationStatus::Canceled => "canceled",
        DiarizationStatus::NotEnoughSpeech => "not_enough_speech",
    }
}

fn parse_diarization_source(value: String) -> rusqlite::Result<DiarizationSource> {
    match value.as_str() {
        "none" => Ok(DiarizationSource::None),
        "native_model" => Ok(DiarizationSource::NativeModel),
        "post_process" => Ok(DiarizationSource::PostProcess),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_diarization_source(value: DiarizationSource) -> &'static str {
    match value {
        DiarizationSource::None => "none",
        DiarizationSource::NativeModel => "native_model",
        DiarizationSource::PostProcess => "post_process",
    }
}

fn parse_speaker_attribution(value: String) -> rusqlite::Result<SpeakerAttribution> {
    match value.as_str() {
        "none" => Ok(SpeakerAttribution::None),
        "assigned" => Ok(SpeakerAttribution::Assigned),
        "uncertain" => Ok(SpeakerAttribution::Uncertain),
        "likely" => Ok(SpeakerAttribution::Likely),
        "overlap" => Ok(SpeakerAttribution::Overlap),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_speaker_attribution(value: SpeakerAttribution) -> &'static str {
    match value {
        SpeakerAttribution::None => "none",
        SpeakerAttribution::Assigned => "assigned",
        SpeakerAttribution::Uncertain => "uncertain",
        SpeakerAttribution::Likely => "likely",
        SpeakerAttribution::Overlap => "overlap",
    }
}

fn parse_transcript_quality_status(value: String) -> rusqlite::Result<TranscriptQualityStatus> {
    match value.as_str() {
        "clean" => Ok(TranscriptQualityStatus::Clean),
        "recovered" => Ok(TranscriptQualityStatus::Recovered),
        "partial" => Ok(TranscriptQualityStatus::Partial),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_transcript_quality_status(value: TranscriptQualityStatus) -> &'static str {
    match value {
        TranscriptQualityStatus::Clean => "clean",
        TranscriptQualityStatus::Recovered => "recovered",
        TranscriptQualityStatus::Partial => "partial",
    }
}

fn parse_default_mode(value: String) -> rusqlite::Result<DefaultMode> {
    match value.as_str() {
        "quick_dictate" => Ok(DefaultMode::QuickDictate),
        "file_transcribe" => Ok(DefaultMode::FileTranscribe),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_shortcut_mode(value: String) -> rusqlite::Result<ShortcutMode> {
    match value.as_str() {
        "push_to_talk" => Ok(ShortcutMode::PushToTalk),
        "toggle" => Ok(ShortcutMode::Toggle),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_language_mode(value: String) -> rusqlite::Result<LanguageMode> {
    match value.as_str() {
        "auto" => Ok(LanguageMode::Auto),
        "fixed" => Ok(LanguageMode::Fixed),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_insert_behavior(value: String) -> rusqlite::Result<InsertBehavior> {
    match value.as_str() {
        "paste" => Ok(InsertBehavior::Paste),
        "clipboard_only" => Ok(InsertBehavior::ClipboardOnly),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_model_profile(value: String) -> rusqlite::Result<ModelProfile> {
    match value.as_str() {
        "fast" => Ok(ModelProfile::Fast),
        "balanced" => Ok(ModelProfile::Balanced),
        "accurate" | "1.7B BF16" | "0.9B F16" | "8-bit MLX" => Ok(ModelProfile::Accurate),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_source_type(value: String) -> rusqlite::Result<SourceType> {
    match value.as_str() {
        "quick_dictate" => Ok(SourceType::QuickDictate),
        "file_upload" => Ok(SourceType::FileUpload),
        "meeting" => Ok(SourceType::Meeting),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn parse_transcript_status(value: String) -> rusqlite::Result<TranscriptStatus> {
    match value.as_str() {
        "queued" => Ok(TranscriptStatus::Queued),
        "recording" => Ok(TranscriptStatus::Recording),
        "processing" => Ok(TranscriptStatus::Processing),
        "completed" => Ok(TranscriptStatus::Completed),
        "failed" => Ok(TranscriptStatus::Failed),
        "canceled" => Ok(TranscriptStatus::Canceled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_default_mode(value: DefaultMode) -> &'static str {
    match value {
        DefaultMode::QuickDictate => "quick_dictate",
        DefaultMode::FileTranscribe => "file_transcribe",
    }
}

fn to_shortcut_mode(value: ShortcutMode) -> &'static str {
    match value {
        ShortcutMode::PushToTalk => "push_to_talk",
        ShortcutMode::Toggle => "toggle",
    }
}

fn to_language_mode(value: LanguageMode) -> &'static str {
    match value {
        LanguageMode::Auto => "auto",
        LanguageMode::Fixed => "fixed",
    }
}

fn to_insert_behavior(value: InsertBehavior) -> &'static str {
    match value {
        InsertBehavior::Paste => "paste",
        InsertBehavior::ClipboardOnly => "clipboard_only",
    }
}

fn to_model_profile(value: ModelProfile) -> &'static str {
    match value {
        ModelProfile::Fast => "fast",
        ModelProfile::Balanced => "balanced",
        ModelProfile::Accurate => "accurate",
    }
}

fn to_source_type(value: SourceType) -> &'static str {
    match value {
        SourceType::QuickDictate => "quick_dictate",
        SourceType::FileUpload => "file_upload",
        SourceType::Meeting => "meeting",
    }
}

/// The tables and default settings, for tests in other modules.
#[cfg(test)]
pub(crate) fn initialize_database_for_tests(connection: &Connection) -> Result<()> {
    ensure_schema(connection)?;
    seed_default_settings(connection)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_settings_use_the_shortcut_defaults_from_the_code() {
        let path = std::env::temp_dir().join(format!("murmela-defaults-{}.sqlite", Uuid::new_v4()));
        initialize_database(&path).unwrap();
        let settings = get_settings_from_db_path(&path).unwrap();
        assert_eq!(settings.translation_cycle_shortcut, crate::translation::DEFAULT_SHORTCUT);
        assert_eq!(settings.paste_last_shortcut, crate::settings::DEFAULT_PASTE_LAST_SHORTCUT);
        assert_eq!(settings.shortcut, crate::key_gesture::FN_KEY);
        // Running it again on an existing database changes nothing.
        initialize_database(&path).unwrap();
        assert_eq!(get_settings_from_db_path(&path).unwrap().shortcut, settings.shortcut);
    }

    #[test]
    fn live_pair_settings_roundtrip_and_validate() {
        let path =
            std::env::temp_dir().join(format!("murmela-live-pair-{}.sqlite", Uuid::new_v4()));
        let connection = open_connection_by_path(&path).unwrap();
        ensure_schema(&connection).unwrap();
        seed_default_settings(&connection).unwrap();
        let defaults = get_settings_from_db_path(&path).unwrap();
        assert_eq!(defaults.live_pair_chunk_ms, 560);
        assert!(defaults.live_pair_keep_loaded);
        let updated = update_settings_for_db_path(
            &path,
            SettingsPatch {
                live_pair_chunk_ms: Some(1120),
                live_pair_keep_loaded: Some(false),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!((updated.live_pair_chunk_ms, updated.live_pair_keep_loaded), (1120, false));
        assert!(update_settings_for_db_path(
            &path,
            SettingsPatch {
                live_pair_chunk_ms: Some(2240),
                ..Default::default()
            },
        )
        .is_err());
        assert_eq!(get_settings_from_db_path(&path).unwrap().live_pair_chunk_ms, 1120);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn model_terms_acceptance_is_per_model_and_version() {
        let path =
            std::env::temp_dir().join(format!("murmela-model-terms-{}.sqlite", Uuid::new_v4()));
        let id = crate::translation::MODEL_ID;
        let version = crate::translation::GEMMA_TERMS_VERSION;
        ensure_schema(&open_connection_by_path(&path).unwrap()).unwrap();
        assert!(!model_terms_accepted(&path, id, version).unwrap());
        accept_model_terms(&path, id, version).unwrap();
        assert!(model_terms_accepted(&path, id, version).unwrap());
        assert!(!model_terms_accepted(&path, id, "gemma-terms-next").unwrap());
        assert!(!model_terms_accepted(&path, "other-model", version).unwrap());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn installed_model_profiles_roundtrip_independently_of_variant_labels() {
        let path =
            std::env::temp_dir().join(format!("murmela-model-profiles-{}.sqlite", Uuid::new_v4()));
        let connection = open_connection_by_path(&path).unwrap();
        ensure_schema(&connection).unwrap();
        let rows = [
            (
                crate::live_pair::MODEL_ID,
                crate::live_pair::ENGINE,
                "Live pair",
                "CoreML · streaming + final pass",
                ModelProfile::Fast,
                1_400_000_000,
            ),
            (
                crate::model_metadata::MOSS_MODEL_ID,
                "moss-transcribe-cpp",
                "MOSS",
                "0.9B F16",
                ModelProfile::Accurate,
                1_833_647_104,
            ),
            (
                "qwen3-asr-1.7b-bf16",
                "qwen3_asr_c",
                "Qwen",
                "1.7B BF16",
                ModelProfile::Accurate,
                4_703_041_355,
            ),
            (
                "ggml-small-bin",
                "whisper.cpp",
                "Whisper Small",
                "balanced",
                ModelProfile::Balanced,
                487_601_967,
            ),
            (
                "future-model",
                "future-engine",
                "Future model",
                "Arbitrary display label",
                ModelProfile::Fast,
                10,
            ),
        ];
        let models: Vec<_> = rows
            .iter()
            .map(
                |(id, engine, name, variant, profile, size)| InstalledModel {
                    id: (*id).into(),
                    engine: (*engine).into(),
                    model_name: (*name).into(),
                    variant: (*variant).into(),
                    local_path: format!("/models/{id}"),
                    size_bytes: *size,
                    is_default: false,
                    profile: *profile,
                    capabilities: crate::model_metadata::capabilities_for_model(id, engine),
                },
            )
            .collect();
        sync_installed_models_for_db_path(&path, &models).unwrap();
        drop(connection);
        // The same readback command used by startup and download completion.
        let restored = list_installed_models_for_db_path(&path).unwrap();
        assert_eq!(restored.len(), models.len());
        for original in &models {
            let saved = restored
                .iter()
                .find(|model| model.id == original.id)
                .unwrap();
            assert_eq!(saved.profile, original.profile);
            assert_eq!(saved.variant, original.variant);
            assert_eq!(saved.size_bytes, original.size_bytes);
            assert_eq!(saved.capabilities, original.capabilities);
        }
        let live = restored
            .iter()
            .find(|model| model.id == crate::live_pair::MODEL_ID)
            .unwrap();
        assert!(live.capabilities.streaming_transcription);
        assert_eq!(
            live.capabilities.supported_contexts,
            [crate::model_metadata::ModelUseContext::ShortcutDictation]
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn translated_history_preserves_original_recovers_interruption_and_cascades() {
        let path =
            std::env::temp_dir().join(format!("murmela-translation-{}.sqlite", Uuid::new_v4()));
        let connection = open_connection_by_path(&path).unwrap();
        ensure_schema(&connection).unwrap();
        let original = crate::review::fixture_result();
        let mut output = crate::translation::DictationOutput {
            session_id: "session".into(),
            source_text: original.plain_text.clone(),
            output_text: None,
            target_language: crate::translation::OutputMode::SpanishArgentina,
            status: "pending".into(),
            model_id: Some(crate::translation::MODEL_ID.into()),
            model_revision: Some(crate::translation::MODEL_REVISION.into()),
            prompt_version: Some(1),
            transcript_id: None,
            error_message: None,
            error_code: None,
            source_languages: vec!["de".into()],
            fallback_to_original: false,
        };
        save_translated_dictation_source(&path, &original, 20000, &mut output).unwrap();
        let saved = fetch_transcript_summary(&connection, output.transcript_id.as_deref().unwrap())
            .unwrap();
        recover_interrupted_translations(&connection).unwrap();
        assert_eq!(
            load_translation(&connection, &saved.id)
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        output.status = "completed".into();
        output.output_text = Some("¿Podés venir mañana?".into());
        save_translation(&path, &output).unwrap();
        save_translation(&path, &output).unwrap();
        let detail = fetch_transcript_detail(&connection, &saved.id).unwrap();
        assert_eq!(detail.summary.plain_text, original.plain_text);
        assert_eq!(detail.segments[0].text, original.segments[0].text);
        assert_eq!(
            detail.translation.unwrap().output_text.as_deref(),
            Some("¿Podés venir mañana?")
        );
        let count: i64 = connection.query_row("SELECT count(*) FROM transcript_translations WHERE lower(output_text) LIKE '%podés%'", [], |row| row.get(0)).unwrap();
        assert_eq!(count, 1);
        connection
            .execute("DELETE FROM transcripts WHERE id=?1", [&saved.id])
            .unwrap();
        save_translation(&path, &output).unwrap();
        assert!(load_translation(&connection, &saved.id).unwrap().is_none());
        output.transcript_id = None;
        save_translation(&path, &output).unwrap();
        drop(connection);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn native_model_variants_map_to_accurate_profile() {
        for variant in ["1.7B BF16", "0.9B F16", "8-bit MLX"] {
            assert_eq!(
                parse_model_profile(variant.to_string()).expect("known native model variant"),
                ModelProfile::Accurate,
            );
        }
    }

    #[test]
    fn speaker_metadata_roundtrips_atomically() {
        let connection = Connection::open_in_memory().expect("in-memory database");
        ensure_schema(&connection).expect("schema");
        let transaction = connection.unchecked_transaction().expect("transaction");
        let result = TranscriptResult {
            job_id: "job".into(),
            model_name: "test".into(),
            full_text: "Hello".into(),
            plain_text: "Hello".into(),
            timestamped_text: "[00:00 - 00:01] en: Hello".into(),
            detected_languages: vec!["en".into()],
            segments: vec![TranscriptSegment {
                id: "segment".into(),
                start_ms: 0,
                end_ms: 1_000,
                text: "Hello".into(),
                language_code: "en".into(),
                segment_order: 0,
                confidence: Some(0.9),
                speaker_id: Some("speaker_0".into()),
                speaker_ids: Some(vec!["speaker_0".into()]),
                speaker_attribution: SpeakerAttribution::Assigned,
                speaker_confidence: Some(1.0),
            }],
            quality_status: TranscriptQualityStatus::Clean,
            recovered_region_count: 0,
            warnings: vec![],
            diarization_status: DiarizationStatus::Completed,
            diarization_model_id: Some(crate::diarization::DIARIZATION_MODEL_ID.into()),
            diarization_source: DiarizationSource::PostProcess,
            diarization_warning: None,
            diarization_policy_version: Some(1),
            diarization_clustering_threshold: Some(1.1),
            diarization_speaker_count_hint: None,
            speakers: vec![TranscriptSpeaker {
                speaker_id: "speaker_0".into(),
                display_name: "Speaker 1".into(),
                speaker_order: 0,
            }],
            diarization_turns: vec![DiarizationTurn {
                id: "turn_0".into(),
                start_ms: 0,
                end_ms: 1_000,
                speaker_ids: vec!["speaker_0".into()],
                confidence: None,
                is_overlap: false,
                is_uncertain: false,
                turn_order: 0,
            }],
        };
        insert_transcript(
            &transaction,
            "transcript",
            SourceType::FileUpload,
            "Test".into(),
            &result,
            Some(1_000),
        )
        .expect("insert transcript");
        transaction.commit().expect("commit");

        let detail = fetch_transcript_detail(&connection, "transcript").expect("detail");
        assert_eq!(detail.summary.speaker_count, Some(1));
        assert_eq!(detail.segments[0].speaker_id.as_deref(), Some("speaker_0"));
        assert_eq!(detail.speakers[0].display_name, "Speaker 1");
        assert_eq!(detail.diarization_turns.len(), 1);
        assert_eq!(detail.diarization_source, DiarizationSource::PostProcess);
    }

    fn catalog_model(id: &str, engine: &str, name: &str, profile: ModelProfile) -> InstalledModel {
        InstalledModel {
            id: id.into(),
            engine: engine.into(),
            model_name: name.into(),
            variant: String::new(),
            local_path: format!("/models/{id}"),
            size_bytes: 1,
            is_default: false,
            profile,
            capabilities: crate::model_metadata::capabilities_for_model(id, engine),
        }
    }

    fn fresh_settings_db(label: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("murmela-{label}-{}.sqlite", Uuid::new_v4()));
        let connection = open_connection_by_path(&path).unwrap();
        ensure_schema(&connection).unwrap();
        seed_default_settings(&connection).unwrap();
        path
    }

    #[test]
    fn murmelmeter_counts_words_days_and_streaks() {
        let path = fresh_settings_db("murmelmeter");
        assert_eq!(count_words("Hallo Welt, ça va? — 42 !"), 5);
        record_dictation_stats_on(&path, "2026-10-01", "one two three", 2_000).unwrap();
        record_dictation_stats_on(&path, "2026-10-03", "four five", 1_000).unwrap();
        record_dictation_stats_on(&path, "2026-10-04", "six", 500).unwrap();
        record_dictation_stats_on(&path, "2026-10-05", "seven eight", 700).unwrap();
        record_dictation_stats_on(&path, "2026-10-05", "   ", 300).unwrap();
        let stats = dictation_stats_for_today(&path, "2026-10-05", "2026-10-04").unwrap();
        assert_eq!(
            stats,
            DictationStats {
                today_words: 2,
                today_dictations: 1,
                total_words: 8,
                total_dictations: 4,
                total_duration_ms: 4_200,
                streak_days: 3,
            }
        );
        // A streak survives until the end of the next day, then breaks.
        assert_eq!(
            dictation_stats_for_today(&path, "2026-10-06", "2026-10-05").unwrap().streak_days,
            3
        );
        assert_eq!(
            dictation_stats_for_today(&path, "2026-10-07", "2026-10-06").unwrap().streak_days,
            0
        );
        reset_dictation_stats(&path).unwrap();
        assert_eq!(dictation_stats(&path).unwrap(), DictationStats::default());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn default_language_shortcut_is_a_valid_global_shortcut() {
        use std::str::FromStr;
        let shortcut =
            tauri_plugin_global_shortcut::Shortcut::from_str(crate::translation::DEFAULT_SHORTCUT)
                .unwrap();
        let dictation =
            tauri_plugin_global_shortcut::Shortcut::from_str("CmdOrCtrl+Shift+Space").unwrap();
        assert_ne!(shortcut.id(), dictation.id());
    }

    #[test]
    fn fresh_install_selects_a_shortcut_model_from_current_downloads() {
        // Regression: the macOS shortcut default only looked for the retired
        // Whisper Medium and then a Balanced model, so an install with only
        // current downloads (all Accurate) left the shortcut without a model.
        let path = fresh_settings_db("fresh-shortcut");
        let models = [
            catalog_model(
                "ggml-large-v3-turbo-q5_0-bin",
                "whisper.cpp",
                "ggml-large-v3-turbo-q5_0.bin",
                ModelProfile::Accurate,
            ),
            catalog_model(
                crate::model_metadata::MOSS_MODEL_ID,
                "moss-transcribe-cpp",
                crate::model_metadata::MOSS_MODEL_NAME,
                ModelProfile::Accurate,
            ),
        ];
        apply_preferred_model_defaults_for_db_path(&path, &models).unwrap();
        let settings = get_settings_from_db_path(&path).unwrap();
        assert_eq!(
            settings.shortcut_dictation_selected_model_id.as_deref(),
            Some("ggml-large-v3-turbo-q5_0-bin")
        );
        assert_eq!(
            settings.quick_dictate_selected_model_id.as_deref(),
            Some("ggml-large-v3-turbo-q5_0-bin")
        );
        assert!(settings.file_transcribe_selected_model_id.is_some());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn files_default_to_the_most_accurate_installed_engine() {
        let path = fresh_settings_db("file-default");
        let models = [
            catalog_model("ggml-large-v3-turbo-q5_0-bin", "whisper.cpp", "ggml-large-v3-turbo-q5_0.bin", ModelProfile::Accurate),
            catalog_model(crate::model_metadata::MOSS_MODEL_ID, "moss-transcribe-cpp", crate::model_metadata::MOSS_MODEL_NAME, ModelProfile::Accurate),
            catalog_model(crate::qwen_asr::QWEN_MODEL_ID, "qwen3_asr_c", crate::qwen_asr::QWEN_MODEL_NAME, ModelProfile::Accurate),
        ];
        apply_preferred_model_defaults_for_db_path(&path, &models).unwrap();
        assert_eq!(
            get_settings_from_db_path(&path).unwrap().file_transcribe_selected_model_id.as_deref(),
            Some(crate::qwen_asr::QWEN_MODEL_ID)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn defaults_never_assign_a_model_to_a_workflow_it_cannot_run() {
        let path = fresh_settings_db("context-defaults");
        let live = catalog_model(
            crate::live_pair::MODEL_ID,
            crate::live_pair::ENGINE,
            crate::live_pair::MODEL_NAME,
            ModelProfile::Fast,
        );
        let diarization = catalog_model(
            "offline-speaker-diarization",
            "sherpa-onnx",
            "Offline speaker diarization",
            ModelProfile::Balanced,
        );
        apply_preferred_model_defaults_for_db_path(&path, &[live, diarization]).unwrap();
        let settings = get_settings_from_db_path(&path).unwrap();
        assert_eq!(
            settings.shortcut_dictation_selected_model_id.as_deref(),
            Some(crate::live_pair::MODEL_ID)
        );
        assert_eq!(settings.quick_dictate_selected_model_id, None);
        assert_eq!(settings.file_transcribe_selected_model_id, None);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn unusable_saved_selection_is_replaced() {
        let path = fresh_settings_db("unusable-selection");
        update_settings_for_db_path(
            &path,
            SettingsPatch {
                quick_dictate_selected_model_id: Some(Some(crate::live_pair::MODEL_ID.into())),
                ..Default::default()
            },
        )
        .unwrap();
        let models = [
            catalog_model(
                crate::live_pair::MODEL_ID,
                crate::live_pair::ENGINE,
                crate::live_pair::MODEL_NAME,
                ModelProfile::Fast,
            ),
            catalog_model(
                crate::qwen_asr::QWEN_MODEL_ID,
                "qwen3_asr_c",
                "Qwen3-ASR-1.7B",
                ModelProfile::Accurate,
            ),
        ];
        assert!(!selection_usable(
            &models,
            Some(crate::live_pair::MODEL_ID),
            ModelUseContext::QuickDictate
        ));
        apply_preferred_model_defaults_for_db_path(&path, &models).unwrap();
        assert_eq!(
            get_settings_from_db_path(&path)
                .unwrap()
                .quick_dictate_selected_model_id
                .as_deref(),
            Some(crate::qwen_asr::QWEN_MODEL_ID)
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn live_only_model_is_the_shortcut_default_only_when_nothing_else_fits() {
        let live = catalog_model(
            crate::live_pair::MODEL_ID,
            crate::live_pair::ENGINE,
            crate::live_pair::MODEL_NAME,
            ModelProfile::Fast,
        );
        let qwen = catalog_model(
            crate::qwen_asr::QWEN_MODEL_ID,
            "qwen3_asr_c",
            "Qwen3-ASR-1.7B",
            ModelProfile::Accurate,
        );
        assert_eq!(
            default_model_for(&[live.clone(), qwen.clone()], ModelUseContext::ShortcutDictation)
                .map(|model| model.id),
            Some(qwen.id.clone())
        );
        assert_eq!(
            default_model_for(&[live.clone()], ModelUseContext::ShortcutDictation)
                .map(|model| model.id),
            Some(live.id.clone())
        );
        assert!(default_model_for(&[live], ModelUseContext::QuickDictate).is_none());
    }

}
