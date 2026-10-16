//! The Translate screen: typed, pasted or opened text, translated paragraph by
//! paragraph on the resident TranslateGemma helper.
//!
//! The screen splits the text into segments (paragraphs) and sends only the ones
//! it has no translation for yet, so editing one paragraph re-translates just
//! that one. Each finished segment is emitted at once. A newer request cancels
//! the older one; the helper stops within a token and stays loaded. Dictation
//! keeps priority: a running request steps aside between segments.
use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::Emitter;

use crate::resident_translator::TranslatorProcess;
use crate::translation::{TranslationService, PROMPT_VERSION, WORKER_PROTOCOL};

pub const EVENT: &str = "text-translation";
/// Codes and English names of the languages the screen offers (prompt names live in the helper).
pub const LANGUAGES: [(&str, &str); 9] = [
    ("en", "English"),
    ("de", "German"),
    ("fr", "French"),
    ("es", "Spanish (Spain)"),
    ("es-AR", "Spanish (Argentina)"),
    ("it", "Italian"),
    ("pt-BR", "Portuguese (Brazil)"),
    ("nl", "Dutch"),
    ("pl", "Polish"),
];
const TONES: [&str; 3] = ["auto", "informal", "formal"];
/// About 60 pages; longer texts are split by the user.
pub const MAX_CHARACTERS: usize = 200_000;
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const FIRST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(150);
const CHUNK_TIMEOUT: Duration = Duration::from_secs(180);
const CANCEL_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextSegment {
    pub index: u32,
    pub text: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextTranslationRequest {
    pub request_id: String,
    pub segments: Vec<TextSegment>,
    /// A language code, or "auto"/None when the language is unknown.
    pub source_language: Option<String>,
    pub target_language: String,
    #[serde(default)]
    pub tone: Option<String>,
    /// A specific installed model; None uses the one chosen in Settings.
    #[serde(default)]
    pub model_id: Option<String>,
    /// The user chose to translate although macOS reports too little free memory.
    #[serde(default)]
    pub allow_low_memory: bool,
}

/// One paragraph of the Translate screen and the point in its translation to work on.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParagraphEditRequest {
    pub request_id: String,
    /// The source paragraph and its translation as shown.
    pub source_text: String,
    pub translation: String,
    /// For alternatives: the translation before the clicked word. For a rewrite: the
    /// translation to keep, ending with the chosen word.
    pub prefix: String,
    /// The clicked word (alternatives only).
    #[serde(default)]
    pub word: Option<String>,
    pub source_language: Option<String>,
    pub target_language: String,
    #[serde(default)]
    pub tone: Option<String>,
    #[serde(default)]
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct WordAlternative {
    pub text: String,
    pub probability: f64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RewrittenParagraph {
    pub text: String,
    pub issues: Vec<&'static str>,
}

/// Other words the model offers for alternatives; the menu shows up to this many.
const ALTERNATIVE_COUNT: u32 = 8;
const EDIT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TextTranslationEvent {
    /// `stage`: waiting, checking, loading or translating.
    #[serde(rename_all = "camelCase")]
    Stage {
        request_id: String,
        stage: &'static str,
        index: Option<u32>,
        chunk_index: Option<u64>,
        chunk_count: Option<u64>,
    },
    #[serde(rename_all = "camelCase")]
    Segment {
        request_id: String,
        index: u32,
        text: String,
        /// Automatic checks that failed (numbers or links changed, a preface…).
        issues: Vec<&'static str>,
    },
    #[serde(rename_all = "camelCase")]
    SegmentFailed {
        request_id: String,
        index: u32,
        code: String,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextTranslationOutcome {
    pub request_id: String,
    pub model_id: String,
    /// completed, canceled or partial (some segments failed).
    pub status: &'static str,
    pub translated: u32,
    pub failed: u32,
    pub prompt_version: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslationLanguage {
    pub code: &'static str,
    pub name: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranslatorStatus {
    pub supported: bool,
    /// The model a translation would use now; None until one is downloaded.
    pub model_id: Option<String>,
    pub model_name: Option<String>,
    pub installed_model_ids: Vec<String>,
    /// The helper has this model in memory, so the next translation starts at once.
    pub loaded: bool,
    pub memory_available_bytes: Option<u64>,
    pub memory_needed_bytes: Option<u64>,
    pub languages: Vec<TranslationLanguage>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectedLanguage {
    pub code: &'static str,
    pub reliable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedTextFile {
    pub name: String,
    pub text: String,
}

/// Cancellation flags of running requests, by request id.
static ACTIVE: LazyLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = LazyLock::new(Default::default);

fn active() -> std::sync::MutexGuard<'static, HashMap<String, Arc<AtomicBool>>> {
    ACTIVE.lock().unwrap_or_else(|error| error.into_inner())
}

/// Stops one request, or every request when `request_id` is None.
pub fn cancel(request_id: Option<&str>) {
    for (id, flag) in active().iter() {
        if request_id.is_none_or(|wanted| wanted == id) {
            flag.store(true, Ordering::SeqCst);
        }
    }
}

/// Removes a request's flag when it ends, however it ends.
struct Registration(String);
impl Drop for Registration {
    fn drop(&mut self) {
        active().remove(&self.0);
    }
}

pub fn languages() -> Vec<TranslationLanguage> {
    LANGUAGES
        .iter()
        .map(|(code, name)| TranslationLanguage { code, name })
        .collect()
}

fn known_language(code: &str) -> bool {
    LANGUAGES.iter().any(|(known, _)| *known == code)
}

/// The language of `text` among the offered ones. Spanish and Portuguese map
/// to the variants the screen offers.
pub fn detect_language(text: &str) -> Option<DetectedLanguage> {
    use whatlang::{Detector, Lang};
    let sample: String = text.chars().take(2000).collect();
    if sample.chars().filter(|c| c.is_alphabetic()).count() < 12 {
        return None;
    }
    let detector = Detector::with_allowlist(vec![
        Lang::Eng,
        Lang::Deu,
        Lang::Fra,
        Lang::Spa,
        Lang::Ita,
        Lang::Por,
        Lang::Nld,
        Lang::Pol,
    ]);
    let info = detector.detect(&sample)?;
    let code = match info.lang() {
        Lang::Eng => "en",
        Lang::Deu => "de",
        Lang::Fra => "fr",
        Lang::Spa => "es",
        Lang::Ita => "it",
        Lang::Por => "pt-BR",
        Lang::Nld => "nl",
        Lang::Pol => "pl",
        _ => return None,
    };
    Some(DetectedLanguage {
        code,
        reliable: info.is_reliable() || info.confidence() >= 0.5,
    })
}

pub fn status(service: &TranslationService) -> TranslatorStatus {
    let supported = crate::translation::platform_supported();
    let models_dir = service.models_dir();
    let installed: Vec<String> = crate::translation::MODELS
        .iter()
        .filter(|model| {
            crate::model_downloads::installed_translation_model_path(&models_dir, model.id).is_some()
        })
        .map(|model| model.id.to_string())
        .collect();
    let selected = service.selected_model().ok();
    TranslatorStatus {
        supported,
        model_id: selected.as_ref().map(|(spec, _)| spec.id.to_string()),
        model_name: selected.as_ref().map(|(spec, _)| spec.name.to_string()),
        installed_model_ids: installed,
        loaded: selected
            .as_ref()
            .is_some_and(|(_, path)| service.resident().holds(path)),
        memory_available_bytes: crate::memory_pressure::available_bytes(),
        memory_needed_bytes: selected
            .as_ref()
            .map(|(spec, _)| crate::resident_translator::memory_needed(spec.size as u64)),
        languages: languages(),
    }
}

fn validate_languages(source: Option<&str>, target: &str, tone: Option<&str>) -> Result<()> {
    if !crate::translation::platform_supported() {
        bail!("TRANSLATION_UNSUPPORTED: Translation currently requires a Mac with Apple Silicon.");
    }
    if !known_language(target) {
        bail!("TRANSLATION_INVALID_REQUEST: Unknown target language.");
    }
    if let Some(source) = source {
        if source != "auto" && !known_language(source) {
            bail!("TRANSLATION_INVALID_REQUEST: Unknown source language.");
        }
    }
    if !TONES.contains(&tone.unwrap_or("auto")) {
        bail!("TRANSLATION_INVALID_REQUEST: Unknown tone.");
    }
    Ok(())
}

fn validate(request: &TextTranslationRequest) -> Result<()> {
    validate_languages(
        request.source_language.as_deref(),
        &request.target_language,
        request.tone.as_deref(),
    )?;
    let characters: usize = request.segments.iter().map(|s| s.text.chars().count()).sum();
    if characters > MAX_CHARACTERS {
        bail!(
            "TRANSLATION_TOO_LONG: This text has {characters} characters. The limit is {MAX_CHARACTERS}. Split it and try again."
        );
    }
    Ok(())
}

/// Keeps one segment one paragraph, so it lines up with its source.
fn normalize_output(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.trim().lines() {
        let line = line.trim_end();
        if line.trim().is_empty() {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n")
}

pub fn translate(
    service: &TranslationService,
    request: TextTranslationRequest,
) -> Result<TextTranslationOutcome> {
    let _work = crate::shutdown::begin_work(false)?;
    validate(&request)?;
    let (spec, model) = match request.model_id.as_deref() {
        Some(id) => service.installed_model(Some(id)),
        None => service.selected_model(),
    }
    .map_err(|_| anyhow!("TRANSLATION_MODEL_MISSING: Download a translation model to start."))?;
    let helper = crate::translation::helper_path(service.app_handle()).ok_or_else(|| {
        anyhow!("TRANSLATION_RUNTIME_MISSING: The bundled translation runtime is missing. Rebuild or reinstall Murmela.")
    })?;

    // A newer request replaces every older one.
    cancel(None);
    let cancelled = Arc::new(AtomicBool::new(false));
    active().insert(request.request_id.clone(), cancelled.clone());
    let _registration = Registration(request.request_id.clone());
    let mut outcome = TextTranslationOutcome {
        request_id: request.request_id.clone(),
        model_id: spec.id.to_string(),
        status: "completed",
        translated: 0,
        failed: 0,
        prompt_version: PROMPT_VERSION,
    };
    let segments: Vec<&TextSegment> = request
        .segments
        .iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .collect();
    if segments.is_empty() {
        return Ok(outcome);
    }
    let app = service.app_handle().clone();
    let emit = move |event: TextTranslationEvent| {
        let _ = app.emit(EVENT, event);
    };
    let run = Run {
        emit: &emit,
        request: &request,
        cancelled: &cancelled,
        prefer_gpu: service.prefer_gpu(),
    };

    if !service.model_verified(&model) {
        run.stage("checking", None, None);
        service
            .verify_model(spec, &model, &cancelled, Instant::now() + Duration::from_secs(180))
            .map_err(|error| {
                if cancelled.load(Ordering::SeqCst) {
                    anyhow!("TRANSLATION_CANCELED: Translation stopped.")
                } else {
                    anyhow!("TRANSLATION_MODEL_DAMAGED: {error}")
                }
            })?;
    }
    if !request.allow_low_memory && !service.resident().holds(&model) {
        let available = crate::memory_pressure::available_bytes();
        if available.is_some()
            && !crate::resident_translator::room_to_load(available, spec.size as u64)
        {
            bail!(
                "TRANSLATION_MEMORY_LOW: {} needs about {:.0} GB of free memory; {:.1} GB are free.",
                spec.name,
                crate::resident_translator::memory_needed(spec.size as u64) as f64 / 1e9,
                available.unwrap_or(0) as f64 / 1e9
            );
        }
    }

    let queue = service.processing_queue();
    let key = format!("text-translation-{}", request.request_id);
    let mut next = 0;
    while next < segments.len() {
        if run.stopped() {
            outcome.status = "canceled";
            break;
        }
        if queue.busy() {
            run.stage("waiting", None, None);
        }
        queue.enqueue(&key);
        // A loaded model needs no new memory, so the warm dictation engine may stay.
        let permit = if service.resident().holds(&model) {
            queue.acquire_keeping_idle(&key, &cancelled)
        } else {
            queue.acquire(&key, &cancelled)
        };
        let Ok(_permit) = permit else {
            outcome.status = "canceled";
            break;
        };
        let warm = service.resident().take(&|| run.stopped());
        let process = match warm.filter(|process| process.serves(&model, run.prefer_gpu)) {
            Some(process) => Ok(process),
            None => {
                run.stage("loading", None, None);
                TranslatorProcess::spawn(&helper, &model, run.prefer_gpu)
            }
        };
        let mut process = match process {
            Ok(process) => process,
            Err(error) => {
                service.resident().give_back(None);
                return Err(error);
            }
        };
        while next < segments.len() {
            // Step aside for a dictation between segments; it may use the loaded model.
            if queue.has_priority_waiter() {
                run.stage("waiting", None, None);
                break;
            }
            match run.segment(&mut process, &model, segments[next]) {
                Ok(SegmentResult::Done) => outcome.translated += 1,
                Ok(SegmentResult::Failed) => outcome.failed += 1,
                Ok(SegmentResult::Canceled) => {
                    outcome.status = "canceled";
                    break;
                }
                Err(error) => {
                    // A broken exchange: the helper is not reused.
                    service.resident().give_back(None);
                    if run.stopped() {
                        outcome.status = "canceled";
                        return Ok(outcome);
                    }
                    return Err(error);
                }
            }
            next += 1;
        }
        service.resident().give_back(Some(process));
        if outcome.status == "canceled" {
            break;
        }
    }
    if outcome.status == "completed" && outcome.failed > 0 {
        outcome.status = "partial";
    }
    Ok(outcome)
}

enum SegmentResult {
    Done,
    Failed,
    Canceled,
}

struct Run<'a> {
    emit: &'a dyn Fn(TextTranslationEvent),
    request: &'a TextTranslationRequest,
    cancelled: &'a AtomicBool,
    prefer_gpu: bool,
}

impl Run<'_> {
    fn stopped(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down()
    }

    fn emit(&self, event: TextTranslationEvent) {
        (self.emit)(event);
    }

    fn stage(&self, stage: &'static str, index: Option<u32>, chunk: Option<(u64, u64)>) {
        self.emit(TextTranslationEvent::Stage {
            request_id: self.request.request_id.clone(),
            stage,
            index,
            chunk_index: chunk.map(|c| c.0),
            chunk_count: chunk.map(|c| c.1),
        });
    }

    /// Translates one segment. `Err` means the helper can no longer be trusted.
    fn segment(
        &self,
        process: &mut TranslatorProcess,
        model: &Path,
        segment: &TextSegment,
    ) -> Result<SegmentResult> {
        if self.stopped() {
            return Ok(SegmentResult::Canceled);
        }
        let id = format!("{}#{}", self.request.request_id, segment.index);
        let source = self.request.source_language.clone().unwrap_or_else(|| "auto".into());
        process.send(&serde_json::json!({
            "version": WORKER_PROTOCOL, "type": "translate", "requestId": id,
            "modelPath": model, "text": segment.text, "sourceLanguage": source,
            "targetLanguage": self.request.target_language,
            "tone": self.request.tone.as_deref().unwrap_or("auto"),
            "preferGpu": self.prefer_gpu,
        }))?;
        let mut deadline = Instant::now() + FIRST_RESPONSE_TIMEOUT;
        let mut cancel_sent: Option<Instant> = None;
        loop {
            if self.stopped() && cancel_sent.is_none() {
                process.cancel(&id)?;
                cancel_sent = Some(Instant::now());
            }
            if cancel_sent.is_some_and(|sent| sent.elapsed() > CANCEL_TIMEOUT) {
                bail!("Translation runtime did not stop.");
            }
            if Instant::now() > deadline {
                bail!("TRANSLATION_TIMEOUT: Translation timed out.");
            }
            let Some(line) = process.next_line(Duration::from_millis(100))? else {
                continue;
            };
            let value: serde_json::Value = serde_json::from_str(&line)
                .map_err(|_| anyhow!("Invalid translation worker response."))?;
            if value["version"] != WORKER_PROTOCOL
                || value["promptVersion"] != PROMPT_VERSION
                || value["requestId"].as_str() != Some(&id)
            {
                bail!("Translation protocol mismatch.");
            }
            match value["type"].as_str() {
                Some("ready") => {
                    deadline = Instant::now() + CHUNK_TIMEOUT;
                    if cancel_sent.is_none() {
                        self.stage("translating", Some(segment.index), None);
                    }
                }
                Some("progress") => {
                    deadline = Instant::now() + CHUNK_TIMEOUT;
                    let chunk = value["chunkIndex"].as_u64().zip(value["chunkCount"].as_u64());
                    if cancel_sent.is_none() && chunk.is_some_and(|(_, count)| count > 1) {
                        self.stage("translating", Some(segment.index), chunk.map(|(i, n)| (i + 1, n)));
                    }
                }
                Some("result") => {
                    if cancel_sent.is_some() {
                        return Ok(SegmentResult::Canceled);
                    }
                    let text = normalize_output(value["text"].as_str().unwrap_or_default());
                    if text.is_empty() {
                        self.failed(segment, "EMPTY_TRANSLATION");
                        return Ok(SegmentResult::Failed);
                    }
                    let issues = crate::translation_check::check_translation(
                        &segment.text,
                        &text,
                        &self.request.target_language,
                    );
                    self.emit(TextTranslationEvent::Segment {
                        request_id: self.request.request_id.clone(),
                        index: segment.index,
                        text,
                        issues,
                    });
                    return Ok(SegmentResult::Done);
                }
                Some("error") => {
                    let code = value["code"].as_str().unwrap_or("TRANSLATION_FAILED");
                    if code == "CANCELED" {
                        return Ok(SegmentResult::Canceled);
                    }
                    if cancel_sent.is_some() {
                        return Ok(SegmentResult::Canceled);
                    }
                    self.failed(segment, code);
                    return Ok(SegmentResult::Failed);
                }
                _ => bail!("Invalid translation worker response."),
            }
        }
    }

    fn failed(&self, segment: &TextSegment, code: &str) {
        self.emit(TextTranslationEvent::SegmentFailed {
            request_id: self.request.request_id.clone(),
            index: segment.index,
            code: code.to_string(),
        });
    }
}

/// Other words for one place in a paragraph's translation, best first.
pub fn alternatives(
    service: &TranslationService,
    request: ParagraphEditRequest,
) -> Result<Vec<WordAlternative>> {
    let word = request.word.clone().unwrap_or_default();
    let reply = edit_exchange(
        service,
        &request,
        serde_json::json!({
            "type": "alternatives", "word": word, "count": ALTERNATIVE_COUNT,
        }),
    )?;
    if reply["type"] != "alternatives" {
        bail!("Invalid translation worker response.");
    }
    let alternatives: Vec<WordAlternative> =
        serde_json::from_value(reply["alternatives"].clone()).map_err(|_| anyhow!("Invalid translation worker response."))?;
    Ok(alternatives
        .into_iter()
        .filter(|alternative| !alternative.text.trim().is_empty() && alternative.text != word)
        .collect())
}

/// Keeps the translation up to a chosen word and lets the model finish the paragraph.
pub fn rewrite(
    service: &TranslationService,
    request: ParagraphEditRequest,
) -> Result<RewrittenParagraph> {
    if request.prefix.trim().is_empty() {
        bail!("TRANSLATION_INVALID_REQUEST: Nothing to keep.");
    }
    let reply = edit_exchange(service, &request, serde_json::json!({ "type": "rewrite" }))?;
    if reply["type"] != "result" {
        bail!("Invalid translation worker response.");
    }
    let text = normalize_output(reply["text"].as_str().unwrap_or_default());
    if text.is_empty() {
        bail!("TRANSLATION_FAILED: The model returned no text.");
    }
    let issues = crate::translation_check::check_translation(&request.source_text, &text, &request.target_language);
    Ok(RewrittenParagraph { text, issues })
}

/// Sends one alternatives or rewrite request to the loaded helper and returns its
/// reply. Like a translation it waits its turn behind a dictation and reuses the
/// warm helper; a newer translation cancels it.
fn edit_exchange(
    service: &TranslationService,
    request: &ParagraphEditRequest,
    extra: serde_json::Value,
) -> Result<serde_json::Value> {
    let _work = crate::shutdown::begin_work(false)?;
    validate_languages(request.source_language.as_deref(), &request.target_language, request.tone.as_deref())?;
    if request.source_text.chars().count() > MAX_CHARACTERS || request.translation.chars().count() > MAX_CHARACTERS {
        bail!("TRANSLATION_TOO_LONG: This paragraph is too long.");
    }
    let (spec, model) = match request.model_id.as_deref() {
        Some(id) => service.installed_model(Some(id)),
        None => service.selected_model(),
    }
    .map_err(|_| anyhow!("TRANSLATION_MODEL_MISSING: Download a translation model to start."))?;
    let helper = crate::translation::helper_path(service.app_handle()).ok_or_else(|| {
        anyhow!("TRANSLATION_RUNTIME_MISSING: The bundled translation runtime is missing. Rebuild or reinstall Murmela.")
    })?;
    let cancelled = Arc::new(AtomicBool::new(false));
    active().insert(request.request_id.clone(), cancelled.clone());
    let _registration = Registration(request.request_id.clone());
    let stopped = || cancelled.load(Ordering::SeqCst) || crate::shutdown::is_shutting_down();
    let canceled = || anyhow!("TRANSLATION_CANCELED: Translation stopped.");

    if !service.model_verified(&model) {
        service
            .verify_model(spec, &model, &cancelled, Instant::now() + Duration::from_secs(180))
            .map_err(|error| if stopped() { canceled() } else { anyhow!("TRANSLATION_MODEL_DAMAGED: {error}") })?;
    }
    if !service.resident().holds(&model) {
        let available = crate::memory_pressure::available_bytes();
        if available.is_some() && !crate::resident_translator::room_to_load(available, spec.size as u64) {
            bail!(
                "TRANSLATION_MEMORY_LOW: {} needs about {:.0} GB of free memory; {:.1} GB are free.",
                spec.name,
                crate::resident_translator::memory_needed(spec.size as u64) as f64 / 1e9,
                available.unwrap_or(0) as f64 / 1e9
            );
        }
    }

    let queue = service.processing_queue();
    let key = format!("text-translation-edit-{}", request.request_id);
    queue.enqueue(&key);
    let permit = if service.resident().holds(&model) {
        queue.acquire_keeping_idle(&key, &cancelled)
    } else {
        queue.acquire(&key, &cancelled)
    };
    let Ok(_permit) = permit else {
        return Err(canceled());
    };
    let prefer_gpu = service.prefer_gpu();
    let warm = service.resident().take(&stopped);
    let mut process = match warm.filter(|process| process.serves(&model, prefer_gpu)) {
        Some(process) => process,
        None => match TranslatorProcess::spawn(&helper, &model, prefer_gpu) {
            Ok(process) => process,
            Err(error) => {
                service.resident().give_back(None);
                return Err(error);
            }
        },
    };

    let id = request.request_id.clone();
    let mut message = serde_json::json!({
        "version": WORKER_PROTOCOL, "requestId": id, "modelPath": model,
        "text": request.source_text, "translation": request.translation, "prefix": request.prefix,
        "sourceLanguage": request.source_language.clone().unwrap_or_else(|| "auto".into()),
        "targetLanguage": request.target_language,
        "tone": request.tone.as_deref().unwrap_or("auto"),
        "preferGpu": prefer_gpu,
    });
    if let (Some(message), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        message.extend(extra.clone());
    }
    let reply = (|| -> Result<serde_json::Value> {
        process.send(&message)?;
        let deadline = Instant::now() + FIRST_RESPONSE_TIMEOUT.max(EDIT_TIMEOUT);
        let mut cancel_sent: Option<Instant> = None;
        loop {
            if stopped() && cancel_sent.is_none() {
                process.cancel(&id)?;
                cancel_sent = Some(Instant::now());
            }
            if cancel_sent.is_some_and(|sent| sent.elapsed() > CANCEL_TIMEOUT) {
                bail!("Translation runtime did not stop.");
            }
            if Instant::now() > deadline {
                bail!("TRANSLATION_TIMEOUT: Translation timed out.");
            }
            let Some(line) = process.next_line(Duration::from_millis(100))? else {
                continue;
            };
            let value: serde_json::Value =
                serde_json::from_str(&line).map_err(|_| anyhow!("Invalid translation worker response."))?;
            if value["version"] != WORKER_PROTOCOL
                || value["promptVersion"] != PROMPT_VERSION
                || value["requestId"].as_str() != Some(&id)
            {
                bail!("Translation protocol mismatch.");
            }
            match value["type"].as_str() {
                Some("ready") | Some("progress") => continue,
                Some(_) => return Ok(value),
                None => bail!("Invalid translation worker response."),
            }
        }
    })();
    match reply {
        Ok(value) => {
            service.resident().give_back(Some(process));
            if value["type"] != "error" {
                return Ok(value);
            }
            match value["code"].as_str().unwrap_or("TRANSLATION_FAILED") {
                "CANCELED" => Err(canceled()),
                "PARAGRAPH_TOO_LONG" => {
                    bail!("TRANSLATION_PARAGRAPH_TOO_LONG: This paragraph is too long for alternatives. Split it into shorter paragraphs.")
                }
                // A helper from before alternatives existed.
                "INVALID_REQUEST" => bail!("TRANSLATION_RUNTIME_OUTDATED: Update Murmela to use alternatives."),
                code => bail!("{code}: Murmela could not find alternatives here."),
            }
        }
        Err(error) => {
            // A broken exchange: the helper is not reused.
            service.resident().give_back(None);
            if stopped() {
                return Err(canceled());
            }
            Err(error)
        }
    }
}

/// Text from a file: UTF-8 (with or without BOM), UTF-16 with BOM, otherwise Windows-1252.
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    let utf16 = |rest: &[u8], little: bool| {
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|pair| if little { u16::from_le_bytes([pair[0], pair[1]]) } else { u16::from_be_bytes([pair[0], pair[1]]) })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text.to_string();
    }
    const CP1252: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž', '\u{8f}',
        '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}', 'ž', 'Ÿ',
    ];
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9F => CP1252[(byte - 0x80) as usize],
            _ => byte as char,
        })
        .collect()
}

const TEXT_EXTENSIONS: [&str; 4] = ["txt", "md", "markdown", "text"];

pub fn read_text_file(path: &Path) -> Result<OpenedTextFile> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    if !TEXT_EXTENSIONS.contains(&extension.as_str()) {
        bail!("TRANSLATION_FILE_UNSUPPORTED: Translate opens .txt and .md files for now.");
    }
    let metadata = path.metadata().context("Could not read the file.")?;
    if metadata.len() > MAX_FILE_BYTES {
        bail!("TRANSLATION_TOO_LONG: This file is larger than 2 MB. Split it and try again.");
    }
    let text = decode_text(&std::fs::read(path).context("Could not read the file.")?)
        .replace("\r\n", "\n");
    if text.contains('\0') {
        bail!("TRANSLATION_FILE_UNSUPPORTED: This file does not contain plain text.");
    }
    if text.trim().is_empty() {
        bail!("TRANSLATION_FILE_EMPTY: This file is empty.");
    }
    let characters = text.chars().count();
    if characters > MAX_CHARACTERS {
        bail!(
            "TRANSLATION_TOO_LONG: This file has {characters} characters. The limit is {MAX_CHARACTERS}. Split it and try again."
        );
    }
    Ok(OpenedTextFile {
        name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Text".into()),
        text,
    })
}

pub async fn pick_text_file(window: &tauri::Window) -> Result<Option<OpenedTextFile>> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;
    let (tx, rx) = std::sync::mpsc::channel();
    window
        .app_handle()
        .dialog()
        .file()
        .set_title("Open a text to translate")
        .add_filter("Text", &TEXT_EXTENSIONS)
        .pick_file(move |file| {
            let _ = tx.send(file);
        });
    let file = tauri::async_runtime::spawn_blocking(move || rx.recv())
        .await
        .map_err(|error| anyhow!("{error}"))?
        .map_err(|_| anyhow!("The file picker did not return."))?;
    match file {
        Some(tauri_plugin_dialog::FilePath::Path(path)) => read_text_file(&path).map(Some),
        Some(_) => bail!("The selected file is not a local file."),
        None => Ok(None),
    }
}

