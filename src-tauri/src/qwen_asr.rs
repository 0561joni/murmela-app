use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::asr::{
    build_transcript_result, detect_vad_splits, FileTranscriptionRequest, InstalledModel,
    TranscriptResult, TranscriptSegment, TranscriptWarning,
};
use crate::audio_chunks::{
    plan_audio_chunks, plan_audio_chunks_with_splits, split_chunk_near_middle, AudioChunk,
};
use crate::audio_preprocess::PreparedAudio;
use crate::settings::{LanguageMode, ModelProfile};
use crate::transcript_stitching::{align_segments_to_sentences, stitch_segments};
use crate::transcription_policy::{MAX_CHUNK_MS, MIN_SPLIT_RETRY_MS};
use crate::transcription_quality::{normalize_text, repetition_reason};

pub const QWEN_MODEL_ID: &str = "qwen3-asr-1.7b-bf16";
pub const QWEN_MODEL_NAME: &str = "Qwen3-ASR-1.7B";
pub const QWEN_MODEL_DIR: &str = "qwen3-asr-1.7b-bf16";
pub const QWEN_MODEL_REVISION: &str = "b188e100bd85038c06d2812d24a39776eba774ca";
pub const QWEN_COMPLETE_FILE: &str = ".blabber-model.json";
pub const QWEN_TOTAL_SIZE: i64 = 4_703_041_355;

pub const QWEN_REQUIRED_ARTIFACTS: &[(&str, i64, &str)] = &[
    (
        "config.json",
        6_194,
        "2e74a751548b8ad7d7526d29365ad8144c345d8b412b1152d25dc6698452712f",
    ),
    (
        "generation_config.json",
        142,
        "1da527824d81e07118facff437e03f2e24a23311e3bdeb2368973fe77e5f275c",
    ),
    (
        "model.safetensors.index.json",
        64_821,
        "f994739fe38e5210b9e3e8ce6c6307315e2ceac3cb630e7b7414d69dce520f60",
    ),
    (
        "model-00001-of-00002.safetensors",
        4_220_320_824,
        "a4cd1f1a04d90b757dc7f7dd26254e69a013b19e80efe590a83c6a3bde8608d6",
    ),
    (
        "model-00002-of-00002.safetensors",
        478_200_688,
        "6e0b9d9e09e2e0238e7ef3cc8a484ab387e91b90f1900bedf88bc92d7929ccfc",
    ),
    (
        "vocab.json",
        2_776_833,
        "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    ),
    (
        "merges.txt",
        1_671_853,
        "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
    ),
];

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletionManifest {
    model_id: String,
    revision: String,
    artifacts: Vec<CompletedArtifact>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompletedArtifact {
    path: String,
    size_bytes: i64,
    sha256: String,
}

pub fn discover_model(models_dir: &Path) -> Result<Option<InstalledModel>> {
    let model_dir = models_dir.join(QWEN_MODEL_DIR);
    let manifest_path = model_dir.join(QWEN_COMPLETE_FILE);
    if !manifest_path.is_file() {
        return Ok(None);
    }
    let manifest: CompletionManifest = match File::open(&manifest_path)
        .ok()
        .and_then(|file| serde_json::from_reader(file).ok())
    {
        Some(manifest) => manifest,
        None => return Ok(None),
    };
    if manifest.model_id != QWEN_MODEL_ID || manifest.revision != QWEN_MODEL_REVISION {
        return Ok(None);
    }

    if !matches!(
        validate_artifacts(&model_dir, &manifest, QWEN_REQUIRED_ARTIFACTS),
        Ok(true)
    ) {
        return Ok(None);
    }

    Ok(Some(InstalledModel {
        id: QWEN_MODEL_ID.to_string(),
        engine: "qwen3_asr_c".to_string(),
        model_name: QWEN_MODEL_NAME.to_string(),
        variant: "1.7B BF16".to_string(),
        local_path: model_dir.display().to_string(),
        size_bytes: QWEN_TOTAL_SIZE,
        is_default: false,
        profile: ModelProfile::Accurate,
        capabilities: crate::model_metadata::ModelCapabilities::standard_asr(),
    }))
}

fn validate_artifacts(
    model_dir: &Path,
    manifest: &CompletionManifest,
    required: &[(&str, i64, &str)],
) -> Result<bool> {
    for (path, expected_size, expected_sha256) in required {
        let Some(record) = manifest
            .artifacts
            .iter()
            .find(|artifact| artifact.path == *path)
        else {
            return Ok(false);
        };
        if record.size_bytes != *expected_size || record.sha256 != *expected_sha256 {
            return Ok(false);
        }
        let artifact_path = model_dir.join(path);
        if !verify_artifact(&artifact_path, *expected_size, expected_sha256)? {
            return Ok(false);
        }
    }
    Ok(true)
}

fn verify_artifact(path: &Path, expected_size: i64, expected_sha256: &str) -> Result<bool> {
    if path.metadata().map(|value| value.len() as i64).ok() != Some(expected_size) {
        return Ok(false);
    }
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()) == expected_sha256)
}

