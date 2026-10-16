//! Dictation ASR that starts when the shortcut is pressed.
//!
//! For batch models (whisper.cpp, Qwen3-ASR) the selected model is loaded into
//! the persistent transcription worker while the user is still speaking. Long
//! dictations are also transcribed window by window as audio arrives, so on
//! release only the remaining tail has to be decoded.
//!
//! Short dictations keep exactly the release-time decode: a window is only
//! committed once more than `MAX_CHUNK_MS + LOOKAHEAD_MS` of audio is waiting,
//! so anything shorter is still decoded as one file with the preloaded model.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};

use crate::asr::{FileTranscriptionRequest, TranscriptResult, TranscriptSegment};
use crate::audio_capture::CaptureTap;
use crate::audio_chunks::plan_audio_chunks;
use crate::audio_preprocess::{PreparedAudio, StreamingNormalizer, TARGET_SAMPLE_RATE_HZ};
use crate::review_jobs::ProcessingPermit;
use crate::transcription_policy::MAX_CHUNK_MS;
use crate::translation::{DictationSession, TranslationService, WarmAsrWorker, WARM_ASR_OWNER};

/// Extra audio required past a full window so the quiet-point search for the
/// window's end sees the whole candidate range.
const LOOKAHEAD_MS: i64 = 2_000;
/// Tails shorter than this carry no words; decoding them only risks a
/// hallucinated filler.
const MIN_TAIL_MS: i64 = 300;

fn ms_to_samples(ms: i64) -> usize {
    (ms.max(0) as u64 * TARGET_SAMPLE_RATE_HZ as u64 / 1000) as usize
}

fn samples_to_ms(samples: usize) -> i64 {
    (samples as u64 * 1000 / TARGET_SAMPLE_RATE_HZ as u64) as i64
}

/// One window of the recording transcribed while capture was still running.
/// `result` is `None` when the window held no speech.
struct Window {
    start_sample: usize,
    end_sample: usize,
    result: Option<TranscriptResult>,
}

/// Everything prepared before release. Field order matters: the worker process
/// is reaped before the admission permit lets another model allocate.
pub(crate) struct Handoff {
    worker: WarmAsrWorker,
    permit: ProcessingPermit,
    request: FileTranscriptionRequest,
    windows: Vec<Window>,
    committed: usize,
    _work: crate::shutdown::WorkGuard,
}

#[derive(Clone)]
pub(crate) struct EarlyAsr {
    session_id: String,
    stop: Arc<AtomicBool>,
    abandon: Arc<AtomicBool>,
    result: Arc<Mutex<mpsc::Receiver<Result<Handoff>>>>,
}

impl EarlyAsr {
    /// Admit, load and (for long dictations) transcribe in the background.
    /// `request.file_path` is ignored until release.
    pub(crate) fn start(
        service: TranslationService,
        session: DictationSession,
        tap: Option<CaptureTap>,
        request: FileTranscriptionRequest,
        temp_dir: PathBuf,
    ) -> Result<Self> {
        let work = crate::shutdown::begin_work(true)?;
        let (tx, rx) = mpsc::channel();
        let early = Self {
            session_id: session.id.clone(),
            stop: Default::default(),
            abandon: Default::default(),
            result: Arc::new(Mutex::new(rx)),
        };
        let stop = early.stop.clone();
        let abandon = early.abandon.clone();
        std::thread::spawn(move || {
            let queue = service.processing_queue();
            let outcome = prepare(
                &service, &session, tap, request, &temp_dir, &stop, &abandon, work,
            );
            queue.remove(&session.id);
            if let Err(error) = &outcome {
                if !session.cancelled.load(Ordering::SeqCst) && !abandon.load(Ordering::SeqCst) {
                    eprintln!("[dictation] early model preparation stopped: {error:#}");
                }
            }
            // A receiver that is gone (finish never came) drops the handoff,
            // which reaps the worker and releases admission.
            let _ = tx.send(outcome);
        });
        Ok(early)
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Capture has stopped: take over the loaded worker and finished windows.
    /// Waits while admission is still queued behind other local processing.
    pub(crate) fn finish(
        &self,
        service: &TranslationService,
        session: &DictationSession,
    ) -> Result<Handoff> {
        self.stop.store(true, Ordering::SeqCst);
        let receiver = self
            .result
            .lock()
            .map_err(|_| anyhow!("Dictation model state unavailable"))?;
        loop {
            if let Err(error) = service.ensure_active(session) {
                self.cancel();
                return Err(error);
            }
            match receiver.recv_timeout(Duration::from_millis(50)) {
                Ok(outcome) => return outcome,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    bail!("Dictation model preparation stopped unexpectedly.")
                }
            }
        }
    }

