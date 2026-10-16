use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuBuilder, SubmenuBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindowBuilder};

use crate::startup::{StartupCoordinator, StartupPhase};

const OVERLAY_LABEL: &str = "dictation-overlay";
const OVERLAY_EVENT: &str = "quick-dictation-overlay";
/// The overlay window never resizes: the pill grows and shrinks inside this
/// transparent, click-through area, which also leaves room for its glow.
const OVERLAY_WIDTH: f64 = 560.0;
const OVERLAY_HEIGHT: f64 = 120.0;
/// Gap between the window and the bottom of the usable screen (above the Dock).
const OVERLAY_MARGIN_BOTTOM: f64 = 4.0;
/// How long the pill's exit animation runs before the window hides.
const OVERLAY_EXIT_MS: u64 = 260;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverlayPhase {
    Hidden,
    Mode,
    Listening,
    Processing,
    Inserted,
    ClipboardOnly,
    Failed,
    /// A neutral, short-lived message such as "No speech heard" or "Canceled".
    Notice,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StreamingState {
    Preparing,
    Waiting,
    Listening,
    CatchingUp,
    Finishing,
    /// Live pair: the final pass runs on the whole recording.
    Finalizing,
    /// Live pair: models are loading because they were not resident.
    Loading,
    Translating,
    Failed,
}
impl StreamingState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Preparing => "Preparing",
            Self::Waiting => "Waiting for local processing",
            Self::Listening => "Listening",
            Self::CatchingUp => "Catching up",
            Self::Finishing => "Finishing",
            Self::Finalizing => "Finalising…",
            Self::Loading => "Loading models",
            Self::Translating => "Translating",
            Self::Failed => "Needs attention",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationOverlayPayload {
    pub phase: OverlayPhase,
    pub audio_level: f32,
    pub output_mode: crate::translation::OutputMode,
    pub status_text: Option<String>,
    pub revision: u64,
    pub session_id: Option<String>,
    pub live_text: String,
    #[serde(default)]
    pub tentative_text: String,
    #[serde(skip)]
    pub stream_text_final: bool,
    pub streaming_state: Option<StreamingState>,
    pub lag_ms: u64,
    pub duration_limit_reached: bool,
}

impl Default for DictationOverlayPayload {
    fn default() -> Self {
        Self {
            phase: OverlayPhase::Hidden,
            audio_level: 0.0,
            output_mode: Default::default(),
            status_text: None,
            revision: 0,
            session_id: None,
            live_text: String::new(),
            tentative_text: String::new(),
            stream_text_final: false,
            streaming_state: None,
            lag_ms: 0,
            duration_limit_reached: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct StreamingText {
    pub committed: String,
    pub tentative: String,
}

impl DictationOverlayPayload {
    fn owns_stream(&self, session_id: &str) -> bool {
        self.session_id.as_deref() == Some(session_id)
            && matches!(
                self.phase,
                OverlayPhase::Listening | OverlayPhase::Processing
            )
    }

    fn apply_stream(
        &mut self,
        session_id: &str,
        state: StreamingState,
        text: Option<StreamingText>,
        lag_ms: u64,
    ) -> bool {
        if !self.owns_stream(session_id)
            || (self.stream_text_final && state != StreamingState::Failed)
            || self.streaming_state == Some(StreamingState::Failed)
        {
            return false;
        }
        if let Some(text) = text {
            self.live_text = text.committed;
            self.tentative_text = text.tentative;
        }
        if state == StreamingState::Failed {
            self.tentative_text.clear();
            self.stream_text_final = true;
        }
        self.streaming_state = Some(state);
        self.status_text = Some(
            if self.duration_limit_reached
                && matches!(state, StreamingState::Finishing | StreamingState::Finalizing)
            {
                "5-minute limit · Finishing"
            } else {
                state.label()
            }
            .into(),
        );
        self.lag_ms = lag_ms;
        self.revision = self.revision.wrapping_add(1);
        true
    }

    fn finalize_stream_text(&mut self, session_id: &str, text: String) -> bool {
        if !self.owns_stream(session_id)
            || self.streaming_state.is_none()
            || self.streaming_state == Some(StreamingState::Failed)
        {
            return false;
        }
        self.live_text = text;
        self.tentative_text.clear();
        self.stream_text_final = true;
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

#[derive(Clone)]
pub struct DesktopShellController {
    app: AppHandle,
    overlay_payload: Arc<Mutex<DictationOverlayPayload>>,
    tray: Arc<TrayIcon>,
    tray_activity: Arc<Mutex<TrayActivity>>,
    /// (transcript id or "last", text) behind the tray's Recent dictations.
    tray_recents: Arc<Mutex<Vec<(String, String)>>>,
    overlay_dispatch_pending: Arc<AtomicBool>,
}

/// What the menu-bar icon shows: a red dot while listening, an amber dot
/// while transcribing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrayActivity {
    Idle,
    Listening,
    Working,
}

impl DesktopShellController {
    pub fn initialize(app: &AppHandle) -> Result<Self> {
        ensure_overlay_window(app)?;
        let tray = Arc::new(build_tray_icon(app)?);
        Ok(Self {
            app: app.clone(),
            overlay_payload: Arc::new(Mutex::new(DictationOverlayPayload::default())),
            tray,
            tray_activity: Arc::new(Mutex::new(TrayActivity::Idle)),
            tray_recents: Default::default(),
            overlay_dispatch_pending: Default::default(),
        })
    }

    /// Shows a short result or notice chip, then hides it unless something
    /// newer replaced it in the meantime.
    pub fn flash(&self, phase: OverlayPhase, text: &str, hold: std::time::Duration) -> Result<()> {
        self.set_overlay_payload(DictationOverlayPayload {
            phase,
            status_text: Some(text.into()),
            ..Default::default()
        })?;
        let revision = self.overlay_revision();
        let shell = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            let _ = shell.set_overlay_if_revision(revision, DictationOverlayPayload::default());
        });
        Ok(())
    }

    /// Replaces the status line of a running transcription, e.g. to explain a
    /// shortcut press that arrived while Murmela was still busy.
    pub fn annotate_processing(&self, text: &str) -> Result<bool> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if current.phase != OverlayPhase::Processing {
            return Ok(false);
        }
        current.status_text = Some(text.into());
        current.revision = current.revision.wrapping_add(1);
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)?;
        Ok(true)
    }

    /// Rebuilds the tray menu with the latest dictations (newest first).
    pub fn set_tray_recents(&self, recents: Vec<(String, String)>) {
        if let Ok(mut current) = self.tray_recents.lock() {
            if *current == recents {
                return;
            }
            *current = recents.clone();
        }
        match build_tray_menu(&self.app, &recents) {
            Ok(menu) => {
                let _ = self.tray.set_menu(Some(menu));
            }
            Err(error) => eprintln!("[tray] menu rebuild failed: {error:#}"),
        }
    }

    pub fn tray_recent_text(&self, id: &str) -> Option<String> {
        self.tray_recents
            .lock()
            .ok()?
            .iter()
            .find(|(recent_id, _)| recent_id == id)
            .map(|(_, text)| text.clone())
    }

    fn sync_tray_activity(&self, phase: OverlayPhase) {
        let next = match phase {
            OverlayPhase::Listening => TrayActivity::Listening,
            OverlayPhase::Processing => TrayActivity::Working,
            _ => TrayActivity::Idle,
        };
        let Ok(mut current) = self.tray_activity.lock() else {
            return;
        };
        if *current == next {
            return;
        }
        *current = next;
        drop(current);
        let base = self.app.default_window_icon().cloned();
        let icon = match (next, base) {
            (TrayActivity::Idle, base) => base,
            (TrayActivity::Listening, Some(base)) => Some(icon_with_dot(&base, [226, 54, 54])),
            (TrayActivity::Working, Some(base)) => Some(icon_with_dot(&base, [232, 160, 32])),
            (_, None) => None,
        };
        if let Some(icon) = icon {
            let _ = self.tray.set_icon(Some(icon));
        }
        let _ = self.tray.set_tooltip(Some(match next {
            TrayActivity::Idle => "Murmela",
            TrayActivity::Listening => "Murmela · Listening · Esc cancels",
            TrayActivity::Working => "Murmela · Transcribing · Esc cancels",
        }));
    }

    pub fn overlay_payload(&self) -> DictationOverlayPayload {
        self.overlay_payload
            .lock()
            .map(|payload| payload.clone())
            .unwrap_or_default()
    }

    pub fn set_output_mode(&self, mode: crate::translation::OutputMode) {
        if let Ok(mut current) = self.overlay_payload.lock() {
            current.output_mode = mode;
        }
    }
    /// Shows a new output language in the running "Listening" overlay.
    pub fn show_output_mode(&self, mode: crate::translation::OutputMode) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        current.output_mode = mode;
        current.revision = current.revision.wrapping_add(1);
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub fn overlay_revision(&self) -> u64 {
        self.overlay_payload().revision
    }
    pub fn flash_mode(&self) -> Result<()> {
        self.set_overlay_payload(DictationOverlayPayload {
            phase: OverlayPhase::Mode,
            ..Default::default()
        })?;
        let revision = self.overlay_revision();
        let shell = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(2));
            let _ = shell.set_overlay_if_revision(revision, DictationOverlayPayload::default());
        });
        Ok(())
    }
    /// Short notice shown in place of "Listening" (e.g. the 5-minute warning).
    pub fn set_listening_notice(&self, session_id: &str, text: &str) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if current.session_id.as_deref() != Some(session_id)
            || current.phase != OverlayPhase::Listening
        {
            return Ok(());
        }
        current.status_text = Some(text.into());
        current.revision = current.revision.wrapping_add(1);
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub fn update_level(&self, session_id: &str, audio_level: f32) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if current.session_id.as_deref() != Some(session_id)
            || current.phase != OverlayPhase::Listening
        {
            return Ok(());
        }
        current.audio_level = audio_level;
        current.revision = current.revision.wrapping_add(1);
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub(crate) fn update_stream(
        &self,
        session_id: &str,
        state: StreamingState,
        text: Option<StreamingText>,
        lag_ms: u64,
    ) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if !current.apply_stream(session_id, state, text, lag_ms) {
            return Ok(());
        }
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub fn finalize_stream_text(&self, session_id: &str, text: String) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if !current.finalize_stream_text(session_id, text) {
            return Ok(());
        }
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub fn stream_duration_limit(&self, session_id: &str) {
        if let Ok(mut current) = self.overlay_payload.lock() {
            if current.session_id.as_deref() == Some(session_id) {
                current.duration_limit_reached = true;
            }
        }
    }
    pub fn processing_for_session(
        &self,
        session_id: &str,
        text: &str,
        translating: bool,
    ) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if current
            .session_id
            .as_deref()
            .is_some_and(|id| id != session_id)
        {
            return Ok(());
        }
        current.session_id = Some(session_id.into());
        current.phase = OverlayPhase::Processing;
        current.audio_level = 0.0;
        current.status_text = Some(
            if current.duration_limit_reached && !translating {
                "5-minute limit · Finishing"
            } else {
                text
            }
            .into(),
        );
        if current.streaming_state.is_some() {
            if translating {
                current.tentative_text.clear();
                current.stream_text_final = true;
            }
            current.streaming_state = Some(if translating {
                StreamingState::Translating
            } else {
                StreamingState::Finishing
            });
        }
        current.revision = current.revision.wrapping_add(1);
        let payload = current.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    pub fn set_overlay_if_revision(
        &self,
        revision: u64,
        payload: DictationOverlayPayload,
    ) -> Result<()> {
        self.replace_overlay(Some(revision), payload)
    }
    pub fn set_overlay_payload(&self, payload: DictationOverlayPayload) -> Result<()> {
        self.replace_overlay(None, payload)
    }
    fn replace_overlay(
        &self,
        expected: Option<u64>,
        mut payload: DictationOverlayPayload,
    ) -> Result<()> {
        let mut current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if expected.is_some_and(|revision| revision != current.revision) {
            return Ok(());
        }
        payload.output_mode = current.output_mode;
        if matches!(
            payload.phase,
            OverlayPhase::Inserted | OverlayPhase::ClipboardOnly
        ) {
            payload.duration_limit_reached |= current.duration_limit_reached;
        }
        payload.revision = current.revision.wrapping_add(1);
        *current = payload.clone();
        drop(current);
        self.dispatch_overlay(payload)
    }
    fn dispatch_overlay(&self, _payload: DictationOverlayPayload) -> Result<()> {
        if self.overlay_dispatch_pending.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let shell = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let apply = shell.clone();
            if shell
                .app
                .run_on_main_thread(move || {
                    apply
                        .overlay_dispatch_pending
                        .store(false, Ordering::SeqCst);
                    let payload = apply.overlay_payload();
                    let _ = apply.apply_overlay(payload);
                })
                .is_err()
            {
                shell
                    .overlay_dispatch_pending
                    .store(false, Ordering::SeqCst);
            }
        });
        Ok(())
    }
    fn apply_overlay(&self, payload: DictationOverlayPayload) -> Result<()> {
        // Serialize window effects and recheck ownership on the main thread.
        let current = self
            .overlay_payload
            .lock()
            .map_err(|_| anyhow::anyhow!("Overlay unavailable"))?;
        if current.revision != payload.revision {
            return Ok(());
        }

        let payload = if crate::shutdown::is_shutting_down() {
            DictationOverlayPayload::default()
        } else {
            payload
        };
        if let Some(window) = self.app.get_webview_window(OVERLAY_LABEL) {
            match payload.phase {
                OverlayPhase::Hidden => {
                    // Let the pill animate out first; a newer payload cancels the hide.
                    let shell = self.clone();
                    let revision = payload.revision;
                    std::thread::spawn(move || {
                        std::thread::sleep(std::time::Duration::from_millis(OVERLAY_EXIT_MS));
                        let apply = shell.clone();
                        let _ = shell.app.run_on_main_thread(move || {
                            let current = apply.overlay_payload();
                            if current.revision == revision && current.phase == OverlayPhase::Hidden {
                                if let Some(window) = apply.app.get_webview_window(OVERLAY_LABEL) {
                                    let _ = window.hide();
                                }
                            }
                        });
                    });
                }
                OverlayPhase::Mode
                | OverlayPhase::Listening
                | OverlayPhase::Processing
                | OverlayPhase::Inserted
                | OverlayPhase::ClipboardOnly
                | OverlayPhase::Failed
                | OverlayPhase::Notice => {
                    // Place it once per appearance, on the screen being worked on,
                    // so it never jumps while visible.
                    if !window.is_visible().unwrap_or(false) {
                        let _ = position_overlay_window(&window);
                    }
                    let _ = window.show();
                }
            }
        }
        self.sync_tray_activity(payload.phase);

        self.app.emit(OVERLAY_EVENT, payload)?;
        Ok(())
    }
}