#[derive(Debug)]
pub struct QwenAsrEngine {
    models_dir: PathBuf,
    cache: Mutex<Option<CachedContext>>,
}

impl QwenAsrEngine {
    pub fn new(models_dir: PathBuf) -> Self {
        Self {
            models_dir,
            cache: Mutex::new(None),
        }
    }

    pub fn invalidate_context_cache(&self) {
        if let Ok(mut cache) = self.cache.lock() {
            *cache = None;
        }
    }

    /// Load the model into the cache without decoding (dictation warm-up).
    pub fn preload(&self, model: &InstalledModel) -> Result<()> {
        let _run_guard = QwenRunGuard::acquire(&self.models_dir)?;
        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow!("Qwen model context cache is unavailable"))?;
        load_into_cache(&mut cache, model)
    }

    pub fn transcribe(
        &self,
        model: &InstalledModel,
        prepared: &PreparedAudio,
        request: &FileTranscriptionRequest,
        progress: Option<Arc<AtomicI32>>,
        vad_model_path: Option<&Path>,
    ) -> Result<TranscriptResult> {
        let transcription_started = Instant::now();
        if prepared.samples.is_empty() {
            return Err(anyhow!(
                "TRANSCRIPTION_EMPTY: prepared audio had no samples"
            ));
        }

        let _run_guard = QwenRunGuard::acquire(&self.models_dir)?;
        let forced_language = match request.language_mode {
            LanguageMode::Auto => None,
            LanguageMode::Fixed => {
                let requested = request.fixed_language.as_deref().ok_or_else(|| {
                    anyhow!(
                        "MODEL_UNSUPPORTED_LANGUAGE: choose a fixed language before transcribing"
                    )
                })?;
                Some(qwen_language_name(requested).ok_or_else(|| {
                    anyhow!(
                        "MODEL_UNSUPPORTED_LANGUAGE: Qwen3-ASR does not support fixed language '{requested}'"
                    )
                })?)
            }
        };

        let duration_ms = samples_to_ms(prepared.samples.len(), prepared.sample_rate_hz);
        let direct_limit = if request.timestamps {
            MAX_CHUNK_MS
        } else {
            60_000
        };
        let chunks = if duration_ms <= direct_limit {
            vec![AudioChunk {
                start_sample: 0,
                end_sample: prepared.samples.len(),
            }]
        } else {
            let preferred_splits = vad_model_path
                .and_then(|path| detect_vad_splits(path, prepared).ok())
                .unwrap_or_default();
            if preferred_splits.is_empty() {
                plan_audio_chunks(&prepared.samples, prepared.sample_rate_hz)
            } else {
                plan_audio_chunks_with_splits(
                    &prepared.samples,
                    prepared.sample_rate_hz,
                    &preferred_splits,
                )
            }
        };

        let mut cache = self
            .cache
            .lock()
            .map_err(|_| anyhow!("Qwen model context cache is unavailable"))?;
        load_into_cache(&mut cache, model)?;
        let context = &mut cache.as_mut().expect("Qwen cache populated").context;
        context.set_forced_language(forced_language)?;
        // The runtime caches the encoded prompt and reuses it for each
        // app-managed audio chunk. Recovery may temporarily clear it.
        context.set_prompt(request.context_prompt.as_deref())?;

        let total_samples = prepared.samples.len().max(1) as f32;
        let mut segments = Vec::new();
        let mut warnings = Vec::new();
        for chunk in chunks {
            crate::shutdown::ensure_running()?;
            let start_percent = (chunk.start_sample as f32 / total_samples * 100.0).floor() as i32;
            let end_percent = (chunk.end_sample as f32 / total_samples * 100.0).ceil() as i32;
            if let Some(progress) = &progress {
                progress.fetch_max(start_percent, Ordering::Relaxed);
            }
            let recovery = decode_with_recovery(
                context,
                prepared,
                chunk,
                request.context_prompt.as_deref(),
                &request.context_terms,
                progress.as_ref(),
                end_percent,
                true,
            );
            segments.extend(recovery.segments);
            warnings.extend(recovery.warnings);
            if let Some(progress) = &progress {
                progress.fetch_max(end_percent, Ordering::Relaxed);
            }
        }

        if let Some(progress) = &progress {
            progress.store(100, Ordering::Relaxed);
        }
        crate::shutdown::ensure_running()?;
        // Chunks are cut at quiet points, not sentence ends; realign so a
        // sentence is never split across two paragraphs.
        let segments = align_segments_to_sentences(stitch_segments(segments));
        if segments.is_empty() {
            return Err(anyhow!("TRANSCRIPTION_EMPTY: Qwen3-ASR produced no text"));
        }
        let result = build_transcript_result(Uuid::new_v4().to_string(), model, segments, warnings);
        let wall_ms = transcription_started.elapsed().as_millis().max(1) as f64;
        let realtime_factor = duration_ms.max(1) as f64 / wall_ms;
        eprintln!(
            "[qwen] completed locally: audio_ms={} wall_ms={} realtime_factor={:.2} prompt_enabled={} prompt_terms={} recoveries={} peak_memory_bytes={}",
            duration_ms,
            wall_ms as u128,
            realtime_factor,
            request.context_prompt.is_some(),
            request.context_terms.len(),
            result.recovered_region_count,
            peak_memory_bytes()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unavailable".to_string())
        );
        Ok(result)
    }
}

