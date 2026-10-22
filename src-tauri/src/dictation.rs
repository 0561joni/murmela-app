use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use tauri::{AppHandle, Emitter};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use crate::asr::{FileTranscriptionRequest, TranscriptionEngine};
use crate::audio_capture::RecordingController;
use crate::desktop_shell::{DesktopShellController, DictationOverlayPayload, OverlayPhase};
use crate::insertion::{
    detect_frontmost_paste_target, InsertionOutcome, InsertionReport, PasteTarget,
};
use crate::settings::{AppSettings, ShortcutMode};
use crate::sound::SoundPlayer;
use crate::storage;
use crate::system_volume::{self, VolumeSnapshot};
use crate::vocabulary;

const QUICK_DICTATE_STATUS_EVENT: &str = "quick-dictate-status";
pub const DICTATION_STATS_EVENT: &str = "dictation-stats";

/// Adds a finished dictation to the Murmelmeter and tells every window.
pub fn record_dictation_stats(
    app: &AppHandle,
    db_path: &std::path::Path,
    text: &str,
    duration_ms: i64,
) {
    match storage::record_dictation_stats(db_path, text, duration_ms) {
        Ok(stats) => {
            let _ = app.emit(DICTATION_STATS_EVENT, stats);
        }
        Err(error) => eprintln!("[dictation] stats update failed: {error:#}"),
    }
}

/// "blah blah blah" (any case, three or more, light punctuation): the one
/// dictation that earns a wink instead of a plain "Pasted".
fn is_blah_blah(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|word| {
            word.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect();
    words.len() >= 3 && words.iter().all(|word| word == "blah" || word == "bla")
}
/// Shortcut dictation stops and transcribes automatically at five minutes,
/// matching the live dictation limit. The overlay warns 30 seconds before.
pub(crate) const DICTATION_CAPTURE_LIMIT_MS: u64 = 300_000;
const DICTATION_LIMIT_WARNING_MS: u64 = 270_000;
/// Long enough to read a result chip; short enough not to block the next dictation.
const RESULT_CHIP_HOLD: Duration = Duration::from_millis(1800);
const ERROR_CHIP_HOLD: Duration = Duration::from_millis(3500);
const NOTICE_CHIP_HOLD: Duration = Duration::from_millis(2200);

/// What a registered global shortcut does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShortcutRole {
    Dictation,
    CycleLanguage,
    PasteLast,
}
impl ShortcutRole {
    fn label(self) -> &'static str {
        match self {
            Self::Dictation => "Dictation",
            Self::CycleLanguage => "language switching",
            Self::PasteLast => "pasting the last dictation",
        }
    }
}

/// Escape cancels a running shortcut dictation; it is registered only while
/// one is listening or transcribing.
const CANCEL_SHORTCUT: &str = "Escape";

/// Validates the shortcuts Murmela registers and rejects two roles sharing one
/// key combination (in any modifier order).
fn shortcut_set(entries: &[(ShortcutRole, &str)]) -> Result<Vec<(ShortcutRole, String)>> {
    let mut parsed: Vec<(ShortcutRole, u32)> = Vec::new();
    for (role, value) in entries {
        let id = Shortcut::from_str(value)?.id();
        if let Some((other, _)) = parsed.iter().find(|(_, other_id)| *other_id == id) {
            let first = other.label();
            let second = role.label();
            let first = first[..1].to_uppercase() + &first[1..];
            return Err(anyhow!("{first} and {second} need different shortcuts."));
        }
        parsed.push((*role, id));
    }
    Ok(entries
        .iter()
        .map(|(role, value)| (*role, (*value).to_owned()))
        .collect())
}

/// A short, plain reason for the overlay chip; the full message stays in the app.
fn overlay_reason(message: &str) -> &'static str {
    let lower = message.to_ascii_lowercase();
    if message.contains("TRANSCRIPTION_EMPTY") {
        "No speech heard"
    } else if message.contains("MODEL_MISSING") || message.contains("MODEL_CONTEXT_UNSUPPORTED") {
        "No dictation engine · open Murmela"
    } else if message.contains("LIVE_PAIR_SETUP") {
        "Engine not ready · open Murmela"
    } else if lower.contains("microphone")
        || lower.contains("input device")
        || lower.contains("capture")
        || lower.contains("recording worker")
    {
        "Microphone isn't working"
    } else if lower.contains("translation") {
        "Translation failed · open Murmela"
    } else if lower.contains("still transcribing") {
        "Still on the last one…"
    } else if lower.contains("recording is already active") {
        "Recording in the app"
    } else if lower.contains("timed out") {
        "Took too long · try again"
    } else {
        "Didn't work · open Murmela"
    }
}