fn ensure_overlay_window(app: &AppHandle) -> Result<()> {
    if app.get_webview_window(OVERLAY_LABEL).is_some() {
        return Ok(());
    }

    let window =
        WebviewWindowBuilder::new(app, OVERLAY_LABEL, WebviewUrl::App("overlay.html".into()))
            .title("Murmela Overlay")
            .inner_size(OVERLAY_WIDTH, OVERLAY_HEIGHT)
            .transparent(true)
            .decorations(false)
            .shadow(false)
            .resizable(false)
            .visible(false)
            .always_on_top(true)
            .skip_taskbar(true)
            .focused(false)
            .focusable(false)
            .build()?;
    window.set_ignore_cursor_events(true)?;
    position_overlay_window(&window)?;
    Ok(())
}

/// Bottom centre of the usable area (above the Dock) of the screen with the pointer.
fn position_overlay_window(window: &tauri::WebviewWindow) -> Result<()> {
    let pointer_monitor = window
        .cursor_position()
        .ok()
        .and_then(|point| window.monitor_from_point(point.x, point.y).ok().flatten());
    let Some(monitor) = pointer_monitor
        .or(window.current_monitor()?)
        .or(window.primary_monitor()?)
    else {
        return Ok(());
    };
    let area = monitor.work_area();
    let scale = monitor.scale_factor();
    let (x, y) = overlay_origin(
        (area.position.x, area.position.y),
        (area.size.width, area.size.height),
        scale,
    );
    window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x, y)))?;
    Ok(())
}