    /// Stop preparing and release the worker and admission.
    pub(crate) fn cancel(&self) {
        self.abandon.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare(
    service: &TranslationService,
    session: &DictationSession,
    tap: Option<CaptureTap>,
    request: FileTranscriptionRequest,
    temp_dir: &Path,
    stop: &AtomicBool,
    abandon: &AtomicBool,
    work: crate::shutdown::WorkGuard,
) -> Result<Handoff> {
    let check = || -> Result<()> {
        if abandon.load(Ordering::SeqCst) {
            bail!("DICTATION_CANCELED: Dictation was canceled.");
        }
        service.ensure_active(session)
    };
    let queue = service.processing_queue();
    let queued = Instant::now();
    queue.enqueue_priority(&session.id);
    let (permit, cached) =
        queue.acquire_with_idle(&session.id, &session.cancelled, Some(WARM_ASR_OWNER))?;
    check()?;
    let queue_ms = queued.elapsed().as_millis();
    // The parent's own model cache must not hold memory next to the worker.
    service.release_asr_resources();
    let loading = Instant::now();
    let reused = cached.is_some();
    let mut worker = WarmAsrWorker::reuse_or_spawn(
        cached.and_then(|value| value.downcast::<WarmAsrWorker>().ok().map(|w| *w)),
    )?;
    service.preload_asr(session, &mut worker, request.clone(), check)?;
    eprintln!(
        "[dictation] model ready while recording queue_ms={queue_ms} load_ms={} warm={reused}",
        loading.elapsed().as_millis()
    );
    let mut handoff = Handoff {
        worker,
        permit,
        request,
        windows: Vec::new(),
        committed: 0,
        _work: work,
    };

    let mut tap = tap;
    let mut normalizer = match &tap {
        Some(tap) => StreamingNormalizer::new(tap.rate, tap.channels).ok(),
        None => None,
    };
    let mut cursor = 0;
    let window_trigger = ms_to_samples(MAX_CHUNK_MS + LOOKAHEAD_MS);
    while !stop.load(Ordering::SeqCst) {
        check()?;
        // Audio problems only end early transcription: release still decodes
        // the recording file from `committed` on.
        if let (Some(source), Some(normalizer_ref)) = (&tap, normalizer.as_mut()) {
            match source
                .read_since(cursor, 1 << 16)
                .and_then(|part| normalizer_ref.push(&part).map(|_| part.len()))
            {
                Ok(read) => cursor += read,
                Err(_) => {
                    tap = None;
                    normalizer = None;
                }
            }
        }
        let ready = normalizer
            .as_ref()
            .map(|n| n.stable_samples().len().saturating_sub(handoff.committed))
            .unwrap_or(0);
        if ready < window_trigger {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        let stable = normalizer
            .as_ref()
            .expect("normalizer present")
            .stable_samples();
        let start = handoff.committed;
        let planned = plan_audio_chunks(
            &stable[start..start + window_trigger],
            TARGET_SAMPLE_RATE_HZ,
        );
        let end = start
            + planned
                .first()
                .map(|chunk| chunk.end_sample)
                .unwrap_or(window_trigger);
        let audio = PreparedAudio {
            sample_rate_hz: TARGET_SAMPLE_RATE_HZ,
            channels: 1,
            samples: stable[start..end].to_vec(),
        };
        let index = handoff.windows.len();
        let path = temp_dir.join(format!("murmela-dictation-{}-{index}.wav", session.id));
        // A failed or timed-out request may still answer later, so this worker
        // cannot be trusted with the next one: give up, and release decodes the
        // whole recording with a fresh worker.
        let result = transcribe_samples(service, session, &mut handoff, &audio, &path)?;
        eprintln!(
            "[dictation] transcribed window {index} {}-{} ms while recording",
            samples_to_ms(start),
            samples_to_ms(end)
        );
        handoff.windows.push(Window {
            start_sample: start,
            end_sample: end,
            result,
        });
        handoff.committed = end;
    }
    check()?;
    Ok(handoff)
}

/// Decode `audio` with the handoff's worker. `Ok(None)` means no speech.
fn transcribe_samples(
    service: &TranslationService,
    session: &DictationSession,
    handoff: &mut Handoff,
    audio: &PreparedAudio,
    path: &Path,
) -> Result<Option<TranscriptResult>> {
    crate::audio_preprocess::write_wav(path, audio)?;
    let mut request = handoff.request.clone();
    request.file_path = path.to_string_lossy().into_owned();
    let result = service.run_asr(session, &mut handoff.worker, request);
    let _ = std::fs::remove_file(path);
    match result {
        Ok(result) if result.plain_text.trim().is_empty() => Ok(None),
        Ok(result) => Ok(Some(result)),
        Err(error) if error.to_string().starts_with("TRANSCRIPTION_EMPTY") => Ok(None),
        Err(error) => Err(error),
    }
}

/// The settings that decide what a window decodes to. Windows made with other
/// settings (changed between press and release) are discarded.
fn same_decode(left: &FileTranscriptionRequest, right: &FileTranscriptionRequest) -> bool {
    left.use_context == right.use_context
        && left.profile == right.profile
        && left.selected_model_id == right.selected_model_id
        && left.language_mode == right.language_mode
        && left.fixed_language == right.fixed_language
        && left.timestamps == right.timestamps
        && left.prefer_gpu == right.prefer_gpu
        && left.context_prompt == right.context_prompt
        && left.context_terms == right.context_terms
}

/// Final ASR on release. Returns the transcript and the admission permit the
/// rest of the dictation (translation, insertion) keeps holding.
pub(crate) fn complete(
    service: &TranslationService,
    session: &DictationSession,
    request: FileTranscriptionRequest,
    handoff: Handoff,
    temp_dir: &Path,
) -> Result<(TranscriptResult, ProcessingPermit)> {
    let Handoff {
        mut worker,
        permit,
        request: early_request,
        windows,
        committed,
        _work,
    } = handoff;
    let outcome = (|| -> Result<TranscriptResult> {
        service.ensure_active(session)?;
        if windows.is_empty() || !same_decode(&early_request, &request) {
            return service.run_asr(session, &mut worker, request.clone());
        }
        let recording = crate::audio_preprocess::decode_audio_file(Path::new(&request.file_path))?;
        if recording.sample_rate_hz != TARGET_SAMPLE_RATE_HZ
            || recording.channels != 1
            || recording.samples.len() < committed
        {
            eprintln!("[dictation] recording does not match streamed audio; decoding it whole");
            return service.run_asr(session, &mut worker, request.clone());
        }
        let mut parts: Vec<(usize, usize, TranscriptResult)> = windows
            .into_iter()
            .filter_map(|window| {
                window
                    .result
                    .map(|result| (window.start_sample, window.end_sample, result))
            })
            .collect();
        let tail = &recording.samples[committed..];
        if samples_to_ms(tail.len()) >= MIN_TAIL_MS {
            let path = temp_dir.join(format!("murmela-dictation-{}-tail.wav", session.id));
            crate::audio_preprocess::write_wav(
                &path,
                &PreparedAudio {
                    sample_rate_hz: TARGET_SAMPLE_RATE_HZ,
                    channels: 1,
                    samples: tail.to_vec(),
                },
            )?;
            let mut tail_request = request.clone();
            tail_request.file_path = path.to_string_lossy().into_owned();
            let decoded = service.run_asr(session, &mut worker, tail_request);
            let _ = std::fs::remove_file(&path);
            match decoded {
                Ok(result) => parts.push((committed, recording.samples.len(), result)),
                Err(error) if error.to_string().starts_with("TRANSCRIPTION_EMPTY") => {}
                Err(error) => return Err(error),
            }
        }
        eprintln!(
            "[dictation] release decoded {} ms tail after {} ms transcribed while recording",
            samples_to_ms(tail.len()),
            samples_to_ms(committed)
        );
        merge(parts)
    })();
    match outcome {
        Ok(result) => {
            service.keep_or_release_asr(session, &permit, worker);
            Ok((result, permit))
        }
        Err(error) => {
            drop(worker);
            drop(permit);
            Err(error)
        }
    }
}

/// Join window transcripts in recording order, moving sentence fragments across
/// window boundaries the same way chunked Qwen transcripts are aligned.
fn merge(parts: Vec<(usize, usize, TranscriptResult)>) -> Result<TranscriptResult> {
    let Some((_, _, first)) = parts.first() else {
        bail!("TRANSCRIPTION_EMPTY: no speech was recognized");
    };
    let job_id = first.job_id.clone();
    let model_name = first.model_name.clone();
    let mut partial = false;
    let mut recovered = false;
    let mut segments: Vec<TranscriptSegment> = Vec::new();
    let mut warnings = Vec::new();
    for (start, end, result) in parts {
        let offset = samples_to_ms(start);
        let limit = samples_to_ms(end);
        partial |= result.quality_status == crate::asr::TranscriptQualityStatus::Partial;
        recovered |= result.quality_status == crate::asr::TranscriptQualityStatus::Recovered;
        for mut segment in result.segments {
            segment.start_ms = (segment.start_ms + offset).min(limit);
            segment.end_ms = (segment.end_ms + offset).clamp(segment.start_ms, limit);
            segments.push(segment);
        }
        for mut warning in result.warnings {
            warning.start_ms = (warning.start_ms + offset).min(limit);
            warning.end_ms = (warning.end_ms + offset).min(limit);
            warnings.push(warning);
        }
    }
    let segments = crate::transcript_stitching::align_segments_to_sentences(segments);
    let mut merged =
        crate::asr::build_transcript_result_named(job_id, &model_name, segments, warnings);
    if partial {
        merged.quality_status = crate::asr::TranscriptQualityStatus::Partial;
    } else if recovered && merged.quality_status == crate::asr::TranscriptQualityStatus::Clean {
        merged.quality_status = crate::asr::TranscriptQualityStatus::Recovered;
    }
    if merged.plain_text.trim().is_empty() {
        bail!("TRANSCRIPTION_EMPTY: no speech was recognized");
    }
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asr::TranscriptQualityStatus;

    fn segment(start_ms: i64, end_ms: i64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: String::new(),
            start_ms,
            end_ms,
            text: text.into(),
            language_code: "en".into(),
            segment_order: 0,
            confidence: None,
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: Default::default(),
            speaker_confidence: None,
        }
    }

    fn result(segments: Vec<TranscriptSegment>) -> TranscriptResult {
        crate::asr::build_transcript_result_named("job".into(), "Model", segments, Vec::new())
    }

    #[test]
    fn windows_are_shifted_into_recording_time_and_joined_in_order() {
        let first = result(vec![segment(0, 20_000, "The first window ends here.")]);
        let second = result(vec![segment(0, 5_000, "The second one follows.")]);
        let merged = merge(vec![
            (0, ms_to_samples(25_000), first),
            (ms_to_samples(25_000), ms_to_samples(31_000), second),
        ])
        .unwrap();
        assert_eq!(
            merged.plain_text,
            "The first window ends here. The second one follows."
        );
        assert_eq!(merged.segments.len(), 2);
        assert_eq!(merged.segments[1].start_ms, 25_000);
        assert_eq!(merged.segments[1].end_ms, 30_000);
        assert_eq!(merged.segments[1].segment_order, 1);
        assert_eq!(merged.model_name, "Model");
        assert_eq!(merged.quality_status, TranscriptQualityStatus::Clean);
    }

    #[test]
    fn segments_never_extend_past_their_window() {
        let late = result(vec![segment(0, 99_000, "Clamped.")]);
        let merged = merge(vec![(ms_to_samples(10_000), ms_to_samples(12_000), late)]).unwrap();
        assert_eq!(merged.segments[0].start_ms, 10_000);
        assert_eq!(merged.segments[0].end_ms, 12_000);
    }

    #[test]
    fn nothing_recognized_is_reported_as_empty() {
        let error = merge(Vec::new()).unwrap_err();
        assert!(error.to_string().starts_with("TRANSCRIPTION_EMPTY"));
    }

    #[test]
    fn changed_settings_discard_early_windows() {
        let request = FileTranscriptionRequest {
            use_context: None,
            profile: crate::settings::ModelProfile::Balanced,
            selected_model_id: Some("a".into()),
            language_mode: crate::settings::LanguageMode::Auto,
            fixed_language: None,
            timestamps: false,
            prefer_gpu: true,
            file_path: "one.wav".into(),
            context_prompt: None,
            context_terms: Vec::new(),
        };
        let mut other_file = request.clone();
        other_file.file_path = "two.wav".into();
        assert!(same_decode(&request, &other_file));
        let mut other_model = request.clone();
        other_model.selected_model_id = Some("b".into());
        assert!(!same_decode(&request, &other_model));
    }

    #[test]
    fn short_dictations_never_start_a_window() {
        // A window is only cut once more than one full decoder window waits.
        assert!(ms_to_samples(MAX_CHUNK_MS + LOOKAHEAD_MS) > ms_to_samples(29_000));
        let quiet = vec![0.0_f32; ms_to_samples(MAX_CHUNK_MS + LOOKAHEAD_MS)];
        let planned = plan_audio_chunks(&quiet, TARGET_SAMPLE_RATE_HZ);
        assert!(planned.len() >= 2);
        assert!(planned[0].end_sample <= ms_to_samples(MAX_CHUNK_MS));
    }
}