#[derive(Debug)]
struct CachedContext {
    model_path: String,
    context: NativeContext,
}

fn load_into_cache(cache: &mut Option<CachedContext>, model: &InstalledModel) -> Result<()> {
    let needs_reload = cache
        .as_ref()
        .map(|cached| cached.model_path != model.local_path)
        .unwrap_or(true);
    if needs_reload {
        *cache = None;
        let load_started = Instant::now();
        let context = NativeContext::load(Path::new(&model.local_path)).with_context(|| {
            "MODEL_LOAD_FAILED: Qwen3-ASR needs roughly 7 GB of working memory; try Turbo if this device cannot load it"
        })?;
        *cache = Some(CachedContext {
            model_path: model.local_path.clone(),
            context,
        });
        eprintln!(
            "[qwen] model loaded locally in {} ms",
            load_started.elapsed().as_millis()
        );
    }
    Ok(())
}

struct QwenRunGuard {
    file: File,
}

impl QwenRunGuard {
    fn acquire(models_dir: &Path) -> Result<Self> {
        fs::create_dir_all(models_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(models_dir.join(".qwen3-asr.lock"))?;
        file.try_lock_exclusive().map_err(|_| {
            anyhow!("MODEL_BUSY: another Qwen3-ASR transcription is already running")
        })?;
        Ok(Self { file })
    }
}

impl Drop for QwenRunGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct RecoveryOutput {
    segments: Vec<TranscriptSegment>,
    warnings: Vec<TranscriptWarning>,
}

#[allow(clippy::too_many_arguments)]
fn decode_with_recovery(
    context: &mut NativeContext,
    prepared: &PreparedAudio,
    chunk: AudioChunk,
    prompt: Option<&str>,
    prompt_terms: &[String],
    progress: Option<&Arc<AtomicI32>>,
    progress_ceiling: i32,
    allow_split: bool,
) -> RecoveryOutput {
    if crate::shutdown::is_shutting_down() {
        return RecoveryOutput {
            segments: Vec::new(),
            warnings: Vec::new(),
        };
    }
    let start_ms = chunk.start_ms(prepared.sample_rate_hz);
    let end_ms = chunk.end_ms(prepared.sample_rate_hz);
    let samples = &prepared.samples[chunk.start_sample..chunk.end_sample];
    let initial = context.transcribe(samples, progress, progress_ceiling);
    let initial_reason = initial
        .as_ref()
        .ok()
        .and_then(|output| invalid_output_reason(&output.text, samples, prompt_terms));
    if let Ok(output) = &initial {
        if initial_reason.is_none() {
            return RecoveryOutput {
                segments: output_to_segments(output, start_ms, end_ms),
                warnings: Vec::new(),
            };
        }
    }

    let reason = initial_reason.clone().unwrap_or_else(|| match &initial {
        Err(error) => clean_error(error),
        Ok(_) => "Qwen output failed validation".to_string(),
    });
    let prompt_leakage_detected = initial_reason
        .as_deref()
        .map(|value| value.starts_with("dictionary prompt leakage"))
        .unwrap_or(false);
    let mut prompt_cleared = false;
    if let Some(prompt) = prompt {
        prompt_cleared = context.set_prompt(None).is_ok();
        if prompt_cleared {
            let retry = context.transcribe(samples, progress, progress_ceiling);
            if let Ok(retry) = &retry {
                if invalid_output_reason(&retry.text, samples, &[]).is_none() {
                    let _ = context.set_prompt(Some(prompt));
                    return RecoveryOutput {
                        segments: output_to_segments(retry, start_ms, end_ms),
                        warnings: vec![warning(start_ms, end_ms, reason, 2, "recovered")],
                    };
                }
            }
        }
    }

    if prompt_leakage_detected {
        if let Ok(initial) = &initial {
            if let Some(trimmed_text) = trim_prompt_leakage_suffix(&initial.text, prompt_terms) {
                let trimmed = NativeOutput {
                    text: trimmed_text,
                    language: initial.language.clone(),
                };
                if invalid_output_reason(&trimmed.text, samples, &[]).is_none() {
                    if prompt_cleared {
                        let _ = context.set_prompt(prompt);
                    }
                    return RecoveryOutput {
                        segments: output_to_segments(&trimmed, start_ms, end_ms),
                        warnings: vec![warning(
                            start_ms,
                            end_ms,
                            reason,
                            if prompt_cleared { 2 } else { 1 },
                            "prompt_leakage_trimmed",
                        )],
                    };
                }
            }
        }
    }

    if allow_split && chunk.duration_ms(prepared.sample_rate_hz) >= MIN_SPLIT_RETRY_MS {
        if let Some((left, right)) =
            split_chunk_near_middle(&prepared.samples, chunk, prepared.sample_rate_hz)
        {
            let mut combined = RecoveryOutput {
                segments: Vec::new(),
                warnings: vec![warning(start_ms, end_ms, reason, 3, "split_recovery")],
            };
            for piece in [left, right] {
                // If clearing the native prompt failed, keep validating child
                // chunks against the dictionary terms instead of treating a
                // still-prompted decode as clean.
                let (piece_prompt, piece_prompt_terms) = if prompt_cleared {
                    (None, &[][..])
                } else {
                    (prompt, prompt_terms)
                };
                let recovered = decode_with_recovery(
                    context,
                    prepared,
                    piece,
                    piece_prompt,
                    piece_prompt_terms,
                    progress,
                    progress_ceiling,
                    false,
                );
                combined.segments.extend(recovered.segments);
                combined.warnings.extend(recovered.warnings);
            }
            if prompt_cleared {
                let _ = context.set_prompt(prompt);
            }
            return combined;
        }
    }

    if prompt_cleared {
        let _ = context.set_prompt(prompt);
    }
    RecoveryOutput {
        segments: vec![gap_segment(start_ms, end_ms)],
        warnings: vec![warning(start_ms, end_ms, reason, 3, "skipped")],
    }
}

fn invalid_output_reason(text: &str, samples: &[f32], prompt_terms: &[String]) -> Option<String> {
    if text.trim().is_empty() {
        let average_energy = samples
            .iter()
            .map(|sample| sample.abs() as f64)
            .sum::<f64>()
            / samples.len().max(1) as f64;
        return (average_energy >= 0.0015).then(|| "voiced chunk produced no text".to_string());
    }
    if let Some(reason) = repetition_reason([(0, 1, text)]) {
        return Some(reason);
    }
    prompt_leakage_reason(text, prompt_terms)
}

fn prompt_leakage_reason(text: &str, prompt_terms: &[String]) -> Option<String> {
    if prompt_terms.len() < 3 {
        return None;
    }
    if prompt_leakage_suffix_start(text, prompt_terms).is_some() {
        return Some("dictionary prompt leakage was appended to the decoded text".to_string());
    }
    let normalized_text = normalize_text(text);
    let transcript_words = normalized_text.split_whitespace().count();
    if transcript_words == 0 {
        return None;
    }
    let mut cursor = 0usize;
    let mut matched_terms = 0usize;
    let mut matched_words = 0usize;
    for term in prompt_terms {
        let normalized_term = normalize_text(term);
        if normalized_term.is_empty() {
            continue;
        }
        if let Some(relative) = normalized_text[cursor..].find(&normalized_term) {
            cursor += relative + normalized_term.len();
            matched_terms += 1;
            matched_words += normalized_term.split_whitespace().count();
        }
    }
    (matched_terms >= 3 && matched_words * 2 >= transcript_words)
        .then(|| "dictionary prompt leakage dominated the decoded text".to_string())
}

#[derive(Debug)]
struct TextWord {
    start: usize,
    normalized: String,
}

/// Remove a contiguous, prompt-ordered dictionary list at the end of decoded
/// text. This is only used after the broader leakage detector has fired and a
/// clean no-prompt retry failed, so ordinary mentions of vocabulary terms are
/// left to the retry result.
fn trim_prompt_leakage_suffix(text: &str, prompt_terms: &[String]) -> Option<String> {
    let suffix_start = prompt_leakage_suffix_start(text, prompt_terms)?;
    let prefix = text[..suffix_start]
        .trim_end()
        .trim_end_matches([',', ';', ':', '-', '–', '—'])
        .trim_end();
    (!prefix.is_empty()).then(|| prefix.to_string())
}

fn prompt_leakage_suffix_start(text: &str, prompt_terms: &[String]) -> Option<usize> {
    let words = text_words(text);
    if words.is_empty() {
        return None;
    }
    let term_patterns = prompt_terms
        .iter()
        .map(|term| {
            normalize_text(term)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    for suffix_start in 0..words.len() {
        let mut word_cursor = suffix_start;
        let mut term_cursor = 0usize;
        let mut matched_terms = 0usize;

        while word_cursor < words.len() {
            let Some((matched_index, pattern)) = term_patterns
                .iter()
                .enumerate()
                .skip(term_cursor)
                .find(|(_, pattern)| {
                    !pattern.is_empty()
                        && words[word_cursor..]
                            .iter()
                            .map(|word| word.normalized.as_str())
                            .take(pattern.len())
                            .eq(pattern.iter().map(String::as_str))
                })
            else {
                break;
            };

            word_cursor += pattern.len();
            term_cursor = matched_index + 1;
            matched_terms += 1;
        }

        if word_cursor == words.len() && matched_terms >= 3 {
            return Some(words[suffix_start].start);
        }
    }

    None
}

fn text_words(text: &str) -> Vec<TextWord> {
    let mut words = Vec::new();
    let mut word_start = None;

    for (index, character) in text.char_indices() {
        if character.is_alphanumeric() {
            word_start.get_or_insert(index);
        } else if let Some(start) = word_start.take() {
            let normalized = normalize_text(&text[start..index]);
            if !normalized.is_empty() {
                words.push(TextWord { start, normalized });
            }
        }
    }

    if let Some(start) = word_start {
        let normalized = normalize_text(&text[start..]);
        if !normalized.is_empty() {
            words.push(TextWord { start, normalized });
        }
    }

    words
}

fn output_to_segments(output: &NativeOutput, start_ms: i64, end_ms: i64) -> Vec<TranscriptSegment> {
    if output.text.trim().is_empty() {
        return Vec::new();
    }
    vec![TranscriptSegment {
        id: format!("qwen:{start_ms}"),
        start_ms,
        end_ms,
        text: output.text.trim().to_string(),
        language_code: output
            .language
            .as_deref()
            .and_then(qwen_language_code)
            .map(str::to_string)
            .unwrap_or_else(|| {
                crate::output_format::normalize_language_code(output.language.as_deref())
            }),
        segment_order: 0,
        confidence: None,
        speaker_id: None,
        speaker_ids: None,
        speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
        speaker_confidence: None,
    }]
}

fn gap_segment(start_ms: i64, end_ms: i64) -> TranscriptSegment {
    TranscriptSegment {
        id: format!("qwen:gap:{start_ms}"),
        start_ms,
        end_ms,
        text: format!(
            "[Unclear audio {}–{}]",
            crate::output_format::clock_ms(start_ms),
            crate::output_format::clock_ms(end_ms)
        ),
        language_code: "und".to_string(),
        segment_order: 0,
        confidence: None,
        speaker_id: None,
        speaker_ids: None,
        speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
        speaker_confidence: None,
    }
}

fn warning(
    start_ms: i64,
    end_ms: i64,
    reason: String,
    attempts: i32,
    outcome: &str,
) -> TranscriptWarning {
    TranscriptWarning {
        start_ms,
        end_ms,
        reason,
        attempts,
        outcome: outcome.to_string(),
    }
}

fn clean_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    message
        .strip_prefix("QWEN_INFERENCE_FAILED: ")
        .unwrap_or(&message)
        .to_string()
}

fn samples_to_ms(samples: usize, sample_rate_hz: u32) -> i64 {
    ((samples as u128 * 1000) / sample_rate_hz.max(1) as u128) as i64
}

/// Peak resident memory of this process (macOS reports `ru_maxrss` in bytes).
fn peak_memory_bytes() -> Option<u64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    let max_rss = unsafe { usage.assume_init() }.ru_maxrss;
    u64::try_from(max_rss).ok()
}

pub fn qwen_language_name(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    LANGUAGE_MAP
        .iter()
        .find(|(code, name)| normalized == *code || normalized == name.to_ascii_lowercase())
        .map(|(_, name)| *name)
}

fn qwen_language_code(value: &str) -> Option<&'static str> {
    let normalized = value.trim().to_ascii_lowercase();
    LANGUAGE_MAP
        .iter()
        .find(|(_, name)| normalized == name.to_ascii_lowercase())
        .map(|(code, _)| *code)
}