/// Saves `contents` where the user chooses. Returns the path, None when canceled.
pub fn save_text_file_blocking(
    app: &tauri::AppHandle,
    window: &tauri::WebviewWindow,
    file_name: &str,
    contents: &str,
) -> Result<Option<String>> {
    use tauri_plugin_dialog::{DialogExt, FilePath};
    if unsafe { libc::pthread_main_np() } != 0 {
        bail!("Saving must run outside the macOS main thread.");
    }
    let extension = Path::new(file_name)
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| TEXT_EXTENSIONS.contains(e))
        .unwrap_or("txt")
        .to_string();
    let label = if extension == "txt" || extension == "text" { "Plain text" } else { "Markdown" };
    let selected = app
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Save translation")
        .set_file_name(file_name)
        .add_filter(label, &[extension.as_str()])
        .blocking_save_file();
    let path = match selected {
        Some(FilePath::Path(path)) => path,
        Some(FilePath::Url(_)) => bail!("The selected destination is not a local file."),
        None => return Ok(None),
    };
    std::fs::write(&path, contents)
        .with_context(|| format!("Could not save the translation to {}", path.display()))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// One paragraph of a saved translation, so reopening it needs no new work.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedSegment {
    pub source: String,
    pub output: Option<String>,
}

/// A translation kept in the Library (only while history is on).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SavedTextTranslation {
    pub id: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    pub title: String,
    /// text, file or transcript.
    pub source_kind: String,
    pub file_name: Option<String>,
    pub source_text: String,
    pub output_text: String,
    pub segments: Vec<SavedSegment>,
    pub source_language: Option<String>,
    pub target_language: String,
    pub tone: String,
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TextTranslationSummary {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub source_kind: String,
    pub source_language: Option<String>,
    pub target_language: String,
    pub snippet: String,
}

