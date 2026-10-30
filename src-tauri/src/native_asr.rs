use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::asr::{TranscriptQualityStatus, TranscriptResult, TranscriptSegment, TranscriptWarning};
use crate::diarization::{
    DiarizationSource, DiarizationStatus, DiarizationTurn, TranscriptSpeaker,
};
use crate::speaker_reconciliation::SpeakerAttribution;
use crate::transcription_progress::{set_step, Step};
use crate::{audio_preprocess, model_metadata, native_chunks, native_sounds};

pub const WORKER_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkerRequest {
    pub protocol_version: u32,
    pub job_id: String,
    pub model_path: String,
    pub audio_path: String,
    pub prompt: Option<String>,
    pub context_terms: Vec<String>,
    pub greedy: bool,
    pub max_tokens: u32,
    pub offline: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeOutputSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    #[serde(default)]
    pub speaker: Option<String>,
    #[serde(default)]
    pub language_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeWorkerResult {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub segments: Vec<NativeOutputSegment>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub warning: Option<String>,
    /// Stretches the worker left out because the model kept looping there.
    #[serde(default)]
    pub gaps: Vec<NativeGap>,
}

/// A stretch of audio a worker did not transcribe; times are relative to the
/// audio the worker was given until the chunks are stitched.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeGap {
    pub start_ms: i64,
    pub end_ms: i64,
    /// How the model got stuck there, e.g. "repeated a word".
    pub reason: String,
    /// The engine that transcribed the stretch instead.
    #[serde(default)]
    pub filled_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NativeWorkerRecord {
    Progress { progress_percent: i32 },
    Heartbeat { progress_percent: i32 },
    Result { result: NativeWorkerResult },
    Error { code: String, message: String },
    Canceled,
}

pub fn parse_worker_record(line: &str) -> anyhow::Result<NativeWorkerRecord> {
    serde_json::from_str(line.trim()).map_err(Into::into)
}

pub fn transcribe_with_native_worker(
    model: &crate::asr::InstalledModel,
    request: &crate::asr::FileTranscriptionRequest,
    progress: Option<Arc<AtomicI32>>,
    models_dir: &Path,
) -> anyhow::Result<TranscriptResult> {
    let prepared = audio_preprocess::decode_audio_file(Path::new(&request.file_path))?;
    let duration_ms = prepared.samples.len() as i64 * 1_000 / prepared.sample_rate_hz as i64;
    if let Some(limit) = model.capabilities.maximum_audio_duration_ms {
        if duration_ms > limit {
            anyhow::bail!(
                "MODEL_AUDIO_TOO_LONG: {} supports audio up to {} minutes",
                model.model_name,
                limit / 60_000
            );
        }
    }

    let report = |percent: i32| {
        if let Some(progress) = &progress {
            progress.fetch_max(percent.clamp(0, 100), Ordering::Relaxed);
        }
    };
    let log = EngineLog::new(models_dir);
    let mut fallback = GapFallback::new(models_dir, request);
    let chunk_ms = native_chunks::chunk_length_ms(crate::memory_pressure::installed_bytes());
    let output = if model.id == model_metadata::VIBEVOICE_MODEL_ID && duration_ms > chunk_ms {
        transcribe_in_chunks(model, request, &prepared, duration_ms, chunk_ms, models_dir, &log, &mut fallback, &report)?
    } else {
        let mut output = run_worker_or_gap(model, request, &prepared, &log, &report)?;
        if !output.gaps.is_empty() {
            let embedder = crate::model_downloads::installed_diarization_package_path(models_dir)
                .and_then(|package| native_chunks::VoiceEmbedder::load(&package));
            let voices = embedder
                .as_ref()
                .map(|embedder| voices_of(embedder, &output.segments, &prepared))
                .unwrap_or_default();
            fallback.fill(&mut output, &prepared, &voices, embedder.as_ref(), &log)?;
        }
        output
    };
    let mut output = output;
    tidy_sounds(&mut output, &prepared, models_dir, &log);
    Ok(normalize_native_result(&model.id, &model.model_name, duration_ms, output))
}

/// Runs the worker; a worker that gives up on looping audio altogether (an
/// older worker did) leaves the whole stretch as a gap instead of failing the
/// recording.
fn run_worker_or_gap(
    model: &crate::asr::InstalledModel,
    request: &crate::asr::FileTranscriptionRequest,
    audio: &audio_preprocess::PreparedAudio,
    log: &EngineLog,
    report: &dyn Fn(i32),
) -> anyhow::Result<NativeWorkerResult> {
    match run_worker_on(model, request, audio, log, report) {
        Err(error) if error.to_string().starts_with("DECODER_REPETITION") => {
            log.line(&format!("{} gave up on a piece: {error}", model.model_name));
            let end_ms = audio.samples.len() as i64 * 1_000 / audio.sample_rate_hz.max(1) as i64;
            Ok(NativeWorkerResult {
                text: String::new(),
                segments: Vec::new(),
                truncated: false,
                warning: None,
                gaps: vec![NativeGap {
                    start_ms: 0,
                    end_ms,
                    reason: "repeated itself".to_string(),
                    filled_by: None,
                }],
            })
        }
        result => result,
    }
}

/// A voice embedding for every speaker label in `segments` (times relative
/// to `audio`).
fn voices_of(
    embedder: &native_chunks::VoiceEmbedder,
    segments: &[NativeOutputSegment],
    audio: &audio_preprocess::PreparedAudio,
) -> native_chunks::ChunkVoices {
    let segments: Vec<NativeOutputSegment> = segments
        .iter()
        .map(|segment| NativeOutputSegment {
            speaker: segment.speaker.as_deref().map(str::trim).map(str::to_string),
            ..segment.clone()
        })
        .collect();
    let mut labels: Vec<String> = segments.iter().filter_map(|s| s.speaker.clone()).collect();
    labels.sort();
    labels.dedup();
    labels
        .into_iter()
        .filter_map(|label| {
            let ranges = native_chunks::voice_sample_ranges(&segments, &label);
            let voice = embedder.embed(&audio.samples, audio.sample_rate_hz, &ranges)?;
            Some((label, voice))
        })
        .collect()
}

/// Fills the gaps a worker left with Whisper, loaded only when a gap comes up.
struct GapFallback<'a> {
    models_dir: &'a Path,
    request: &'a crate::asr::FileTranscriptionRequest,
    whisper: Option<Result<crate::asr::WhisperFallback, String>>,
}

impl<'a> GapFallback<'a> {
    fn new(models_dir: &'a Path, request: &'a crate::asr::FileTranscriptionRequest) -> Self {
        Self { models_dir, request, whisper: None }
    }