const LANGUAGE_MAP: &[(&str, &str)] = &[
    ("zh", "Chinese"),
    ("en", "English"),
    ("yue", "Cantonese"),
    ("ar", "Arabic"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish"),
    ("pt", "Portuguese"),
    ("id", "Indonesian"),
    ("it", "Italian"),
    ("ko", "Korean"),
    ("ru", "Russian"),
    ("th", "Thai"),
    ("vi", "Vietnamese"),
    ("ja", "Japanese"),
    ("tr", "Turkish"),
    ("hi", "Hindi"),
    ("ms", "Malay"),
    ("nl", "Dutch"),
    ("sv", "Swedish"),
    ("da", "Danish"),
    ("fi", "Finnish"),
    ("pl", "Polish"),
    ("cs", "Czech"),
    ("fil", "Filipino"),
    ("fa", "Persian"),
    ("el", "Greek"),
    ("ro", "Romanian"),
    ("hu", "Hungarian"),
    ("mk", "Macedonian"),
];

#[derive(Debug)]
struct NativeOutput {
    text: String,
    language: Option<String>,
}

#[derive(Debug)]
struct NativeContext {
    raw: *mut QwenContext,
}

unsafe impl Send for NativeContext {}

impl NativeContext {
    fn load(model_dir: &Path) -> Result<Self> {
        configure_native_threads();
        let path = CString::new(model_dir.to_string_lossy().as_bytes())?;
        let raw = unsafe { qwen_load(path.as_ptr()) };
        if raw.is_null() {
            return Err(anyhow!(
                "native Qwen runtime could not load the model directory"
            ));
        }
        Ok(Self { raw })
    }

    fn set_forced_language(&mut self, language: Option<&str>) -> Result<()> {
        let value = language.map(CString::new).transpose()?;
        let pointer = value
            .as_ref()
            .map(|value| value.as_ptr())
            .unwrap_or(ptr::null());
        if unsafe { qwen_set_force_language(self.raw, pointer) } != 0 {
            return Err(anyhow!(
                "MODEL_UNSUPPORTED_LANGUAGE: native runtime rejected the language"
            ));
        }
        Ok(())
    }

    fn set_prompt(&mut self, prompt: Option<&str>) -> Result<()> {
        let value = prompt.map(CString::new).transpose()?;
        let pointer = value
            .as_ref()
            .map(|value| value.as_ptr())
            .unwrap_or(ptr::null());
        if unsafe { qwen_set_prompt(self.raw, pointer) } != 0 {
            return Err(anyhow!(
                "QWEN_INFERENCE_FAILED: failed to encode the dictionary prompt"
            ));
        }
        Ok(())
    }

    fn transcribe(
        &mut self,
        samples: &[f32],
        progress: Option<&Arc<AtomicI32>>,
        progress_ceiling: i32,
    ) -> Result<NativeOutput> {
        crate::shutdown::ensure_running()?;
        if samples.len() > c_int::MAX as usize {
            return Err(anyhow!("QWEN_INFERENCE_FAILED: audio chunk is too large"));
        }

        let callback_state = progress.map(|progress| TokenProgress {
            progress: Arc::clone(progress),
            ceiling: progress_ceiling.saturating_sub(1),
            tokens: AtomicI32::new(0),
        });
        if let Some(state) = callback_state.as_ref() {
            unsafe {
                qwen_set_token_callback(
                    self.raw,
                    Some(token_progress_callback),
                    state as *const TokenProgress as *mut c_void,
                );
            }
        }

        let output =
            unsafe { qwen_transcribe_audio(self.raw, samples.as_ptr(), samples.len() as c_int) };
        unsafe { qwen_set_token_callback(self.raw, None, ptr::null_mut()) };
        if output.is_null() {
            return Err(anyhow!(
                "QWEN_INFERENCE_FAILED: native decoder returned no result"
            ));
        }
        let text = unsafe { CStr::from_ptr(output) }
            .to_string_lossy()
            .into_owned();
        unsafe { qwen_free_text(output) };
        let language_pointer = unsafe { qwen_last_detected_language(self.raw) };
        let language = (!language_pointer.is_null()).then(|| unsafe {
            CStr::from_ptr(language_pointer)
                .to_string_lossy()
                .into_owned()
        });
        Ok(NativeOutput { text, language })
    }
}

impl Drop for NativeContext {
    fn drop(&mut self) {
        unsafe { qwen_free(self.raw) };
    }
}

struct TokenProgress {
    progress: Arc<AtomicI32>,
    ceiling: i32,
    tokens: AtomicI32,
}

extern "C" fn token_progress_callback(_piece: *const c_char, userdata: *mut c_void) {
    if userdata.is_null() {
        return;
    }
    let state = unsafe { &*(userdata as *const TokenProgress) };
    let tokens = state.tokens.fetch_add(1, Ordering::Relaxed) + 1;
    if tokens % 8 == 0 {
        let _ = state
            .progress
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                Some(current.saturating_add(1).min(state.ceiling))
            });
    }
}