fn cycle_key_transition(pressed: &AtomicBool, event: ShortcutState) -> bool {
    match event {
        ShortcutState::Released => {
            pressed.store(false, Ordering::SeqCst);
            false
        }
        ShortcutState::Pressed => !pressed.swap(true, Ordering::SeqCst),
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickDictationState {
    Idle,
    Listening,
    Processing,
    Inserted,
    ClipboardOnly,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QuickDictationStatusResponse {
    pub state: QuickDictationState,
    pub registered_shortcut: Option<String>,
    pub shortcut_mode: ShortcutMode,
    pub is_registered: bool,
    pub last_transcript_text: Option<String>,
    pub last_transcript_id: Option<String>,
    pub last_recording_path: Option<String>,
    pub last_error_message: Option<String>,
    pub last_model_name: Option<String>,
    pub last_insert_outcome: Option<InsertionOutcome>,
    pub last_insert_warning: Option<String>,
    pub last_duration_ms: Option<i64>,
    pub can_retry_streaming: bool,
}

impl Default for QuickDictationStatusResponse {
    fn default() -> Self {
        Self {
            state: QuickDictationState::Idle,
            registered_shortcut: None,
            shortcut_mode: ShortcutMode::PushToTalk,
            is_registered: false,
            last_transcript_text: None,
            last_transcript_id: None,
            last_recording_path: None,
            last_error_message: None,
            last_model_name: None,
            last_insert_outcome: None,
            last_insert_warning: None,
            last_duration_ms: None,
            can_retry_streaming: false,
        }
    }
}

#[derive(Clone)]
pub struct QuickDictationController {
    app: AppHandle,
    engine: Arc<dyn TranscriptionEngine>,
    recording_controller: RecordingController,
    db_path: std::path::PathBuf,
    desktop_shell: DesktopShellController,
    sound_player: Arc<Option<SoundPlayer>>,
    status: Arc<Mutex<QuickDictationStatusResponse>>,
    is_suspended: Arc<Mutex<bool>>,
    registered_shortcut: Arc<Mutex<Option<String>>>,
    paste_target: Arc<Mutex<Option<PasteTarget>>>,
    volume_snapshot: Arc<Mutex<Option<VolumeSnapshot>>>,
    // Bumped on every `begin_listening`. The overlay-level poller captures the
    // generation it was spawned for and exits as soon as it is superseded, so
    // exactly one poller runs at a time (no zombie threads on intensive use).
    poller_generation: Arc<AtomicU64>,
    // Unix-millis timestamp of the last state transition, used by the watchdog
    // to detect a dictation that has been stuck in Listening/Processing.
    state_since_ms: Arc<AtomicI64>,
    translation: crate::translation::TranslationService,
    shortcut_pair: Arc<Mutex<Vec<(ShortcutRole, String)>>>,
    // Serializes registering and releasing the Escape cancel key.
    cancel_key: Arc<Mutex<()>>,
    // Text of the most recent shortcut dictation, kept for "Paste last dictation"
    // even after a later attempt fails or hears nothing.
    last_output: Arc<Mutex<Option<String>>>,
    cycle_pressed: Arc<AtomicBool>,
    live: Arc<Mutex<Option<LiveDictation>>>,
    // Batch-model ASR admitted and loaded at press time (see `early_asr`).
    early: Arc<Mutex<Option<crate::early_asr::EarlyAsr>>>,
    recovery: Arc<Mutex<Option<LiveRecovery>>>,
    /// Resident live-pair helper; owned here, not by the processing queue.
    live_pair: crate::live_pair::LivePair,
    /// Event tap for the one-key shortcut (🌐 fn or Right ⌥), started on first use.
    key_tap: Arc<Mutex<Option<crate::key_gesture::tap::KeyTap>>>,
    /// Whether key + → / ← switches the output language (translation is on).
    language_combo: Arc<AtomicBool>,
    /// The current dictation was started from the app's own window (the
    /// Translate screen's microphone): its text stays in the app, unpasted.
    in_app: Arc<AtomicBool>,
}

/// Runs one-key gestures in order on a worker thread, off the event tap.
struct GestureSink {
    actions: Mutex<std::sync::mpsc::Sender<crate::key_gesture::Action>>,
    status: Arc<Mutex<QuickDictationStatusResponse>>,
    language_combo: Arc<AtomicBool>,
}

impl GestureSink {
    fn new(controller: QuickDictationController) -> Self {
        use crate::key_gesture::{Action, Arrow};
        let (tx, rx) = std::sync::mpsc::channel::<Action>();
        let status = controller.status.clone();
        let language_combo = controller.language_combo.clone();
        thread::Builder::new()
            .name("murmela-key-actions".into())
            .spawn(move || {
                for action in rx {
                    if controller.is_suspended.lock().map(|value| *value).unwrap_or(true) {
                        continue;
                    }
                    let result = match action {
                        Action::StartHold | Action::StartHandsFree => controller.begin_listening(),
                        Action::StopHold | Action::StopHandsFree => controller.finish_dictation(),
                        Action::Cycle(arrow) => {
                            let changed = match arrow {
                                Arrow::Next => controller.translation.cycle(),
                                Arrow::Previous => controller.translation.cycle_back(),
                            };
                            if let Err(error) = changed {
                                let _ = controller.app.emit("dictation-mode-error", error.to_string());
                            }
                            Ok(())
                        }
                    };
                    if let Err(error) = result {
                        eprintln!("[dictation] key gesture failed: {error}");
                        if matches!(action, Action::StartHold | Action::StartHandsFree) {
                            controller.explain_rejected_press(&error);
                        }
                    }
                }
            })
            .expect("key action thread");
        Self {
            actions: Mutex::new(tx),
            status,
            language_combo,
        }
    }
}

impl crate::key_gesture::tap::Sink for GestureSink {
    fn act(&self, action: crate::key_gesture::Action) {
        if let Ok(sender) = self.actions.lock() {
            let _ = sender.send(action);
        }
    }
    fn dictating(&self) -> bool {
        self.status
            .lock()
            .map(|status| status.state == QuickDictationState::Listening)
            .unwrap_or(false)
    }
    fn language_combo_enabled(&self) -> bool {
        self.language_combo.load(Ordering::SeqCst)
    }
}

#[derive(Clone)]
struct LiveDictation {
    session_id: String,
    settings: AppSettings,
    vocabulary: Vec<vocabulary::VocabularyTerm>,
    handle: LiveHandle,
}

/// A streaming shortcut dictation on the live pair.
#[derive(Clone)]
enum LiveHandle {
    Pair(crate::live_pair::LiveSession),
}
impl LiveHandle {
    fn activate(&self) {
        match self {
            Self::Pair(handle) => handle.activate(),
        }
    }
    fn cancel(&self) {
        match self {
            Self::Pair(handle) => handle.cancel(),
        }
    }
    fn model_name(&self) -> &'static str {
        match self {
            Self::Pair(_) => crate::live_pair::MODEL_NAME,
        }
    }
}

enum LiveEngine {
    Pair {
        config: crate::live_pair::Config,
        language: String,
    },
}
#[derive(Clone)]
struct LiveRecovery {
    recording: crate::audio_capture::RecordingResult,
    settings: AppSettings,
    vocabulary: Vec<vocabulary::VocabularyTerm>,
}

impl QuickDictationController {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        app: AppHandle,
        engine: Arc<dyn TranscriptionEngine>,
        recording_controller: RecordingController,
        db_path: std::path::PathBuf,
        desktop_shell: DesktopShellController,
        sound_player: Arc<Option<SoundPlayer>>,
        translation: crate::translation::TranslationService,
        live_pair: crate::live_pair::LivePair,
    ) -> Self {
        Self {
            app,
            translation,
            live_pair,
            shortcut_pair: Default::default(),
            cancel_key: Default::default(),
            last_output: Default::default(),
            cycle_pressed: Default::default(),
            live: Default::default(),
            early: Default::default(),
            recovery: Default::default(),
            engine,
            recording_controller,
            db_path,
            desktop_shell,
            sound_player,
            status: Arc::new(Mutex::new(QuickDictationStatusResponse::default())),
            is_suspended: Arc::new(Mutex::new(false)),
            registered_shortcut: Arc::new(Mutex::new(None)),
            paste_target: Arc::new(Mutex::new(None)),
            volume_snapshot: Arc::new(Mutex::new(None)),
            poller_generation: Arc::new(AtomicU64::new(0)),
            state_since_ms: Arc::new(AtomicI64::new(chrono::Utc::now().timestamp_millis())),
            key_tap: Default::default(),
            language_combo: Default::default(),
            in_app: Default::default(),
        }
    }

    /// Starts, retargets or pauses the one-key shortcut for `settings.shortcut`.
    fn sync_key_gesture(&self, settings: &AppSettings, paused: bool) -> Result<()> {
        use crate::key_gesture::tap::{Key, KeyTap};
        use crate::key_gesture::{FN_KEY, RIGHT_OPTION_KEY};
        self.language_combo.store(
            settings.translation_enabled && crate::translation::platform_supported(),
            Ordering::SeqCst,
        );
        let key = match settings.shortcut.as_str() {
            FN_KEY => Some(Key::Fn),
            RIGHT_OPTION_KEY => Some(Key::RightOption),
            _ => None,
        }
        .filter(|_| !paused);
        let mut tap = self
            .key_tap
            .lock()
            .map_err(|_| anyhow!("Shortcut state unavailable"))?;
        if key.is_some() && tap.is_none() {
            // Creating the tap without Accessibility access makes macOS show
            // its "control this computer" prompt with no context. The app
            // explains the permission first; readiness polling starts the
            // tap once it is granted.
            if !crate::insertion::accessibility_trusted() {
                anyhow::bail!(
                    "KEY_TAP_UNAVAILABLE: Allow Murmela under Privacy & Security → Accessibility to dictate with the 🌐 or Right ⌥ key."
                );
            }
            *tap = Some(KeyTap::start(GestureSink::new(self.clone()))?);
        }
        if let Some(tap) = tap.as_ref() {
            tap.set_key(key);
        }
        Ok(())
    }

    pub fn status(&self) -> QuickDictationStatusResponse {
        self.status
            .lock()
            .map(|status| status.clone())
            .unwrap_or_default()
    }

    fn register_pair(&self, pair: &[(ShortcutRole, String)]) -> Result<()> {
        for (role, shortcut) in pair.iter() {
            let role = *role;
            let controller = self.clone();
            self.app.global_shortcut().on_shortcut(
                shortcut.as_str(),
                move |_app, _shortcut, event| {
                    if controller
                        .is_suspended
                        .lock()
                        .map(|value| *value)
                        .unwrap_or(true)
                    {
                        return;
                    }
                    match role {
                        ShortcutRole::CycleLanguage => {
                            if cycle_key_transition(&controller.cycle_pressed, event.state()) {
                                if let Err(error) = controller.translation.cycle() {
                                    let _ = controller
                                        .app
                                        .emit("dictation-mode-error", error.to_string());
                                }
                            }
                        }
                        // On release, so the held modifiers do not reach the paste.
                        ShortcutRole::PasteLast => {
                            if event.state() == ShortcutState::Released {
                                let controller = controller.clone();
                                thread::spawn(move || {
                                    if let Err(error) = controller.paste_last() {
                                        eprintln!("[dictation] paste last failed: {error:#}");
                                    }
                                });
                            }
                        }
                        ShortcutRole::Dictation => {
                            if let Err(error) =
                                controller.handle_shortcut_event(event.state(), event.id)
                            {
                                eprintln!("[dictation] shortcut failed: {error}");
                                controller.explain_rejected_press(&error);
                            }
                        }
                    }
                },
            )?;
        }
        Ok(())
    }
    /// Keep the live-pair helper resident exactly while it is the selected,
    /// installed shortcut model.
    pub fn sync_live_pair(&self) {
        let Ok(settings) = storage::get_settings_from_db_path(&self.db_path) else {
            return;
        };
        self.sync_live_pair_with(&settings);
    }
    fn sync_live_pair_with(&self, settings: &AppSettings) {
        if settings.shortcut_dictation_selected_model_id.as_deref()
            != Some(crate::live_pair::MODEL_ID)
        {
            self.live_pair.configure(None);
            return;
        }
        match crate::live_pair::config_for(&self.app, &self.translation.models_dir(), settings) {
            Ok(config) => self.live_pair.configure(Some(config)),
            Err(error) => self.live_pair.report_setup_error(error.to_string()),
        }
    }

    pub fn sync_shortcut_registration(&self) -> Result<QuickDictationStatusResponse> {
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        self.sync_live_pair_with(&settings);
        // A settings/model change must not leave a previous helper resident.
        self.translation.processing_queue().evict_idle();
        let mut registered = self
            .shortcut_pair
            .lock()
            .map_err(|_| anyhow!("Shortcut state unavailable"))?;
        let one_key = crate::key_gesture::is_key_gesture(&settings.shortcut);
        let mut entries = Vec::new();
        if !one_key {
            entries.push((ShortcutRole::Dictation, settings.shortcut.as_str()));
        }
        if settings.translation_enabled && crate::translation::platform_supported() {
            entries.push((
                ShortcutRole::CycleLanguage,
                settings.translation_cycle_shortcut.as_str(),
            ));
        }
        if !settings.paste_last_shortcut.trim().is_empty() {
            entries.push((ShortcutRole::PasteLast, settings.paste_last_shortcut.as_str()));
        }
        let next = shortcut_set(&entries)?;
        if *self
            .is_suspended
            .lock()
            .map_err(|_| anyhow!("Shortcut state unavailable"))?
        {
            return Ok(self.status());
        }
        let previous = registered.clone();
        self.app.global_shortcut().unregister_all()?;
        if let Err(error) = self.register_pair(&next) {
            let _ = self.app.global_shortcut().unregister_all();
            if let Err(restore) = self.register_pair(&previous) {
                let _ = self.app.global_shortcut().unregister_all();
                registered.clear();
                self.update_status(|status| status.is_registered = false)?;
                return Err(anyhow!(
                    "{error}; could not restore previous shortcuts: {restore}"
                ));
            }
            return Err(error);
        }
        *registered = next;
        self.cycle_pressed.store(false, Ordering::SeqCst);
        if let Err(error) = self.sync_key_gesture(&settings, false) {
            self.update_status(|status| {
                status.registered_shortcut = Some(settings.shortcut.clone());
                status.is_registered = false;
                status.last_error_message = Some(error.to_string());
            })?;
            return Err(error);
        }
        *self
            .registered_shortcut
            .lock()
            .map_err(|_| anyhow!("Shortcut state unavailable"))? = Some(settings.shortcut.clone());
        self.update_status(|status| {
            status.registered_shortcut = Some(settings.shortcut.clone());
            status.shortcut_mode = settings.shortcut_mode;
            status.is_registered = true;
            status.last_error_message = None;
        })?;
        // Re-registering cleared every shortcut, including a live Escape key.
        self.sync_cancel_key();
        Ok(self.status())
    }

    pub fn suspend_shortcut_registration(&self) -> Result<QuickDictationStatusResponse> {
        if self.translation.snapshot().busy {
            return Err(anyhow!(
                "Finish or cancel dictation before changing shortcuts."
            ));
        }
        let registration = self
            .shortcut_pair
            .lock()
            .map_err(|_| anyhow!("Shortcut state unavailable"))?;
        self.cycle_pressed.store(false, Ordering::SeqCst);
        if let Ok(mut suspended) = self.is_suspended.lock() {
            *suspended = true;
        }
        if let Ok(settings) = storage::get_settings_from_db_path(&self.db_path) {
            let _ = self.sync_key_gesture(&settings, true);
        }
        if let Err(error) = self.app.global_shortcut().unregister_all() {
            if let Ok(mut suspended) = self.is_suspended.lock() {
                *suspended = false;
            }
            let _ = self.app.global_shortcut().unregister_all();
            if let Err(restore) = self.register_pair(&registration) {
                self.update_status(|status| status.is_registered = false)?;
                return Err(anyhow!("{error}; restoring shortcuts failed: {restore}"));
            }
            return Err(error.into());
        }
        self.update_status(|status| {
            status.is_registered = false;
        })?;
        Ok(self.status())
    }

    pub fn resume_shortcut_registration(&self) -> Result<QuickDictationStatusResponse> {
        if let Ok(mut suspended) = self.is_suspended.lock() {
            *suspended = false;
        }
        self.sync_shortcut_registration()
    }

    fn handle_shortcut_event(
        &self,
        shortcut_state: ShortcutState,
        _shortcut_id: u32,
    ) -> Result<()> {
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        match settings.shortcut_mode {
            ShortcutMode::PushToTalk => match shortcut_state {
                ShortcutState::Pressed => self.begin_listening(),
                ShortcutState::Released => self.finish_dictation(),
            },
            ShortcutMode::Toggle => {
                if shortcut_state == ShortcutState::Pressed {
                    if self.status().state == QuickDictationState::Listening {
                        self.finish_dictation()
                    } else {
                        self.begin_listening()
                    }
                } else {
                    Ok(())
                }
            }
        }
    }

    fn begin_listening(&self) -> Result<()> {
        self.begin_listening_as(false)
    }

    /// Starts a dictation from the app's own window. The text streams to the
    /// overlay as usual and ends in `last_transcript_text` instead of being
    /// pasted. Returns the session id the overlay events carry.
    pub fn start_in_app(&self) -> Result<String> {
        if self.status().state == QuickDictationState::Listening {
            return Err(anyhow!("A dictation is already running."));
        }
        self.begin_listening_as(true)?;
        Ok(self.translation.current()?.id)
    }

    pub fn finish_in_app(&self) -> Result<()> {
        self.finish_dictation()
    }

    fn begin_listening_as(&self, in_app: bool) -> Result<()> {
        let _work = crate::shutdown::begin_work(true)?;
        match self.status().state {
            QuickDictationState::Listening => return Ok(()),
            QuickDictationState::Processing => {
                return Err(anyhow!("Dictation is still transcribing."))
            }
            _ => {}
        }
        if self
            .recording_controller
            .status()
            .map(|status| {
                matches!(
                    status.state,
                    crate::audio_capture::RecordingOverlayState::Listening
                )
            })
            .unwrap_or(false)
        {
            return Err(anyhow!("Another recording is already active."));
        }

        let mut session = self.translation.begin()?;
        if in_app {
            // The app's text box takes the words as spoken; it translates them itself.
            session.mode = crate::translation::OutputMode::Original;
        }
        let generation = {
            let _status = self
                .status
                .lock()
                .map_err(|_| anyhow!("Dictation state unavailable"))?;
            self.poller_generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        let reservation = self.translation.guard(&session);
        let settings = Some(storage::get_settings_from_db_path(&self.db_path)?);
        let selected = settings
            .as_ref()
            .and_then(|s| s.shortcut_dictation_selected_model_id.as_deref());
        let live_setup = if selected == Some(crate::live_pair::MODEL_ID) {
            // No queue admission: the models are resident, so pressing the
            // shortcut only resets streaming state.
            let settings = settings.as_ref().unwrap().clone();
            let setup = crate::live_pair::config_for(
                &self.app,
                &self.translation.models_dir(),
                &settings,
            )
            .and_then(|config| Ok((config, crate::live_pair::source_language(&settings)?)));
            let (config, language) = setup.inspect_err(|error| {
                let _ = self.set_error(error.to_string());
            })?;
            let terms = vocabulary::list_vocabulary_terms_from_db_path(&self.db_path)?;
            Some((settings, terms, LiveEngine::Pair { config, language }))
        } else {
            None
        };
        let early_request = match (&live_setup, &settings) {
            (None, Some(settings)) => self.early_asr_request(settings),
            _ => None,
        };
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.prepare_capture(
                settings
                    .as_ref()
                    .map(|settings| settings.sounds_enabled)
                    .unwrap_or(false),
            )?;
        }

        if settings
            .as_ref()
            .map(|settings| settings.volume_ducking_enabled)
            .unwrap_or(false)
        {
            self.duck_system_volume();
        }

        if let Ok(mut paste_target) = self.paste_target.lock() {
            *paste_target = if in_app { None } else { detect_frontmost_paste_target() };
        }
        self.in_app.store(in_app, Ordering::SeqCst);

        let capture = match self.recording_controller.start() {
            Ok(capture) => capture,
            Err(error) => {
                self.restore_system_volume();
                // Surface the failure (overlay "Failed" + status + log) so a wedged
                // microphone never silently swallows the dictation, then reset.
                let _ = self.set_error(error.to_string());
                return Err(error);
            }
        };
        session.recording_id = capture.current_session_id.clone();
        if let Some(id) = &session.recording_id {
            self.translation.bind_recording(id.clone(), None);
        }
        *self
            .recovery
            .lock()
            .map_err(|_| anyhow!("Recovery state unavailable"))? = None;
        if crate::shutdown::is_shutting_down() {
            let _ = self.recording_controller.cancel();
            self.restore_system_volume();
            return Ok(());
        }
        self.desktop_shell
            .set_overlay_payload(DictationOverlayPayload {
                phase: OverlayPhase::Listening,
                audio_level: 0.0,
                session_id: Some(session.id.clone()),
                // The live pair is resident, so it listens at once.
                streaming_state: live_setup.as_ref().map(|(_, _, engine)| match engine {
                    LiveEngine::Pair { .. } => crate::desktop_shell::StreamingState::Listening,
                }),
                ..Default::default()
            })?;
        self.update_status(|status| {
            status.state = QuickDictationState::Listening;
            status.last_error_message = None;
            status.last_transcript_text = None;
            status.last_transcript_id = None;
            status.last_insert_outcome = None;
            status.last_insert_warning = None;
            status.can_retry_streaming = false;
        })?;
        // Bump the generation so any previous poller exits, then start the one
        // poller that belongs to this listening session.
        self.spawn_overlay_level_poller(generation, session.id.clone());
        if let Some((settings, terms, engine)) = live_setup {
            let tap = match self.recording_controller.live_tap() {
                Ok(tap) => tap,
                Err(error) => {
                    let _ = self.recording_controller.cancel();
                    let _ = self.set_error_owned(error.to_string(), generation);
                    return Err(error);
                }
            };
            let controller = self.clone();
            let owner = session.clone();
            let stop = Arc::new(move || {
                if controller.poller_generation.load(Ordering::SeqCst) == generation
                    && controller.translation.ensure_active(&owner).is_ok()
                {
                    let _ = controller.finish_dictation();
                }
            });
            let handle = match engine {
                LiveEngine::Pair { config, language } => {
                    LiveHandle::Pair(crate::live_pair::start(
                        session.id.clone(),
                        tap,
                        session.cancelled.clone(),
                        self.live_pair.clone(),
                        config,
                        self.desktop_shell.clone(),
                        language,
                        stop,
                    )?)
                }
            };
            *self
                .live
                .lock()
                .map_err(|_| anyhow!("Live dictation state unavailable"))? = Some(LiveDictation {
                session_id: session.id.clone(),
                settings,
                vocabulary: terms,
                handle: handle.clone(),
            });
            handle.activate();
        }
        if let Some(request) = early_request {
            // Without a tap the model is still loaded; only the
            // while-recording transcription of long dictations is skipped.
            let tap = self.recording_controller.observe_tap().ok();
            match crate::early_asr::EarlyAsr::start(
                self.translation.clone(),
                session.clone(),
                tap,
                request,
                std::env::temp_dir(),
            ) {
                Ok(early) => match self.early.lock() {
                    Ok(mut slot) => *slot = Some(early),
                    Err(_) => early.cancel(),
                },
                Err(error) => eprintln!("[dictation] early model loading unavailable: {error:#}"),
            }
        }
        reservation.disarm();
        Ok(())
    }

    /// The ASR request for a batch-model shortcut dictation, built from the
    /// settings at press time. `None` when the selected model has nothing to
    /// load ahead of release (or cannot run); release then decides as before.
    fn early_asr_request(&self, settings: &AppSettings) -> Option<FileTranscriptionRequest> {
        let vocabulary_prompt = vocabulary::build_asr_prompt_from_db_path(&self.db_path)
            .ok()
            .flatten();
        let request = shortcut_asr_request(settings, vocabulary_prompt.as_ref(), String::new());
        match self.translation.batch_model_for(&request) {
            Ok(model) if crate::asr::engine_supports_preload(&model.engine) => Some(request),
            Ok(_) => None,
            Err(error) => {
                eprintln!("[dictation] model cannot be loaded early: {error:#}");
                None
            }
        }
    }

    fn take_early(&self, session_id: &str) -> Option<crate::early_asr::EarlyAsr> {
        let mut early = self.early.lock().ok()?;
        if early
            .as_ref()
            .is_some_and(|early| early.session_id() == session_id)
        {
            early.take()
        } else {
            None
        }
    }

    fn finish_dictation(&self) -> Result<()> {
        let Ok(session) = self.translation.current() else {
            return Ok(());
        };
        let generation = self.translation.with_active(&session, || {
            let mut status = self
                .status
                .lock()
                .map_err(|_| anyhow!("Dictation state unavailable"))?;
            if status.state != QuickDictationState::Listening {
                return Ok(None);
            }
            status.state = QuickDictationState::Processing;
            status.last_error_message = None;
            self.state_since_ms.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
            drop(status);
            self.restore_system_volume();
            self.desktop_shell
                .processing_for_session(&session.id, "Finishing", false)?;
            self.update_status(|_| {})?;
            Ok(Some(self.poller_generation.load(Ordering::SeqCst)))
        })?;
        let Some(generation) = generation else {
            return Ok(());
        };

        let work = crate::shutdown::begin_work(true)?;
        let controller = self.clone();
        thread::spawn(move || {
            let _work = work;
            if let Err(error) = controller.finish_dictation_worker(generation, session) {
                if controller.poller_generation.load(Ordering::SeqCst) != generation {
                    return;
                }
                eprintln!("[dictation] finish worker failed: {error:?}");
                // Silence is not a failure: say so neutrally instead of an error chip.
                if error.to_string().contains("TRANSCRIPTION_EMPTY") {
                    controller.restore_system_volume();
                    let _ = controller.update_status(|status| {
                        status.state = QuickDictationState::Idle;
                    });
                    let _ = controller.desktop_shell.flash(
                        OverlayPhase::Notice,
                        "No speech heard",
                        NOTICE_CHIP_HOLD,
                    );
                    return;
                }
                let _ = controller.set_error_owned(error.to_string(), generation);
            }
        });
        Ok(())
    }

    fn finish_dictation_worker(
        &self,
        generation: u64,
        session: crate::translation::DictationSession,
    ) -> Result<()> {
        let _session_guard = self.translation.guard(&session);
        if self.poller_generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let in_app = self.in_app.load(Ordering::SeqCst);
        let recording = match self.recording_controller.stop_owned(
            session
                .recording_id
                .as_deref()
                .ok_or_else(|| anyhow!("Missing capture identity"))?,
        ) {
            Ok(result) => result,
            Err(error) => {
                if let Some(early) = self.take_early(&session.id) {
                    early.cancel();
                }
                if let Ok(mut live) = self.live.lock() {
                    if live
                        .as_ref()
                        .is_some_and(|live| live.session_id == session.id)
                    {
                        if let Some(live) = live.take() {
                            live.handle.cancel();
                        }
                    }
                }
                return Err(error);
            }
        };

        if self.poller_generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        let live = {
            let mut live = self
                .live
                .lock()
                .map_err(|_| anyhow!("Live dictation state unavailable"))?;
            if live
                .as_ref()
                .is_some_and(|live| live.session_id == session.id)
            {
                live.take()
            } else {
                None
            }
        };
        if live.is_some() {
            self.translation
                .phase(&session, "streaming_finishing", "Finishing")?;
        }
        let settings = match &live {
            Some(live) => live.settings.clone(),
            None => storage::get_settings_from_db_path(&self.db_path)?,
        };
        // Keep recovery material even when streaming failed before stop. Retry
        // is explicit and must use a copy action rather than the old paste target.
        self.translation.with_active(&session, || {
            self.update_status(|status| {
                status.last_recording_path = Some(recording.file_path.clone());
                status.last_duration_ms = Some(recording.duration_ms);
            })
        })?;
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.finish_capture(settings.sounds_enabled, false);
        }
        let resolved_model_name = if let Some(live) = &live {
            Some(live.handle.model_name().into())
        } else {
            resolve_model_name(self.engine.as_ref(), &settings)?
        };
        let _permit;
        let corrected = if let Some(LiveDictation {
            handle: LiveHandle::Pair(handle),
            settings: live_settings,
            vocabulary: terms,
            ..
        }) = &live
        {
            let completed = match handle.finish(&recording.session_id, recording.sample_count) {
                Ok(completed) => completed,
                Err(error) => {
                    let _ = self.translation.with_active(&session, || {
                        *self
                            .recovery
                            .lock()
                            .map_err(|_| anyhow!("Recovery unavailable"))? = Some(LiveRecovery {
                            recording: recording.clone(),
                            settings: live_settings.clone(),
                            vocabulary: terms.clone(),
                        });
                        self.update_status(|status| status.can_retry_streaming = true)?;
                        Ok(())
                    });
                    handle.cancel();
                    return Err(error);
                }
            };
            self.translation.ensure_active(&session)?;
            // The helper holds no admission; translation still must not run
            // beside another heavy job.
            if session.mode != crate::translation::OutputMode::Original {
                _permit = self.translation.acquire(&session)?;
            }
            vocabulary::correct_transcript_with_terms(
                terms,
                crate::live_pair::transcript(
                    &session.id,
                    completed.text,
                    recording.duration_ms,
                    completed.language,
                    completed.warning,
                ),
            )?
        } else {
            // The model was admitted and loaded at press time; take over the
            // worker (and anything already transcribed). If that preparation
            // failed, fall back to loading on release.
            let prepared = match self.take_early(&session.id) {
                Some(early) => {
                    self.translation
                        .phase(&session, "waiting", "Waiting for local processing…")?;
                    match early.finish(&self.translation, &session) {
                        Ok(handoff) => Some(handoff),
                        Err(error) => {
                            self.translation.ensure_active(&session)?;
                            eprintln!(
                                "[dictation] early model unavailable, loading on release: {error:#}"
                            );
                            None
                        }
                    }
                }
                None => None,
            };
            let vocabulary_prompt = vocabulary::build_asr_prompt_from_db_path(&self.db_path)?;
            if let Some(prompt) = &vocabulary_prompt {
                eprintln!(
                    "[dictation] dictionary prompt enabled: included={} truncated={}",
                    prompt.included_count, prompt.truncated_count
                );
            }
            let request = shortcut_asr_request(
                &settings,
                vocabulary_prompt.as_ref(),
                recording.file_path.clone(),
            );
            let transcript = if let Some(handoff) = prepared {
                self.translation
                    .phase(&session, "transcribing", "Transcribing")?;
                let (transcript, permit) = crate::early_asr::complete(
                    &self.translation,
                    &session,
                    request,
                    handoff,
                    &std::env::temp_dir(),
                )?;
                _permit = permit;
                transcript
            } else {
                let (permit, warm_worker) = self.translation.acquire_for_asr(&session)?;
                _permit = permit;
                self.translation
                    .transcribe(&session, request, &_permit, warm_worker)?
            };

            match vocabulary::correct_transcript_result(&self.db_path, transcript) {
                Ok(result) => result,
                Err(error) => {
                    return Err(error);
                }
            }
        };

        if self.poller_generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        self.translation.with_active(&session, || {
            self.desktop_shell
                .finalize_stream_text(&session.id, corrected.plain_text.clone())
        })?;
        let output = self
            .translation
            .process(&session, &corrected, recording.duration_ms)?;
        self.translation.ensure_active(&session)?;
        let translated = session.mode != crate::translation::OutputMode::Original;
        let mut saved_transcript_id = output.transcript_id.clone();
        // A translation that failed its checks twice falls back to the original.
        let fallback_warning = (output.status != "completed" && output.fallback_to_original)
            .then(|| {
                output
                    .error_message
                    .clone()
                    .unwrap_or_else(|| "Translation check failed. The original was used.".into())
            });
        if output.status != "completed" && fallback_warning.is_none() {
            self.translation.with_active(&session, || {
                self.update_status(|status| {
                    status.last_transcript_text = Some(corrected.plain_text.clone());
                    status.last_transcript_id = saved_transcript_id.clone();
                })
            })?;
            return Err(anyhow!(output
                .error_message
                .unwrap_or_else(|| "Translation failed.".into())));
        }
        let output_text = if fallback_warning.is_some() {
            corrected.plain_text.clone()
        } else {
            output.output_text.unwrap_or_default()
        };
        if output_text.trim().is_empty() {
            self.translation.with_active(&session, || {
                self.update_status(|status| status.state = QuickDictationState::Idle)?;
                self.desktop_shell
                    .flash(OverlayPhase::Notice, "No speech heard", NOTICE_CHIP_HOLD)
            })?;
            return Ok(());
        }
        if let Ok(mut last) = self.last_output.lock() {
            *last = Some(output_text.clone());
        }
        self.translation.ensure_active(&session)?;
        if in_app {
            // The app shows the text itself (and keeps it in its own Library
            // entry): nothing to paste, save or flash.
            self.translation.with_active(&session, || {
                self.desktop_shell
                    .set_overlay_payload(DictationOverlayPayload::default())?;
                self.update_status(|status| {
                    status.state = QuickDictationState::Idle;
                    status.last_transcript_text = Some(output_text.clone());
                    status.last_transcript_id = None;
                    status.last_recording_path = Some(recording.file_path.clone());
                    status.last_error_message = None;
                    status.last_model_name = resolved_model_name.clone();
                    status.last_insert_outcome = None;
                    status.last_insert_warning = fallback_warning.clone();
                    status.last_duration_ms = Some(recording.duration_ms);
                })
            })?;
            crate::sound::notify(
                &self.app,
                crate::sound::FeedbackCue::Complete,
                &format!("dictation:{}", recording.file_path),
            );
            self.refresh_tray_recents();
            record_dictation_stats(&self.app, &self.db_path, &output_text, recording.duration_ms);
            return Ok(());
        }
        if !translated && session.save_history {
            saved_transcript_id = self
                .translation
                .with_active(&session, || {
                    storage::save_quick_dictation_transcript(
                        &self.db_path,
                        &corrected,
                        recording.duration_ms,
                    )
                })
                .ok()
                .map(|s| s.id);
        }

        let insert_report = match self.perform_insertion_on_main_thread(
            output_text.clone(),
            settings.insert_behavior,
            generation,
            &session,
        ) {
            Ok(outcome) => outcome,
            Err(error) => {
                if self.poller_generation.load(Ordering::SeqCst) != generation {
                    return Ok(());
                }
                self.translation.ensure_active(&session)?;
                if saved_transcript_id.is_none() && !translated && session.save_history {
                    saved_transcript_id = self
                        .translation
                        .with_active(&session, || {
                            storage::save_quick_dictation_transcript(
                                &self.db_path,
                                &corrected,
                                recording.duration_ms,
                            )
                        })
                        .ok()
                        .map(|s| s.id);
                }
                self.translation.with_active(&session, || {
                    self.update_status(|status| {
                        status.last_transcript_text = Some(output_text.clone());
                        status.last_transcript_id = saved_transcript_id.clone();
                        status.last_recording_path = Some(recording.file_path.clone());
                        status.last_model_name = resolved_model_name.clone();
                        status.last_duration_ms = Some(recording.duration_ms);
                    })
                })?;
                return Err(error);
            }
        };
        let insert_outcome = insert_report.outcome;
        let insert_outcome_is_pasted = matches!(insert_outcome, InsertionOutcome::Pasted);

        if self.poller_generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        if saved_transcript_id.is_none()
            && !translated
            && session.save_history
            && matches!(insert_outcome, InsertionOutcome::ClipboardOnly)
        {
            saved_transcript_id = self
                .translation
                .with_active(&session, || {
                    storage::save_quick_dictation_transcript(
                        &self.db_path,
                        &corrected,
                        recording.duration_ms,
                    )
                })
                .ok()
                .map(|s| s.id);
        }

        let next_state = match insert_outcome {
            InsertionOutcome::Pasted => QuickDictationState::Inserted,
            InsertionOutcome::ClipboardOnly => QuickDictationState::ClipboardOnly,
        };
        let result_phase = match insert_outcome {
            InsertionOutcome::Pasted => OverlayPhase::Inserted,
            InsertionOutcome::ClipboardOnly => OverlayPhase::ClipboardOnly,
        };
        self.translation.with_active(&session, || {
            self.update_status(|status| {
                status.state = next_state;
                status.last_transcript_text = Some(output_text.clone());
                status.last_transcript_id = saved_transcript_id.clone();
                status.last_recording_path = Some(recording.file_path.clone());
                status.last_error_message = None;
                status.last_model_name = resolved_model_name.clone();
                status.last_insert_outcome = Some(insert_outcome);
                status.last_insert_warning = insert_report
                    .warning
                    .map(|warning| warning.message().to_string())
                    .or_else(|| fallback_warning.clone());
                status.last_duration_ms = Some(recording.duration_ms);
            })
        })?;
        // Flash the result on the overlay so users get feedback even when
        // they're focused on a different app (the in-window toast can't reach
        // them there). The hide is scheduled in `schedule_idle_reset`.
        self.translation.with_active(&session, || {
            self.desktop_shell
                .set_overlay_payload(DictationOverlayPayload {
                    phase: result_phase,
                    audio_level: 0.0,
                    status_text: insert_report
                        .warning
                        .map(|warning| warning.overlay_label().to_string())
                        .or_else(|| {
                            fallback_warning
                                .as_ref()
                                .map(|_| "Original pasted · translation check failed".to_string())
                        })
                        .or_else(|| {
                            (!settings.serious_mode
                                && insert_outcome_is_pasted
                                && is_blah_blah(&output_text))
                            .then(|| "Very profound. Pasted anyway. 😉".to_string())
                        }),
                    duration_limit_reached: recording.duration_ms as u64
                        >= DICTATION_CAPTURE_LIMIT_MS - 1_000,
                    ..Default::default()
                })
        })?;
        crate::sound::notify(
            &self.app,
            crate::sound::FeedbackCue::Complete,
            &format!("dictation:{}", recording.file_path),
        );
        self.schedule_idle_reset();
        self.refresh_tray_recents();
        record_dictation_stats(&self.app, &self.db_path, &output_text, recording.duration_ms);
        Ok(())
    }

    fn perform_insertion_on_main_thread(
        &self,
        text: String,
        behavior: crate::settings::InsertBehavior,
        generation: u64,
        session: &crate::translation::DictationSession,
    ) -> Result<InsertionReport> {
        let app = self.app.clone();
        let desktop_shell = self.desktop_shell.clone();
        let paste_target = self
            .paste_target
            .lock()
            .ok()
            .and_then(|target| target.clone());
        let (response_tx, response_rx) = mpsc::channel();
        let active_generation = self.poller_generation.clone();
        let service = self.translation.clone();
        let session = session.clone();
        let canceled = session.cancelled.clone();
        self.app.run_on_main_thread(move || {
            if crate::shutdown::is_shutting_down()
                || active_generation.load(Ordering::SeqCst) != generation
                || service.ensure_active(&session).is_err()
            {
                let _ = response_tx.send(Err("Dictation was reset.".to_string()));
                return;
            }
            if let Err(error) = service.with_active(&session, || {
                desktop_shell.set_overlay_payload(DictationOverlayPayload::default())
            }) {
                let _ = response_tx.send(Err(error.to_string()));
                return;
            }
            let result =
                crate::insertion::insert_text(&app, &text, behavior, paste_target.as_ref(), || {
                    if active_generation.load(Ordering::SeqCst) != generation {
                        return Err(anyhow!("Dictation was reset."));
                    }
                    service.ensure_active(&session)
                })
                .map_err(|error| error.to_string());
            let _ = response_tx.send(result);
        })?;

        response_rx
            .recv_timeout(Duration::from_secs(5))
            .inspect_err(|_| canceled.store(true, Ordering::SeqCst))
            .map_err(|_| anyhow!("timed out while inserting shortcut dictation"))?
            .map_err(anyhow::Error::msg)
    }

    /// Explicit recovery returns text for the user to copy. It never uses the
    /// original application's paste target or changes the clipboard.
    pub fn retry_streaming(&self) -> Result<QuickDictationStatusResponse> {
        let _work = crate::shutdown::begin_work(true)?;
        let recovery = self
            .recovery
            .lock()
            .map_err(|_| anyhow!("Recovery unavailable"))?
            .clone()
            .ok_or_else(|| anyhow!("No failed live recording is available."))?;
        let session = self.translation.begin()?;
        let _guard = self.translation.guard(&session);
        let generation = {
            let _status = self
                .status
                .lock()
                .map_err(|_| anyhow!("Dictation state unavailable"))?;
            self.poller_generation.fetch_add(1, Ordering::SeqCst) + 1
        };
        let result = (|| -> Result<()> {
            let prepared = crate::audio_preprocess::decode_audio_file(std::path::Path::new(
                &recovery.recording.file_path,
            ))?;
            if prepared.samples.len() > crate::live_pair::MAX_SAMPLES {
                return Err(anyhow!("LIVE_PAIR_LIMIT: This recording exceeds five minutes."));
            }
            let count = prepared.samples.len();
            let tap =
                crate::audio_capture::CaptureTap::from_recording(session.id.clone(), prepared);
            self.translation.with_active(&session, || {
                self.update_status(|status| {
                    status.state = QuickDictationState::Processing;
                    status.last_error_message = None;
                })?;
                self.desktop_shell
                    .set_overlay_payload(DictationOverlayPayload {
                        session_id: Some(session.id.clone()),
                        phase: OverlayPhase::Processing,
                        streaming_state: Some(crate::desktop_shell::StreamingState::Preparing),
                        ..Default::default()
                    })
            })?;
            self.translation.phase(
                &session,
                "streaming_finishing",
                "Retrying live transcription",
            )?;
            let _permit;
            let source = {
                // The same live session handling, fed from the stored recording.
                let handle = crate::live_pair::start(
                    session.id.clone(),
                    tap,
                    session.cancelled.clone(),
                    self.live_pair.clone(),
                    crate::live_pair::config_for(
                        &self.app,
                        &self.translation.models_dir(),
                        &recovery.settings,
                    )?,
                    self.desktop_shell.clone(),
                    crate::live_pair::source_language(&recovery.settings)?,
                    Arc::new(|| {}),
                )?;
                handle.activate();
                let completion = match handle.finish(&session.id, count) {
                    Ok(done) => done,
                    Err(error) => {
                        handle.cancel();
                        return Err(error);
                    }
                };
                self.translation.ensure_active(&session)?;
                if session.mode != crate::translation::OutputMode::Original {
                    _permit = self.translation.acquire(&session)?;
                }
                vocabulary::correct_transcript_with_terms(
                    &recovery.vocabulary,
                    crate::live_pair::transcript(
                        &session.id,
                        completion.text,
                        recovery.recording.duration_ms,
                        completion.language,
                        completion.warning,
                    ),
                )?
            };
            let output =
                self.translation
                    .process(&session, &source, recovery.recording.duration_ms)?;
            self.translation.ensure_active(&session)?;
            if output.status != "completed" {
                return Err(anyhow!(output.error_message.unwrap_or_else(|| {
                    "Translation failed. The original text is available.".into()
                })));
            }
            let mut saved = output.transcript_id;
            if session.mode == crate::translation::OutputMode::Original && session.save_history {
                saved = Some(
                    self.translation
                        .with_active(&session, || {
                            storage::save_quick_dictation_transcript(
                                &self.db_path,
                                &source,
                                recovery.recording.duration_ms,
                            )
                        })?
                        .id,
                );
            }
            self.translation.with_active(&session, || {
                self.update_status(|status| {
                    status.state = QuickDictationState::Idle;
                    status.last_transcript_text = output.output_text.clone();
                    status.last_transcript_id = saved.clone();
                    status.last_model_name = Some(crate::live_pair::MODEL_NAME.into());
                    status.last_error_message = None;
                    status.last_insert_outcome = None;
                    status.last_insert_warning = None;
                    status.can_retry_streaming = false;
                })?;
                *self
                    .recovery
                    .lock()
                    .map_err(|_| anyhow!("Recovery unavailable"))? = None;
                self.desktop_shell.set_overlay_payload(Default::default())
            })?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = self.set_error_owned(error.to_string(), generation);
            return Err(error);
        }
        Ok(self.status())
    }

    fn spawn_overlay_level_poller(&self, generation: u64, session_id: String) {
        let controller = self.clone();
        thread::spawn(move || {
            let started = std::time::Instant::now();
            let mut warned = false;
            // Exit as soon as this poller is superseded by a newer listening
            // session OR the state leaves Listening. Both guards together make
            // it impossible to accumulate zombie pollers under intensive use.
            while controller.poller_generation.load(Ordering::SeqCst) == generation
                && controller.status().state == QuickDictationState::Listening
            {
                let level = controller.recording_controller.input_level().unwrap_or(0.0);
                let _ = controller.desktop_shell.update_level(&session_id, level);
                // Live dictation sessions enforce the same limit in their own stream.
                let live = controller
                    .live
                    .lock()
                    .map(|live| live.is_some())
                    .unwrap_or(false);
                let elapsed_ms = started.elapsed().as_millis() as u64;
                if !live && !warned && elapsed_ms >= DICTATION_LIMIT_WARNING_MS {
                    warned = true;
                    let _ = controller
                        .desktop_shell
                        .set_listening_notice(&session_id, "30 s left · 5-minute limit");
                }
                if !live && elapsed_ms >= DICTATION_CAPTURE_LIMIT_MS {
                    controller.desktop_shell.stream_duration_limit(&session_id);
                    if let Err(error) = controller.finish_dictation() {
                        eprintln!("[dictation] limit finish failed: {error:?}");
                    }
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        });
    }

    fn schedule_idle_reset(&self) {
        let revision = self.desktop_shell.overlay_revision();
        let generation = self.poller_generation.load(Ordering::SeqCst);
        let transition = self.state_since_ms.load(Ordering::SeqCst);
        self.schedule_idle_reset_at(revision, generation, transition, RESULT_CHIP_HOLD);
    }
    fn schedule_idle_reset_at(
        &self,
        revision: u64,
        generation: u64,
        transition: i64,
        hold: Duration,
    ) {
        let controller = self.clone();
        thread::spawn(move || {
            thread::sleep(hold);
            if controller.poller_generation.load(Ordering::SeqCst) != generation
                || controller.state_since_ms.load(Ordering::SeqCst) != transition
            {
                return;
            }
            let current = controller.status();
            if matches!(
                current.state,
                QuickDictationState::Inserted
                    | QuickDictationState::ClipboardOnly
                    | QuickDictationState::Error
            ) {
                let _ = controller
                    .desktop_shell
                    .set_overlay_if_revision(revision, DictationOverlayPayload::default());
                if let Err(error) = controller.update_status(|status| {
                    if controller.poller_generation.load(Ordering::SeqCst) == generation
                        && controller.state_since_ms.load(Ordering::SeqCst) == transition
                        && matches!(
                            status.state,
                            QuickDictationState::Inserted
                                | QuickDictationState::ClipboardOnly
                                | QuickDictationState::Error
                        )
                    {
                        status.state = QuickDictationState::Idle;
                    }
                }) {
                    eprintln!("[dictation] failed to reset to idle: {error:?}");
                }
            }
        });
    }

    fn set_error(&self, message: String) -> Result<()> {
        self.set_error_owned(message, self.poller_generation.load(Ordering::SeqCst))
    }
    fn set_error_owned(&self, message: String, generation: u64) -> Result<()> {
        let mut status = self
            .status
            .lock()
            .map_err(|_| anyhow!("Dictation state unavailable"))?;
        if self.poller_generation.load(Ordering::SeqCst) != generation {
            return Ok(());
        }
        self.restore_system_volume();
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.finish_capture(false, true);
        }
        crate::sound::notify(
            &self.app,
            crate::sound::FeedbackCue::Error,
            &format!(
                "dictation-error:{}",
                self.state_since_ms.load(Ordering::SeqCst)
            ),
        );
        self.desktop_shell
            .set_overlay_payload(DictationOverlayPayload {
                phase: OverlayPhase::Failed,
                audio_level: 0.0,
                status_text: Some(overlay_reason(&message).into()),
                ..Default::default()
            })?;
        status.state = QuickDictationState::Error;
        status.last_error_message = Some(message);
        let transition = chrono::Utc::now().timestamp_millis();
        self.state_since_ms.store(transition, Ordering::SeqCst);
        let revision = self.desktop_shell.overlay_revision();
        self.app.emit(QUICK_DICTATE_STATUS_EVENT, status.clone())?;
        drop(status);
        self.sync_cancel_key();
        // Auto-hide the error chip too — longer than a success, so the reason
        // can be read.
        self.schedule_idle_reset_at(revision, generation, transition, ERROR_CHIP_HOLD);
        Ok(())
    }

    fn update_status(
        &self,
        mut apply: impl FnMut(&mut QuickDictationStatusResponse),
    ) -> Result<()> {
        let next_status = {
            let mut status = self
                .status
                .lock()
                .map_err(|_| anyhow!("quick dictation status unavailable"))?;
            let previous_state = status.state;
            apply(&mut status);
            let changed = status.state != previous_state;
            if changed {
                // Stamp every transition so the watchdog can tell how long the
                // controller has been parked in Listening/Processing.
                self.state_since_ms.store(chrono::Utc::now().timestamp_millis(), Ordering::SeqCst);
            }
            (status.clone(), changed)
        };
        let (next_status, changed) = next_status;
        self.app.emit(QUICK_DICTATE_STATUS_EVENT, next_status)?;
        if changed {
            self.sync_cancel_key();
        }
        Ok(())
    }

    fn duck_system_volume(&self) {
        if self
            .volume_snapshot
            .lock()
            .map(|snapshot| snapshot.is_some())
            .unwrap_or(true)
        {
            return;
        }

        match system_volume::duck_to_30_percent_of_current() {
            Ok(Some(snapshot)) => {
                if let Ok(mut current) = self.volume_snapshot.lock() {
                    *current = Some(snapshot);
                } else if let Err(error) = system_volume::restore(snapshot) {
                    eprintln!("[volume] restore after state lock failure failed: {error:?}");
                }
            }
            Ok(None) => {}
            Err(error) => {
                eprintln!("[volume] ducking failed: {error:?}");
            }
        }
    }

    fn restore_system_volume(&self) {
        let snapshot = self
            .volume_snapshot
            .lock()
            .ok()
            .and_then(|mut snapshot| snapshot.take());

        if let Some(snapshot) = snapshot {
            if let Err(error) = system_volume::restore(snapshot) {
                eprintln!("[volume] restore failed: {error:?}");
            }
        }
    }

    /// Stop capture and insertion without re-registering shortcuts.
    pub fn prepare_shutdown(&self) {
        self.translation.cancel();
        if let Some(live) = self.live.lock().ok().and_then(|mut live| live.take()) {
            live.handle.cancel();
        }
        self.live_pair.shutdown();
        self.translation.processing_queue().evict_idle();
        self.poller_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.suspend_shortcut_registration();
        let _ = self.recording_controller.cancel();
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.finish_capture(false, true);
        }
        self.restore_system_volume();
        let _ = self.update_status(|status| status.state = QuickDictationState::Idle);
        let _ = self
            .desktop_shell
            .set_overlay_payload(DictationOverlayPayload::default());
    }

    /// Force the controller back to a clean Idle state, abandoning any wedged
    /// recording worker and tearing down overlay/volume side effects. Used by
    /// both the manual reset command and the watchdog.
    pub fn force_reset(&self) -> Result<QuickDictationStatusResponse> {
        self.translation.cancel();
        if let Some(live) = self.live.lock().ok().and_then(|mut live| live.take()) {
            live.handle.cancel();
        }
        self.translation.processing_queue().evict_idle();
        if crate::shutdown::is_shutting_down() {
            return Ok(self.status());
        }
        // Supersede any running poller and drop a possibly-wedged worker.
        self.poller_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.recording_controller.cancel();
        self.recording_controller.recover();
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.finish_capture(false, true);
        }
        self.restore_system_volume();
        let _ = self
            .desktop_shell
            .set_overlay_payload(DictationOverlayPayload::default());
        self.update_status(|status| {
            status.state = QuickDictationState::Idle;
        })?;
        // Re-arm the shortcut in case registration was affected.
        let _ = self.sync_shortcut_registration();
        Ok(self.status())
    }

    /// Registers Escape while a shortcut dictation is listening or
    /// transcribing, and releases it afterwards so other apps keep their Esc.
    fn sync_cancel_key(&self) {
        let controller = self.clone();
        // Off the caller's thread: shortcut registration hops to the main
        // thread, and this can be reached from a shortcut handler.
        thread::spawn(move || {
            let Ok(_serial) = controller.cancel_key.lock() else {
                return;
            };
            let suspended = controller.is_suspended.lock().map(|value| *value).unwrap_or(true);
            let active = !suspended
                && matches!(
                    controller.status().state,
                    QuickDictationState::Listening | QuickDictationState::Processing
                );
            let shortcuts = controller.app.global_shortcut();
            let registered = shortcuts.is_registered(CANCEL_SHORTCUT);
            if active && !registered {
                let handler = controller.clone();
                if let Err(error) = shortcuts.on_shortcut(CANCEL_SHORTCUT, move |_app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        let handler = handler.clone();
                        thread::spawn(move || {
                            if let Err(error) = handler.cancel_active() {
                                eprintln!("[dictation] cancel failed: {error:#}");
                            }
                        });
                    }
                }) {
                    eprintln!("[dictation] Escape cancel unavailable: {error}");
                }
            } else if !active && registered {
                let _ = shortcuts.unregister(CANCEL_SHORTCUT);
            }
        });
    }

    /// Cancels a listening or transcribing shortcut dictation. Nothing is
    /// pasted, and the overlay confirms it.
    pub fn cancel_active(&self) -> Result<()> {
        if !matches!(
            self.status().state,
            QuickDictationState::Listening | QuickDictationState::Processing
        ) {
            return Ok(());
        }
        self.translation.cancel();
        if let Some(live) = self.live.lock().ok().and_then(|mut live| live.take()) {
            live.handle.cancel();
        }
        if let Some(early) = self.early.lock().ok().and_then(|mut early| early.take()) {
            early.cancel();
        }
        // Supersede the poller and any finishing worker for this session.
        self.poller_generation.fetch_add(1, Ordering::SeqCst);
        let _ = self.recording_controller.cancel();
        if let Some(player) = self.sound_player.as_ref().as_ref() {
            player.finish_capture(false, true);
        }
        self.restore_system_volume();
        self.update_status(|status| {
            status.state = QuickDictationState::Idle;
            status.last_error_message = None;
        })?;
        self.desktop_shell
            .flash(OverlayPhase::Notice, "Canceled · nothing pasted", NOTICE_CHIP_HOLD)
    }

    /// Explains a shortcut press that could not start a dictation, unless the
    /// failure already showed its own error chip.
    fn explain_rejected_press(&self, error: &anyhow::Error) {
        let state = self.status().state;
        if state == QuickDictationState::Error {
            return;
        }
        let reason = overlay_reason(&error.to_string());
        if state == QuickDictationState::Processing {
            if let Ok(true) = self.desktop_shell.annotate_processing(reason) {
                return;
            }
        }
        let _ = self
            .desktop_shell
            .flash(OverlayPhase::Notice, reason, NOTICE_CHIP_HOLD);
    }

    /// The text "Paste last dictation" uses: this session's last dictation,
    /// otherwise the newest saved one.
    fn last_dictation_text(&self) -> Option<String> {
        self.last_output
            .lock()
            .ok()
            .and_then(|last| last.clone())
            .filter(|text| !text.trim().is_empty())
            .or_else(|| {
                storage::recent_quick_dictations(&self.db_path, 1)
                    .ok()?
                    .into_iter()
                    .next()
                    .map(|(_, text)| text)
            })
    }

    /// Pastes the last dictation again into the app in front, for when the
    /// first paste landed nowhere.
    pub fn paste_last(&self) -> Result<()> {
        if matches!(
            self.status().state,
            QuickDictationState::Listening | QuickDictationState::Processing
        ) {
            return self.desktop_shell.flash(
                OverlayPhase::Notice,
                "Finish the current dictation first",
                NOTICE_CHIP_HOLD,
            );
        }
        let Some(text) = self.last_dictation_text() else {
            return self
                .desktop_shell
                .flash(OverlayPhase::Notice, "Nothing to paste yet", NOTICE_CHIP_HOLD);
        };
        let settings = storage::get_settings_from_db_path(&self.db_path)?;
        crate::insertion::wait_for_modifier_release(Duration::from_millis(1500));
        let target = detect_frontmost_paste_target();
        let app = self.app.clone();
        let (response_tx, response_rx) = mpsc::channel();
        self.app.run_on_main_thread(move || {
            let result = crate::insertion::insert_text(
                &app,
                &text,
                settings.insert_behavior,
                target.as_ref(),
                || Ok(()),
            )
            .map_err(|error| error.to_string());
            let _ = response_tx.send(result);
        })?;
        let report = response_rx
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| anyhow!("timed out while pasting the last dictation"))?
            .map_err(anyhow::Error::msg)?;
        let (phase, label) = match (report.outcome, report.warning) {
            (InsertionOutcome::Pasted, _) => (OverlayPhase::Inserted, "Pasted last dictation"),
            (InsertionOutcome::ClipboardOnly, Some(warning)) => {
                (OverlayPhase::ClipboardOnly, warning.overlay_label())
            }
            (InsertionOutcome::ClipboardOnly, None) => {
                (OverlayPhase::ClipboardOnly, "Copied last dictation")
            }
        };
        self.desktop_shell.flash(phase, label, RESULT_CHIP_HOLD)
    }

    pub fn copy_last(&self) -> Result<()> {
        match self.last_dictation_text() {
            Some(text) => self.copy_to_clipboard(&text, "Copied last dictation"),
            None => self
                .desktop_shell
                .flash(OverlayPhase::Notice, "Nothing to copy yet", NOTICE_CHIP_HOLD),
        }
    }

    /// Copies one entry of the tray's Recent dictations.
    pub fn copy_recent(&self, id: &str) -> Result<()> {
        match self.desktop_shell.tray_recent_text(id) {
            Some(text) => self.copy_to_clipboard(&text, "Copied"),
            None => Ok(()),
        }
    }

    fn copy_to_clipboard(&self, text: &str, label: &str) -> Result<()> {
        use tauri_plugin_clipboard_manager::ClipboardExt;
        self.app.clipboard().write_text(text.to_string())?;
        self.desktop_shell
            .flash(OverlayPhase::ClipboardOnly, label, RESULT_CHIP_HOLD)
    }

    /// Updates the tray's Recent dictations from saved history (or, with
    /// history off, this session's last dictation only).
    pub fn refresh_tray_recents(&self) {
        let saved = storage::get_settings_from_db_path(&self.db_path)
            .map(|settings| settings.save_history)
            .unwrap_or(false);
        let mut recents = if saved {
            storage::recent_quick_dictations(&self.db_path, 5).unwrap_or_default()
        } else {
            Vec::new()
        };
        if recents.is_empty() {
            if let Some(text) = self.last_output.lock().ok().and_then(|last| last.clone()) {
                recents.push(("last".into(), text));
            }
        }
        self.desktop_shell.set_tray_recents(recents);
    }

    /// Forgets the in-memory last dictation, e.g. after history was deleted.
    pub fn forget_last_dictation(&self) {
        if let Ok(mut last) = self.last_output.lock() {
            *last = None;
        }
        self.refresh_tray_recents();
    }

    /// Spawn the single watchdog thread that auto-recovers a dictation stuck in
    /// Listening or Processing for longer than `STUCK_THRESHOLD`. This is the
    /// last-resort safety net behind the worker-level self-healing.
    pub fn spawn_watchdog(&self) {
        const STUCK_THRESHOLD_MS: i64 = 90_000;
        const POLL_INTERVAL: Duration = Duration::from_secs(5);
        let controller = self.clone();
        thread::spawn(move || loop {
            thread::sleep(POLL_INTERVAL);
            let state = controller.status().state;
            let phase = controller.translation.snapshot().stage;
            // Live capture has sample-progress checks, a five-minute cap, and
            // separate loading/finalization deadlines in its streaming service.
            if controller
                .live
                .lock()
                .map(|live| live.is_some())
                .unwrap_or(false)
            {
                continue;
            }
            if matches!(
                phase.as_str(),
                "waiting" | "transcribing" | "loading" | "translating" | "streaming_finishing"
            ) {
                continue;
            }
            let is_active = matches!(
                state,
                QuickDictationState::Listening | QuickDictationState::Processing
            );
            if !is_active {
                continue;
            }
            // Listening ends by itself at the five-minute limit; only treat it
            // as stuck well after that, so long dictations are never discarded.
            let threshold = if state == QuickDictationState::Listening {
                DICTATION_CAPTURE_LIMIT_MS as i64 + 30_000
            } else {
                STUCK_THRESHOLD_MS
            };
            let since = controller.state_since_ms.load(Ordering::SeqCst);
            if chrono::Utc::now().timestamp_millis() - since >= threshold {
                eprintln!(
                    "[dictation] watchdog: stuck in {:?} for too long — forcing reset",
                    state
                );
                if let Err(error) = controller.force_reset() {
                    eprintln!("[dictation] watchdog reset failed: {error:?}");
                }
            }
        });
    }
}