    /// Transcribes `output`'s gaps (times relative to `audio`) and puts the
    /// text in place. Each filled stretch goes to the speaker whose voice it
    /// matches, if any; a stretch Whisper cannot transcribe either is marked
    /// as unclear, as Whisper marks its own.
    fn fill(
        &mut self,
        output: &mut NativeWorkerResult,
        audio: &audio_preprocess::PreparedAudio,
        voices: &native_chunks::ChunkVoices,
        embedder: Option<&native_chunks::VoiceEmbedder>,
        log: &EngineLog,
    ) -> anyhow::Result<()> {
        let rate = audio.sample_rate_hz as i64;
        for gap in &mut output.gaps {
            crate::shutdown::ensure_running()?;
            let from = ((gap.start_ms * rate / 1_000) as usize).min(audio.samples.len());
            let to = ((gap.end_ms * rate / 1_000) as usize).min(audio.samples.len());
            let piece = audio_preprocess::PreparedAudio {
                sample_rate_hz: audio.sample_rate_hz,
                channels: audio.channels,
                samples: audio.samples[from..to].to_vec(),
            };
            let request = self.request;
            let heard = self.whisper().and_then(|whisper| whisper.transcribe(&piece, request));
            crate::shutdown::ensure_running()?;
            let segments = match heard {
                Ok(segments) if !segments.is_empty() => segments,
                Ok(_) => {
                    log.line(&format!("Whisper heard no speech in {}–{} ms", gap.start_ms, gap.end_ms));
                    output.segments.push(unclear_segment(gap));
                    continue;
                }
                Err(error) => {
                    log.line(&format!("Whisper could not fill {}–{} ms: {error}", gap.start_ms, gap.end_ms));
                    output.segments.push(unclear_segment(gap));
                    continue;
                }
            };
            let speaker = embedder.and_then(|embedder| matching_voice(embedder, &piece, voices));
            log.line(&format!(
                "Whisper filled {}–{} ms with {} segments, speaker {}",
                gap.start_ms,
                gap.end_ms,
                segments.len(),
                speaker.as_deref().unwrap_or("unknown")
            ));
            for segment in segments {
                let start_ms = (gap.start_ms + segment.start_ms).clamp(gap.start_ms, gap.end_ms);
                output.segments.push(NativeOutputSegment {
                    start_ms,
                    end_ms: (gap.start_ms + segment.end_ms).clamp(start_ms, gap.end_ms),
                    text: segment.text,
                    speaker: speaker.clone(),
                    language_code: Some(segment.language_code).filter(|code| code != "und"),
                });
            }
            gap.filled_by = Some("Whisper".to_string());
        }
        output.segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
        Ok(())
    }
}

impl GapFallback<'_> {
    fn whisper(&mut self) -> anyhow::Result<&crate::asr::WhisperFallback> {
        let models_dir = self.models_dir;
        let prefer_gpu = self.request.prefer_gpu;
        let whisper = self.whisper.get_or_insert_with(|| {
            crate::asr::WhisperFallback::load(models_dir, prefer_gpu).map_err(|error| error.to_string())
        });
        whisper.as_ref().map_err(|error| anyhow::anyhow!("{error}"))
    }
}

/// The speaker label whose voice `piece` matches, if any.
fn matching_voice(
    embedder: &native_chunks::VoiceEmbedder,
    piece: &audio_preprocess::PreparedAudio,
    voices: &native_chunks::ChunkVoices,
) -> Option<String> {
    let length_ms = piece.samples.len() as i64 * 1_000 / piece.sample_rate_hz.max(1) as i64;
    let voice = embedder.embed(&piece.samples, piece.sample_rate_hz, &[(0, length_ms)])?;
    voices
        .iter()
        .map(|(label, known)| (label, native_chunks::cosine(&voice, known)))
        .filter(|(_, similarity)| *similarity >= native_chunks::SAME_VOICE_SIMILARITY)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(label, _)| label.clone())
}

fn unclear_segment(gap: &NativeGap) -> NativeOutputSegment {
    NativeOutputSegment {
        start_ms: gap.start_ms,
        end_ms: gap.end_ms,
        text: native_sounds::unclear_text(gap.start_ms, gap.end_ms),
        speaker: None,
        language_code: None,
    }
}

/// Unclear stretches closer than this, with no words between, are one.
const UNCLEAR_JOIN_MS: i64 = 1_500;
/// Shorter unclear blips (a cough of a word) are dropped, not marked.
const MIN_UNCLEAR_MS: i64 = 1_500;

/// Drops sound tags and marks each stretch of tagged unclear speech once,
/// with the speaker whose voice it matches; see `native_sounds`. Whisper is
/// not asked to fill these in: on a real call it wrote a confident, wrong
/// sentence for every one (see `unclear_speech_is_left_to_the_listener`).
/// Segment times are relative to `audio`.
fn tidy_sounds(output: &mut NativeWorkerResult, audio: &audio_preprocess::PreparedAudio, models_dir: &Path, log: &EngineLog) {
    struct Stretch {
        start_ms: i64,
        end_ms: i64,
        speaker: Option<String>,
        /// Kept segments before it: nothing was said between joined stretches.
        after: usize,
    }
    let mut kept = Vec::with_capacity(output.segments.len());
    let mut stretches: Vec<Stretch> = Vec::new();
    let mut sounds = 0;
    for segment in std::mem::take(&mut output.segments) {
        if native_sounds::is_unclear_mark(&segment.text) {
            kept.push(segment);
            continue;
        }
        match native_sounds::kind(&segment.text) {
            native_sounds::Kind::Speech => kept.push(segment),
            native_sounds::Kind::Sound => sounds += 1,
            native_sounds::Kind::Unclear => {
                let speaker = segment.speaker.as_deref().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
                match stretches.last_mut() {
                    Some(last)
                        if last.after == kept.len()
                            && segment.start_ms <= last.end_ms + UNCLEAR_JOIN_MS
                            && (speaker.is_none() || last.speaker.is_none() || speaker == last.speaker) =>
                    {
                        last.end_ms = last.end_ms.max(segment.end_ms);
                        last.speaker = last.speaker.take().or(speaker);
                    }
                    _ => stretches.push(Stretch {
                        start_ms: segment.start_ms,
                        end_ms: segment.end_ms,
                        speaker,
                        after: kept.len(),
                    }),
                }
            }
        }
    }
    stretches.retain(|stretch| stretch.end_ms - stretch.start_ms >= MIN_UNCLEAR_MS);
    if !stretches.is_empty() {
        let embedder = crate::model_downloads::installed_diarization_package_path(models_dir)
            .and_then(|package| native_chunks::VoiceEmbedder::load(&package));
        let voices = embedder
            .as_ref()
            .map(|embedder| voices_of(embedder, &kept, audio))
            .unwrap_or_default();
        let rate = audio.sample_rate_hz as i64;
        for stretch in &stretches {
            let speaker = stretch.speaker.clone().or_else(|| {
                let from = ((stretch.start_ms * rate / 1_000) as usize).min(audio.samples.len());
                let to = ((stretch.end_ms * rate / 1_000) as usize).min(audio.samples.len());
                let piece = audio_preprocess::PreparedAudio {
                    sample_rate_hz: audio.sample_rate_hz,
                    channels: audio.channels,
                    samples: audio.samples[from..to].to_vec(),
                };
                embedder.as_ref().and_then(|embedder| matching_voice(embedder, &piece, &voices))
            });
            kept.push(NativeOutputSegment {
                start_ms: stretch.start_ms,
                end_ms: stretch.end_ms,
                text: native_sounds::unclear_text(stretch.start_ms, stretch.end_ms),
                speaker,
                language_code: None,
            });
        }
        kept.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
    }
    output.segments = kept;
    if sounds > 0 || !stretches.is_empty() {
        log.line(&format!("dropped {sounds} sound tags; marked {} unclear stretches", stretches.len()));
    }
}

