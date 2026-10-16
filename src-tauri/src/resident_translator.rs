//! Keeps the TranslateGemma helper loaded between translations.
//!
//! Loading the 9.7 GB model takes many seconds, so a translated dictation starts
//! loading it as soon as recording begins, and a finished translation keeps it
//! resident for five more minutes. Both only happen when macOS has enough free
//! memory; memory pressure, turning translation off and quitting release it.
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};

use crate::managed_process::ManagedChild;

/// How long an idle, loaded model stays resident after its last translation.
pub const KEEP_LOADED_FOR: Duration = Duration::from_secs(5 * 60);
/// Besides the weights: the 4096-token context, Metal buffers and the runtime.
const RUNTIME_OVERHEAD: u64 = 1_600_000_000;
/// Memory that must stay available for the rest of the system and the dictation.
const HEADROOM: u64 = 1_500_000_000;
const LOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// Memory a loaded model of `model_bytes` uses.
pub fn loaded_footprint(model_bytes: u64) -> u64 {
    model_bytes + RUNTIME_OVERHEAD
}

/// Memory that must be available before a model of `model_bytes` is loaded.
pub fn memory_needed(model_bytes: u64) -> u64 {
    loaded_footprint(model_bytes) + HEADROOM
}

/// Whether loading a model of `model_bytes` now leaves enough memory free.
pub fn room_to_load(available: Option<u64>, model_bytes: u64) -> bool {
    available.is_some_and(|bytes| bytes >= memory_needed(model_bytes))
}

/// Whether an already loaded model may stay loaded (its memory is in use).
pub fn room_to_keep(available: Option<u64>) -> bool {
    available.is_some_and(|bytes| bytes >= HEADROOM)
}

/// One running helper process with its model loaded or about to be.
pub struct TranslatorProcess {
    child: ManagedChild,
    stdin: ChildStdin,
    lines: mpsc::Receiver<std::io::Result<String>>,
    model: PathBuf,
    prefer_gpu: bool,
}

impl TranslatorProcess {
    pub fn spawn(helper: &Path, model: &Path, prefer_gpu: bool) -> Result<Self> {
        let mut command = Command::new(helper);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::managed_process::isolate(&mut command);
        let mut child = ManagedChild::new(
            command
                .spawn()
                .context("Could not start translation runtime")?,
        );
        let stdin = child
            .0
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Translation input unavailable"))?;
        let stdout = child
            .0
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Translation output unavailable"))?;
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
            model: model.to_owned(),
            prefer_gpu,
        })
    }

    pub fn serves(&self, model: &Path, prefer_gpu: bool) -> bool {
        self.model == model && self.prefer_gpu == prefer_gpu
    }

    /// Asks the helper to stop request `id`; it answers with a CANCELED error.
    pub fn cancel(&mut self, id: &str) -> Result<()> {
        self.send(&serde_json::json!({"type": "cancel", "requestId": id}))
    }

    pub fn send(&mut self, request: &serde_json::Value) -> Result<()> {
        serde_json::to_writer(&mut self.stdin, request)?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// The next protocol line, `None` after `timeout`.
    pub fn next_line(&mut self, timeout: Duration) -> Result<Option<String>> {
        match self.lines.recv_timeout(timeout) {
            Ok(line) => Ok(Some(line?)),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Translation runtime exited unexpectedly. The original text is available.")
            }
        }
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.0.try_wait(), Ok(None))
    }

    /// Loads the model without translating anything.
    pub fn load(&mut self, stop: &dyn Fn() -> bool) -> Result<()> {
        let id = format!("load-{}", uuid::Uuid::new_v4());
        self.send(&serde_json::json!({
            "version": crate::translation::WORKER_PROTOCOL, "type": "load", "requestId": id,
            "modelPath": self.model, "preferGpu": self.prefer_gpu,
        }))?;
        let deadline = Instant::now() + LOAD_TIMEOUT;
        loop {
            if stop() {
                bail!("Translation preload stopped.");
            }
            if Instant::now() > deadline {
                bail!("Loading the translation model timed out.");
            }
            let Some(line) = self.next_line(Duration::from_millis(100))? else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("Invalid translation worker response."))?;
            if value["requestId"].as_str() != Some(&id) {
                bail!("Translation protocol mismatch.");
            }
            return match value["type"].as_str() {
                Some("loaded") => Ok(()),
                _ => Err(anyhow!(
                    "Translation model could not be loaded ({}).",
                    value["code"].as_str().unwrap_or("LOAD_FAILED")
                )),
            };
        }
    }
}

enum Slot {
    Empty,
    Loading,
    Ready(TranslatorProcess),
    /// A translation holds the process.
    InUse,
}

struct State {
    slot: Slot,
    /// Bumped on every change that invalidates pending loads and expiry timers.
    generation: u64,
}

/// The single resident helper, shared by preload, translation and eviction.
#[derive(Clone)]
pub struct ResidentTranslator {
    inner: Arc<(Mutex<State>, Condvar)>,
}

impl Default for ResidentTranslator {
    fn default() -> Self {
        Self {
            inner: Arc::new((
                Mutex::new(State {
                    slot: Slot::Empty,
                    generation: 0,
                }),
                Condvar::new(),
            )),
        }
    }
}