#[repr(C)]
struct QwenContext {
    _private: [u8; 0],
}

#[link(name = "qwen_asr", kind = "static")]
extern "C" {
    fn qwen_load(model_dir: *const c_char) -> *mut QwenContext;
    fn qwen_free(context: *mut QwenContext);
    fn qwen_set_token_callback(
        context: *mut QwenContext,
        callback: Option<extern "C" fn(*const c_char, *mut c_void)>,
        userdata: *mut c_void,
    );
    fn qwen_set_prompt(context: *mut QwenContext, prompt: *const c_char) -> c_int;
    fn qwen_set_force_language(context: *mut QwenContext, language: *const c_char) -> c_int;
    fn qwen_transcribe_audio(
        context: *mut QwenContext,
        samples: *const f32,
        sample_count: c_int,
    ) -> *mut c_char;
    fn qwen_last_detected_language(context: *const QwenContext) -> *const c_char;
    fn qwen_free_text(text: *mut c_char);
    fn qwen_set_threads(n: c_int);
}

/// The vendored runtime defaults to a single-thread pool; without this call the
/// per-token decoder matvecs run on one core. Configure the pool once per
/// process (it is persistent and not safe to resize during inference).
fn configure_native_threads() {
    static CONFIGURE: std::sync::Once = std::sync::Once::new();
    CONFIGURE.call_once(|| {
        let threads = native_thread_count();
        unsafe { qwen_set_threads(threads as c_int) };
    });
}

