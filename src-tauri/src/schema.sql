-- Murmela's database. Tables owned by one module (license, network activity, review,
-- text translation) are created by that module's ensure_schema.

CREATE TABLE IF NOT EXISTS settings (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  default_mode TEXT NOT NULL,
  shortcut TEXT NOT NULL,
  shortcut_mode TEXT NOT NULL,
  language_mode TEXT NOT NULL,
  fixed_language TEXT NULL,
  preferred_input_device TEXT NULL,
  insert_behavior TEXT NOT NULL,
  launch_at_login_enabled INTEGER NOT NULL DEFAULT 0,
  metal_enabled INTEGER NOT NULL DEFAULT 1,
  shortcut_dictation_model_profile TEXT NOT NULL DEFAULT 'balanced',
  shortcut_dictation_selected_model_id TEXT NULL,
  quick_dictate_model_profile TEXT NOT NULL DEFAULT 'balanced',
  quick_dictate_selected_model_id TEXT NULL,
  file_transcribe_model_profile TEXT NOT NULL DEFAULT 'balanced',
  file_transcribe_selected_model_id TEXT NULL,
  save_history INTEGER NOT NULL DEFAULT 1,
  sounds_enabled INTEGER NOT NULL DEFAULT 1,
  volume_ducking_enabled INTEGER NOT NULL DEFAULT 1,
  file_diarization_enabled INTEGER NOT NULL DEFAULT 0,
  -- Defaults match translation::DEFAULT_SHORTCUT and settings::DEFAULT_PASTE_LAST_SHORTCUT.
  translation_enabled INTEGER NOT NULL DEFAULT 0,
  translation_cycle_shortcut TEXT NOT NULL DEFAULT 'Ctrl+Alt+L',
  -- Comma-separated; matches translation::DEFAULT_CYCLE_LANGUAGES. Added after 1.1 (see storage::ensure_schema).
  -- The quick_translate_* columns (⌘C twice) were added after 1.2.4 the same way.
  translation_cycle_languages TEXT NOT NULL DEFAULT 'en,de,fr,es-AR',
  paste_last_shortcut TEXT NOT NULL DEFAULT 'Ctrl+Alt+V',
  translation_model_id TEXT NOT NULL DEFAULT 'translategemma-12b-q6-k',
  appearance TEXT NOT NULL DEFAULT 'system',
  motion_preference TEXT NOT NULL DEFAULT 'system',
  live_pair_chunk_ms INTEGER NOT NULL DEFAULT 560,
  live_pair_keep_loaded INTEGER NOT NULL DEFAULT 1,
  serious_mode INTEGER NOT NULL DEFAULT 0,
  auto_update_enabled INTEGER NOT NULL DEFAULT 1,
  quick_translate_enabled INTEGER NOT NULL DEFAULT 1,
  quick_translate_shortcut TEXT NOT NULL DEFAULT 'DoubleCopy',
  quick_translate_display TEXT NOT NULL DEFAULT 'popup',
  quick_translate_ignored_apps TEXT NOT NULL DEFAULT 'com.apple.keychainaccess,com.apple.Passwords,com.1password.1password,com.agilebits.onepassword7',
  quick_translate_my_language TEXT NULL,
  quick_translate_last_target TEXT NULL,
  quick_translate_save_history INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS transcripts (
  id TEXT PRIMARY KEY,
  created_at TEXT NOT NULL,
  source_type TEXT NOT NULL,
  title TEXT NOT NULL,
  full_text TEXT NOT NULL,
  plain_text TEXT NOT NULL,
  timestamped_text TEXT NOT NULL,
  detected_languages TEXT NOT NULL DEFAULT '[]',
  duration_ms INTEGER NULL,
  status TEXT NOT NULL,
  model_name TEXT NULL,
  quality_status TEXT NOT NULL DEFAULT 'clean',
  recovered_region_count INTEGER NOT NULL DEFAULT 0,
  transcription_warnings TEXT NOT NULL DEFAULT '[]',
  diarization_status TEXT NOT NULL DEFAULT 'not_requested',
  diarization_model_id TEXT NULL,
  diarization_source TEXT NOT NULL DEFAULT 'none',
  diarization_warning TEXT NULL,
  diarization_policy_version INTEGER NULL,
  diarization_clustering_threshold REAL NULL,
  diarization_speaker_count_hint INTEGER NULL,
  speaker_count INTEGER NULL
);

CREATE TABLE IF NOT EXISTS transcript_segments (
  id TEXT PRIMARY KEY,
  transcript_id TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  text TEXT NOT NULL,
  language_code TEXT NULL,
  speaker_label TEXT NULL,
  confidence REAL NULL,
  segment_order INTEGER NOT NULL,
  speaker_id TEXT NULL,
  speaker_ids_json TEXT NULL,
  speaker_attribution TEXT NOT NULL DEFAULT 'none',
  speaker_confidence REAL NULL,
  FOREIGN KEY (transcript_id) REFERENCES transcripts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS transcript_speakers (
  transcript_id TEXT NOT NULL,
  speaker_id TEXT NOT NULL,
  display_name TEXT NOT NULL,
  speaker_order INTEGER NOT NULL,
  PRIMARY KEY (transcript_id, speaker_id),
  FOREIGN KEY (transcript_id) REFERENCES transcripts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS diarization_turns (
  id TEXT PRIMARY KEY,
  transcript_id TEXT NOT NULL,
  start_ms INTEGER NOT NULL,
  end_ms INTEGER NOT NULL,
  speaker_ids_json TEXT NOT NULL,
  confidence REAL NULL,
  is_overlap INTEGER NOT NULL DEFAULT 0,
  is_uncertain INTEGER NOT NULL DEFAULT 0,
  turn_order INTEGER NOT NULL,
  FOREIGN KEY (transcript_id) REFERENCES transcripts(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_diarization_turns_order ON diarization_turns(transcript_id, turn_order);
CREATE INDEX IF NOT EXISTS idx_diarization_turns_start ON diarization_turns(transcript_id, start_ms);

CREATE TABLE IF NOT EXISTS source_files (
  id TEXT PRIMARY KEY,
  transcript_id TEXT NOT NULL,
  original_name TEXT NOT NULL,
  mime_type TEXT NOT NULL,
  local_path TEXT NOT NULL,
  duration_ms INTEGER NULL,
  size_bytes INTEGER NOT NULL,
  sha256 TEXT NOT NULL,
  FOREIGN KEY (transcript_id) REFERENCES transcripts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS transcript_translations (
  transcript_id TEXT PRIMARY KEY REFERENCES transcripts(id) ON DELETE CASCADE,
  output_text TEXT,
  payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS installed_models (
  id TEXT PRIMARY KEY,
  engine TEXT NOT NULL,
  model_name TEXT NOT NULL,
  variant TEXT NOT NULL,
  local_path TEXT NOT NULL,
  size_bytes INTEGER NOT NULL,
  is_default INTEGER NOT NULL DEFAULT 0,
  profile TEXT NOT NULL DEFAULT 'accurate'
);

CREATE TABLE IF NOT EXISTS installed_model_packages (
  id TEXT PRIMARY KEY,
  capability TEXT NOT NULL,
  local_path TEXT NOT NULL,
  manifest_version INTEGER NOT NULL,
  hashes_json TEXT NOT NULL,
  installed_at TEXT NOT NULL
);

-- Licence terms a user must accept before a model is downloaded (Gemma).
CREATE TABLE IF NOT EXISTS model_terms_acceptances (
  model_id TEXT PRIMARY KEY,
  terms_version TEXT NOT NULL,
  accepted_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS custom_vocabulary_terms (
  id TEXT PRIMARY KEY,
  canonical TEXT NOT NULL,
  normalized_canonical TEXT NOT NULL UNIQUE,
  match_mode TEXT NOT NULL DEFAULT 'exact_and_fuzzy',
  is_builtin INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS custom_vocabulary_aliases (
  id TEXT PRIMARY KEY,
  term_id TEXT NOT NULL,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL UNIQUE,
  FOREIGN KEY (term_id) REFERENCES custom_vocabulary_terms(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS file_transcription_model_performance (
  model_id TEXT PRIMARY KEY,
  avg_audio_ms_per_wall_ms REAL NOT NULL,
  sample_count INTEGER NOT NULL,
  updated_at TEXT NOT NULL
);

-- Aggregates only, never text: the Murmelmeter works with history off.
CREATE TABLE IF NOT EXISTS dictation_stats (
  day TEXT PRIMARY KEY,
  words INTEGER NOT NULL DEFAULT 0,
  dictations INTEGER NOT NULL DEFAULT 0,
  duration_ms INTEGER NOT NULL DEFAULT 0
);

-- One-time steps that already ran (first-run setup).
CREATE TABLE IF NOT EXISTS app_migrations (
  migration_key TEXT PRIMARY KEY,
  completed_at TEXT NOT NULL
);