fn overlay_origin(position: (i32, i32), size: (u32, u32), scale: f64) -> (i32, i32) {
    let x = position.0 as f64 + ((size.0 as f64 - OVERLAY_WIDTH * scale) / 2.0).max(0.0);
    let y = position.1 as f64 + size.1 as f64 - (OVERLAY_HEIGHT + OVERLAY_MARGIN_BOTTOM) * scale;
    (x.round() as i32, y.round() as i32)
}

fn build_tray_menu(app: &AppHandle, recents: &[(String, String)]) -> Result<Menu<tauri::Wry>> {
    let mut recent_menu = SubmenuBuilder::new(app, "Recent dictations");
    if recents.is_empty() {
        recent_menu = recent_menu.item(
            &tauri::menu::MenuItemBuilder::with_id("recent-none", "Nothing yet")
                .enabled(false)
                .build(app)?,
        );
    }
    for (id, text) in recents {
        recent_menu = recent_menu.text(format!("recent:{id}"), menu_preview(text));
    }
    MenuBuilder::new(app)
        .text("paste-last", "Paste last dictation")
        .text("copy-last", "Copy last dictation")
        .item(&recent_menu.build()?)
        .separator()
        .text("show", "Open Murmela")
        .text("quit", "Quit Murmela")
        .build()
        .map_err(Into::into)
}

