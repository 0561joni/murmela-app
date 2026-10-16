use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const STARTUP_STATUS_EVENT: &str = "app://startup-status";
pub const STARTUP_TOTAL_STEPS: u8 = 6;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupPhase {
    Files,
    Models,
    Audio,
    Library,
    Shortcuts,
    Workspace,
    Ready,
    Failed,
}

impl StartupPhase {
    fn step(self) -> u8 {
        match self {
            Self::Files => 1,
            Self::Models => 2,
            Self::Audio => 3,
            Self::Library => 4,
            Self::Shortcuts => 5,
            Self::Workspace | Self::Ready => 6,
            Self::Failed => 0,
        }
    }

    fn rank(self) -> u8 {
        match self {
            Self::Files => 1,
            Self::Models => 2,
            Self::Audio => 3,
            Self::Library => 4,
            Self::Shortcuts => 5,
            Self::Workspace => 6,
            Self::Ready => 7,
            Self::Failed => 8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartupStatus {
    pub phase: StartupPhase,
    pub step: u8,
    pub total_steps: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

impl StartupStatus {
    fn running(phase: StartupPhase) -> Self {
        Self {
            phase,
            step: phase.step(),
            total_steps: STARTUP_TOTAL_STEPS,
            error_message: None,
        }
    }
}

struct StartupInner {
    status: StartupStatus,
    frontend_ready: bool,
    handoff_claimed: bool,
    started_at: Instant,
    last_transition_at: Instant,
}

#[derive(Clone)]
pub struct StartupCoordinator {
    inner: Arc<Mutex<StartupInner>>,
}

impl StartupCoordinator {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            inner: Arc::new(Mutex::new(StartupInner {
                status: StartupStatus::running(StartupPhase::Files),
                frontend_ready: false,
                handoff_claimed: false,
                started_at: now,
                last_transition_at: now,
            })),
        }
    }

    pub fn status(&self) -> StartupStatus {
        self.inner
            .lock()
            .map(|inner| inner.status.clone())
            .unwrap_or_else(|_| StartupStatus {
                phase: StartupPhase::Failed,
                step: 0,
                total_steps: STARTUP_TOTAL_STEPS,
                error_message: Some("Startup status became unavailable.".to_string()),
            })
    }

    pub fn advance(&self, app: &AppHandle, phase: StartupPhase) {
        let status = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if inner.status.phase == StartupPhase::Failed
                || phase == StartupPhase::Failed
                || phase.rank() <= inner.status.phase.rank()
            {
                return;
            }

            let now = Instant::now();
            eprintln!(
                "[startup] {:?} completed in {} ms ({} ms total)",
                inner.status.phase,
                now.duration_since(inner.last_transition_at).as_millis(),
                now.duration_since(inner.started_at).as_millis(),
            );
            inner.last_transition_at = now;
            inner.status = StartupStatus::running(phase);
            inner.status.clone()
        };
        let _ = app.emit(STARTUP_STATUS_EVENT, status);
    }

    pub fn fail(&self, app: &AppHandle, message: impl Into<String>) {
        let status = {
            let Ok(mut inner) = self.inner.lock() else {
                return;
            };
            if inner.status.phase == StartupPhase::Failed
                || inner.status.phase == StartupPhase::Ready
            {
                return;
            }
            let message = message.into();
            eprintln!(
                "[startup] failed during {:?} after {} ms: {}",
                inner.status.phase,
                inner.started_at.elapsed().as_millis(),
                message,
            );
            inner.status.phase = StartupPhase::Failed;
            inner.status.error_message = Some(message);
            inner.status.clone()
        };
        let _ = app.emit(STARTUP_STATUS_EVENT, status);
    }

    pub fn mark_frontend_ready(&self, app: &AppHandle) -> bool {
        let should_emit = {
            let Ok(mut inner) = self.inner.lock() else {
                return false;
            };
            if inner.frontend_ready || inner.status.phase != StartupPhase::Workspace {
                return false;
            }
            inner.frontend_ready = true;
            inner.status = StartupStatus::running(StartupPhase::Ready);
            eprintln!(
                "[startup] workspace completed in {} ms ({} ms total)",
                inner.last_transition_at.elapsed().as_millis(),
                inner.started_at.elapsed().as_millis(),
            );
            true
        };

        if should_emit {
            let _ = app.emit(STARTUP_STATUS_EVENT, self.status());
        }
        should_emit
    }

    pub fn claim_handoff(&self) -> bool {
        let Ok(mut inner) = self.inner.lock() else {
            return false;
        };
        if inner.handoff_claimed || inner.status.phase != StartupPhase::Ready {
            return false;
        }
        inner.handoff_claimed = true;
        true
    }

    #[cfg(test)]
    fn advance_without_app(&self, phase: StartupPhase) {
        let Ok(mut inner) = self.inner.lock() else {
            return;
        };
        if inner.status.phase != StartupPhase::Failed
            && phase != StartupPhase::Failed
            && phase.rank() > inner.status.phase.rank()
        {
            inner.status = StartupStatus::running(phase);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_only_move_forward() {
        let startup = StartupCoordinator::new();
        startup.advance_without_app(StartupPhase::Audio);
        startup.advance_without_app(StartupPhase::Models);
        assert_eq!(startup.status().phase, StartupPhase::Audio);
        assert_eq!(startup.status().step, 3);
    }

    #[test]
    fn handoff_can_only_be_claimed_once_after_ready() {
        let startup = StartupCoordinator::new();
        assert!(!startup.claim_handoff());
        startup.advance_without_app(StartupPhase::Ready);
        assert!(startup.claim_handoff());
        assert!(!startup.claim_handoff());
    }

    #[test]
    fn status_has_six_real_steps() {
        let startup = StartupCoordinator::new();
        assert_eq!(startup.status().step, 1);
        assert_eq!(startup.status().total_steps, 6);
        startup.advance_without_app(StartupPhase::Workspace);
        assert_eq!(startup.status().step, 6);
    }
}