pub fn ensure_schema(connection: &rusqlite::Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS text_translations (
            id TEXT PRIMARY KEY,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            title TEXT NOT NULL,
            source_kind TEXT NOT NULL,
            file_name TEXT,
            source_text TEXT NOT NULL,
            output_text TEXT NOT NULL,
            segments_json TEXT NOT NULL,
            source_language TEXT,
            target_language TEXT NOT NULL,
            tone TEXT NOT NULL,
            model_id TEXT,
            prompt_version INTEGER
        );",
    )?;
    Ok(())
}

/// Creates or updates a Library entry. Returns None (and saves nothing) while history is off.
pub fn save(db_path: &Path, document: &SavedTextTranslation) -> Result<Option<SavedTextTranslation>> {
    if !crate::storage::get_settings_from_db_path(db_path)?.save_history {
        return Ok(None);
    }
    if document.id.trim().is_empty() || document.title.chars().count() > 300 {
        bail!("TRANSLATION_INVALID_REQUEST: Invalid Library entry.");
    }
    if !known_language(&document.target_language)
        || document.source_text.chars().count() > MAX_CHARACTERS
    {
        bail!("TRANSLATION_INVALID_REQUEST: Invalid Library entry.");
    }
    let connection = crate::storage::open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    let now = chrono::Utc::now().to_rfc3339();
    connection.execute(
        "INSERT INTO text_translations (id, created_at, updated_at, title, source_kind, file_name, source_text, output_text, segments_json, source_language, target_language, tone, model_id, prompt_version)
         VALUES (?1, ?2, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(id) DO UPDATE SET updated_at=?2, title=?3, source_kind=?4, file_name=?5, source_text=?6, output_text=?7, segments_json=?8, source_language=?9, target_language=?10, tone=?11, model_id=?12, prompt_version=?13",
        rusqlite::params![
            document.id, now, document.title, document.source_kind, document.file_name,
            document.source_text, document.output_text, serde_json::to_string(&document.segments)?,
            document.source_language, document.target_language, document.tone, document.model_id,
            PROMPT_VERSION,
        ],
    )?;
    get(db_path, &document.id)
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SavedTextTranslation> {
    let segments: String = row.get("segments_json")?;
    Ok(SavedTextTranslation {
        id: row.get("id")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        title: row.get("title")?,
        source_kind: row.get("source_kind")?,
        file_name: row.get("file_name")?,
        source_text: row.get("source_text")?,
        output_text: row.get("output_text")?,
        segments: serde_json::from_str(&segments).unwrap_or_default(),
        source_language: row.get("source_language")?,
        target_language: row.get("target_language")?,
        tone: row.get("tone")?,
        model_id: row.get("model_id")?,
    })
}

pub fn get(db_path: &Path, id: &str) -> Result<Option<SavedTextTranslation>> {
    use rusqlite::OptionalExtension;
    let connection = crate::storage::open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    Ok(connection
        .query_row("SELECT * FROM text_translations WHERE id = ?1", [id], read_row)
        .optional()?)
}

pub fn list(db_path: &Path, query: &str) -> Result<Vec<TextTranslationSummary>> {
    let connection = crate::storage::open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    let pattern = format!("%{}%", query.trim().to_lowercase());
    let mut statement = connection.prepare(
        "SELECT id, created_at, updated_at, title, source_kind, source_language, target_language, substr(output_text, 1, 240) AS snippet
         FROM text_translations
         WHERE ?1 = '%%' OR lower(title) LIKE ?1 OR lower(source_text) LIKE ?1 OR lower(output_text) LIKE ?1
         ORDER BY datetime(updated_at) DESC",
    )?;
    let rows = statement.query_map([pattern], |row| {
        Ok(TextTranslationSummary {
            id: row.get("id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
            title: row.get("title")?,
            source_kind: row.get("source_kind")?,
            source_language: row.get("source_language")?,
            target_language: row.get("target_language")?,
            snippet: row.get("snippet")?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn delete(db_path: &Path, id: Option<&str>) -> Result<()> {
    let connection = crate::storage::open_connection_by_path(db_path)?;
    ensure_schema(&connection)?;
    match id {
        Some(id) => connection.execute("DELETE FROM text_translations WHERE id = ?1", [id])?,
        None => connection.execute("DELETE FROM text_translations", [])?,
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detection_finds_the_offered_languages() {
        let cases = [
            ("Danke für deine schnelle Rückmeldung. Wir verschieben das Treffen auf Donnerstag.", "de"),
            ("Thanks for the quick reply. We are moving the meeting to Thursday morning.", "en"),
            ("Merci pour ta réponse rapide. Nous décalons la réunion à jeudi matin.", "fr"),
            ("Gracias por tu respuesta rápida. Movemos la reunión al jueves por la mañana.", "es"),
            ("Grazie per la risposta veloce. Spostiamo la riunione a giovedì mattina.", "it"),
            ("Obrigado pela resposta rápida. Vamos mudar a reunião para quinta-feira de manhã.", "pt-BR"),
            ("Bedankt voor je snelle reactie. We verplaatsen de vergadering naar donderdagochtend.", "nl"),
            ("Dziękuję za szybką odpowiedź. Przenosimy spotkanie na czwartek rano.", "pl"),
        ];
        for (text, code) in cases {
            assert_eq!(detect_language(text).map(|d| d.code), Some(code), "{text}");
        }
        assert!(detect_language("ok 123").is_none());
    }

    #[test]
    fn requests_are_validated_before_any_work() {
        let request = |target: &str, source: Option<&str>, tone: Option<&str>, text: String| TextTranslationRequest {
            request_id: "r".into(),
            segments: vec![TextSegment { index: 0, text }],
            source_language: source.map(Into::into),
            target_language: target.into(),
            tone: tone.map(Into::into),
            model_id: None,
            allow_low_memory: false,
        };
        if !crate::translation::platform_supported() {
            return;
        }
        assert!(validate(&request("it", Some("auto"), None, "Hallo".into())).is_ok());
        assert!(validate(&request("pl", Some("de"), Some("formal"), "Hallo".into())).is_ok());
        assert!(validate(&request("xx", None, None, "Hallo".into())).is_err());
        assert!(validate(&request("it", Some("xx"), None, "Hallo".into())).is_err());
        assert!(validate(&request("it", None, Some("rude"), "Hallo".into())).is_err());
        let error = validate(&request("it", None, None, "a".repeat(MAX_CHARACTERS + 1))).unwrap_err();
        assert!(error.to_string().starts_with("TRANSLATION_TOO_LONG"));
    }

    #[test]
    fn output_stays_one_paragraph() {
        assert_eq!(normalize_output("  Salut Lea,\n\nmerci.  \n"), "Salut Lea,\nmerci.");
        assert_eq!(normalize_output("- un\n- deux"), "- un\n- deux");
    }

    #[test]
    fn text_files_decode_common_encodings() {
        assert_eq!(decode_text(b"\xEF\xBB\xBFGr\xC3\xBC\xC3\x9Fe"), "Grüße");
        assert_eq!(decode_text(&[0xFF, 0xFE, b'H', 0, b'i', 0]), "Hi");
        assert_eq!(decode_text(&[0xFE, 0xFF, 0, b'H', 0, b'i']), "Hi");
        assert_eq!(decode_text(b"Gr\xFC\xDFe \x80 \x93ok\x94"), "Grüße € “ok”");
    }

    #[test]
    fn only_text_files_open() {
        let dir = std::env::temp_dir().join(format!("murmela-text-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("notes.md");
        std::fs::write(&md, "# Titel\r\n\r\nHallo").unwrap();
        let opened = read_text_file(&md).unwrap();
        assert_eq!(opened.name, "notes.md");
        assert_eq!(opened.text, "# Titel\n\nHallo");
        let pdf = dir.join("scan.pdf");
        std::fs::write(&pdf, "%PDF").unwrap();
        assert!(read_text_file(&pdf).unwrap_err().to_string().starts_with("TRANSLATION_FILE_UNSUPPORTED"));
        let empty = dir.join("empty.txt");
        std::fs::write(&empty, "  \n").unwrap();
        assert!(read_text_file(&empty).unwrap_err().to_string().starts_with("TRANSLATION_FILE_EMPTY"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cancel_reaches_only_the_named_request() {
        let first = Arc::new(AtomicBool::new(false));
        let second = Arc::new(AtomicBool::new(false));
        active().insert("first-test".into(), first.clone());
        active().insert("second-test".into(), second.clone());
        cancel(Some("first-test"));
        assert!(first.load(Ordering::SeqCst));
        assert!(!second.load(Ordering::SeqCst));
        drop(Registration("first-test".into()));
        drop(Registration("second-test".into()));
        assert!(!active().contains_key("first-test"));
    }

    #[test]
    fn library_entries_follow_the_history_setting() {
        let dir = std::env::temp_dir().join(format!("murmela-library-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("test.sqlite");
        let connection = crate::storage::open_connection_by_path(&db).unwrap();
        crate::storage::initialize_database_for_tests(&connection).unwrap();
        drop(connection);
        let mut document = SavedTextTranslation {
            id: "doc-1".into(),
            created_at: String::new(),
            updated_at: String::new(),
            title: "Hallo Lea".into(),
            source_kind: "text".into(),
            file_name: None,
            source_text: "Hallo Lea,\n\ndanke.".into(),
            output_text: "Salut Lea,\n\nmerci.".into(),
            segments: vec![
                SavedSegment { source: "Hallo Lea,".into(), output: Some("Salut Lea,".into()) },
                SavedSegment { source: "danke.".into(), output: Some("merci.".into()) },
            ],
            source_language: Some("de".into()),
            target_language: "fr".into(),
            tone: "auto".into(),
            model_id: Some(crate::translation::MODEL_ID.into()),
        };
        let saved = save(&db, &document).unwrap().expect("history is on by default");
        assert_eq!(saved.segments, document.segments);
        document.output_text = "Salut Lea,\n\nmerci beaucoup.".into();
        let updated = save(&db, &document).unwrap().unwrap();
        assert_eq!(updated.created_at, saved.created_at);
        assert_eq!(list(&db, "").unwrap().len(), 1);
        assert_eq!(list(&db, "BEAUCOUP").unwrap().len(), 1);
        assert!(list(&db, "nowhere").unwrap().is_empty());
        crate::storage::update_settings_for_db_path(
            &db,
            crate::settings::SettingsPatch { save_history: Some(false), ..Default::default() },
        )
        .unwrap();
        document.id = "doc-2".into();
        assert!(save(&db, &document).unwrap().is_none());
        delete(&db, Some("doc-1")).unwrap();
        assert!(get(&db, "doc-1").unwrap().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Real helper and model: `MURMELA_TRANSLATION_MODEL=/path/to/model.gguf cargo test --lib
    /// text_translation::tests::native -- --ignored --nocapture` (after `npm run build:translation`).
    #[test]
    #[ignore]
    fn native_segments_translate_and_cancel_on_the_real_helper() {
        let model = std::path::PathBuf::from(std::env::var("MURMELA_TRANSLATION_MODEL").expect("model path"));
        let helper = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/translation-runtime/build/murmela-translation-worker");
        let mut process = TranslatorProcess::spawn(&helper, &model, true).unwrap();
        process.load(&|| false).unwrap();
        let events = Mutex::new(Vec::new());
        let emit = |event: TextTranslationEvent| events.lock().unwrap().push(event);
        let cancelled = AtomicBool::new(false);
        let request = TextTranslationRequest {
            request_id: "native".into(),
            segments: vec![],
            source_language: Some("de".into()),
            target_language: "it".into(),
            tone: Some("formal".into()),
            model_id: None,
            allow_low_memory: false,
        };
        let run = Run { emit: &emit, request: &request, cancelled: &cancelled, prefer_gpu: true };
        let started = Instant::now();
        let first = TextSegment { index: 0, text: "Vielen Dank für Ihre Nachricht. Wir treffen uns am Donnerstag um 10:30 Uhr.".into() };
        assert!(matches!(run.segment(&mut process, &model, &first).unwrap(), SegmentResult::Done));
        eprintln!("segment translated in {:.1} s", started.elapsed().as_secs_f64());
        let text = events.lock().unwrap().iter().find_map(|event| match event {
            TextTranslationEvent::Segment { text, issues, .. } => Some((text.clone(), issues.clone())),
            _ => None,
        });
        let (text, issues) = text.expect("a translated segment");
        eprintln!("it: {text} {issues:?}");
        assert!(text.contains("10:30") || text.contains("10.30"), "{text}");
        assert!(!text.contains("Donnerstag"), "{text}");

        // A cancel during a long paragraph stops it quickly and leaves the helper usable.
        let long = TextSegment { index: 1, text: "Das ist ein langer Absatz über Übersetzung und Sprache. ".repeat(30) };
        let flag = &cancelled;
        std::thread::scope(|scope| {
            scope.spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                flag.store(true, Ordering::SeqCst);
            });
            let started = Instant::now();
            assert!(matches!(run.segment(&mut process, &model, &long).unwrap(), SegmentResult::Canceled));
            let waited = started.elapsed();
            eprintln!("canceled after {:.1} s", waited.as_secs_f64());
            assert!(waited < Duration::from_secs(6));
        });
        cancelled.store(false, Ordering::SeqCst);
        let again = TextSegment { index: 2, text: "Bis bald.".into() };
        assert!(matches!(run.segment(&mut process, &model, &again).unwrap(), SegmentResult::Done));
    }
}
