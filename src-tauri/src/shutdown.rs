//! One quit path for Tauri, the tray, and AppKit termination requests.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, Result};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_dialog::{
    DialogExt, MessageDialogButtons, MessageDialogKind, MessageDialogResult,
};

use crate::app_state::AppState;

/// Confirms quitting while work is running; it stops that work.
const QUIT_BUTTON: &str = "Stop and quit";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Running,
    Confirming,
    Stopping,
    Ready,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitAction {
    Quit,
    Restart,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    Confirm,
    Stop,
    Ignore,
}
struct State {
    phase: Phase,
    active: usize,
    transcriptions: usize,
    manual_handoff: bool,
}
struct Lifecycle {
    state: Mutex<State>,
    drained: Condvar,
    stopping: AtomicBool,
}
impl Lifecycle {
    fn new() -> Self {
        Self {
            state: Mutex::new(State {
                phase: Phase::Running,
                active: 0,
                transcriptions: 0,
                manual_handoff: false,
            }),
            drained: Condvar::new(),
            stopping: AtomicBool::new(false),
        }
    }
    fn work(self: &Arc<Self>, transcription: bool) -> Result<WorkGuard> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(state.phase, Phase::Stopping | Phase::Ready) {
            return Err(anyhow!("APP_SHUTTING_DOWN: Murmela is quitting."));
        }
        state.active += 1;
        state.transcriptions += usize::from(transcription);
        Ok(WorkGuard {
            lifecycle: self.clone(),
            transcription,
        })
    }
    fn request(&self, recording_or_queued_work: bool) -> Decision {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Running {
            return Decision::Ignore;
        }
        if recording_or_queued_work || state.transcriptions > 0 || state.manual_handoff {
            state.phase = Phase::Confirming;
            Decision::Confirm
        } else {
            state.phase = Phase::Stopping;
            self.stopping.store(true, Ordering::SeqCst);
            Decision::Stop
        }
    }
    fn confirm(&self, quit: bool) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.phase != Phase::Confirming {
            return false;
        }
        state.phase = if quit {
            Phase::Stopping
        } else {
            Phase::Running
        };
        self.stopping.store(quit, Ordering::SeqCst);
        quit
    }
    fn wait_for_work(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .drained
            .wait_timeout_while(state, Duration::from_millis(100), |s| s.active != 0)
            .unwrap_or_else(|e| e.into_inner());
        state.active == 0
    }
    fn mark_ready(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            state.active, 0,
            "resources cannot be released while work is active"
        );
        state.phase = Phase::Ready;
    }
}

pub struct WorkGuard {
    lifecycle: Arc<Lifecycle>,
    transcription: bool,
}
impl Drop for WorkGuard {
    fn drop(&mut self) {
        let mut state = self
            .lifecycle
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        state.transcriptions -= usize::from(self.transcription);
        if state.active == 0 {
            self.lifecycle.drained.notify_all();
        }
    }
}
fn lifecycle() -> &'static Arc<Lifecycle> {
    static INSTANCE: OnceLock<Arc<Lifecycle>> = OnceLock::new();
    INSTANCE.get_or_init(|| Arc::new(Lifecycle::new()))
}
pub fn begin_work(transcription: bool) -> Result<WorkGuard> {
    lifecycle().work(transcription)
}
// Bridge the two IPC commands (stop recording -> preview transcription), so
// a quit between them still recognizes the unfinished dictation.
pub fn set_manual_handoff(pending: bool) {
    lifecycle()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .manual_handoff = pending;
}
pub fn is_shutting_down() -> bool {
    lifecycle().stopping.load(Ordering::SeqCst)
}
pub fn ensure_running() -> Result<()> {
    if is_shutting_down() {
        Err(anyhow!("TRANSCRIPTION_CANCELED: Murmela is quitting."))
    } else {
        Ok(())
    }
}
pub fn ready_to_exit() -> bool {
    lifecycle()
        .state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .phase
        == Phase::Ready
}