/// Engine diagnostics (where a model looped, what filled a gap) for the file
/// transcription log the app writes its job events to. The engine runs in its
/// own process, so it appends to the file directly.
#[derive(Clone)]
pub(crate) struct EngineLog(Option<PathBuf>);

impl EngineLog {
    pub(crate) fn new(models_dir: &Path) -> Self {
        Self(models_dir.parent().map(|dir| dir.join("file-transcription.log")))
    }

    pub(crate) fn line(&self, message: &str) {
        let line = format!("[{}] engine {}\n", chrono::Utc::now().timestamp_millis(), message);
        eprintln!("{}", line.trim_end());
        if let Some(path) = &self.0 {
            if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

fn run_worker_on(
    model: &crate::asr::InstalledModel,
    request: &crate::asr::FileTranscriptionRequest,
    audio: &audio_preprocess::PreparedAudio,
    log: &EngineLog,
    report: &dyn Fn(i32),
) -> anyhow::Result<NativeWorkerResult> {
    // Always hand the worker the audio Murmela already decoded. The Python
    // runtimes cannot open video containers (MP4/MOV/MKV/…) or every audio
    // codec Murmela accepts, so passing the original file would fail there.
    let path = std::env::temp_dir().join(format!("murmela-worker-{}.wav", Uuid::new_v4()));
    let temporary_wav = TemporaryAudio(path.clone());
    audio_preprocess::write_wav(&path, audio)?;
    let result = run_worker_process(model, request, &path, log, report);
    drop(temporary_wav);
    result
}

/// VibeVoice keeps the whole recording in memory, so a long one is
/// transcribed in overlapping chunks, one worker process each (the worker
/// then returns all of its memory), and stitched with matched speakers.
fn transcribe_in_chunks(
    model: &crate::asr::InstalledModel,
    request: &crate::asr::FileTranscriptionRequest,
    prepared: &audio_preprocess::PreparedAudio,
    duration_ms: i64,
    chunk_ms: i64,
    models_dir: &Path,
    log: &EngineLog,
    fallback: &mut GapFallback,
    report: &dyn Fn(i32),
) -> anyhow::Result<NativeWorkerResult> {
    let rate = prepared.sample_rate_hz;
    let spans = native_chunks::plan_chunks(duration_ms, chunk_ms, |at| {
        native_chunks::loudness(&prepared.samples, rate, at)
    });
    let count = spans.len() as i32;
    let mut transcripts = Vec::with_capacity(spans.len());
    for (index, span) in spans.iter().enumerate() {
        let from = (span.start_ms * rate as i64 / 1_000) as usize;
        let to = ((span.end_ms * rate as i64 / 1_000) as usize).min(prepared.samples.len());
        let audio = audio_preprocess::PreparedAudio {
            sample_rate_hz: rate,
            channels: prepared.channels,
            samples: prepared.samples[from..to].to_vec(),
        };
        let base = index as i32 * 100 / count;
        let share = (index as i32 + 1) * 100 / count - base;
        log.line(&format!("piece {}/{count}: {}–{} ms", index + 1, span.start_ms, span.end_ms));
        let result = run_worker_or_gap(model, request, &audio, log, &|percent| report(base + percent * share / 100))?;
        transcripts.push(native_chunks::ChunkTranscript { span: *span, result });
    }

    set_step(Step::MatchingSpeakers);
    let embedder = crate::model_downloads::installed_diarization_package_path(models_dir)
        .and_then(|package| native_chunks::VoiceEmbedder::load(&package));
    let voices: Vec<native_chunks::ChunkVoices> = match &embedder {
        Some(embedder) => transcripts
            .iter()
            .map(|chunk| {
                let segments: Vec<NativeOutputSegment> = chunk
                    .result
                    .segments
                    .iter()
                    .map(|segment| NativeOutputSegment {
                        start_ms: chunk.span.start_ms + segment.start_ms,
                        end_ms: chunk.span.start_ms + segment.end_ms,
                        ..segment.clone()
                    })
                    .collect();
                voices_of(embedder, &segments, prepared)
            })
            .collect(),
        None => Vec::new(),
    };
    for (index, chunk) in transcripts.iter_mut().enumerate() {
        if chunk.result.gaps.is_empty() {
            continue;
        }
        let from = (chunk.span.start_ms * rate as i64 / 1_000) as usize;
        let to = ((chunk.span.end_ms * rate as i64 / 1_000) as usize).min(prepared.samples.len());
        let audio = audio_preprocess::PreparedAudio {
            sample_rate_hz: rate,
            channels: prepared.channels,
            samples: prepared.samples[from..to].to_vec(),
        };
        let empty = native_chunks::ChunkVoices::new();
        let chunk_voices = voices.get(index).unwrap_or(&empty);
        fallback.fill(&mut chunk.result, &audio, chunk_voices, embedder.as_ref(), log)?;
    }
    let (result, stitched) = native_chunks::stitch(transcripts, &voices);
    log.line(&format!(
        "{} chunks of up to {} min; {} speakers; voices {}; matches {:?}",
        count,
        chunk_ms / 60_000,
        stitched.speakers,
        if embedder.is_some() { "used" } else { "unavailable" },
        stitched.matches
    ));
    report(100);
    Ok(result)
}

struct TemporaryAudio(PathBuf);

impl Drop for TemporaryAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn run_worker_process(
    model: &crate::asr::InstalledModel,
    request: &crate::asr::FileTranscriptionRequest,
    audio_path: &Path,
    log: &EngineLog,
    report: &dyn Fn(i32),
) -> anyhow::Result<NativeWorkerResult> {
    let worker = resolve_worker(&model.id).ok_or_else(|| {
        anyhow::anyhow!(
            "MODEL_RUNTIME_MISSING: the verified {} inference worker is not bundled",
            model.model_name
        )
    })?;
    let max_tokens = if model.id == model_metadata::VIBEVOICE_MODEL_ID {
        32_768
    } else {
        65_536
    };
    let prompt = if model.id == model_metadata::MOSS_MODEL_ID {
        let hotwords = request.context_terms.join(", ");
        request
            .context_prompt
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|base| {
                if hotwords.is_empty() {
                    base.to_string()
                } else {
                    format!("{base}\nHotwords: {hotwords}")
                }
            })
            .or_else(|| (!hotwords.is_empty()).then(|| format!("Hotwords: {hotwords}")))
    } else {
        request.context_prompt.clone()
    };
    let worker_request = NativeWorkerRequest {
        protocol_version: WORKER_PROTOCOL_VERSION,
        job_id: Uuid::new_v4().to_string(),
        model_path: model.local_path.clone(),
        audio_path: audio_path.display().to_string(),
        prompt,
        context_terms: request.context_terms.clone(),
        greedy: true,
        max_tokens,
        offline: true,
    };

    let worker_path = worker.display_path().to_path_buf();
    let mut command = worker.command();
    crate::managed_process::isolate(&mut command);
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HF_HUB_OFFLINE", "1")
        .env("TRANSFORMERS_OFFLINE", "1")
        .spawn()
        .map_err(|error| {
            anyhow::anyhow!(
                "MODEL_RUNTIME_MISSING: failed to start {}: {error}",
                worker_path.display()
            )
        })?;
    let mut child = crate::managed_process::ManagedChild::new(child);
    // Drain diagnostics to avoid blocking a verbose model process on stderr,
    // keeping the last lines to explain an exit without a result.
    let stderr_tail = Arc::new(Mutex::new(Vec::<String>::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = stderr_tail.clone();
        let log = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                // The worker's own diagnostics, such as where the model looped.
                if let Some(note) = line.strip_prefix("murmela: ") {
                    log.line(note);
                }
                if let Ok(mut tail) = tail.lock() {
                    if tail.len() == STDERR_TAIL_LINES {
                        tail.remove(0);
                    }
                    tail.push(line);
                }
            }
        });
    }
    if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(&mut stdin, &worker_request)?;
        stdin.write_all(b"\n")?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow::anyhow!("native worker stdout unavailable"))?;
    let mut result = None;
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    });
    loop {
        crate::shutdown::ensure_running()?;
        let line = match receiver.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(line) => line?,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        match parse_worker_record(&line)? {
            NativeWorkerRecord::Progress { progress_percent } => {
                // The worker's first progress comes once its model is loaded.
                set_step(Step::Transcribing);
                report(progress_percent);
            }
            NativeWorkerRecord::Heartbeat { progress_percent } => report(progress_percent),
            NativeWorkerRecord::Result { result: output } => {
                result = Some(output);
                break;
            }
            NativeWorkerRecord::Error { code, message } => {
                let _ = child.kill();
                let code = if is_out_of_memory(&message) { OUT_OF_MEMORY } else { code.as_str() };
                anyhow::bail!("{code}: {message}");
            }
            NativeWorkerRecord::Canceled => {
                let _ = child.kill();
                anyhow::bail!("TRANSCRIPTION_CANCELED: native model inference was canceled");
            }
        }
    }
    let status = loop {
        crate::shutdown::ensure_running()?;
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    if let Some(result) = result {
        return Ok(result);
    }
    // Give the drain thread a moment to read what the worker wrote last.
    std::thread::sleep(std::time::Duration::from_millis(50));
    let tail = stderr_tail.lock().map(|tail| tail.join("\n")).unwrap_or_default();
    Err(exit_without_result(&model.model_name, &status, &tail))
}

