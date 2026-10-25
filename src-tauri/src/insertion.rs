use std::ffi::c_void;
use std::process::Command;
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::settings::InsertBehavior;

mod clipboard_restore;

use clipboard_restore::ClipboardSnapshot;

const KCG_HID_EVENT_TAP: u32 = 0;
const KCG_EVENT_FLAG_MASK_COMMAND: u64 = 1 << 20;
const MACOS_KEYCODE_V: u16 = 0x09;

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    static kAXTrustedCheckOptionPrompt: *const c_void;
    fn CGEventCreateKeyboardEvent(
        source: *const c_void,
        virtual_key: u16,
        key_down: bool,
    ) -> *mut c_void;
    fn CGEventSetFlags(event: *mut c_void, flags: u64);
    fn CGEventPost(tap: u32, event: *mut c_void);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventSourceFlagsState(state_id: i32) -> u64;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(cf: *const c_void);
    fn CFDictionaryCreate(
        allocator: *const c_void,
        keys: *const *const c_void,
        values: *const *const c_void,
        count: isize,
        key_callbacks: *const CFDictionaryCallBacks,
        value_callbacks: *const CFDictionaryCallBacks,
    ) -> *const c_void;
    static kCFBooleanTrue: *const c_void;
    static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
    static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
}

#[repr(C)]
struct CFDictionaryCallBacks {
    _opaque: [u8; 0],
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InsertionOutcome {
    Pasted,
    ClipboardOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertionWarning {
    AccessibilityRequired,
    PasteUnavailable,
}

impl InsertionWarning {
    pub fn message(self) -> &'static str {
        match self {
            Self::AccessibilityRequired => "Text copied. To paste automatically, use Grant access and allow Murmela under Privacy & Security → Accessibility. Already allowed? Remove Murmela there and add it again.",
            Self::PasteUnavailable => "Text copied, but Murmela couldn't paste it into the app. Paste it yourself.",
        }
    }

    pub fn overlay_label(self) -> &'static str {
        match self {
            Self::AccessibilityRequired => "Copied · Enable auto-paste",
            Self::PasteUnavailable => "Copied · Paste manually",
        }
    }
}

#[derive(Debug)]
pub struct InsertionReport {
    pub outcome: InsertionOutcome,
    pub warning: Option<InsertionWarning>,
}

impl InsertionReport {
    fn copied(warning: Option<InsertionWarning>) -> Self {
        Self {
            outcome: InsertionOutcome::ClipboardOnly,
            warning,
        }
    }

    fn pasted() -> Self {
        Self {
            outcome: InsertionOutcome::Pasted,
            warning: None,
        }
    }
}

fn paste_warning(trusted: bool) -> Option<InsertionWarning> {
    (!trusted).then_some(InsertionWarning::AccessibilityRequired)
}

/// The app that was frontmost when dictation started, by bundle identifier.
#[derive(Debug, Clone)]
pub struct PasteTarget(String);

pub fn insert_text(
    app: &AppHandle,
    text: &str,
    behavior: InsertBehavior,
    paste_target: Option<&PasteTarget>,
    check_active: impl Fn() -> Result<()>,
) -> Result<InsertionReport> {
    check_active()?;
    let warning = paste_warning(accessibility_trusted());
    let can_paste = should_attempt_paste(behavior, warning.is_none());
    let clipboard_snapshot = can_paste.then(ClipboardSnapshot::capture);

    check_active()?;
    app.clipboard().write_text(text.to_string())?;
    let dictated_clipboard_change_count = clipboard_restore::change_count();

    // A reset may arrive during clipboard propagation, refocusing, or a retry.
    // Restore only our unchanged temporary clipboard, then stop before the key event.
    macro_rules! ensure_current {
        () => {
            if let Err(error) = check_active() {
                restore_clipboard_after_paste(clipboard_snapshot, dictated_clipboard_change_count);
                return Err(error);
            }
        };
    }

    match behavior {
        InsertBehavior::ClipboardOnly => Ok(InsertionReport::copied(None)),
        InsertBehavior::Paste => {
            if !can_paste {
                return Ok(InsertionReport::copied(warning));
            }

            // Give the system clipboard a moment to propagate before sending ⌘V.
            thread::sleep(Duration::from_millis(120));
            ensure_current!();

            let focus = refocus_paste_target(paste_target);
            ensure_current!();
            if focus.is_err() {
                return Ok(InsertionReport::copied(Some(
                    InsertionWarning::PasteUnavailable,
                )));
            }

            match simulate_paste_with_retry(3, &check_active, simulate_paste) {
                Ok(()) => {
                    restore_clipboard_after_paste(
                        clipboard_snapshot,
                        dictated_clipboard_change_count,
                    );
                    Ok(InsertionReport::pasted())
                }
                Err(_) => {
                    ensure_current!();
                    Ok(InsertionReport::copied(Some(
                        paste_warning(accessibility_trusted())
                            .unwrap_or(InsertionWarning::PasteUnavailable),
                    )))
                }
            }
        }
    }
}

