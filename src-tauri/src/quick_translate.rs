//! Translate selected text from any app. Copy it twice (⌘C ⌘C) or press the
//! quick-translate shortcut, and a small panel next to the pointer shows the
//! translation with Copy, Edit in Murmela and Replace.
//!
//! ⌘C twice is noticed on the clipboard, not on the keyboard, so it needs no
//! permission: two copies of the same text within `DOUBLE_COPY_WINDOW`. Only
//! Replace (which presses ⌘V) and a key combination (which presses ⌘C) need
//! Accessibility, and the panel explains that when it is missing.
//!
//! The panel is an NSPanel that never activates Murmela: the app being worked
//! in stays in front with its selection, while the panel takes ↩ and Esc.

use std::sync::{LazyLock, Mutex, Once, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::app_state::AppState;
use crate::insertion::{FrontmostApp, PasteTarget};
use crate::settings::{AppSettings, InsertBehavior, QuickTranslateDisplay};

/// Settings value for "copy twice" instead of a key combination.
pub const DOUBLE_COPY: &str = "DoubleCopy";
/// Password managers: their copies are secrets, never translated.
pub const DEFAULT_IGNORED_APPS: [&str; 4] = [
    "com.apple.keychainaccess",
    "com.apple.Passwords",
    "com.1password.1password",
    "com.agilebits.onepassword7",
];
pub const PANEL_LABEL: &str = "quick-translate";
/// A new text for the panel (`QuickTranslateRequest`).
pub const REQUEST_EVENT: &str = "quick-translate-request";
/// Opens the Translate screen with a text (`OpenInApp`).
pub const OPEN_IN_APP_EVENT: &str = "quick-translate-open";

/// Two copies of the same text closer than this are "copy twice".
const DOUBLE_COPY_WINDOW: Duration = Duration::from_millis(700);
/// Two clipboard changes closer than this come from an app writing twice, not
/// from a person pressing ⌘C twice.
const DOUBLE_COPY_MIN_GAP: Duration = Duration::from_millis(60);
const POLL_INTERVAL: Duration = Duration::from_millis(40);
/// Clipboard changes this soon after Murmela wrote to it are its own.
const OWN_WRITE_QUIET: Duration = Duration::from_millis(1500);
/// Longer selections open on the Translate screen instead of the small panel.
const PANEL_CHARACTER_LIMIT: usize = 6_000;
const PANEL_WIDTH: f64 = 420.0;
const PANEL_INITIAL_HEIGHT: f64 = 190.0;
const PANEL_MAX_HEIGHT: f64 = 520.0;
/// Clipboard types password managers and similar apps mark secrets with
/// (nspasteboard.org).
const SECRET_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct QuickTranslateRequest {
    pub id: String,
    pub text: String,
    /// Detected language of the text; None when it is too short to tell.
    pub source_language: Option<String>,
    pub target_language: String,
    pub my_language: String,
    /// Where text in `my_language` goes; the panel switches to it when the
    /// text turns out to be in your language after all.
    pub other_language: String,
    /// The app the text came from, for "Replace in TextEdit".
    pub app_name: Option<String>,
    /// There is an app to paste the translation back into.
    pub can_replace: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenInApp {
    pub text: String,
    pub source_language: Option<String>,
    pub target_language: String,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReplaceOutcome {
    /// The panel closed and the translation is being pasted.
    Replacing,
    /// macOS has not allowed Murmela to press ⌘V yet.
    NeedsAccessibility,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub bundle_id: String,
    /// None when the app is not installed.
    pub name: Option<String>,
}

struct Shown {
    request: QuickTranslateRequest,
    target: Option<PasteTarget>,
}

/// What the clipboard watcher needs from the settings, kept in memory so the
/// 40 ms poll never touches the database.
#[derive(Default)]
struct WatchConfig {
    double_copy: bool,
    ignored_apps: Vec<String>,
}

static CONFIG: LazyLock<RwLock<WatchConfig>> = LazyLock::new(Default::default);
static SHOWN: Mutex<Option<Shown>> = Mutex::new(None);
static PAUSED_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static OWN_WRITE_AT: Mutex<Option<Instant>> = Mutex::new(None);
/// The pointer when the panel opened (physical pixels); the panel stays by it
/// while its height changes.
static ANCHOR: Mutex<Option<(f64, f64)>> = Mutex::new(None);
/// The app used before Murmela came to the front: where "Insert into…" on
/// the Translate screen pastes.
static LAST_OTHER_APP: Mutex<Option<FrontmostApp>> = Mutex::new(None);
/// The latest clipboard change the user made, for filling in the Translate screen.
static LAST_COPY: Mutex<Option<Copied>> = Mutex::new(None);

/// A clipboard change by the user (not by Murmela). The text itself is read
/// only when asked for, and only if nothing changed since.
#[derive(Debug, Clone)]
struct Copied {
    change_count: isize,
    at: Instant,
    /// The app in front when it was copied; copies from ignored apps and from
    /// Murmela itself are not offered.
    bundle_id: Option<String>,
}

const FRONT_APP_INTERVAL: Duration = Duration::from_millis(400);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Murmela is about to write the clipboard (a paste, a restore, Copy); the
/// watcher must not take that for the user copying twice.
pub fn note_own_clipboard_write() {
    *lock(&OWN_WRITE_AT) = Some(Instant::now());
}

fn own_write_recently(now: Instant) -> bool {
    lock(&OWN_WRITE_AT).is_some_and(|at| now.saturating_duration_since(at) < OWN_WRITE_QUIET)
}

/// Applies changed settings to the running watcher.
pub fn configure(settings: &AppSettings) {
    let mut config = CONFIG.write().unwrap_or_else(|error| error.into_inner());
    config.double_copy = settings.quick_translate_enabled
        && settings.quick_translate_shortcut == DOUBLE_COPY
        && crate::translation::platform_supported();
    config.ignored_apps = settings.quick_translate_ignored_apps.clone();
}

/// The key combination to register as a global shortcut, if one is chosen.
pub fn registered_shortcut(settings: &AppSettings) -> Option<&str> {
    let shortcut = settings.quick_translate_shortcut.trim();
    (settings.quick_translate_enabled
        && shortcut != DOUBLE_COPY
        && !shortcut.is_empty()
        && crate::translation::platform_supported())
    .then_some(shortcut)
}

pub fn validate_settings(settings: &AppSettings) -> Result<()> {
    for code in [&settings.quick_translate_my_language, &settings.quick_translate_last_target]
        .into_iter()
        .flatten()
    {
        if !crate::text_translation::LANGUAGES.iter().any(|(known, _)| known == code) {
            bail!("Unsupported translation language: {code}.");
        }
    }
    if settings.quick_translate_shortcut.trim().is_empty() {
        bail!("Choose ⌘C twice or a key combination for translating selected text.");
    }
    Ok(())
}

/// Starts the clipboard watcher and prepares the panel. Call once the app
/// state exists.
pub fn start(app: &AppHandle) {
    static STARTED: Once = Once::new();
    let Some(state) = app.try_state::<AppState>() else {
        return;
    };
    if let Ok(settings) = crate::storage::get_settings_from_db_path(&state.db_path) {
        configure(&settings);
    }
    STARTED.call_once(|| {
        let watcher = app.clone();
        thread::spawn(move || watch_clipboard(&watcher));
        thread::spawn(watch_front_app);
        // Load the panel's page now, so the first ⌘C ⌘C shows it at once.
        let _ = ensure_panel(app);
    });
}

/// Recognises "copy twice" from clipboard changes.
#[derive(Default)]
struct DoubleCopy {
    last: Option<(Instant, String)>,
}

impl DoubleCopy {
    /// A new clipboard change at `at` with `text` (None: not text, or a
    /// secret). Returns the text when it completes a double copy.
    fn copied(&mut self, at: Instant, text: Option<String>) -> Option<String> {
        let Some(text) = text.filter(|text| !text.trim().is_empty()) else {
            self.last = None;
            return None;
        };
        if let Some((previous_at, previous)) = &self.last {
            let gap = at.saturating_duration_since(*previous_at);
            if *previous == text && gap >= DOUBLE_COPY_MIN_GAP && gap <= DOUBLE_COPY_WINDOW {
                // A third copy starts over instead of opening the panel again.
                self.last = None;
                return Some(text);
            }
        }
        self.last = Some((at, text));
        None
    }
}

fn watch_clipboard(app: &AppHandle) {
    use objc2_app_kit::NSPasteboard;
    let mut seen = NSPasteboard::generalPasteboard().changeCount();
    let mut detector = DoubleCopy::default();
    loop {
        thread::sleep(POLL_INTERVAL);
        let count = NSPasteboard::generalPasteboard().changeCount();
        if count == seen {
            continue;
        }
        seen = count;
        let now = Instant::now();
        if own_write_recently(now) {
            detector = DoubleCopy::default();
            continue;
        }
        let front = crate::insertion::frontmost_app();
        *lock(&LAST_COPY) = Some(Copied {
            change_count: count,
            at: now,
            bundle_id: front
                .as_ref()
                .map(|front| if front.is_murmela() { String::new() } else { front.bundle_id.clone() }),
        });
        if !CONFIG.read().map(|config| config.double_copy).unwrap_or(false) {
            detector = DoubleCopy::default();
            continue;
        }
        if let Some(text) = detector.copied(now, clipboard_text()) {
            let app = app.clone();
            thread::spawn(move || {
                if let Err(error) = open(&app, text, front) {
                    eprintln!("[quick-translate] {error:#}");
                }
            });
        }
    }
}

/// Remembers the last app other than Murmela that was in front.
fn watch_front_app() {
    loop {
        if let Some(front) = crate::insertion::frontmost_app() {
            if !front.is_murmela() && !front.bundle_id.is_empty() {
                *lock(&LAST_OTHER_APP) = Some(front);
            }
        }
        thread::sleep(FRONT_APP_INTERVAL);
    }
}

/// The text the user copied within `max_age`, unless it came from Murmela, an
/// ignored app, or is a secret.
fn recent_copy(max_age: Duration, ignored_apps: &[String]) -> Option<String> {
    use objc2_app_kit::NSPasteboard;
    let copied = lock(&LAST_COPY).clone()?;
    if copied.at.elapsed() > max_age
        || NSPasteboard::generalPasteboard().changeCount() != copied.change_count
    {
        return None;
    }
    match copied.bundle_id.as_deref() {
        None | Some("") => return None,
        Some(bundle_id) if ignored_apps.iter().any(|ignored| ignored == bundle_id) => return None,
        Some(_) => {}
    }
    clipboard_text().filter(|text| !text.trim().is_empty())
}

/// The clipboard's plain text, or None when it holds no text or a secret.
fn clipboard_text() -> Option<String> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::{NSPasteboard, NSPasteboardTypeString};
    autoreleasepool(|_| {
        let pasteboard = NSPasteboard::generalPasteboard();
        let types = pasteboard.types()?;
        if types
            .iter()
            .any(|kind| SECRET_TYPES.contains(&kind.to_string().as_str()))
        {
            return None;
        }
        pasteboard
            .stringForType(unsafe { NSPasteboardTypeString })
            .map(|text| text.to_string())
    })
}

/// The quick-translate key combination was released.
pub fn trigger_from_shortcut(app: &AppHandle) {
    let front = crate::insertion::frontmost_app();
    if front.as_ref().is_some_and(FrontmostApp::is_murmela) {
        return;
    }
    if !crate::insertion::accessibility_trusted() {
        flash(app, "Allow Accessibility to translate with a shortcut · open Murmela");
        return;
    }
    crate::insertion::wait_for_modifier_release(Duration::from_secs(1));
    let text = match crate::insertion::copy_selection() {
        Ok(Some(text)) => text,
        Ok(None) => {
            flash(app, "Select some text first");
            return;
        }
        Err(error) => {
            eprintln!("[quick-translate] could not copy the selection: {error:#}");
            flash(app, "Couldn't read the selection");
            return;
        }
    };
    if let Err(error) = open(app, text, front) {
        eprintln!("[quick-translate] {error:#}");
    }
}

fn flash(app: &AppHandle, text: &str) {
    if let Some(state) = app.try_state::<AppState>() {
        let _ = state.desktop_shell.flash(
            crate::desktop_shell::OverlayPhase::Notice,
            text,
            Duration::from_millis(2600),
        );
    }
}

fn paused(now: Instant) -> bool {
    let mut until = lock(&PAUSED_UNTIL);
    match *until {
        Some(at) if now < at => true,
        Some(_) => {
            *until = None;
            false
        }
        None => false,
    }
}

/// Shows `text`'s translation, in the panel or on the Translate screen.
fn open(app: &AppHandle, text: String, front: Option<FrontmostApp>) -> Result<()> {
    let state = app
        .try_state::<AppState>()
        .ok_or_else(|| anyhow!("Murmela is still starting"))?;
    let settings = crate::storage::get_settings_from_db_path(&state.db_path)?;
    if !settings.quick_translate_enabled || paused(Instant::now()) {
        return Ok(());
    }
    if let Some(front) = &front {
        if front.is_murmela() || settings.quick_translate_ignored_apps.contains(&front.bundle_id) {
            return Ok(());
        }
    }
    // Without a translation engine ⌘C twice stays an ordinary copy; Settings
    // and the Translate screen explain how to get one.
    let status = crate::text_translation::status(&state.translation);
    if !status.supported || status.model_id.is_none() {
        return Ok(());
    }

    // An unsure guess is no guess: short texts fool the detector (German read
    // as French), and the panel would show the wrong language.
    let source = crate::text_translation::detect_language(&text)
        .filter(|found| found.reliable)
        .map(|found| found.code.to_string());
    let mine = my_language(&settings);
    let last_target = settings.quick_translate_last_target.as_deref();
    let target = choose_target(source.as_deref(), &mine, last_target, &settings.translation_cycle_languages);
    let other = choose_target(Some(&mine), &mine, last_target, &settings.translation_cycle_languages);

    if settings.quick_translate_display == QuickTranslateDisplay::App
        || text.chars().count() > PANEL_CHARACTER_LIMIT
    {
        return open_in_app(
            app,
            OpenInApp { text, source_language: source, target_language: target },
        );
    }

    let target_app = front.as_ref().and_then(FrontmostApp::paste_target);
    let request = QuickTranslateRequest {
        id: uuid::Uuid::new_v4().to_string(),
        text,
        source_language: source,
        target_language: target,
        my_language: mine,
        other_language: other,
        app_name: front.and_then(|front| front.name),
        can_replace: target_app.is_some(),
    };
    *lock(&SHOWN) = Some(Shown { request: request.clone(), target: target_app });
    show_panel(app)?;
    app.emit_to(PANEL_LABEL, REQUEST_EVENT, request)?;
    Ok(())
}

fn open_in_app(app: &AppHandle, request: OpenInApp) -> Result<()> {
    crate::desktop_shell::show_main_window(app)?;
    app.emit_to("main", OPEN_IN_APP_EVENT, request)?;
    Ok(())
}

/// The language translations go into: the user's choice, else macOS's.
pub fn my_language(settings: &AppSettings) -> String {
    settings
        .quick_translate_my_language
        .clone()
        .unwrap_or_else(system_language)
}

/// The first of macOS's preferred languages that Murmela translates, else English.
pub fn system_language() -> String {
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::msg_send;
    use objc2_foundation::NSString;
    let preferred: Vec<String> = autoreleasepool(|_| unsafe {
        let Some(class) = AnyClass::get(c"NSLocale") else {
            return Vec::new();
        };
        let languages: *mut AnyObject = msg_send![class, preferredLanguages];
        let Some(languages) = languages.as_ref() else {
            return Vec::new();
        };
        let count: usize = msg_send![languages, count];
        (0..count)
            .filter_map(|index| {
                let item: *mut NSString = msg_send![languages, objectAtIndex: index];
                item.as_ref().map(|item| item.to_string())
            })
            .collect()
    });
    preferred
        .iter()
        .find_map(|tag| supported_language(tag))
        .unwrap_or("en")
        .to_string()
}

/// Maps a BCP 47 tag ("de-AT", "es-419", "pt-PT") to a language Murmela translates.
fn supported_language(tag: &str) -> Option<&'static str> {
    let tag = tag.replace('_', "-");
    let base = tag.split('-').next().unwrap_or("").to_ascii_lowercase();
    let region = tag.split('-').nth(1).unwrap_or("").to_ascii_uppercase();
    Some(match base.as_str() {
        "en" => "en",
        "de" => "de",
        "fr" => "fr",
        // Latin American Spanish reads closer to Argentina's than to Spain's.
        "es" if region == "AR" || region == "419" || region == "UY" => "es-AR",
        "es" => "es",
        "it" => "it",
        "pt" => "pt-BR",
        "nl" => "nl",
        "pl" => "pl",
        _ => return None,
    })
}

fn base_language(code: &str) -> &str {
    code.split('-').next().unwrap_or(code)
}

/// Text in another language goes into yours; text in yours goes into the last
/// other language you picked (else the first other one on your language list).
fn choose_target(
    source: Option<&str>,
    mine: &str,
    last_target: Option<&str>,
    cycle_languages: &[String],
) -> String {
    let is_mine = |code: &str| base_language(code) == base_language(mine);
    match source {
        Some(source) if is_mine(source) => last_target
            .filter(|code| !is_mine(code))
            .map(String::from)
            .or_else(|| cycle_languages.iter().find(|code| !is_mine(code)).cloned())
            .unwrap_or_else(|| if is_mine("en") { "de".into() } else { "en".into() }),
        _ => mine.to_string(),
    }
}

fn ensure_panel(app: &AppHandle) -> Result<WebviewWindow> {
    if let Some(window) = app.get_webview_window(PANEL_LABEL) {
        return Ok(window);
    }
    let window = WebviewWindowBuilder::new(
        app,
        PANEL_LABEL,
        WebviewUrl::App("quick-translate.html".into()),
    )
    .title("Murmela Translate")
    .inner_size(PANEL_WIDTH, PANEL_INITIAL_HEIGHT)
    .transparent(true)
    .decorations(false)
    .shadow(true)
    .resizable(false)
    .visible(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .visible_on_all_workspaces(true)
    .build()?;
    let panel = window.clone();
    app.run_on_main_thread(move || {
        if let Err(error) = macos::make_panel(&panel) {
            eprintln!("[quick-translate] panel setup failed: {error:#}");
        }
    })?;
    Ok(window)
}

fn show_panel(app: &AppHandle) -> Result<()> {
    let window = ensure_panel(app)?;
    let anchor = window
        .cursor_position()
        .ok()
        .map(|point| (point.x, point.y));
    *lock(&ANCHOR) = anchor;
    window.set_size(tauri::LogicalSize::new(PANEL_WIDTH, PANEL_INITIAL_HEIGHT))?;
    place(&window)?;
    // `with_webview` runs on the main thread.
    macos::show(&window);
    Ok(())
}

/// Below and right of the pointer; above it when there is no room below.
fn place(window: &WebviewWindow) -> Result<()> {
    let Some((x, y)) = *lock(&ANCHOR) else {
        return window.center().map_err(Into::into);
    };
    let Some(monitor) = window
        .monitor_from_point(x, y)?
        .or(window.primary_monitor()?)
    else {
        return Ok(());
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let size = window.outer_size()?;
    let (left, top) = panel_origin(
        (x, y),
        (size.width as f64, size.height as f64),
        (area.position.x as f64, area.position.y as f64),
        (area.size.width as f64, area.size.height as f64),
        scale,
    );
    window.set_position(tauri::PhysicalPosition::new(left, top))?;
    Ok(())
}

fn panel_origin(
    pointer: (f64, f64),
    size: (f64, f64),
    area_origin: (f64, f64),
    area_size: (f64, f64),
    scale: f64,
) -> (i32, i32) {
    let gap = 14.0 * scale;
    let margin = 8.0 * scale;
    let right = area_origin.0 + area_size.0;
    let bottom = area_origin.1 + area_size.1;
    let x = (pointer.0 - 24.0 * scale)
        .min(right - size.0 - margin)
        .max(area_origin.0 + margin);
    let below = pointer.1 + gap;
    let y = if below + size.1 <= bottom - margin {
        below
    } else {
        (pointer.1 - gap - size.1).max(area_origin.1 + margin)
    };
    (x.round() as i32, y.round() as i32)
}

fn hide_panel(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(PANEL_LABEL) {
        let _ = window.hide();
    }
}

fn shown_request() -> Option<QuickTranslateRequest> {
    lock(&SHOWN).as_ref().map(|shown| shown.request.clone())
}

// Commands of the panel.

/// The text the panel shows now (it may have missed the event while loading).
#[tauri::command]
pub fn quick_translate_current() -> Option<QuickTranslateRequest> {
    shown_request()
}

#[tauri::command]
pub fn quick_translate_close(app: AppHandle) {
    hide_panel(&app);
    *lock(&SHOWN) = None;
}

/// The panel's content changed height (logical pixels).
#[tauri::command]
pub fn quick_translate_resize(app: AppHandle, height: f64) -> Result<(), String> {
    let window = app
        .get_webview_window(PANEL_LABEL)
        .ok_or("The translation panel is closed.")?;
    let height = height.clamp(96.0, PANEL_MAX_HEIGHT);
    window
        .set_size(tauri::LogicalSize::new(PANEL_WIDTH, height))
        .map_err(|error| error.to_string())?;
    place(&window).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn quick_translate_copy(app: AppHandle, text: String) -> Result<(), String> {
    use tauri_plugin_clipboard_manager::ClipboardExt;
    note_own_clipboard_write();
    app.clipboard().write_text(text).map_err(|error| error.to_string())
}

/// Pastes `text` over the selection in the app it came from.
#[tauri::command]
pub fn quick_translate_replace(app: AppHandle, text: String) -> Result<ReplaceOutcome, String> {
    if !crate::insertion::accessibility_trusted() {
        return Ok(ReplaceOutcome::NeedsAccessibility);
    }
    let target = lock(&SHOWN)
        .take()
        .and_then(|shown| shown.target)
        .ok_or("There is no app to put the translation into. Copy it instead.")?;
    hide_panel(&app);
    thread::spawn(move || {
        // The panel never activated Murmela, but give the target app's window
        // a moment to become key again before ⌘V.
        thread::sleep(Duration::from_millis(60));
        if let Err(error) =
            crate::insertion::insert_text(&app, &text, InsertBehavior::Paste, Some(&target), || Ok(()))
        {
            eprintln!("[quick-translate] replace failed: {error:#}");
        }
    });
    Ok(ReplaceOutcome::Replacing)
}

/// Opens the text on the Translate screen to edit it there.
#[tauri::command]
pub fn quick_translate_edit(
    app: AppHandle,
    text: String,
    source_language: Option<String>,
    target_language: String,
) -> Result<(), String> {
    hide_panel(&app);
    *lock(&SHOWN) = None;
    open_in_app(&app, OpenInApp { text, source_language, target_language })
        .map_err(|error| error.to_string())
}

/// The user picked a target in the panel. Another language than their own
/// becomes the one their own-language text goes into next time.
#[tauri::command]
pub fn quick_translate_set_target(
    state: tauri::State<'_, AppState>,
    target_language: String,
) -> Result<(), String> {
    let settings = crate::storage::get_settings(state.inner()).map_err(|error| error.to_string())?;
    if base_language(&target_language) == base_language(&my_language(&settings)) {
        return Ok(());
    }
    crate::storage::update_settings(
        state.inner(),
        crate::settings::SettingsPatch {
            quick_translate_last_target: Some(Some(target_language)),
            ..Default::default()
        },
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Pauses for `minutes`, or turns quick translation off when None.
#[tauri::command]
pub fn quick_translate_pause(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
    minutes: Option<u64>,
) -> Result<(), String> {
    quick_translate_close(app.clone());
    match minutes {
        Some(minutes) => {
            *lock(&PAUSED_UNTIL) = Some(Instant::now() + Duration::from_secs(minutes * 60));
            Ok(())
        }
        None => {
            let settings = crate::storage::update_settings(
                state.inner(),
                crate::settings::SettingsPatch {
                    quick_translate_enabled: Some(false),
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
            configure(&settings);
            state
                .dictation_controller
                .sync_shortcut_registration()
                .map(|_| ())
                .map_err(|error| error.to_string())?;
            let _ = app.emit("settings-changed", ());
            Ok(())
        }
    }
}

// Commands of the Translate screen.

/// The app "Insert into…" pastes into: the last one used before Murmela.
#[tauri::command]
pub fn quick_translate_insert_target() -> Option<String> {
    lock(&LAST_OTHER_APP)
        .as_ref()
        .map(|front| front.name.clone().unwrap_or_else(|| front.bundle_id.clone()))
}

/// Pastes `text` where the cursor is in the last app used before Murmela.
#[tauri::command]
pub fn quick_translate_insert(app: AppHandle, text: String) -> Result<ReplaceOutcome, String> {
    if !crate::insertion::accessibility_trusted() {
        return Ok(ReplaceOutcome::NeedsAccessibility);
    }
    let target = lock(&LAST_OTHER_APP)
        .as_ref()
        .and_then(FrontmostApp::paste_target)
        .ok_or("Murmela doesn't know which app to put the text into. Copy it instead.")?;
    thread::spawn(move || {
        if let Err(error) =
            crate::insertion::insert_text(&app, &text, InsertBehavior::Paste, Some(&target), || Ok(()))
        {
            eprintln!("[quick-translate] insert failed: {error:#}");
        }
    });
    Ok(ReplaceOutcome::Replacing)
}

/// Text copied in another app during the last `max_age_seconds`, to fill in
/// an empty Translate screen.
#[tauri::command]
pub fn quick_translate_recent_copy(
    state: tauri::State<'_, AppState>,
    max_age_seconds: u64,
) -> Option<String> {
    let settings = crate::storage::get_settings(state.inner()).ok()?;
    recent_copy(
        Duration::from_secs(max_age_seconds.min(600)),
        &settings.quick_translate_ignored_apps,
    )
}

// Commands of Settings.

#[tauri::command]
pub fn quick_translate_system_language() -> String {
    system_language()
}

/// Names of the apps quick translation ignores.
#[tauri::command]
pub fn quick_translate_describe_apps(bundle_ids: Vec<String>) -> Vec<AppInfo> {
    bundle_ids
        .into_iter()
        .map(|bundle_id| AppInfo {
            name: macos::app_path(&bundle_id).and_then(|path| macos::app_name(&path)),
            bundle_id,
        })
        .collect()
}

/// Lets the user choose an app in /Applications to ignore.
#[tauri::command]
pub async fn quick_translate_pick_app(app: AppHandle) -> Result<Option<AppInfo>, String> {
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel();
    app.dialog()
        .file()
        .set_title("Choose an app where ⌘C twice stays a normal copy")
        .set_directory("/Applications")
        .add_filter("Apps", &["app"])
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let file = tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|error| error.to_string())?
        .map_err(|_| "The app picker did not return.".to_string())?;
    let Some(tauri_plugin_dialog::FilePath::Path(path)) = file else {
        return Ok(None);
    };
    let path = path.to_string_lossy().into_owned();
    let bundle_id = macos::bundle_id(&path).ok_or("That doesn't look like an app.")?;
    Ok(Some(AppInfo { name: macos::app_name(&path), bundle_id }))
}

mod macos {
    use super::*;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject, Bool, ClassBuilder, Sel};
    use objc2::{msg_send, sel};
    use objc2_foundation::NSString;

    const NONACTIVATING_PANEL: usize = 1 << 7;
    const FLOATING_WINDOW_LEVEL: isize = 3;
    // canJoinAllSpaces | fullScreenAuxiliary: shows over full-screen apps too.
    const COLLECTION_BEHAVIOR: usize = (1 << 0) | (1 << 8);

    unsafe extern "C-unwind" fn yes(_: *mut AnyObject, _: Sel) -> Bool {
        Bool::YES
    }

    /// An NSPanel subclass that can become key, so the page gets ↩ and Esc.
    fn panel_class() -> &'static AnyClass {
        static CLASS: LazyLock<&'static AnyClass> = LazyLock::new(|| {
            let superclass = AnyClass::get(c"NSPanel").expect("AppKit provides NSPanel");
            let mut builder = ClassBuilder::new(c"MurmelaQuickTranslatePanel", superclass)
                .expect("the panel class is registered once");
            unsafe {
                builder.add_method(
                    sel!(canBecomeKeyWindow),
                    yes as unsafe extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool,
                );
            }
            builder.register()
        });
        *CLASS
    }

    /// Turns the window into a panel that takes keys without activating
    /// Murmela. Main thread only.
    pub(super) fn make_panel(window: &WebviewWindow) -> Result<()> {
        let ns_window = window.ns_window()? as *mut AnyObject;
        if ns_window.is_null() {
            bail!("the panel has no NSWindow");
        }
        // SAFETY: on the main thread; NSPanel is an NSWindow subclass that adds
        // no state the window lacks, the same swap tauri-nspanel relies on.
        unsafe {
            objc2::ffi::object_setClass(ns_window, panel_class());
            let window = &*ns_window;
            let mask: usize = msg_send![window, styleMask];
            let _: () = msg_send![window, setStyleMask: mask | NONACTIVATING_PANEL];
            let _: () = msg_send![window, setFloatingPanel: Bool::YES];
            let _: () = msg_send![window, setHidesOnDeactivate: Bool::NO];
            let _: () = msg_send![window, setBecomesKeyOnlyIfNeeded: Bool::NO];
            let _: () = msg_send![window, setLevel: FLOATING_WINDOW_LEVEL];
            let _: () = msg_send![window, setCollectionBehavior: COLLECTION_BEHAVIOR];
            // The style bit set after the window exists does not stop a click
            // from activating Murmela on its own; AppKit reads this flag.
            let prevents = sel!(_setPreventsActivation:);
            let responds: Bool = msg_send![window, respondsToSelector: prevents];
            if responds.as_bool() {
                let _: () = msg_send![window, _setPreventsActivation: Bool::YES];
            }
            let mask: usize = msg_send![window, styleMask];
            eprintln!(
                "[quick-translate] panel ready: class {}, style mask {mask:#x}, prevents activation {}",
                window.class().name().to_string_lossy(),
                responds.as_bool()
            );
        }
        Ok(())
    }

    /// Shows the panel and gives its page the keyboard. Main thread only.
    pub(super) fn show(window: &WebviewWindow) {
        let _ = window.with_webview(|webview| unsafe {
            let panel = webview.ns_window() as *mut AnyObject;
            let view = webview.inner() as *mut AnyObject;
            let Some(panel) = panel.as_ref() else {
                return;
            };
            let _: () = msg_send![panel, orderFrontRegardless];
            let _: () = msg_send![panel, makeKeyWindow];
            if let Some(view) = view.as_ref() {
                let _: Bool = msg_send![panel, makeFirstResponder: view];
            }
        });
    }

    pub(super) fn app_path(bundle_id: &str) -> Option<String> {
        autoreleasepool(|_| unsafe {
            let workspace: *mut AnyObject = msg_send![AnyClass::get(c"NSWorkspace")?, sharedWorkspace];
            let id = NSString::from_str(bundle_id);
            let url: *mut AnyObject =
                msg_send![workspace.as_ref()?, URLForApplicationWithBundleIdentifier: &*id];
            let path: *mut NSString = msg_send![url.as_ref()?, path];
            path.as_ref().map(|path| path.to_string())
        })
    }

    fn bundle(path: &str) -> Option<*mut AnyObject> {
        let path = NSString::from_str(path);
        let bundle: *mut AnyObject =
            unsafe { msg_send![AnyClass::get(c"NSBundle")?, bundleWithPath: &*path] };
        (!bundle.is_null()).then_some(bundle)
    }

    pub(super) fn bundle_id(path: &str) -> Option<String> {
        autoreleasepool(|_| unsafe {
            let bundle = bundle(path)?;
            let id: *mut NSString = msg_send![&*bundle, bundleIdentifier];
            id.as_ref().map(|id| id.to_string())
        })
    }

    /// The name Finder shows, without ".app".
    pub(super) fn app_name(path: &str) -> Option<String> {
        autoreleasepool(|_| unsafe {
            let manager: *mut AnyObject = msg_send![AnyClass::get(c"NSFileManager")?, defaultManager];
            let ns_path = NSString::from_str(path);
            let name: *mut NSString = msg_send![manager.as_ref()?, displayNameAtPath: &*ns_path];
            name.as_ref()
                .map(|name| name.to_string().trim_end_matches(".app").to_string())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    #[test]
    fn two_copies_of_the_same_text_in_quick_succession_open_the_panel() {
        let start = Instant::now();
        let mut detector = DoubleCopy::default();
        assert_eq!(detector.copied(start, Some("Hola".into())), None);
        assert_eq!(detector.copied(at(start, 180), Some("Hola".into())), Some("Hola".into()));
        assert_eq!(
            detector.copied(at(start, 360), Some("Hola".into())),
            None,
            "a third copy starts over"
        );
    }

    #[test]
    fn ordinary_copies_stay_ordinary() {
        let start = Instant::now();
        let mut detector = DoubleCopy::default();
        detector.copied(start, Some("one".into()));
        assert_eq!(detector.copied(at(start, 200), Some("two".into())), None, "different text");
        assert_eq!(detector.copied(at(start, 1200), Some("two".into())), None, "too slow");
        assert_eq!(detector.copied(at(start, 1220), Some("two".into())), None, "an app writing twice");
        detector.copied(at(start, 3000), Some("x".into()));
        assert_eq!(detector.copied(at(start, 3200), None), None, "a secret or an image");
        assert_eq!(detector.copied(at(start, 3300), Some("x".into())), None);
        assert_eq!(detector.copied(at(start, 5000), Some("  \n".into())), None);
        assert_eq!(detector.copied(at(start, 5200), Some("  \n".into())), None, "blank");
    }

    #[test]
    fn other_languages_go_into_yours_and_yours_into_the_last_one_you_picked() {
        let cycle = vec!["en".to_string(), "de".to_string(), "es-AR".to_string()];
        assert_eq!(choose_target(Some("en"), "de", Some("es-AR"), &cycle), "de");
        assert_eq!(choose_target(Some("de"), "de", Some("es-AR"), &cycle), "es-AR");
        assert_eq!(choose_target(Some("de"), "de", None, &cycle), "en", "first other on the list");
        assert_eq!(choose_target(Some("de"), "de", Some("de"), &cycle), "en", "never your own");
        assert_eq!(choose_target(None, "de", Some("en"), &cycle), "de", "too short to tell");
        assert_eq!(
            choose_target(Some("es"), "es-AR", Some("en"), &cycle),
            "en",
            "Spanish variants count as your own"
        );
        assert_eq!(choose_target(Some("en"), "en", None, &["en".to_string()]), "de");
    }

    #[test]
    fn macos_languages_map_to_the_ones_murmela_translates() {
        assert_eq!(supported_language("de-AT"), Some("de"));
        assert_eq!(supported_language("es-419"), Some("es-AR"));
        assert_eq!(supported_language("es_AR"), Some("es-AR"));
        assert_eq!(supported_language("es-ES"), Some("es"));
        assert_eq!(supported_language("pt-PT"), Some("pt-BR"));
        assert_eq!(supported_language("ja-JP"), None);
        assert!(crate::text_translation::LANGUAGES
            .iter()
            .any(|(code, _)| *code == system_language()));
    }

    #[test]
    fn the_panel_opens_by_the_pointer_and_stays_on_screen() {
        // A 1512×900 pt work area at 2× scale.
        let area = ((0.0, 50.0), (3024.0, 1800.0));
        let size = (840.0, 380.0);
        assert_eq!(panel_origin((1000.0, 400.0), size, area.0, area.1, 2.0), (952, 428));
        let (x, _) = panel_origin((3000.0, 400.0), size, area.0, area.1, 2.0);
        assert_eq!(x, 3024 - 840 - 16, "kept off the right edge");
        let (_, y) = panel_origin((1000.0, 1700.0), size, area.0, area.1, 2.0);
        assert_eq!(y, 1700 - 28 - 380, "above the pointer near the bottom");
    }

    #[test]
    fn only_a_key_combination_is_registered_as_a_shortcut() {
        let mut settings: AppSettings =
            serde_json::from_value(serde_json::json!({
                "defaultMode": "quick_dictate", "shortcut": "Fn", "translationEnabled": false,
                "translationCycleShortcut": "Ctrl+Alt+L", "translationModelId": "x",
                "shortcutMode": "push_to_talk", "languageMode": "auto", "fixedLanguage": null,
                "preferredInputDevice": null, "insertBehavior": "paste", "launchAtLoginEnabled": false,
                "gpuEnabled": true, "shortcutDictationModelProfile": "fast",
                "shortcutDictationSelectedModelId": null, "quickDictateModelProfile": "fast",
                "quickDictateSelectedModelId": null, "fileTranscribeModelProfile": "fast",
                "fileTranscribeSelectedModelId": null, "saveHistory": true, "soundsEnabled": true,
                "volumeDuckingEnabled": true, "fileDiarizationEnabled": false
            }))
            .unwrap();
        assert!(settings.quick_translate_enabled, "on by default");
        assert_eq!(settings.quick_translate_shortcut, DOUBLE_COPY);
        assert_eq!(registered_shortcut(&settings), None);
        settings.quick_translate_shortcut = "Ctrl+Alt+T".into();
        assert_eq!(
            registered_shortcut(&settings),
            crate::translation::platform_supported().then_some("Ctrl+Alt+T")
        );
        settings.quick_translate_enabled = false;
        assert_eq!(registered_shortcut(&settings), None);
        settings.quick_translate_my_language = Some("xx".into());
        assert!(validate_settings(&settings).is_err());
    }
}