pub fn request_exit(app: &AppHandle, action: ExitAction) {
    let busy = app
        .try_state::<AppState>()
        .map(|state| {
            let recording = state
                .recording_controller
                .status()
                .map(|s| {
                    matches!(
                        s.state,
                        crate::audio_capture::RecordingOverlayState::Listening
                    )
                })
                .unwrap_or(true);
            recording
                || state.meetings.is_recording()
                || state
                    .file_transcription_controller
                    .statuses()
                    .iter()
                    .any(|job| {
                        matches!(
                            job.stage,
                            crate::file_jobs::FileTranscriptionJobStage::Queued
                                | crate::file_jobs::FileTranscriptionJobStage::Preparing
                                | crate::file_jobs::FileTranscriptionJobStage::Transcribing
                                | crate::file_jobs::FileTranscriptionJobStage::Diarizing
                                | crate::file_jobs::FileTranscriptionJobStage::Saving
                        )
                    })
        })
        .unwrap_or(false);
    match lifecycle().request(busy) {
        Decision::Ignore => (),
        Decision::Stop => start_shutdown(app.clone(), action),
        Decision::Confirm => {
            let app = app.clone();
            let _ = crate::desktop_shell::show_main_window(&app);
            app.dialog().message("A recording or transcription is still running. Quitting stops it. Transcripts that are already saved stay in your Library, and a call being recorded is kept so you can transcribe it later.")
                .title("Quit Murmela?")
                .kind(MessageDialogKind::Warning)
                    .buttons(MessageDialogButtons::OkCancelCustom(QUIT_BUTTON.into(), "Keep working".into()))
                .show_with_result(move |result| {
                    let quit = matches!(result, MessageDialogResult::Custom(ref label) if label == QUIT_BUTTON);
                    if lifecycle().confirm(quit) { start_shutdown(app, action); }
                });
        }
    }
}

fn start_shutdown(app: AppHandle, action: ExitAction) {
    let _ = app.emit("app://shutdown-started", ());
    std::thread::spawn(move || {
        let mut initialized = false;
        loop {
            if let Some(state) = app.try_state::<AppState>() {
                if !initialized {
                    state.dictation_controller.prepare_shutdown();
                    // Keep a call that is still recording: it is offered for transcription next time.
                    if state.meetings.is_recording() {
                        if let Err(error) = state.meetings.stop() {
                            eprintln!("[call] could not save the call on quit: {error:#}");
                        }
                    }
                    initialized = true;
                }
                state.file_transcription_controller.cancel_for_shutdown();
                state.model_download_manager.cancel_for_shutdown();
                state.review_jobs.cancel_all();
            }
            if lifecycle().wait_for_work() {
                break;
            }
        }
        // The operation gate is closed, and all in-flight work has dropped its
        // context references. Release cached Metal buffers before AppKit/libc
        // starts running the ggml C++ static destructors.
        if let Some(state) = app.try_state::<AppState>() {
            if !initialized {
                state.dictation_controller.prepare_shutdown();
            }
            state.review_media.clear();
            state.engine.release_resources();
            if let Some(player) = state.sound_player.as_ref().as_ref() {
                player.shutdown();
            }
        }
        lifecycle().mark_ready();
        match action {
            ExitAction::Quit => app.exit(0),
            ExitAction::Restart => app.restart(),
        }
    });
}

pub fn install_macos_termination_handler(app: &AppHandle) -> Result<()> {
    macos::install(app)
}

mod macos {
    use super::*;
    use objc2::runtime::{AnyClass, AnyObject, Imp, Sel};
    use objc2::{class, msg_send, sel};
    static APP: OnceLock<AppHandle> = OnceLock::new();