/// The batch ASR request for a shortcut dictation.
fn shortcut_asr_request(
    settings: &AppSettings,
    vocabulary_prompt: Option<&vocabulary::VocabularyPrompt>,
    file_path: String,
) -> FileTranscriptionRequest {
    FileTranscriptionRequest {
        use_context: Some(crate::model_metadata::ModelUseContext::ShortcutDictation),
        profile: settings.shortcut_dictation_model_profile,
        selected_model_id: settings.shortcut_dictation_selected_model_id.clone(),
        language_mode: settings.language_mode,
        fixed_language: settings.fixed_language.clone(),
        timestamps: false,
        prefer_gpu: settings.gpu_enabled,
        file_path,
        context_prompt: vocabulary_prompt.map(|prompt| prompt.text.clone()),
        context_terms: vocabulary_prompt
            .map(|prompt| prompt.terms.clone())
            .unwrap_or_default(),
    }
}

fn resolve_model_name(
    engine: &dyn TranscriptionEngine,
    settings: &AppSettings,
) -> Result<Option<String>> {
    let models = engine.list_models()?;
    Ok(
        if let Some(model_id) = settings.shortcut_dictation_selected_model_id.as_deref() {
            models
                .iter()
                .find(|model| model.id == model_id)
                .map(|model| model.model_name.clone())
        } else {
            models
                .iter()
                .find(|model| {
                    model.profile == settings.shortcut_dictation_model_profile && model.is_default
                })
                .or_else(|| {
                    models
                        .iter()
                        .find(|model| model.profile == settings.shortcut_dictation_model_profile)
                })
                .map(|model| model.model_name.clone())
        },
    )
}