/// One line of a dictation for a menu: about 40 characters, whitespace folded.
fn menu_preview(text: &str) -> String {
    let folded = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = folded.chars().take(40).collect();
    if folded.chars().count() > 40 {
        preview = preview.trim_end().to_string() + "…";
    }
    preview
}

/// The app icon with a status dot in its lower-right corner.
fn icon_with_dot(base: &Image<'_>, color: [u8; 3]) -> Image<'static> {
    let (width, height) = (base.width(), base.height());
    let mut rgba = base.rgba().to_vec();
    let size = width.min(height) as f32;
    let radius = size * 0.2;
    let ring = (size * 0.05).max(1.0);
    let (cx, cy) = (width as f32 - radius - ring, height as f32 - radius - ring);
    for y in 0..height {
        for x in 0..width {
            let distance = ((x as f32 + 0.5 - cx).powi(2) + (y as f32 + 0.5 - cy).powi(2)).sqrt();
            let pixel = ((y * width + x) * 4) as usize;
            if distance <= radius {
                rgba[pixel..pixel + 4].copy_from_slice(&[color[0], color[1], color[2], 255]);
            } else if distance <= radius + ring {
                rgba[pixel..pixel + 4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
    }
    Image::new_owned(rgba, width, height)
}

fn with_dictation_controller(
    app: &AppHandle,
    action: impl FnOnce(crate::dictation::QuickDictationController) + Send + 'static,
) {
    if let Some(state) = app.try_state::<crate::app_state::AppState>() {
        let controller = state.dictation_controller.clone();
        // Pasting waits for focus and keys; never block the menu's main thread.
        std::thread::spawn(move || action(controller));
    }
}

fn build_tray_icon(app: &AppHandle) -> Result<TrayIcon> {
    let menu = build_tray_menu(app, &[])?;

    let default_icon = app.default_window_icon().cloned();
    let mut builder = TrayIconBuilder::with_id("murmela-tray")
        .menu(&menu)
        .tooltip("Murmela")
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                let _ = show_main_window(app);
            }
            "quit" => crate::shutdown::request_exit(app, crate::shutdown::ExitAction::Quit),
            "paste-last" => with_dictation_controller(app, |controller| {
                if let Err(error) = controller.paste_last() {
                    eprintln!("[tray] paste last failed: {error:#}");
                }
            }),
            "copy-last" => with_dictation_controller(app, |controller| {
                if let Err(error) = controller.copy_last() {
                    eprintln!("[tray] copy last failed: {error:#}");
                }
            }),
            id => {
                if let Some(recent) = id.strip_prefix("recent:") {
                    let recent = recent.to_string();
                    with_dictation_controller(app, move |controller| {
                        if let Err(error) = controller.copy_recent(&recent) {
                            eprintln!("[tray] copy recent failed: {error:#}");
                        }
                    });
                }
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let _ = show_main_window(tray.app_handle());
            }
        });

    if let Some(icon) = default_icon {
        builder = builder.icon(icon);
    }

    builder.build(app).map_err(Into::into)
}

pub fn show_main_window(app: &AppHandle) -> Result<()> {
    if let Some(startup) = app.try_state::<StartupCoordinator>() {
        if startup.status().phase != StartupPhase::Ready {
            if let Some(splash) = app.get_webview_window("splashscreen") {
                splash.show()?;
                splash.unminimize()?;
                splash.set_focus()?;
            }
            return Ok(());
        }
    }
    if let Some(window) = app.get_webview_window("main") {
        window.show()?;
        window.unminimize()?;
        window.set_focus()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_sits_centred_just_above_the_bottom_of_the_work_area() {
        // A 1512×945 pt Retina work area whose Dock leaves 870 pt usable height.
        let (x, y) = overlay_origin((0, 50), (3024, 1740), 2.0);
        assert_eq!(x, (3024 - 560 * 2) / 2);
        assert_eq!(y, 50 + 1740 - (120 + 4) * 2);
    }

    #[test]
    fn preview_is_atomic_owned_and_cannot_return_after_finalization() {
        let mut state = DictationOverlayPayload {
            phase: OverlayPhase::Listening,
            session_id: Some("live".into()),
            streaming_state: Some(StreamingState::Listening),
            ..Default::default()
        };
        let preview = || {
            Some(StreamingText {
                committed: "Größe".into(),
                tentative: " über".into(),
            })
        };
        assert!(!state.apply_stream("stale", StreamingState::Listening, preview(), 0));
        assert_eq!(state.revision, 0);
        assert!(state.apply_stream("live", StreamingState::Listening, preview(), 50));
        assert_eq!(
            (&*state.live_text, &*state.tentative_text, state.revision),
            ("Größe", " über", 1)
        );
        assert!(state.apply_stream("live", StreamingState::CatchingUp, None, 2100));
        assert_eq!(
            (&*state.live_text, &*state.tentative_text, state.revision),
            ("Größe", " über", 2)
        );
        state.phase = OverlayPhase::Processing;
        assert!(state.apply_stream("live", StreamingState::Finishing, None, 0));
        assert_eq!(state.tentative_text, " über");
        assert!(state.finalize_stream_text("live", "Größe über die Straße.".into()));
        assert!(state.tentative_text.is_empty());
        let revision = state.revision;
        assert!(!state.apply_stream("live", StreamingState::Listening, preview(), 0));
        assert_eq!(state.revision, revision);
        // Vocabulary corrections can replace only the completed source display.
        assert!(state.finalize_stream_text("live", "Corrected source.".into()));
        assert_eq!(state.live_text, "Corrected source.");
        assert!(!state.finalize_stream_text("stale", "old".into()));
    }
    #[test]
    fn failed_hidden_and_new_sessions_cannot_retain_tentative_text() {
        let mut state = DictationOverlayPayload {
            phase: OverlayPhase::Listening,
            session_id: Some("live".into()),
            streaming_state: Some(StreamingState::Listening),
            tentative_text: "draft".into(),
            ..Default::default()
        };
        assert!(state.apply_stream("live", StreamingState::Failed, None, 0));
        assert!(state.tentative_text.is_empty());
        assert!(!state.apply_stream(
            "live",
            StreamingState::Listening,
            Some(StreamingText {
                committed: "".into(),
                tentative: "late".into()
            }),
            0
        ));
        state = Default::default();
        assert!(state.tentative_text.is_empty());
        assert!(!state.apply_stream("live", StreamingState::Listening, None, 0));
        let json = serde_json::to_value(&state).unwrap();
        assert_eq!(json["tentativeText"], "");
        assert!(json.get("streamTextFinal").is_none());
    }

    #[test]
    fn menu_preview_folds_whitespace_and_shortens() {
        assert_eq!(menu_preview("  Hello\n world "), "Hello world");
        let long = "word ".repeat(20);
        let preview = menu_preview(&long);
        assert!(preview.ends_with('…'));
        assert!(preview.chars().count() <= 41);
    }

    #[test]
    fn status_dot_keeps_size_and_paints_the_corner() {
        let base = Image::new_owned(vec![0; 32 * 32 * 4], 32, 32);
        let dotted = icon_with_dot(&base, [226, 54, 54]);
        assert_eq!((dotted.width(), dotted.height()), (32, 32));
        let corner = ((24 * 32 + 24) * 4) as usize;
        assert_eq!(&dotted.rgba()[corner..corner + 4], &[226, 54, 54, 255]);
        assert_eq!(&dotted.rgba()[0..4], &[0, 0, 0, 0]);
    }
}
