use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResponse {
    pub app_name: String,
    pub app_version: String,
    pub db_path: String,
    pub temp_dir: String,
    pub models_dir: String,
    pub startup_notices: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShortcutMode {
    PushToTalk,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageMode {
    Auto,
    Fixed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertBehavior {
    Paste,
    ClipboardOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelProfile {
    Fast,
    Balanced,
    Accurate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultMode {
    QuickDictate,
    FileTranscribe,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionPreference {
    #[default]
    System,
    Reduced,
}

/// Where a quick translation (⌘C twice) appears.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuickTranslateDisplay {
    /// A small panel next to the pointer, over the app being used.
    #[default]
    Popup,
    /// The Translate screen in Murmela's window.
    App,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    pub default_mode: DefaultMode,
    pub shortcut: String,
    pub translation_enabled: bool,
    pub translation_cycle_shortcut: String,
    /// Output language codes the language shortcut visits after Original.
    #[serde(default = "default_cycle_languages")]
    pub translation_cycle_languages: Vec<String>,
    pub translation_model_id: String,
    pub shortcut_mode: ShortcutMode,
    pub language_mode: LanguageMode,
    pub fixed_language: Option<String>,
    pub preferred_input_device: Option<String>,
    pub insert_behavior: InsertBehavior,
    pub launch_at_login_enabled: bool,
    pub gpu_enabled: bool,
    pub shortcut_dictation_model_profile: ModelProfile,
    pub shortcut_dictation_selected_model_id: Option<String>,
    pub quick_dictate_model_profile: ModelProfile,
    pub quick_dictate_selected_model_id: Option<String>,
    pub file_transcribe_model_profile: ModelProfile,
    pub file_transcribe_selected_model_id: Option<String>,
    #[serde(default)]
    pub appearance: Appearance,
    #[serde(default)]
    pub motion_preference: MotionPreference,
    pub save_history: bool,
    pub sounds_enabled: bool,
    pub volume_ducking_enabled: bool,
    pub file_diarization_enabled: bool,
    /// Nemotron preview chunk for the live pair (560 or 1120 ms).
    #[serde(default = "default_live_pair_chunk_ms")]
    pub live_pair_chunk_ms: u32,
    /// Keep the live-pair models resident while Murmela runs.
    #[serde(default = "default_true")]
    pub live_pair_keep_loaded: bool,
    /// Global shortcut that pastes the last dictation again.
    #[serde(default = "default_paste_last_shortcut")]
    pub paste_last_shortcut: String,
    /// Turns off Murmela's playful copy, the Murmelmeter and easter eggs.
    #[serde(default)]
    pub serious_mode: bool,
    /// Look for a new Murmela release in the background and offer to install it.
    #[serde(default = "default_true")]
    pub auto_update_enabled: bool,
    /// Translate selected text in any app with ⌘C twice or `quick_translate_shortcut`.
    #[serde(default = "default_true")]
    pub quick_translate_enabled: bool,
    /// `quick_translate::DOUBLE_COPY`, or a key combination that copies the selection itself.
    #[serde(default = "default_quick_translate_shortcut")]
    pub quick_translate_shortcut: String,
    #[serde(default)]
    pub quick_translate_display: QuickTranslateDisplay,
    /// Bundle identifiers of apps where quick translation does nothing.
    #[serde(default = "default_quick_translate_ignored_apps")]
    pub quick_translate_ignored_apps: Vec<String>,
    /// Text in other languages is translated into this one; None follows macOS.
    #[serde(default)]
    pub quick_translate_my_language: Option<String>,
    /// The last other language picked in the popup. Text already in your own
    /// language is translated into it.
    #[serde(default)]
    pub quick_translate_last_target: Option<String>,
    /// Keep translations made in the panel in the Library (off: quick lookups
    /// would crowd it).
    #[serde(default)]
    pub quick_translate_save_history: bool,
}

/// Control-Option-V: paste the last dictation again.
pub const DEFAULT_PASTE_LAST_SHORTCUT: &str = "Ctrl+Alt+V";
fn default_paste_last_shortcut() -> String {
    DEFAULT_PASTE_LAST_SHORTCUT.into()
}

pub const LIVE_PAIR_CHUNKS_MS: [u32; 2] = [560, 1120];
fn default_live_pair_chunk_ms() -> u32 {
    LIVE_PAIR_CHUNKS_MS[0]
}
fn default_quick_translate_shortcut() -> String {
    crate::quick_translate::DOUBLE_COPY.into()
}
fn default_quick_translate_ignored_apps() -> Vec<String> {
    crate::quick_translate::DEFAULT_IGNORED_APPS.map(String::from).to_vec()
}
fn default_true() -> bool {
    true
}
pub(crate) fn default_cycle_languages() -> Vec<String> {
    crate::translation::DEFAULT_CYCLE_LANGUAGES.map(String::from).to_vec()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub default_mode: Option<DefaultMode>,
    pub shortcut: Option<String>,
    pub translation_enabled: Option<bool>,
    pub translation_cycle_shortcut: Option<String>,
    pub translation_cycle_languages: Option<Vec<String>>,
    pub translation_model_id: Option<String>,
    pub shortcut_mode: Option<ShortcutMode>,
    pub language_mode: Option<LanguageMode>,
    pub fixed_language: Option<Option<String>>,
    pub preferred_input_device: Option<Option<String>>,
    pub insert_behavior: Option<InsertBehavior>,
    pub launch_at_login_enabled: Option<bool>,
    pub gpu_enabled: Option<bool>,
    pub shortcut_dictation_model_profile: Option<ModelProfile>,
    pub shortcut_dictation_selected_model_id: Option<Option<String>>,
    pub quick_dictate_model_profile: Option<ModelProfile>,
    pub quick_dictate_selected_model_id: Option<Option<String>>,
    pub file_transcribe_model_profile: Option<ModelProfile>,
    pub file_transcribe_selected_model_id: Option<Option<String>>,
    pub appearance: Option<Appearance>,
    pub motion_preference: Option<MotionPreference>,
    pub save_history: Option<bool>,
    pub sounds_enabled: Option<bool>,
    pub volume_ducking_enabled: Option<bool>,
    pub file_diarization_enabled: Option<bool>,
    pub live_pair_chunk_ms: Option<u32>,
    pub live_pair_keep_loaded: Option<bool>,
    pub paste_last_shortcut: Option<String>,
    pub serious_mode: Option<bool>,
    pub auto_update_enabled: Option<bool>,
    pub quick_translate_enabled: Option<bool>,
    pub quick_translate_shortcut: Option<String>,
    pub quick_translate_display: Option<QuickTranslateDisplay>,
    pub quick_translate_ignored_apps: Option<Vec<String>>,
    pub quick_translate_my_language: Option<Option<String>>,
    pub quick_translate_last_target: Option<Option<String>>,
    pub quick_translate_save_history: Option<bool>,
}