const OUT_OF_MEMORY: &str = "MODEL_OUT_OF_MEMORY";
const STDERR_TAIL_LINES: usize = 20;

/// Metal and MLX report a full GPU working set in their own words, not as a
/// Python `MemoryError`.
fn is_out_of_memory(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    ["insufficient memory", "outofmemory", "out of memory", "metal::malloc", "memoryerror"]
        .iter()
        .any(|needle| text.contains(needle))
}

fn exit_without_result(
    model_name: &str,
    status: &std::process::ExitStatus,
    stderr_tail: &str,
) -> anyhow::Error {
    use std::os::unix::process::ExitStatusExt;
    // macOS ends a process with SIGKILL when memory runs out.
    if status.signal() == Some(9) || is_out_of_memory(stderr_tail) {
        return anyhow::anyhow!("{OUT_OF_MEMORY}: {model_name} ran out of memory ({status})");
    }
    let last = stderr_tail.lines().rev().find(|line| !line.trim().is_empty());
    match last {
        Some(line) => anyhow::anyhow!(
            "MODEL_WORKER_FAILED: {model_name} exited with {status} without a result: {}",
            line.trim()
        ),
        None => anyhow::anyhow!("MODEL_WORKER_FAILED: {model_name} exited with {status} without a result"),
    }
}

/// How a native model worker is started.
#[derive(Debug, Clone, PartialEq, Eq)]
enum WorkerLaunch {
    /// A self-contained, frozen worker (the normal, bundled case).
    Executable(PathBuf),
    /// A worker script run by a specific interpreter that has the worker's
    /// dependencies installed (development only).
    Script {
        interpreter: PathBuf,
        script: PathBuf,
    },
}

impl WorkerLaunch {
    fn command(&self) -> Command {
        match self {
            Self::Executable(path) => Command::new(path),
            Self::Script {
                interpreter,
                script,
            } => {
                let mut command = Command::new(interpreter);
                command.arg(script);
                command
            }
        }
    }

    fn display_path(&self) -> &Path {
        match self {
            Self::Executable(path) => path,
            Self::Script { script, .. } => script,
        }
    }
}

struct WorkerLayout {
    environment_key: &'static str,
    /// Folder under `workers/` (repository and app resources).
    directory: &'static str,
    executable_name: &'static str,
    script_name: &'static str,
}

fn worker_layout(model_id: &str) -> Option<WorkerLayout> {
    match model_id {
        model_metadata::MOSS_MODEL_ID => Some(WorkerLayout {
            environment_key: "MURMELA_MOSS_WORKER",
            directory: "moss",
            executable_name: "murmela-moss-worker",
            script_name: "murmela_moss_worker.py",
        }),
        model_metadata::VIBEVOICE_MODEL_ID => Some(WorkerLayout {
            environment_key: "MURMELA_VIBEVOICE_WORKER",
            directory: "vibevoice",
            executable_name: "murmela-vibevoice-worker",
            script_name: "murmela_vibevoice_worker.py",
        }),
        _ => None,
    }
}

/// Locate the worker for a model.
///
/// A worker script is never run with the system `python3`: that interpreter
/// does not have the worker's dependencies installed. Only the frozen worker
/// bundled with the app (or built by `scripts/build-moss-worker.mjs` or
/// `scripts/build-vibevoice-worker.mjs`) is used,
/// plus — in debug builds — the worker script with the build script's private
/// virtual environment.
fn resolve_worker(model_id: &str) -> Option<WorkerLaunch> {
    let layout = worker_layout(model_id)?;
    if let Some(path) = std::env::var_os(layout.environment_key)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
    {
        return Some(WorkerLaunch::Executable(path));
    }
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let executable_directory = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    for candidate in executable_candidates(executable_directory.as_deref(), &manifest_dir, &layout)
    {
        if candidate.is_file() {
            return Some(WorkerLaunch::Executable(candidate));
        }
    }
    if cfg!(debug_assertions) {
        let interpreter = manifest_dir
            .join("target")
            .join(format!("{}-runtime", layout.directory))
            .join("venv/bin/python");
        let script = manifest_dir
            .join("..")
            .join("workers")
            .join(layout.directory)
            .join(layout.script_name);
        if interpreter.is_file() && script.is_file() {
            return Some(WorkerLaunch::Script {
                interpreter,
                script,
            });
        }
    }
    None
}