    // Tao 0.34 implements applicationWillTerminate, but not
    // applicationShouldTerminate. The latter must return NSTerminateCancel
    // while our asynchronous confirmation/cleanup runs. Adding this optional
    // delegate method preserves Tao's existing delegate and other callbacks.
    unsafe extern "C-unwind" fn should_terminate(
        _: &AnyObject,
        _: Sel,
        _: *mut AnyObject,
    ) -> usize {
        if ready_to_exit() {
            return 1;
        } // NSTerminateNow
        if let Some(app) = APP.get() {
            // Leave the AppKit delegate callback before showing a modal alert.
            let app = app.clone();
            std::thread::spawn(move || {
                let handle = app.clone();
                let _ = app.run_on_main_thread(move || request_exit(&handle, ExitAction::Quit));
            });
        }
        0 // NSTerminateCancel
    }
    pub fn install(app: &AppHandle) -> Result<()> {
        APP.set(app.clone())
            .map_err(|_| anyhow!("macOS quit handler already installed"))?;
        // SAFETY: setup runs on AppKit's main thread. The delegate is retained
        // by NSApplication; the added IMP and AppHandle live for the process.
        unsafe {
            let application: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let delegate: *mut AnyObject = msg_send![application, delegate];
            let delegate = delegate
                .as_ref()
                .ok_or_else(|| anyhow!("macOS app delegate unavailable"))?;
            let class = delegate.class();
            let selector = sel!(applicationShouldTerminate:);
            if class.instance_method(selector).is_some() {
                return Err(anyhow!(
                    "macOS termination delegate already implements applicationShouldTerminate"
                ));
            }
            let imp: Imp = std::mem::transmute(
                should_terminate
                    as unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize,
            );
            let added = objc2::ffi::class_addMethod(
                class as *const AnyClass as *mut AnyClass,
                selector,
                imp,
                c"Q@:@".as_ptr(),
            );
            if !added.as_bool() {
                return Err(anyhow!("failed to install macOS quit handler"));
            }
        }
        Ok(())
    }
}

// Only for the isolated ignored decoder smoke test; never part of the app API.
#[cfg(test)]
pub(crate) fn begin_shutdown_for_decoder_test() {
    match lifecycle().request(false) {
        Decision::Stop => (),
        Decision::Confirm => {
            assert!(lifecycle().confirm(true));
        }
        Decision::Ignore => panic!("decoder smoke test must run in a fresh process"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn idle_quit_closes_gate_without_prompting() {
        let life = Arc::new(Lifecycle::new());
        assert_eq!(life.request(false), Decision::Stop);
        assert!(life.work(true).is_err());
        assert!(life.wait_for_work());
        life.mark_ready();
    }
    #[test]
    fn continuing_keeps_work_and_allows_future_quit_requests() {
        let life = Arc::new(Lifecycle::new());
        let work = life.work(true).unwrap();
        assert_eq!(life.request(false), Decision::Confirm);
        assert_eq!(life.request(false), Decision::Ignore);
        assert!(!life.confirm(false));
        assert!(life.work(false).is_ok());
        drop(work);
        assert_eq!(life.request(false), Decision::Stop);
    }
    #[test]
    fn confirmed_quit_waits_for_all_work_before_releasing_resources() {
        let life = Arc::new(Lifecycle::new());
        let work = life.work(true).unwrap();
        assert_eq!(life.request(false), Decision::Confirm);
        assert!(life.confirm(true));
        assert!(!life.wait_for_work());
        assert!(life.work(true).is_err());
        drop(work);
        assert!(life.wait_for_work());
        life.mark_ready();
        assert_eq!(life.request(false), Decision::Ignore);
    }
    #[test]
    fn recording_to_transcription_handoff_still_requires_confirmation() {
        let life = Lifecycle::new();
        life.state.lock().unwrap().manual_handoff = true;
        assert_eq!(life.request(false), Decision::Confirm);
    }
    #[test]
    fn microphone_and_queued_jobs_prompt_even_before_inference_begins() {
        let life = Lifecycle::new();
        assert_eq!(life.request(true), Decision::Confirm);
    }
}