fn restore_clipboard_after_paste(
    clipboard_snapshot: Option<ClipboardSnapshot>,
    dictated_clipboard_change_count: isize,
) {
    let Some(snapshot) = clipboard_snapshot else {
        return;
    };

    // CGEventPost queues the shortcut. Give the target app time to consume
    // the dictated text before replacing the temporary clipboard contents.
    thread::sleep(Duration::from_millis(300));

    // The change-count guard protects content copied by the user (or another
    // app) while the paste was in flight.
    if let Err(error) = snapshot.restore_if_unchanged(dictated_clipboard_change_count) {
        eprintln!("failed to restore clipboard after auto-paste: {error:#}");
    }
}

/// Waits until no modifier key is held (or `timeout` passes), so a paste
/// triggered from a shortcut does not combine with the shortcut's modifiers.
pub fn wait_for_modifier_release(timeout: Duration) {
    // kCGEventSourceStateCombinedSessionState; Shift, Control, Option, Command.
    const COMBINED_SESSION_STATE: i32 = 0;
    const MODIFIERS: u64 = (1 << 17) | (1 << 18) | (1 << 19) | (1 << 20);
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        if unsafe { CGEventSourceFlagsState(COMBINED_SESSION_STATE) } & MODIFIERS == 0 {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

/// Whether macOS trusts Murmela to synthesize keystrokes (Accessibility
/// access), which auto-paste needs.
pub fn accessibility_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

/// Shows macOS's own Accessibility prompt ("would like to control this
/// computer"), which also adds Murmela to the list in System Settings so the
/// user only has to switch it on. Call it only after Murmela has explained why
/// it asks; nothing else in the app may trigger the prompt.
pub fn request_accessibility_access() {
    unsafe {
        let keys = [kAXTrustedCheckOptionPrompt];
        let values = [kCFBooleanTrue];
        let options = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            values.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        if options.is_null() {
            return;
        }
        let _ = AXIsProcessTrustedWithOptions(options);
        CFRelease(options);
    }
}

/// Opens the System Settings pane where the user grants Accessibility access.
pub fn open_accessibility_settings() {
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .spawn();
}

fn should_attempt_paste(behavior: InsertBehavior, accessibility_trusted: bool) -> bool {
    matches!(behavior, InsertBehavior::Paste) && accessibility_trusted
}

fn simulate_paste_with_retry(
    attempts: usize,
    check_active: &impl Fn() -> Result<()>,
    mut paste: impl FnMut() -> Result<()>,
) -> Result<()> {
    let mut last_error = None;
    for index in 0..attempts {
        check_active()?;
        match paste() {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                if index + 1 < attempts {
                    thread::sleep(Duration::from_millis(120));
                }
            }
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow!("paste simulation failed")))
}

fn refocus_paste_target(paste_target: Option<&PasteTarget>) -> Result<()> {
    if let Some(PasteTarget(bundle_id)) = paste_target {
        let status = Command::new("open").arg("-b").arg(bundle_id).status()?;
        if !status.success() {
            return Err(anyhow!("failed to reactivate target app for paste"));
        }
        thread::sleep(Duration::from_millis(180));
    }
    Ok(())
}

fn simulate_paste() -> Result<()> {
    unsafe {
        if !AXIsProcessTrusted() {
            return Err(anyhow!(InsertionWarning::AccessibilityRequired.message()));
        }

        let key_down = CGEventCreateKeyboardEvent(std::ptr::null(), MACOS_KEYCODE_V, true);
        let key_up = CGEventCreateKeyboardEvent(std::ptr::null(), MACOS_KEYCODE_V, false);

        if key_down.is_null() || key_up.is_null() {
            if !key_down.is_null() {
                CFRelease(key_down);
            }
            if !key_up.is_null() {
                CFRelease(key_up);
            }
            return Err(anyhow!(
                "paste simulation failed: could not create keyboard event"
            ));
        }

        CGEventSetFlags(key_down, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventSetFlags(key_up, KCG_EVENT_FLAG_MASK_COMMAND);
        CGEventPost(KCG_HID_EVENT_TAP, key_down);
        CGEventPost(KCG_HID_EVENT_TAP, key_up);
        CFRelease(key_down);
        CFRelease(key_up);
    }

    Ok(())
}

/// The app to bring back before pasting. None when Murmela itself is in front
/// (for example its welcome practice box): ⌘V then goes to its own window, and
/// a development build has no bundle identifier to reactivate.
///
/// Read from NSWorkspace, which needs no permission. Asking System Events
/// instead made macOS ask to let Murmela "control System Events" on the first
/// dictation.
pub fn detect_frontmost_paste_target() -> Option<PasteTarget> {
    let (pid, bundle_id) = frontmost_application()?;
    paste_target_from(pid, &bundle_id, std::process::id())
}

/// The frontmost app's process id and bundle identifier ("" when it has none).
fn frontmost_application() -> Option<(i32, String)> {
    use objc2::msg_send;
    use objc2::rc::autoreleasepool;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    autoreleasepool(|_| unsafe {
        let workspace: *mut AnyObject = msg_send![AnyClass::get(c"NSWorkspace")?, sharedWorkspace];
        let app: *mut AnyObject = msg_send![workspace.as_ref()?, frontmostApplication];
        let app = app.as_ref()?;
        let pid: i32 = msg_send![app, processIdentifier];
        let bundle_id: *mut NSString = msg_send![app, bundleIdentifier];
        Some((pid, bundle_id.as_ref().map(|id| id.to_string()).unwrap_or_default()))
    })
}

fn paste_target_from(pid: i32, bundle_id: &str, own_pid: u32) -> Option<PasteTarget> {
    if u32::try_from(pid).ok() == Some(own_pid) {
        return None;
    }
    let bundle_id = bundle_id.trim();
    (!bundle_id.is_empty()).then(|| PasteTarget(bundle_id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn murmela_in_front_needs_no_app_to_reactivate() {
        assert_eq!(paste_target_from(42, "com.apple.TextEdit", 7).map(|t| t.0), Some("com.apple.TextEdit".into()));
        assert!(paste_target_from(7, "com.murmela.app", 7).is_none(), "Murmela itself");
        assert!(paste_target_from(42, "", 7).is_none(), "no bundle to reactivate");
    }

    #[test]
    fn reads_the_frontmost_app_without_system_events() {
        // Runs in the test process; NSWorkspace answers without any permission.
        // A headless run may have no frontmost app, which is fine.
        if let Some((pid, _)) = frontmost_application() {
            assert!(pid > 0);
        }
    }

    #[test]
    fn missing_accessibility_keeps_a_copy_and_explains_why_paste_was_skipped() {
        let warning = paste_warning(false);
        assert!(!should_attempt_paste(
            InsertBehavior::Paste,
            warning.is_none()
        ));
        let report = InsertionReport::copied(warning);
        assert!(matches!(report.outcome, InsertionOutcome::ClipboardOnly));
        assert_eq!(
            report.warning,
            Some(InsertionWarning::AccessibilityRequired)
        );
        assert!(report.warning.unwrap().message().contains("Grant access"));
        assert_eq!(paste_warning(true), None);
        assert!(InsertionReport::copied(None).warning.is_none());
    }

    #[test]
    fn cancellation_before_a_paste_or_retry_never_sends_another_key_event() {
        let active = std::cell::Cell::new(false);
        let attempts = std::cell::Cell::new(0);
        let check = || {
            if active.get() {
                Ok(())
            } else {
                Err(anyhow!("canceled"))
            }
        };
        let paste = || {
            attempts.set(attempts.get() + 1);
            active.set(false);
            Err(anyhow!("temporary insertion failure"))
        };
        assert!(simulate_paste_with_retry(3, &check, paste).is_err());
        assert_eq!(attempts.get(), 0);
        active.set(true);
        assert!(simulate_paste_with_retry(3, &check, paste).is_err());
        assert_eq!(attempts.get(), 1);
    }

    #[test]
    fn paste_is_attempted_only_when_pasting_is_allowed() {
        assert!(!should_attempt_paste(InsertBehavior::Paste, false));
        assert!(should_attempt_paste(InsertBehavior::Paste, true));
        assert!(!should_attempt_paste(InsertBehavior::ClipboardOnly, true));
    }
}
