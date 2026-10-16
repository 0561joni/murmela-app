//! FIFO admission shared by file transcription and speaker-only retries.
use crate::review::{ReviewError, ReviewRef, ReviewStore};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::any::Any;
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

#[derive(Default)]
struct QueueState {
    active: Option<String>,
    pending: VecDeque<(String, bool)>,
    idle: Option<IdleResource>,
    cache_epoch: u64,
}
struct IdleResource {
    owner: &'static str,
    /// `None`: kept until explicit or memory-pressure eviction.
    expires: Option<Instant>,
    value: Box<dyn Any + Send>,
}
#[derive(Clone, Default)]
pub struct ProcessingQueue {
    inner: Arc<(Mutex<QueueState>, Condvar)>,
}
pub struct ProcessingPermit {
    queue: ProcessingQueue,
    key: String,
    cache_epoch: u64,
}
impl Drop for ProcessingPermit {
    fn drop(&mut self) {
        let mut state = self.queue.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.active.as_deref() == Some(&self.key) {
            state.active = None;
        }
        self.queue.inner.1.notify_all();
    }
}
impl ProcessingQueue {
    pub fn enqueue(&self, key: &str) {
        self.enqueue_with_priority(key, false);
    }
    pub fn enqueue_priority(&self, key: &str) {
        self.enqueue_with_priority(key, true);
    }
    fn enqueue_with_priority(&self, key: &str, priority: bool) {
        let mut state = self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.active.as_deref() == Some(key) || state.pending.iter().any(|(id, _)| id == key) {
            return;
        }
        let index = if priority {
            state
                .pending
                .iter()
                .position(|(_, p)| !p)
                .unwrap_or(state.pending.len())
        } else {
            state.pending.len()
        };
        state.pending.insert(index, (key.into(), priority));
        self.inner.1.notify_all();
    }
    pub fn remove(&self, key: &str) {
        // Cancellation removes only pending admission. Active ownership ends with its permit.
        self.inner
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pending
            .retain(|(id, _)| id != key);
        self.inner.1.notify_all();
    }
    pub fn acquire(&self, key: &str, cancelled: &AtomicBool) -> Result<ProcessingPermit> {
        self.acquire_with_idle(key, cancelled, None)
            .map(|(permit, _)| permit)
    }
    /// Reuse or destroy cached inference resources while admission is locked.
    /// A new model cannot allocate until an evicted child's Drop has reaped it.
    pub fn acquire_with_idle(
        &self,
        key: &str,
        cancelled: &AtomicBool,
        owner: Option<&'static str>,
    ) -> Result<(ProcessingPermit, Option<Box<dyn Any + Send>>)> {
        let mut state = self
            .inner
            .0
            .lock()
            .map_err(|_| anyhow!("Processing queue unavailable"))?;
        loop {
            if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() {
                state.pending.retain(|(id, _)| id != key);
                self.inner.1.notify_all();
                bail!("JOB_CANCELED: Local processing stopped.");
            }
            if state.active.as_deref() == Some(key) {
                bail!("JOB_ALREADY_RUNNING: Duplicate inference admission.");
            }
            if state.active.is_none() && state.pending.front().is_some_and(|(id, _)| id == key) {
                let retained = match state.idle.take() {
                    Some(idle)
                        if Some(idle.owner) == owner
                            && idle.expires.is_none_or(|at| at > Instant::now()) =>
                    {
                        Some(idle.value)
                    }
                    other => {
                        drop(other);
                        None
                    }
                };
                state.pending.pop_front();
                state.active = Some(key.into());
                return Ok((
                    ProcessingPermit {
                        queue: self.clone(),
                        key: key.into(),
                        cache_epoch: state.cache_epoch,
                    },
                    retained,
                ));
            }
            if !state.pending.iter().any(|(id, _)| id == key) {
                bail!("JOB_CANCELED: This queued job was removed.");
            }
            state = self
                .inner
                .1
                .wait_timeout(state, Duration::from_millis(100))
                .map_err(|_| anyhow!("Processing queue unavailable"))?
                .0;
        }
    }
    /// Like `acquire`, but leaves a cached idle resource (the warm dictation
    /// engine) in place. Only for work that allocates no new model memory.
    pub fn acquire_keeping_idle(&self, key: &str, cancelled: &AtomicBool) -> Result<ProcessingPermit> {
        let mut state = self
            .inner
            .0
            .lock()
            .map_err(|_| anyhow!("Processing queue unavailable"))?;
        loop {
            if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() {
                state.pending.retain(|(id, _)| id != key);
                self.inner.1.notify_all();
                bail!("JOB_CANCELED: Local processing stopped.");
            }
            if state.active.as_deref() == Some(key) {
                bail!("JOB_ALREADY_RUNNING: Duplicate inference admission.");
            }
            if state.active.is_none() && state.pending.front().is_some_and(|(id, _)| id == key) {
                state.pending.pop_front();
                state.active = Some(key.into());
                return Ok(ProcessingPermit {
                    queue: self.clone(),
                    key: key.into(),
                    cache_epoch: state.cache_epoch,
                });
            }
            if !state.pending.iter().any(|(id, _)| id == key) {
                bail!("JOB_CANCELED: This queued job was removed.");
            }
            state = self
                .inner
                .1
                .wait_timeout(state, Duration::from_millis(100))
                .map_err(|_| anyhow!("Processing queue unavailable"))?
                .0;
        }
    }
    /// A dictation waits for the queue; long work should step aside.
    pub fn has_priority_waiter(&self) -> bool {
        let state = self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.iter().any(|(_, priority)| *priority)
    }
    /// A job holds the queue or waits for it.
    pub fn busy(&self) -> bool {
        let state = self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        state.active.is_some() || !state.pending.is_empty()
    }
    pub fn evict_idle(&self) {
        // Drop under the same lock used by admission and warm reuse.
        let mut state = self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        state.cache_epoch = state.cache_epoch.wrapping_add(1);
        drop(state.idle.take());
    }
    pub fn cache_idle<T: Any + Send>(
        &self,
        permit: &ProcessingPermit,
        owner: &'static str,
        value: T,
    ) {
        self.cache_idle_for(permit, owner, value, Some(Duration::from_secs(60)));
    }
    /// Like `cache_idle`, with a caller-chosen lifetime. `None` keeps the
    /// resource until `evict_idle` (memory pressure, model or mode changes).
    pub fn cache_idle_for<T: Any + Send>(
        &self,
        permit: &ProcessingPermit,
        owner: &'static str,
        value: T,
        ttl: Option<Duration>,
    ) {
        let mut state = self.inner.0.lock().unwrap_or_else(|e| e.into_inner());
        if !Arc::ptr_eq(&self.inner, &permit.queue.inner)
            || state.cache_epoch != permit.cache_epoch
            || state.active.as_deref() != Some(&permit.key)
            || crate::shutdown::is_shutting_down()
        {
            drop(value);
            return;
        }
        drop(state.idle.take());
        state.idle = Some(IdleResource {
            owner,
            expires: ttl.map(|ttl| Instant::now() + ttl),
            value: Box::new(value),
        });
        let Some(ttl) = ttl else { return };
        let weak = Arc::downgrade(&self.inner);
        std::thread::spawn(move || {
            std::thread::sleep(ttl);
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.0.lock().unwrap_or_else(|e| e.into_inner());
                if state
                    .idle
                    .as_ref()
                    .is_some_and(|idle| idle.expires.is_some_and(|at| at <= Instant::now()))
                {
                    drop(state.idle.take());
                }
            }
        });
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewJobStatus {
    pub job_id: String,
    pub reference: ReviewRef,
    pub stage: String,
    pub status_text: String,
    pub error: Option<ReviewError>,
    pub result_revision: Option<u64>,
    pub speaker_count: Option<i32>,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
}
impl ReviewJobStatus {
    pub fn active(&self) -> bool {
        !matches!(self.stage.as_str(), "completed" | "failed" | "canceled")
    }
}
struct Job {
    status: ReviewJobStatus,
    cancelled: Arc<AtomicBool>,
}
#[derive(Clone)]
pub struct ReviewJobController {
    app: AppHandle,
    store: ReviewStore,
    models_dir: PathBuf,
    temp_dir: PathBuf,
    queue: ProcessingQueue,
    jobs: Arc<Mutex<HashMap<String, Job>>>,
    commit: Arc<Mutex<()>>,
}
impl ReviewJobController {
    pub fn new(
        app: AppHandle,
        store: ReviewStore,
        models_dir: PathBuf,
        temp_dir: PathBuf,
        queue: ProcessingQueue,
    ) -> Self {
        Self {
            app,
            store,
            models_dir,
            temp_dir,
            queue,
            jobs: Default::default(),
            commit: Default::default(),
        }
    }
    pub fn statuses(&self) -> Vec<ReviewJobStatus> {
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|j| j.status.clone())
            .collect()
    }
    pub fn start(
        &self,
        reference: ReviewRef,
        count: Option<i32>,
        reset: bool,
        job_id: Option<String>,
    ) -> Result<ReviewJobStatus> {
        let work = crate::shutdown::begin_work(true)?;
        crate::diarization::validate_speaker_count_hint(count).map_err(anyhow::Error::msg)?;
        let document = self.store.get(&reference)?;
        if matches!(
            document.detail.summary.diarization_status,
            crate::diarization::DiarizationStatus::Pending
                | crate::diarization::DiarizationStatus::Running
        ) {
            bail!("JOB_ALREADY_RUNNING: Initial speaker identification is still running for this transcript.");
        }
        let package_path = crate::model_downloads::installed_diarization_package_path(&self.models_dir)
            .ok_or_else(|| anyhow!("MODEL_UNAVAILABLE: Download the speaker identification model in Settings → Engines."))?;
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow!("Speaker job state unavailable"))?;
        if jobs
            .values()
            .any(|j| j.status.reference == reference && j.status.active())
        {
            bail!("JOB_ALREADY_RUNNING: Speaker identification is already queued or running for this transcript.");
        }
        let now = chrono::Utc::now().timestamp_millis();
        let status = ReviewJobStatus {
            job_id: job_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
            reference,
            stage: "queued".into(),
            status_text: "Waiting for local file processing…".into(),
            error: None,
            result_revision: None,
            speaker_count: count,
            started_at_ms: now,
            updated_at_ms: now,
        };
        if jobs.contains_key(&status.job_id) {
            bail!("JOB_ALREADY_EXISTS: This job identifier has already been used.");
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        jobs.insert(
            status.job_id.clone(),
            Job {
                status: status.clone(),
                cancelled: cancelled.clone(),
            },
        );
        self.queue.enqueue(&status.job_id);
        drop(jobs);
        let controller = self.clone();
        let id = status.job_id.clone();
        let reference = status.reference.clone();
        let _ = self.app.emit("review-job-status", status.clone());
        std::thread::spawn(move || {
            let _work = work;
            let result = (|| -> Result<u64> {
                let _permit = controller.queue.acquire(&id, &cancelled)?;
                if let Some(state) = controller.app.try_state::<crate::app_state::AppState>() {
                    state.engine.release_resources();
                }
                controller.update(
                    &id,
                    "validating",
                    "Checking the original audio…",
                    None,
                    None,
                );
                let source =
                    crate::review_media::validated_source(&controller.store, &reference, None)?;
                if cancelled.load(Ordering::SeqCst) {
                    bail!("JOB_CANCELED: Speaker processing stopped.");
                }
                let preparation = Instant::now();
                let prepared = crate::audio_preprocess::prepare_job_audio(
                    &source.file_path,
                    &controller.temp_dir,
                )?;
                eprintln!(
                    "[review-job] {id} preparation_ms={}",
                    preparation.elapsed().as_millis()
                );
                if cancelled.load(Ordering::SeqCst) {
                    bail!("JOB_CANCELED: Speaker processing stopped.");
                }
                controller.update(
                    &id,
                    "diarizing",
                    "Identifying speakers locally…",
                    None,
                    None,
                );
                let inference = Instant::now();
                let turns = crate::diarization_worker::run_subprocess_worker(
                    &crate::diarization_worker::WorkerRequest {
                        job_id: id.clone(),
                        audio_path: prepared.path.clone(),
                        package_path,
                        exact_speaker_count: count,
                        spec_version: crate::diarization::DIARIZATION_MODEL_SPEC_V2
                            .manifest_version,
                    },
                    Some(&cancelled),
                    || {},
                )?;
                eprintln!(
                    "[review-job] {id} diarization_ms={}",
                    inference.elapsed().as_millis()
                );
                if turns.is_empty() {
                    bail!("NO_SPEECH: No distinct speech was found. Your existing speaker results and corrections were kept.");
                }
                if let Some(warning) = crate::diarization::overclustering_warning(&turns, count) {
                    bail!(warning);
                }
                let mut machine = controller.store.machine(&reference)?;
                let reconciliation = Instant::now();
                crate::diarization::apply_turns_to_transcript(&mut machine, turns, count);
                eprintln!(
                    "[review-job] {id} reconciliation_ms={}",
                    reconciliation.elapsed().as_millis()
                );
                // Cancellation and the final commit share a gate. Once committed,
                // cancel cannot report success for a change already on disk.
                let _commit = controller
                    .commit
                    .lock()
                    .map_err(|_| anyhow!("Speaker commit unavailable"))?;
                if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() {
                    bail!("JOB_CANCELED: Speaker processing stopped.");
                }
                controller.update(&id, "saving", "Saving speaker labels…", None, None);
                let saving = Instant::now();
                let document = controller.store.replace_machine_cancellable(
                    &reference,
                    machine,
                    reset,
                    Some(&cancelled),
                )?;
                eprintln!(
                    "[review-job] {id} saving_ms={}",
                    saving.elapsed().as_millis()
                );
                controller.update(
                    &id,
                    "completed",
                    "Speaker identification updated.",
                    None,
                    Some(document.revision),
                );
                let _ = controller.app.emit("review-updated", &document.reference);
                Ok(document.revision)
            })();
            match result {
                Ok(_) => {}
                Err(_e)
                    if cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down() =>
                {
                    controller.update(
                        &id,
                        "canceled",
                        "Speaker processing stopped. Your existing result was kept.",
                        None,
                        None,
                    )
                }
                Err(e) => {
                    let error: ReviewError = e.into();
                    controller.update(
                        &id,
                        "failed",
                        "Speaker identification needs attention.",
                        Some(error),
                        None,
                    );
                }
            }
            controller.queue.remove(&id);
        });
        Ok(status)
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        // Signal first so an in-flight SQLite projection can roll back before
        // commit. The gate then resolves whether cancellation or commit won.
        {
            let jobs = self
                .jobs
                .lock()
                .map_err(|_| anyhow!("Speaker job state unavailable"))?;
            let job = jobs
                .get(id)
                .ok_or_else(|| anyhow!("JOB_NOT_FOUND: Speaker job not found."))?;
            if !job.status.active() {
                return Ok(());
            }
            job.cancelled.store(true, Ordering::SeqCst);
        }
        let _commit = self
            .commit
            .lock()
            .map_err(|_| anyhow!("Speaker commit unavailable"))?;
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow!("Speaker job state unavailable"))?;
        let job = jobs
            .get_mut(id)
            .ok_or_else(|| anyhow!("JOB_NOT_FOUND: Speaker job not found."))?;
        if !job.status.active() {
            return Ok(());
        }
        job.cancelled.store(true, Ordering::SeqCst);
        job.status.stage = "canceling".into();
        job.status.status_text = "Stopping speaker processing…".into();
        job.status.updated_at_ms = chrono::Utc::now()
            .timestamp_millis()
            .max(job.status.updated_at_ms + 1);
        let _ = self.app.emit("review-job-status", job.status.clone());
        Ok(())
    }
    pub fn cancel_all(&self) {
        for j in self.jobs.lock().unwrap_or_else(|e| e.into_inner()).values() {
            j.cancelled.store(true, Ordering::SeqCst);
        }
    }
    pub fn active_for(&self, reference: &ReviewRef) -> bool {
        self.statuses()
            .iter()
            .any(|j| &j.reference == reference && j.active())
    }
    fn update(
        &self,
        id: &str,
        stage: &str,
        text: &str,
        error: Option<ReviewError>,
        revision: Option<u64>,
    ) {
        if let Some(job) = self
            .jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
        {
            if !job.status.active() {
                return;
            }
            if job.cancelled.load(Ordering::SeqCst)
                && !matches!(stage, "canceled" | "failed" | "completed")
            {
                return;
            }
            job.status.stage = stage.into();
            job.status.status_text = text.into();
            job.status.error = error;
            job.status.result_revision = revision;
            job.status.updated_at_ms = chrono::Utc::now()
                .timestamp_millis()
                .max(job.status.updated_at_ms + 1);
            let _ = self.app.emit("review-job-status", job.status.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_workers_are_reused_without_holding_admission_and_reaped_before_other_models() {
        struct Resource(Arc<AtomicBool>);
        impl Drop for Resource {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let queue = ProcessingQueue::default();
        let flag = AtomicBool::new(false);
        let reaped = Arc::new(AtomicBool::new(false));
        queue.enqueue_priority("one");
        let permit = queue.acquire("one", &flag).unwrap();
        queue.cache_idle(&permit, "live", Resource(reaped.clone()));
        drop(permit);
        assert!(queue.inner.0.lock().unwrap().active.is_none());
        queue.enqueue_priority("two");
        let (permit, cached) = queue.acquire_with_idle("two", &flag, Some("live")).unwrap();
        let resource = *cached.unwrap().downcast::<Resource>().ok().unwrap();
        assert!(!reaped.load(Ordering::SeqCst));
        queue.cache_idle(&permit, "live", resource);
        drop(permit);
        queue.enqueue("file");
        let permit = queue.acquire("file", &flag).unwrap();
        assert!(reaped.load(Ordering::SeqCst));
        drop(permit);
    }

    #[test]
    fn expired_cache_and_explicit_eviction_cannot_be_reused() {
        let queue = ProcessingQueue::default();
        let flag = AtomicBool::new(false);
        queue.enqueue("one");
        let permit = queue.acquire("one", &flag).unwrap();
        queue.cache_idle(&permit, "live", 42_u32);
        queue.inner.0.lock().unwrap().idle.as_mut().unwrap().expires = Some(Instant::now());
        drop(permit);
        queue.enqueue("two");
        let (permit, cached) = queue.acquire_with_idle("two", &flag, Some("live")).unwrap();
        assert!(cached.is_none());
        queue.cache_idle(&permit, "live", 42_u32);
        queue.evict_idle();
        queue.cache_idle(&permit, "live", 43_u32);
        assert!(queue.inner.0.lock().unwrap().idle.is_none());
    }

    #[test]
    fn unbounded_cache_survives_until_evicted_and_short_ttl_expires() {
        let queue = ProcessingQueue::default();
        let flag = AtomicBool::new(false);
        queue.enqueue("one");
        let permit = queue.acquire("one", &flag).unwrap();
        queue.cache_idle_for(&permit, "live", 7_u32, None);
        drop(permit);
        assert!(queue.inner.0.lock().unwrap().idle.as_ref().unwrap().expires.is_none());
        queue.enqueue("two");
        let (permit, cached) = queue.acquire_with_idle("two", &flag, Some("live")).unwrap();
        assert_eq!(*cached.unwrap().downcast::<u32>().unwrap(), 7);
        queue.cache_idle_for(&permit, "live", 8_u32, None);
        queue.evict_idle();
        assert!(queue.inner.0.lock().unwrap().idle.is_none());
        queue.cache_idle_for(&permit, "live", 9_u32, Some(Duration::from_millis(20)));
        assert!(queue.inner.0.lock().unwrap().idle.is_none(), "eviction epoch rejects the old permit");
        drop(permit);
        queue.enqueue("three");
        let permit = queue.acquire("three", &flag).unwrap();
        queue.cache_idle_for(&permit, "live", 10_u32, Some(Duration::from_millis(20)));
        std::thread::sleep(Duration::from_millis(200));
        assert!(queue.inner.0.lock().unwrap().idle.is_none());
    }

    #[test]
    fn keeping_idle_admission_leaves_the_warm_engine_and_sees_dictation() {
        let queue = ProcessingQueue::default();
        let cancelled = AtomicBool::new(false);
        queue.enqueue("warm");
        let permit = queue.acquire("warm", &cancelled).unwrap();
        queue.cache_idle_for(&permit, "dictation-asr", 7_u8, None);
        drop(permit);
        queue.enqueue("text");
        let text = queue.acquire_keeping_idle("text", &cancelled).unwrap();
        assert!(!queue.has_priority_waiter());
        queue.enqueue_priority("dictation");
        assert!(queue.has_priority_waiter());
        drop(text);
        let (_permit, retained) = queue
            .acquire_with_idle("dictation", &cancelled, Some("dictation-asr"))
            .unwrap();
        assert_eq!(retained.and_then(|value| value.downcast::<u8>().ok()).map(|v| *v), Some(7));
    }

    #[test]
    fn dictation_priority_never_preempts_an_active_owner() {
        let queue = ProcessingQueue::default();
        let flag = AtomicBool::new(false);
        queue.enqueue("file-running");
        let active = queue.acquire("file-running", &flag).unwrap();
        queue.enqueue("file-waiting");
        queue.enqueue_priority("dictation-1");
        queue.enqueue_priority("dictation-2");
        queue.remove("file-running");
        {
            let state = queue.inner.0.lock().unwrap();
            assert_eq!(state.active.as_deref(), Some("file-running"));
            assert_eq!(
                state
                    .pending
                    .iter()
                    .map(|(id, _)| id.as_str())
                    .collect::<Vec<_>>(),
                ["dictation-1", "dictation-2", "file-waiting"]
            );
        }
        assert!(queue.acquire("file-running", &flag).is_err());
        drop(active);
        drop(queue.acquire("dictation-1", &flag).unwrap());
        drop(queue.acquire("dictation-2", &flag).unwrap());
        drop(queue.acquire("file-waiting", &flag).unwrap());
        assert!(queue.inner.0.lock().unwrap().active.is_none());
    }
    #[test]
    fn shared_queue_is_fifo_and_canceled_waiters_do_not_block_later_work() {
        let queue = ProcessingQueue::default();
        queue.enqueue("file-1");
        queue.enqueue("retry");
        queue.enqueue("file-2");
        let flag = Arc::new(AtomicBool::new(false));
        let first = queue.acquire("file-1", &flag).unwrap();
        let retry_flag = Arc::new(AtomicBool::new(false));
        let q = queue.clone();
        let retry = retry_flag.clone();
        let thread = std::thread::spawn(move || q.acquire("retry", &retry).is_err());
        retry_flag.store(true, Ordering::SeqCst);
        assert!(thread.join().unwrap());
        drop(first);
        let third = queue.acquire("file-2", &flag).unwrap();
        drop(third);
        assert!(queue.inner.0.lock().unwrap().pending.is_empty());
    }
    #[test]
    fn waiting_retry_cancellation_finishes_while_heavy_work_keeps_its_permit() {
        let queue = ProcessingQueue::default();
        queue.enqueue("running");
        queue.enqueue("waiting");
        let permit = queue.acquire("running", &AtomicBool::new(false)).unwrap();
        let canceled = Arc::new(AtomicBool::new(false));
        let flag = canceled.clone();
        let waiting = queue.clone();
        let worker = std::thread::spawn(move || waiting.acquire("waiting", &flag).is_err());
        let start = Instant::now();
        canceled.store(true, Ordering::SeqCst);
        assert!(worker.join().unwrap());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(
            queue.inner.0.lock().unwrap().active.as_deref().unwrap(),
            "running"
        );
        drop(permit);
    }
}