/// Performance cores on Apple Silicon (efficiency cores would straggle behind
/// the pool's barrier); otherwise the available parallelism, capped at 8.
fn native_thread_count() -> usize {
    if let Some(cores) = crate::memory_pressure::sysctl_u64(c"hw.perflevel0.physicalcpu")
        .filter(|&cores| cores > 0)
    {
        return (cores as usize).min(16);
    }
    std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(4)
        .clamp(1, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_iso_codes_and_names() {
        assert_eq!(qwen_language_name("fr"), Some("French"));
        assert_eq!(qwen_language_name("GERMAN"), Some("German"));
        assert_eq!(qwen_language_code("English"), Some("en"));
        assert_eq!(qwen_language_name("xx"), None);
    }

    #[test]
    fn detects_prompt_leakage_in_prompt_order() {
        let terms = vec![
            "Redis".to_string(),
            "PostgreSQL".to_string(),
            "CUDA".to_string(),
        ];
        assert!(prompt_leakage_reason("Redis, PostgreSQL, CUDA", &terms).is_some());
        assert!(prompt_leakage_reason("We deployed Redis after the migration", &terms).is_none());
        assert!(prompt_leakage_reason(
            "This is a deliberately long spoken sentence with enough ordinary words that the dictionary terms do not dominate the transcription at all. Redis, PostgreSQL, CUDA",
            &terms
        )
        .is_some());
    }

    #[test]
    fn detects_reported_dictionary_suffix_leakage() {
        let terms = vec![
            "Moorfield".to_string(),
            "QRX".to_string(),
            "TLMB".to_string(),
            "Nordwind".to_string(),
            "Lumora".to_string(),
            "Blue Harbor".to_string(),
            "ChatGPT".to_string(),
            "GitHub".to_string(),
            "LinkedIn".to_string(),
            "OpenAI".to_string(),
            "WhatsApp".to_string(),
            "YouTube".to_string(),
        ];
        let transcript = "Le bien familial la Moorfield qui est en. Moorfield, QRX, TLMB, Nordwind, Lumora, Blue Harbor, ChatGPT, GitHub, LinkedIn, OpenAI, WhatsApp, YouTube";

        assert!(prompt_leakage_reason(transcript, &terms).is_some());
        assert_eq!(
            trim_prompt_leakage_suffix(transcript, &terms).as_deref(),
            Some("Le bien familial la Moorfield qui est en.")
        );
    }

    #[test]
    fn dictionary_suffix_trimming_does_not_remove_ordinary_mentions() {
        let terms = vec![
            "Moorfield".to_string(),
            "QRX".to_string(),
            "TLMB".to_string(),
        ];

        assert!(trim_prompt_leakage_suffix(
            "Moorfield worked with QRX and TLMB on the project.",
            &terms
        )
        .is_none());
        assert!(trim_prompt_leakage_suffix("Moorfield, QRX, TLMB", &terms).is_none());
    }

    #[test]
    fn artifact_validation_rejects_same_size_corruption() {
        let root = std::env::temp_dir().join(format!("murmela-qwen-discovery-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).expect("model directory");
        fs::write(root.join("artifact.bin"), b"valid").expect("artifact");
        let expected_hash = format!("{:x}", Sha256::digest(b"valid"));
        let manifest = CompletionManifest {
            model_id: QWEN_MODEL_ID.to_string(),
            revision: QWEN_MODEL_REVISION.to_string(),
            artifacts: vec![CompletedArtifact {
                path: "artifact.bin".to_string(),
                size_bytes: 5,
                sha256: expected_hash.clone(),
            }],
        };
        let required = [("artifact.bin", 5, expected_hash.as_str())];
        assert!(validate_artifacts(&root, &manifest, &required).expect("valid artifact"));

        fs::write(root.join("artifact.bin"), b"other").expect("same-size corruption");
        assert!(!validate_artifacts(&root, &manifest, &required).expect("corrupt artifact"));
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn discovery_rejects_an_incomplete_pinned_installation() {
        let root = std::env::temp_dir().join(format!("murmela-qwen-incomplete-{}", Uuid::new_v4()));
        let model_dir = root.join(QWEN_MODEL_DIR);
        fs::create_dir_all(&model_dir).expect("model directory");
        fs::write(
            model_dir.join(QWEN_COMPLETE_FILE),
            serde_json::to_vec(&serde_json::json!({
                "modelId": QWEN_MODEL_ID,
                "revision": QWEN_MODEL_REVISION,
                "artifacts": []
            }))
            .expect("manifest json"),
        )
        .expect("manifest");
        assert!(discover_model(&root).expect("discovery").is_none());
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    #[ignore = "requires MURMELA_QWEN_MODEL_DIR and MURMELA_QWEN_AUDIO_FILE"]
    fn real_model_smoke_test() {
        let model_dir = std::env::var("MURMELA_QWEN_MODEL_DIR").expect("model directory env");
        let audio_file = std::env::var("MURMELA_QWEN_AUDIO_FILE").expect("audio file env");
        let prepared = crate::audio_preprocess::decode_audio_file(Path::new(&audio_file))
            .expect("decode fixture");
        let mut context = NativeContext::load(Path::new(&model_dir)).expect("load Qwen");
        context.set_forced_language(None).expect("auto language");
        context.set_prompt(None).expect("clear prompt");
        let output = context
            .transcribe(&prepared.samples, None, 100)
            .expect("transcribe fixture");
        assert!(!output.text.trim().is_empty());
        assert!(output.language.is_some());
    }
}