impl ResidentTranslator {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.0.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Starts loading the model in the background when nothing is loaded and
    /// enough memory is free. Returns whether a load started.
    pub fn preload(&self, helper: PathBuf, model: PathBuf, prefer_gpu: bool, model_bytes: u64) -> bool {
        let mut state = self.lock();
        if !matches!(state.slot, Slot::Empty) {
            return false;
        }
        let available = crate::memory_pressure::available_bytes();
        if !room_to_load(available, model_bytes) {
            eprintln!(
                "[translation] not preloading: {} GB free",
                available.map_or(0.0, |bytes| bytes as f64 / 1e9)
            );
            return false;
        }
        state.generation += 1;
        let generation = state.generation;
        state.slot = Slot::Loading;
        drop(state);
        let resident = self.clone();
        std::thread::spawn(move || {
            let stale = || resident.lock().generation != generation;
            let started = Instant::now();
            let loaded = TranslatorProcess::spawn(&helper, &model, prefer_gpu).and_then(|mut process| {
                process.load(&stale)?;
                Ok(process)
            });
            let mut state = resident.lock();
            let current = state.generation == generation && matches!(state.slot, Slot::Loading);
            match loaded {
                Ok(process) if current => {
                    eprintln!("[translation] model preloaded in {:.1} s", started.elapsed().as_secs_f64());
                    state.slot = Slot::Ready(process);
                    drop(state);
                    resident.expire_after(generation, KEEP_LOADED_FOR);
                }
                Ok(_) => {}
                Err(error) => {
                    eprintln!("[translation] preload failed: {error:#}");
                    if current {
                        state.slot = Slot::Empty;
                    }
                }
            }
            resident.inner.1.notify_all();
        });
        true
    }

    /// Takes the loaded (or loading) helper for a translation. Waits for a
    /// running preload, which is faster than starting another. `None` means
    /// the caller starts its own process.
    pub fn take(&self, stop: &dyn Fn() -> bool) -> Option<TranslatorProcess> {
        let mut state = self.lock();
        loop {
            match std::mem::replace(&mut state.slot, Slot::InUse) {
                Slot::Ready(mut process) => {
                    state.generation += 1;
                    return process.alive().then_some(process);
                }
                Slot::Loading => {
                    state.slot = Slot::Loading;
                    if stop() {
                        return None;
                    }
                    state = self
                        .inner
                        .1
                        .wait_timeout(state, Duration::from_millis(100))
                        .unwrap_or_else(|error| error.into_inner())
                        .0;
                }
                Slot::Empty | Slot::InUse => return None,
            }
        }
    }

    /// Returns the helper after a translation. It stays loaded for
    /// `KEEP_LOADED_FOR` when memory allows; otherwise it is stopped.
    pub fn give_back(&self, process: Option<TranslatorProcess>) {
        let mut state = self.lock();
        if !matches!(state.slot, Slot::InUse) {
            return; // Evicted meanwhile: drop the process.
        }
        state.generation += 1;
        let generation = state.generation;
        match process {
            Some(process) if room_to_keep(crate::memory_pressure::available_bytes()) => {
                state.slot = Slot::Ready(process);
                drop(state);
                self.expire_after(generation, KEEP_LOADED_FOR);
            }
            _ => state.slot = Slot::Empty,
        }
        self.inner.1.notify_all();
    }

    /// Whether `model` is loaded or loading, so using it needs no new memory.
    pub fn holds(&self, model: &Path) -> bool {
        match &self.lock().slot {
            Slot::Ready(process) => process.model == model,
            Slot::Loading => true,
            Slot::Empty | Slot::InUse => false,
        }
    }

    /// Stops an idle or loading helper (memory pressure, translation turned off).
    pub fn evict(&self) {
        let mut state = self.lock();
        state.generation += 1;
        if matches!(state.slot, Slot::Ready(_) | Slot::Loading) {
            state.slot = Slot::Empty;
        }
        self.inner.1.notify_all();
    }

    fn expire_after(&self, generation: u64, after: Duration) {
        let resident = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(after);
            let mut state = resident.lock();
            if state.generation == generation && matches!(state.slot, Slot::Ready(_)) {
                eprintln!("[translation] model released after {} min idle", after.as_secs() / 60);
                state.slot = Slot::Empty;
            }
        });
    }

    #[cfg(test)]
    fn describe(&self) -> &'static str {
        match self.lock().slot {
            Slot::Empty => "empty",
            Slot::Loading => "loading",
            Slot::Ready(_) => "ready",
            Slot::InUse => "in use",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_rules_need_room_for_the_model_and_headroom() {
        let model = crate::translation::MODEL_SIZE as u64;
        assert!(!room_to_load(None, model));
        assert!(!room_to_load(Some(loaded_footprint(model)), model));
        assert!(room_to_load(Some(loaded_footprint(model) + HEADROOM), model));
        // The compact model fits where the large one does not.
        let compact = crate::translation::MODELS[1].size as u64;
        assert!(room_to_load(Some(loaded_footprint(compact) + HEADROOM), compact));
        assert!(!room_to_load(Some(loaded_footprint(compact) + HEADROOM), model));
        assert!(!room_to_keep(Some(HEADROOM - 1)));
        assert!(room_to_keep(Some(HEADROOM)));
    }

    #[test]
    fn eviction_and_returns_respect_who_holds_the_helper() {
        let resident = ResidentTranslator::default();
        assert!(resident.take(&|| false).is_none());
        assert_eq!(resident.describe(), "in use");
        // Eviction never takes a helper away from a running translation.
        resident.evict();
        assert_eq!(resident.describe(), "in use");
        resident.give_back(None);
        assert_eq!(resident.describe(), "empty");
        // A return after an eviction of an idle slot is ignored.
        resident.evict();
        resident.give_back(None);
        assert_eq!(resident.describe(), "empty");
    }
}