fn executable_candidates(
    executable_directory: Option<&Path>,
    manifest_dir: &Path,
    layout: &WorkerLayout,
) -> Vec<PathBuf> {
    let name = layout.executable_name;
    let mut candidates = Vec::new();
    if let Some(directory) = executable_directory {
        candidates.extend([
            directory.join(name),
            directory.join("workers").join(name),
            directory.join("workers").join(name).join(name),
            // Murmela.app/Contents/MacOS/../Resources/workers/<model>/<name>/<name>
            directory
                .join("../Resources/workers")
                .join(layout.directory)
                .join(name)
                .join(name),
        ]);
    }
    // Development build output of scripts/build-{moss,vibevoice}-worker.mjs.
    if cfg!(debug_assertions) {
        candidates.push(
            manifest_dir
                .join("bundle")
                .join(layout.directory)
                .join(name)
                .join(name),
        );
    }
    candidates
}

pub fn worker_available(model_id: &str) -> bool {
    resolve_worker(model_id).is_some()
}

/// Converts model-specific long-form output into Murmela's stable transcript schema.
/// Speaker IDs are intentionally local to each transcript and ordered by first appearance.
pub fn normalize_native_result(
    model_id: &str,
    model_name: &str,
    audio_duration_ms: i64,
    output: NativeWorkerResult,
) -> TranscriptResult {
    let duration = audio_duration_ms.max(0);
    let mut speaker_ids = HashMap::<String, String>::new();
    let mut speakers = Vec::<TranscriptSpeaker>::new();
    let mut turns = Vec::<DiarizationTurn>::new();
    let mut segments = Vec::<TranscriptSegment>::new();
    let mut incomplete = false;
    let in_gap = |start_ms: i64, end_ms: i64| {
        output.gaps.iter().any(|gap| start_ms >= gap.start_ms && end_ms <= gap.end_ms)
    };

    for raw in output.segments {
        let start_ms = raw.start_ms.clamp(0, duration);
        let end_ms = raw.end_ms.clamp(start_ms, duration);
        let text = raw.text.trim().to_string();
        if text.is_empty() || end_ms <= start_ms {
            incomplete = true;
            continue;
        }
        let normalized_speaker = raw
            .speaker
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|label| {
                speaker_ids
                    .entry(label.to_string())
                    .or_insert_with(|| {
                        let order = speakers.len() as i32;
                        let id = format!("speaker_{order}");
                        speakers.push(TranscriptSpeaker {
                            speaker_id: id.clone(),
                            display_name: format!("Speaker {}", order + 1),
                            speaker_order: order,
                        });
                        id
                    })
                    .clone()
            });
        let has_speaker = normalized_speaker.is_some();
        // Text another engine filled in, or an unclear-audio mark, may have no
        // speaker; that does not make the model's own speakers incomplete.
        if !has_speaker && !in_gap(raw.start_ms, raw.end_ms) && !native_sounds::is_unclear_mark(&text) {
            incomplete = true;
        }
        let order = segments.len() as i32;
        let language_code =
            crate::output_format::normalize_language_code(raw.language_code.as_deref());
        if let Some(speaker_id) = &normalized_speaker {
            turns.push(DiarizationTurn {
                id: format!("turn_{order}"),
                start_ms,
                end_ms,
                speaker_ids: vec![speaker_id.clone()],
                confidence: None,
                is_overlap: false,
                is_uncertain: false,
                turn_order: order,
            });
        }
        segments.push(TranscriptSegment {
            id: Uuid::new_v4().to_string(),
            start_ms,
            end_ms,
            text,
            language_code,
            segment_order: order,
            confidence: None,
            speaker_id: normalized_speaker.clone(),
            speaker_ids: normalized_speaker.map(|id| vec![id]),
            speaker_attribution: if has_speaker {
                SpeakerAttribution::Assigned
            } else {
                SpeakerAttribution::None
            },
            speaker_confidence: None,
        });
    }

    let recovered_text = segments
        .iter()
        .map(|segment| segment.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let plain_text = if recovered_text.is_empty() {
        output.text.trim().to_string()
    } else {
        recovered_text
    };
    if segments.is_empty() && !plain_text.is_empty() {
        incomplete = true;
    }
    let timestamped_text = segments
        .iter()
        .map(|segment| {
            let speaker = segment
                .speaker_id
                .as_deref()
                .and_then(|id| speakers.iter().find(|speaker| speaker.speaker_id == id))
                .map(|speaker| speaker.display_name.as_str())
                .unwrap_or("Unknown speaker");
            format!(
                "[{} - {}] {}: {}",
                crate::output_format::clock_ms(segment.start_ms),
                crate::output_format::clock_ms(segment.end_ms),
                speaker,
                segment.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut detected_languages = Vec::<String>::new();
    for segment in &segments {
        if segment.language_code != "und" && !detected_languages.contains(&segment.language_code) {
            detected_languages.push(segment.language_code.clone());
        }
    }
    let mut warnings = Vec::new();
    if output.truncated {
        warnings.push(TranscriptWarning {
            start_ms: segments.last().map(|segment| segment.start_ms).unwrap_or(0),
            end_ms: duration,
            reason: "token_limit_without_eos".to_string(),
            attempts: 1,
            outcome: "partially_recovered".to_string(),
        });
    }
    if let Some(warning) = output.warning.filter(|warning| !warning.trim().is_empty()) {
        warnings.push(TranscriptWarning {
            start_ms: 0,
            end_ms: duration,
            reason: warning,
            attempts: 1,
            outcome: "partially_recovered".to_string(),
        });
    }
    let mut recovered = 0;
    let mut skipped = false;
    for gap in &output.gaps {
        let place = format!(
            "{}–{}",
            crate::output_format::clock_ms(gap.start_ms),
            crate::output_format::clock_ms(gap.end_ms)
        );
        let (reason, outcome) = match &gap.filled_by {
            Some(engine) => {
                recovered += 1;
                (
                    format!("{model_name} got stuck at {place} ({}); {engine} transcribed this part.", gap.reason),
                    "recovered",
                )
            }
            None => {
                skipped = true;
                (
                    format!("{model_name} got stuck at {place} ({}); this part could not be transcribed.", gap.reason),
                    "skipped",
                )
            }
        };
        warnings.push(TranscriptWarning {
            start_ms: gap.start_ms,
            end_ms: gap.end_ms,
            reason,
            attempts: 3,
            outcome: outcome.to_string(),
        });
    }
    let unclear: Vec<(i64, i64)> = segments
        .iter()
        .filter(|segment| native_sounds::is_unclear_mark(&segment.text))
        .map(|segment| (segment.start_ms, segment.end_ms))
        .filter(|&(start_ms, end_ms)| !in_gap(start_ms, end_ms))
        .collect();
    if let Some(warning) = native_sounds::unclear_warning(&unclear) {
        skipped = true;
        warnings.push(warning);
    }
    let partial = incomplete || output.truncated || skipped;

    TranscriptResult {
        job_id: Uuid::new_v4().to_string(),
        model_name: model_name.to_string(),
        full_text: plain_text.clone(),
        plain_text,
        timestamped_text,
        detected_languages,
        segments,
        quality_status: if partial {
            TranscriptQualityStatus::Partial
        } else if recovered > 0 {
            TranscriptQualityStatus::Recovered
        } else {
            TranscriptQualityStatus::Clean
        },
        recovered_region_count: recovered,
        warnings,
        diarization_status: if incomplete { DiarizationStatus::Failed } else { DiarizationStatus::Completed },
        diarization_model_id: Some(model_id.to_string()),
        diarization_source: DiarizationSource::NativeModel,
        diarization_warning: incomplete.then(|| "The model returned recoverable text, but some native speaker or timestamp data was incomplete.".to_string()),
        diarization_policy_version: None,
        diarization_clustering_threshold: None,
        diarization_speaker_count_hint: None,
        speakers,
        diarization_turns: turns,
    }
}

#[cfg(test)]
mod tests {
    /// Transcribes a real long recording with the installed VibeVoice model:
    /// `MURMELA_LONG_FILE=/path/to/audio.m4a cargo test --release --lib long_recording -- --ignored --nocapture`.
    /// Writes the transcript to `MURMELA_LONG_OUT` (default: the temp folder).
    /// `MURMELA_LONG_DICTIONARY=1` passes the app's dictionary as the app does.
    #[test]
    #[ignore]
    fn long_recording_is_transcribed_in_chunks() {
        let file = std::env::var("MURMELA_LONG_FILE").expect("set MURMELA_LONG_FILE");
        let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/com.murmela.app/models");
        let model = crate::model_downloads::discover_native_asr_models(&models_dir)
            .into_iter()
            .find(|model| model.id == super::model_metadata::VIBEVOICE_MODEL_ID)
            .expect("VibeVoice is not installed");
        let dictionary = std::env::var_os("MURMELA_LONG_DICTIONARY").and_then(|_| {
            crate::vocabulary::build_asr_prompt_from_db_path(&models_dir.parent().unwrap().join("speech_to_text.sqlite")).unwrap()
        });
        let request = crate::asr::FileTranscriptionRequest {
            use_context: None,
            profile: crate::settings::ModelProfile::Accurate,
            selected_model_id: Some(model.id.clone()),
            language_mode: crate::settings::LanguageMode::Auto,
            fixed_language: None,
            timestamps: true,
            prefer_gpu: true,
            file_path: file.clone(),
            context_prompt: dictionary.as_ref().map(|prompt| prompt.text.clone()),
            context_terms: dictionary.map(|prompt| prompt.terms).unwrap_or_default(),
        };
        let started = std::time::Instant::now();
        let result = super::transcribe_with_native_worker(&model, &request, None, &models_dir).unwrap();
        eprintln!("done in {:?}; {} speakers; {} segments", started.elapsed(), result.speakers.len(), result.segments.len());
        let out = std::env::var("MURMELA_LONG_OUT")
            .unwrap_or_else(|_| std::env::temp_dir().join("murmela-long.json").display().to_string());
        std::fs::write(out, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
    }

    /// Fills a stretch of a real recording with the installed Whisper model:
    /// `MURMELA_GAP_AUDIO=/path/to/audio.wav cargo test --release --lib gap_is_filled -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn gap_is_filled_by_whisper_on_a_real_recording() {
        let file = std::env::var("MURMELA_GAP_AUDIO").expect("set MURMELA_GAP_AUDIO");
        let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/com.murmela.app/models");
        let audio = crate::audio_preprocess::decode_audio_file(Path::new(&file)).unwrap();
        let request = crate::asr::FileTranscriptionRequest {
            use_context: Some(crate::model_metadata::ModelUseContext::FileTranscription),
            profile: crate::settings::ModelProfile::Accurate,
            selected_model_id: None,
            language_mode: crate::settings::LanguageMode::Auto,
            fixed_language: None,
            timestamps: true,
            prefer_gpu: true,
            file_path: file.clone(),
            context_prompt: None,
            context_terms: Vec::new(),
        };
        let mut output = NativeWorkerResult {
            text: String::new(),
            segments: Vec::new(),
            truncated: false,
            warning: None,
            gaps: vec![NativeGap { start_ms: 60_000, end_ms: 75_000, reason: "repeated a word".into(), filled_by: None }],
        };
        let log = EngineLog(None);
        let started = std::time::Instant::now();
        GapFallback::new(&models_dir, &request)
            .fill(&mut output, &audio, &native_chunks::ChunkVoices::new(), None, &log)
            .unwrap();
        eprintln!("filled in {:?}: {:#?}", started.elapsed(), output.segments);
        assert_eq!(output.gaps[0].filled_by.as_deref(), Some("Whisper"));
        assert!(output.segments.iter().all(|segment| segment.start_ms >= 60_000 && segment.end_ms <= 75_000));
    }

    /// Why unclear speech is marked, not filled in: prints what Whisper makes of
    /// a finished transcript's unclear stretches next to what a cleaner
    /// recording of the same moments says (a call's other channel). On a
    /// 58-minute call (2026-10-30) all 66 were confident inventions
    /// ("Gracias." at 0.99 where a density meter was being explained).
    /// `MURMELA_GAP_AUDIO=… MURMELA_REFERENCE_AUDIO=… MURMELA_UNCLEAR_TRANSCRIPT=…json cargo test --release --lib unclear_speech_is_left -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn unclear_speech_is_left_to_the_listener() {
        let file = std::env::var("MURMELA_GAP_AUDIO").expect("set MURMELA_GAP_AUDIO");
        let transcript: serde_json::Value = serde_json::from_slice(
            &std::fs::read(std::env::var("MURMELA_UNCLEAR_TRANSCRIPT").expect("set MURMELA_UNCLEAR_TRANSCRIPT")).unwrap(),
        )
        .unwrap();
        let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/com.murmela.app/models");
        let audio = crate::audio_preprocess::decode_audio_file(Path::new(&file)).unwrap();
        let request = crate::asr::FileTranscriptionRequest {
            use_context: Some(crate::model_metadata::ModelUseContext::FileTranscription),
            profile: crate::settings::ModelProfile::Accurate,
            selected_model_id: None,
            language_mode: crate::settings::LanguageMode::Auto,
            fixed_language: None,
            timestamps: true,
            prefer_gpu: true,
            file_path: file.clone(),
            context_prompt: None,
            context_terms: Vec::new(),
        };
        let whisper = crate::asr::WhisperFallback::load(&models_dir, true).unwrap();
        let reference = std::env::var("MURMELA_REFERENCE_AUDIO")
            .ok()
            .map(|path| crate::audio_preprocess::decode_audio_file(Path::new(&path)).unwrap());
        let rate = audio.sample_rate_hz as i64;
        for segment in transcript["segments"].as_array().unwrap() {
            if native_sounds::kind(segment["text"].as_str().unwrap()) != native_sounds::Kind::Unclear {
                continue;
            }
            let (start, end) = (segment["startMs"].as_i64().unwrap(), segment["endMs"].as_i64().unwrap());
            let piece = crate::audio_preprocess::PreparedAudio {
                sample_rate_hz: audio.sample_rate_hz,
                channels: audio.channels,
                samples: audio.samples[(start * rate / 1_000) as usize..((end * rate / 1_000) as usize).min(audio.samples.len())].to_vec(),
            };
            for heard in whisper.transcribe(&piece, &request).unwrap() {
                eprintln!("{:>7}–{:<7} {:.2} {} {}", start, end, heard.confidence.unwrap_or(0.0), heard.language_code, heard.text);
            }
            // The same stretch on a cleaner recording (a call's other channel) shows what was really said.
            if let Some(reference) = &reference {
                let piece = crate::audio_preprocess::PreparedAudio {
                    sample_rate_hz: reference.sample_rate_hz,
                    channels: reference.channels,
                    samples: reference.samples[(start * rate / 1_000) as usize..((end * rate / 1_000) as usize).min(reference.samples.len())].to_vec(),
                };
                let said: Vec<String> = whisper.transcribe(&piece, &request).unwrap().into_iter().map(|s| s.text).collect();
                eprintln!("{:>7}–{:<7} REF {}", start, end, said.join(" "));
            }
        }
    }

    /// Tidies a finished VibeVoice transcript of a call's microphone channel
    /// and merges it with the other channel (transcribed by Whisper), as a call
    /// is: `MURMELA_GAP_AUDIO=mic.wav MURMELA_REFERENCE_AUDIO=call-audio.wav
    /// MURMELA_UNCLEAR_TRANSCRIPT=…json cargo test --release --lib tidied_call -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn tidied_call_on_a_real_recording() {
        let audio = crate::audio_preprocess::decode_audio_file(Path::new(&std::env::var("MURMELA_GAP_AUDIO").unwrap())).unwrap();
        let other = std::env::var("MURMELA_REFERENCE_AUDIO").unwrap();
        let transcript: TranscriptResult = serde_json::from_slice(
            &std::fs::read(std::env::var("MURMELA_UNCLEAR_TRANSCRIPT").unwrap()).unwrap(),
        )
        .unwrap();
        let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/com.murmela.app/models");
        let duration_ms = audio.samples.len() as i64 * 1_000 / audio.sample_rate_hz as i64;
        let mut output = NativeWorkerResult {
            text: String::new(),
            segments: transcript
                .segments
                .iter()
                .map(|segment| NativeOutputSegment {
                    start_ms: segment.start_ms,
                    end_ms: segment.end_ms,
                    text: segment.text.clone(),
                    speaker: segment.speaker_id.clone(),
                    language_code: None,
                })
                .collect(),
            truncated: false,
            warning: None,
            gaps: Vec::new(),
        };
        let before = output.segments.len();
        tidy_sounds(&mut output, &audio, &models_dir, &EngineLog(None));
        let local = normalize_native_result("vibevoice", "VibeVoice", duration_ms, output);
        let marks = |result: &TranscriptResult| result.segments.iter().filter(|s| native_sounds::is_unclear_mark(&s.text)).count();
        eprintln!(
            "microphone: {before} segments → {} ({} unclear marks); quality {:?}, speakers {:?}; warnings {:?}",
            local.segments.len(),
            marks(&local),
            local.quality_status,
            local.diarization_status,
            local.warnings.iter().map(|w| &w.reason).collect::<Vec<_>>()
        );
        let request = crate::asr::FileTranscriptionRequest {
            use_context: Some(crate::model_metadata::ModelUseContext::FileTranscription),
            profile: crate::settings::ModelProfile::Accurate,
            selected_model_id: None,
            language_mode: crate::settings::LanguageMode::Auto,
            fixed_language: None,
            timestamps: true,
            prefer_gpu: true,
            file_path: other.clone(),
            context_prompt: None,
            context_terms: Vec::new(),
        };
        let other_audio = crate::audio_preprocess::decode_audio_file(Path::new(&other)).unwrap();
        let mut remote = transcript.clone();
        remote.segments = crate::asr::WhisperFallback::load(&models_dir, true).unwrap().transcribe(&other_audio, &request).unwrap();
        remote.warnings = Vec::new();
        let merged = crate::meeting::speakers::assemble(crate::meeting::speakers::AssembleInput {
            job_id: "call".into(),
            model_name: "VibeVoice".into(),
            local: Some(local),
            remote: Some(remote),
            remote_turns: None,
            spans: Vec::new(),
            local_name: None,
            separate_remote_speakers: false,
        });
        eprintln!(
            "call: {} segments, {} unclear marks; quality {:?}; warnings {:?}",
            merged.segments.len(),
            marks(&merged),
            merged.quality_status,
            merged.warnings.iter().map(|w| &w.reason).collect::<Vec<_>>()
        );
        for segment in merged.segments.iter().filter(|s| native_sounds::is_unclear_mark(&s.text)) {
            eprintln!("  left: {}", segment.text);
        }
    }

    #[test]
    fn metal_out_of_memory_is_reported_as_out_of_memory() {
        use std::os::unix::process::ExitStatusExt;
        assert!(super::is_out_of_memory(
            "[METAL] Command buffer execution failed: Insufficient Memory (00000008:kIOGPUCommandBufferCallbackErrorOutOfMemory)."
        ));
        assert!(!super::is_out_of_memory("No module named mlx_audio"));
        let killed = std::process::ExitStatus::from_raw(9);
        assert!(super::exit_without_result("VibeVoice", &killed, "")
            .to_string()
            .starts_with("MODEL_OUT_OF_MEMORY:"));
        let crashed = std::process::ExitStatus::from_raw(1 << 8);
        let message = super::exit_without_result("VibeVoice", &crashed, "Traceback\nValueError: bad audio\n").to_string();
        assert!(message.starts_with("MODEL_WORKER_FAILED:"));
        assert!(message.ends_with("ValueError: bad audio"));
    }

    use super::*;

    #[test]
    fn packaged_vibevoice_worker_is_found_in_app_resources() {
        let layout = worker_layout(model_metadata::VIBEVOICE_MODEL_ID).unwrap();
        let candidates = executable_candidates(
            Some(Path::new("/Applications/Murmela.app/Contents/MacOS")),
            Path::new("/source/src-tauri"),
            &layout,
        );
        assert!(candidates.contains(&PathBuf::from(
            "/Applications/Murmela.app/Contents/MacOS/../Resources/workers/vibevoice/murmela-vibevoice-worker/murmela-vibevoice-worker"
        )));
        // A bare script is never a candidate: it would run under the system
        // python3, which lacks the worker's dependencies.
        assert!(candidates
            .iter()
            .all(|path| path.extension().and_then(|value| value.to_str()) != Some("py")));
    }

    #[test]
    fn sound_tags_are_dropped_and_unclear_speech_marked_once() {
        let segment = |start_ms, end_ms, speaker: Option<&str>, text: &str| NativeOutputSegment {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: speaker.map(str::to_string),
            language_code: None,
        };
        let mut output = NativeWorkerResult {
            text: String::new(),
            segments: vec![
                segment(0, 5_000, Some("0"), "Hola, ¿cómo estás?"),
                segment(5_000, 9_000, None, "[Environmental Sounds]"),
                // Two tags with nothing said between are one stretch.
                segment(9_000, 14_000, None, "[Unintelligible Speech]"),
                segment(14_500, 18_000, None, "[Unintelligible Speech]"),
                segment(18_000, 21_000, Some("1"), "Bien. [Laughter] ¿Y vos?"),
                // Too short to mark.
                segment(21_000, 21_800, None, "[Unintelligible Speech]"),
                segment(22_000, 23_000, None, "[Music]"),
            ],
            truncated: false,
            warning: None,
            gaps: Vec::new(),
        };
        let audio = audio_preprocess::PreparedAudio { sample_rate_hz: 16_000, channels: 1, samples: vec![0.0; 16_000 * 25] };
        let models_dir = std::env::temp_dir().join("murmela-no-models");
        tidy_sounds(&mut output, &audio, &models_dir, &EngineLog(None));
        let texts: Vec<_> = output.segments.iter().map(|segment| segment.text.as_str()).collect();
        assert_eq!(texts, vec!["Hola, ¿cómo estás?", "[Unclear audio 00:09–00:18]", "Bien. [Laughter] ¿Y vos?"]);

        // Nine unclear seconds are marked in the text, not flagged.
        let result = normalize_native_result("native", "VibeVoice", 25_000, output);
        assert_eq!(result.diarization_status, DiarizationStatus::Completed);
        assert_eq!(result.quality_status, TranscriptQualityStatus::Clean);
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn gaps_become_warnings_without_breaking_speakers() {
        let segment = |start_ms, end_ms, speaker: Option<&str>, text: &str| NativeOutputSegment {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: speaker.map(str::to_string),
            language_code: Some("es".into()),
        };
        let gap = |start_ms, end_ms, filled_by: Option<&str>| NativeGap {
            start_ms,
            end_ms,
            reason: "repeated a word".into(),
            filled_by: filled_by.map(str::to_string),
        };
        let output = |gaps| NativeWorkerResult {
            text: String::new(),
            segments: vec![
                segment(0, 10_000, Some("0"), "Hola."),
                segment(10_000, 20_000, None, "Filled by Whisper."),
                segment(20_000, 30_000, Some("1"), "Chau."),
            ],
            truncated: false,
            warning: None,
            gaps,
        };

        let filled = normalize_native_result("native", "VibeVoice", 30_000, output(vec![gap(10_000, 20_000, Some("Whisper"))]));
        assert_eq!(filled.quality_status, TranscriptQualityStatus::Recovered);
        assert_eq!(filled.recovered_region_count, 1);
        assert_eq!(filled.diarization_status, DiarizationStatus::Completed);
        assert_eq!(filled.warnings[0].outcome, "recovered");
        assert_eq!(
            filled.warnings[0].reason,
            "VibeVoice got stuck at 00:10–00:20 (repeated a word); Whisper transcribed this part."
        );

        let skipped = normalize_native_result("native", "VibeVoice", 30_000, output(vec![gap(10_000, 20_000, None)]));
        assert_eq!(skipped.quality_status, TranscriptQualityStatus::Partial);
        assert_eq!(skipped.warnings[0].outcome, "skipped");

        // Without a gap, text without a speaker still means incomplete speakers.
        let plain = normalize_native_result("native", "VibeVoice", 30_000, output(Vec::new()));
        assert_eq!(plain.diarization_status, DiarizationStatus::Failed);
    }

    #[test]
    fn normalizes_speakers_by_first_appearance_and_clamps_timestamps() {
        let result = normalize_native_result(
            "native",
            "Native",
            2_000,
            NativeWorkerResult {
                text: String::new(),
                segments: vec![
                    NativeOutputSegment {
                        start_ms: -10,
                        end_ms: 1_000,
                        text: "Hello".into(),
                        speaker: Some("S09".into()),
                        language_code: Some("en".into()),
                    },
                    NativeOutputSegment {
                        start_ms: 1_000,
                        end_ms: 9_000,
                        text: "世界".into(),
                        speaker: Some("S02".into()),
                        language_code: Some("zh".into()),
                    },
                    NativeOutputSegment {
                        start_ms: 1_500,
                        end_ms: 1_900,
                        text: "Again".into(),
                        speaker: Some("S09".into()),
                        language_code: None,
                    },
                ],
                truncated: false,
                warning: None,
                gaps: Vec::new(),
            },
        );
        assert_eq!(result.segments[0].speaker_id.as_deref(), Some("speaker_0"));
        assert_eq!(result.segments[1].speaker_id.as_deref(), Some("speaker_1"));
        assert_eq!(result.segments[2].speaker_id.as_deref(), Some("speaker_0"));
        assert_eq!(result.segments[0].start_ms, 0);
        assert_eq!(result.segments[1].end_ms, 2_000);
        assert_eq!(result.detected_languages, ["en", "zh"]);
        assert_eq!(result.diarization_source, DiarizationSource::NativeModel);
    }

    #[test]
    fn preserves_fallback_text_and_marks_incomplete_structure_partial() {
        let result = normalize_native_result(
            "native",
            "Native",
            1_000,
            NativeWorkerResult {
                text: "Recoverable text".into(),
                segments: vec![],
                truncated: true,
                warning: None,
                gaps: Vec::new(),
            },
        );
        assert_eq!(result.plain_text, "Recoverable text");
        assert_eq!(result.quality_status, TranscriptQualityStatus::Partial);
        assert_eq!(result.diarization_status, DiarizationStatus::Failed);
    }

    #[test]
    fn parses_typed_error_and_cancellation_records() {
        let error = parse_worker_record(
            r#"{"type":"error","code":"MODEL_OUT_OF_MEMORY","message":"no memory"}"#,
        )
        .expect("error record");
        assert!(
            matches!(error, NativeWorkerRecord::Error { code, .. } if code == "MODEL_OUT_OF_MEMORY")
        );
        assert!(matches!(
            parse_worker_record(r#"{"type":"canceled"}"#).expect("cancel record"),
            NativeWorkerRecord::Canceled
        ));
    }
}