#[cfg(test)]
mod translation_shortcut_tests {
    use super::*;
    #[test]
    fn parsed_shortcut_conflicts_ignore_modifier_order() {
        let error = shortcut_set(&[
            (ShortcutRole::Dictation, "CmdOrCtrl+Shift+Right"),
            (ShortcutRole::CycleLanguage, "Shift+CmdOrCtrl+Right"),
        ])
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Dictation and language switching need different shortcuts."
        );
        assert!(shortcut_set(&[
            (ShortcutRole::CycleLanguage, "Ctrl+Alt+V"),
            (ShortcutRole::PasteLast, "Alt+Ctrl+V"),
        ])
        .is_err());
        assert_eq!(
            shortcut_set(&[
                (ShortcutRole::Dictation, "CmdOrCtrl+Shift+Space"),
                (ShortcutRole::CycleLanguage, crate::translation::DEFAULT_SHORTCUT),
                (ShortcutRole::PasteLast, crate::settings::DEFAULT_PASTE_LAST_SHORTCUT),
            ])
            .unwrap()
            .len(),
            3
        );
        assert!(Shortcut::from_str(CANCEL_SHORTCUT).is_ok());
    }
    #[test]
    fn only_blah_blah_blah_earns_the_wink() {
        assert!(is_blah_blah("Blah blah blah."));
        assert!(is_blah_blah("bla, bla, bla, blah!"));
        assert!(!is_blah_blah("blah blah"));
        assert!(!is_blah_blah("blah blah blah and so on"));
        assert!(!is_blah_blah(""));
    }
    #[test]
    fn overlay_reasons_are_short_and_specific() {
        assert_eq!(overlay_reason("TRANSCRIPTION_EMPTY: whisper produced no segments"), "No speech heard");
        assert_eq!(
            overlay_reason("MODEL_MISSING: no whisper.cpp model is installed for profile balanced"),
            "No dictation engine · open Murmela"
        );
        assert_eq!(overlay_reason("No default microphone is available"), "Microphone isn't working");
        assert_eq!(overlay_reason("Dictation is still transcribing."), "Still on the last one…");
        assert_eq!(overlay_reason("Another recording is already active."), "Recording in the app");
        assert_eq!(overlay_reason("something odd"), "Didn't work · open Murmela");
        for message in ["TRANSCRIPTION_EMPTY", "MODEL_MISSING", "microphone", "translation", "timed out", "x"] {
            assert!(overlay_reason(message).chars().count() <= 36);
        }
    }
    #[test]
    fn holding_the_language_key_cycles_only_once() {
        let pressed = AtomicBool::new(false);
        assert!(cycle_key_transition(&pressed, ShortcutState::Pressed));
        assert!(!cycle_key_transition(&pressed, ShortcutState::Pressed));
        assert!(!cycle_key_transition(&pressed, ShortcutState::Released));
        assert!(cycle_key_transition(&pressed, ShortcutState::Pressed));
    }
}
