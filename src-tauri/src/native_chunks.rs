//! Long recordings for engines that hold the whole file in memory at once
//! (VibeVoice): the audio is split into overlapping chunks that each fit in
//! memory, every chunk is transcribed on its own, and the results are stitched
//! back into one transcript. Each chunk numbers its speakers from scratch, so
//! its speakers are matched to the ones heard before, first by who talks when
//! in the overlap both chunks heard, then by voice when Murmela's speaker
//! package is installed.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::native_asr::{NativeOutputSegment, NativeWorkerResult};

/// Audio both neighbouring chunks transcribe. Long enough that both usually
/// hear every active speaker and a sentence break to cut at.
pub const OVERLAP_MS: i64 = 60_000;
/// How far a chunk boundary may move to land on a quiet moment.
const SPLIT_SEARCH_MS: i64 = 30_000;
/// Talking at the same time in the overlap that counts as evidence.
const MIN_SHARED_SPEECH_MS: i64 = 1_500;
/// Share of the shorter talk time in the overlap that two labels must agree on.
const MIN_SHARED_RATIO: f64 = 0.5;
/// Cosine similarity above which two voices count as the same speaker.
pub const SAME_VOICE_SIMILARITY: f32 = 0.55;
/// Speech per speaker and chunk used for a voice embedding.
const VOICE_SAMPLE_MS: i64 = 45_000;
const VOICE_MIN_SEGMENT_MS: i64 = 1_000;
const VOICE_EDGE_TRIM_MS: i64 = 150;

/// The longest audio one VibeVoice pass may take on this Mac. A 15-minute file
/// fit in 18 GB of memory and a 40-minute one did not; larger Macs get more.
pub fn chunk_length_ms(installed_memory_bytes: Option<u64>) -> i64 {
    const GIB: u64 = 1024 * 1024 * 1024;
    match installed_memory_bytes.unwrap_or(0) {
        bytes if bytes >= 48 * GIB => 30 * 60_000,
        bytes if bytes >= 32 * GIB => 20 * 60_000,
        _ => 10 * 60_000,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpan {
    pub start_ms: i64,
    pub end_ms: i64,
}

/// Splits `duration_ms` into chunks of at most `chunk_ms` that overlap by
/// [`OVERLAP_MS`], each boundary moved to the quietest moment near it.
/// `loudness_at` returns the audio level around a time.
pub fn plan_chunks(
    duration_ms: i64,
    chunk_ms: i64,
    loudness_at: impl Fn(i64) -> f32,
) -> Vec<ChunkSpan> {
    if duration_ms <= chunk_ms {
        return vec![ChunkSpan { start_ms: 0, end_ms: duration_ms.max(0) }];
    }
    let mut chunks = Vec::new();
    let mut start = 0;
    loop {
        if duration_ms - start <= chunk_ms {
            chunks.push(ChunkSpan { start_ms: start, end_ms: duration_ms });
            return chunks;
        }
        let latest = start + chunk_ms;
        let earliest = (latest - SPLIT_SEARCH_MS).max(start + OVERLAP_MS * 2);
        let mut end = latest;
        let mut quietest = f32::INFINITY;
        let mut at = earliest;
        while at <= latest {
            let level = loudness_at(at);
            if level < quietest {
                quietest = level;
                end = at;
            }
            at += 250;
        }
        chunks.push(ChunkSpan { start_ms: start, end_ms: end });
        start = end - OVERLAP_MS;
    }
}

/// Mean absolute level of 200 ms of audio around `at_ms`.
pub fn loudness(samples: &[f32], sample_rate_hz: u32, at_ms: i64) -> f32 {
    let rate = sample_rate_hz as i64;
    let center = (at_ms * rate / 1_000) as usize;
    let half = (rate / 10) as usize;
    let from = center.saturating_sub(half).min(samples.len());
    let to = (center + half).min(samples.len());
    if to <= from {
        return 0.0;
    }
    samples[from..to].iter().map(|value| value.abs()).sum::<f32>() / (to - from) as f32
}

/// One chunk's transcript; segment times are relative to the chunk start.
#[derive(Debug, Clone)]
pub struct ChunkTranscript {
    pub span: ChunkSpan,
    pub result: NativeWorkerResult,
}

/// A voice embedding for every speaker label a chunk used.
pub type ChunkVoices = HashMap<String, Vec<f32>>;

/// How one chunk label was matched: (label, global speaker, "overlap" | "voice" | "new").
pub type SpeakerMatch = (String, usize, &'static str);

/// What stitching decided, for the job log.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct StitchReport {
    /// Per chunk after the first.
    pub matches: Vec<Vec<SpeakerMatch>>,
    pub speakers: usize,
}

struct GlobalSpeaker {
    voice: Option<Vec<f32>>,
    voice_weight_ms: i64,
}

/// Joins chunk transcripts into one result on the recording's timeline with
/// speaker labels that mean the same person across chunks.
pub fn stitch(chunks: Vec<ChunkTranscript>, voices: &[ChunkVoices]) -> (NativeWorkerResult, StitchReport) {
    let mut report = StitchReport::default();
    let mut speakers = Vec::<GlobalSpeaker>::new();
    let mut output = Vec::<NativeOutputSegment>::new();
    let mut truncated = false;
    let mut warning = None;
    // The previous chunk's segments with global labels, for overlap matching.
    let mut previous: Vec<(i64, i64, Option<usize>)> = Vec::new();
    let mut previous_span: Option<ChunkSpan> = None;
    let mut keep_from = i64::MIN;
    let absolute: Vec<Vec<NativeOutputSegment>> = chunks.iter().map(absolute_segments).collect();

    for (index, chunk) in chunks.iter().enumerate() {
        truncated |= chunk.result.truncated;
        warning = warning.or_else(|| chunk.result.warning.clone());
        let segments = &absolute[index];
        let empty = ChunkVoices::new();
        let chunk_voices = voices.get(index).unwrap_or(&empty);
        let mapping = match previous_span {
            None => {
                let mut mapping = HashMap::new();
                for label in labels_in_order(segments) {
                    mapping.insert(label, new_speaker(&mut speakers));
                }
                mapping
            }
            Some(before) => {
                let overlap = ChunkSpan { start_ms: chunk.span.start_ms, end_ms: before.end_ms };
                let (mapping, decisions) =
                    match_speakers(segments, &previous, overlap, chunk_voices, &mut speakers);
                report.matches.push(decisions);
                mapping
            }
        };
        for (label, &global) in &mapping {
            if let Some(voice) = chunk_voices.get(label) {
                let weight = segments
                    .iter()
                    .filter(|segment| segment.speaker.as_deref() == Some(label.as_str()))
                    .map(|segment| segment.end_ms - segment.start_ms)
                    .sum::<i64>()
                    .min(VOICE_SAMPLE_MS);
                add_voice(&mut speakers[global], voice, weight);
            }
        }

        let next_span = chunks.get(index + 1).map(|next| next.span);
        let keep_until = match next_span {
            Some(next) => cut_point(
                segments,
                &absolute[index + 1],
                ChunkSpan { start_ms: next.start_ms, end_ms: chunk.span.end_ms },
            ),
            None => i64::MAX,
        };
        let kept_before = output.len();
        for segment in segments {
            let middle = (segment.start_ms + segment.end_ms) / 2;
            if middle < keep_from || middle >= keep_until {
                continue;
            }
            // A long placeholder such as "[Speech]" can reach past the cut;
            // keep it from one chunk only.
            let covered: i64 = output[..kept_before]
                .iter()
                .rev()
                .take_while(|kept| kept.end_ms > chunk.span.start_ms)
                .map(|kept| intersection(kept.start_ms, kept.end_ms, segment.start_ms, segment.end_ms))
                .sum();
            if covered * 2 > segment.end_ms - segment.start_ms {
                continue;
            }
            let global = segment.speaker.as_ref().and_then(|label| mapping.get(label));
            output.push(NativeOutputSegment {
                speaker: global.map(|id| id.to_string()),
                ..segment.clone()
            });
        }
        previous = segments
            .iter()
            .map(|segment| {
                let global = segment.speaker.as_ref().and_then(|label| mapping.get(label)).copied();
                (segment.start_ms, segment.end_ms, global)
            })
            .collect();
        previous_span = Some(chunk.span);
        keep_from = keep_until;
    }

    report.speakers = speakers.len();
    let text = output.iter().map(|segment| segment.text.trim()).collect::<Vec<_>>().join(" ");
    (NativeWorkerResult { text, segments: output, truncated, warning }, report)
}

fn absolute_segments(chunk: &ChunkTranscript) -> Vec<NativeOutputSegment> {
    let mut segments: Vec<_> = chunk
        .result
        .segments
        .iter()
        .map(|segment| {
            let start_ms = (chunk.span.start_ms + segment.start_ms).clamp(chunk.span.start_ms, chunk.span.end_ms);
            let end_ms = (chunk.span.start_ms + segment.end_ms).clamp(start_ms, chunk.span.end_ms);
            let speaker = segment
                .speaker
                .as_deref()
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .map(str::to_string);
            NativeOutputSegment { start_ms, end_ms, speaker, ..segment.clone() }
        })
        .collect();
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
    segments
}

fn labels_in_order(segments: &[NativeOutputSegment]) -> Vec<String> {
    let mut seen = Vec::new();
    for label in segments.iter().filter_map(|segment| segment.speaker.clone()) {
        if !seen.contains(&label) {
            seen.push(label);
        }
    }
    seen
}

fn new_speaker(speakers: &mut Vec<GlobalSpeaker>) -> usize {
    speakers.push(GlobalSpeaker { voice: None, voice_weight_ms: 0 });
    speakers.len() - 1
}

fn add_voice(speaker: &mut GlobalSpeaker, voice: &[f32], weight_ms: i64) {
    let voice = normalized(voice);
    let weight = weight_ms.max(1);
    speaker.voice = Some(match speaker.voice.take() {
        None => voice,
        Some(current) => {
            let total = (speaker.voice_weight_ms + weight) as f32;
            let mixed: Vec<f32> = current
                .iter()
                .zip(&voice)
                .map(|(a, b)| (a * speaker.voice_weight_ms as f32 + b * weight as f32) / total)
                .collect();
            normalized(&mixed)
        }
    });
    speaker.voice_weight_ms += weight;
}

fn normalized(vector: &[f32]) -> Vec<f32> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm == 0.0 {
        return vector.to_vec();
    }
    vector.iter().map(|value| value / norm).collect()
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norms = a.iter().map(|x| x * x).sum::<f32>().sqrt() * b.iter().map(|y| y * y).sum::<f32>().sqrt();
    if norms == 0.0 { 0.0 } else { dot / norms }
}

fn intersection(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
}

/// Maps this chunk's labels to global speakers. Agreement in the overlap is
/// the strongest evidence, since both chunks heard the very same audio; voice
/// similarity links speakers who were silent there. Two labels of one chunk
/// never share a global speaker.
fn match_speakers(
    segments: &[NativeOutputSegment],
    previous: &[(i64, i64, Option<usize>)],
    overlap: ChunkSpan,
    voices: &ChunkVoices,
    speakers: &mut Vec<GlobalSpeaker>,
) -> (HashMap<String, usize>, Vec<SpeakerMatch>) {
    let labels = labels_in_order(segments);
    let talk_in_overlap = |start: i64, end: i64| intersection(start, end, overlap.start_ms, overlap.end_ms);

    let mut shared = HashMap::<(String, usize), i64>::new();
    let mut label_talk = HashMap::<String, i64>::new();
    let mut global_talk = HashMap::<usize, i64>::new();
    for segment in segments {
        let Some(label) = &segment.speaker else { continue };
        *label_talk.entry(label.clone()).or_default() += talk_in_overlap(segment.start_ms, segment.end_ms);
    }
    for &(start, end, global) in previous {
        let Some(global) = global else { continue };
        *global_talk.entry(global).or_default() += talk_in_overlap(start, end);
    }
    for segment in segments {
        let Some(label) = &segment.speaker else { continue };
        for &(start, end, global) in previous {
            let Some(global) = global else { continue };
            let both = intersection(
                segment.start_ms.max(overlap.start_ms),
                segment.end_ms.min(overlap.end_ms),
                start,
                end,
            );
            if both > 0 {
                *shared.entry((label.clone(), global)).or_default() += both;
            }
        }
    }

    let mut mapping = HashMap::new();
    let mut decisions = Vec::new();
    let mut taken = HashSet::new();

    let mut by_overlap: Vec<((String, usize), i64)> = shared
        .into_iter()
        .filter(|((label, global), both)| {
            let shorter = label_talk[label].min(global_talk[global]).max(1);
            *both >= MIN_SHARED_SPEECH_MS && *both as f64 / shorter as f64 >= MIN_SHARED_RATIO
        })
        .collect();
    by_overlap.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    for ((label, global), _) in by_overlap {
        if mapping.contains_key(&label) || taken.contains(&global) {
            continue;
        }
        taken.insert(global);
        decisions.push((label.clone(), global, "overlap"));
        mapping.insert(label, global);
    }

    let mut by_voice = Vec::<(String, usize, f32)>::new();
    for label in labels.iter().filter(|label| !mapping.contains_key(*label)) {
        let Some(voice) = voices.get(label) else { continue };
        for (global, speaker) in speakers.iter().enumerate() {
            if taken.contains(&global) {
                continue;
            }
            if let Some(known) = &speaker.voice {
                let similarity = cosine(voice, known);
                if similarity >= SAME_VOICE_SIMILARITY {
                    by_voice.push((label.clone(), global, similarity));
                }
            }
        }
    }
    by_voice.sort_by(|a, b| b.2.total_cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    for (label, global, _) in by_voice {
        if mapping.contains_key(&label) || taken.contains(&global) {
            continue;
        }
        taken.insert(global);
        decisions.push((label.clone(), global, "voice"));
        mapping.insert(label, global);
    }

    for label in labels {
        if let std::collections::hash_map::Entry::Vacant(entry) = mapping.entry(label.clone()) {
            let global = new_speaker(speakers);
            decisions.push((label, global, "new"));
            entry.insert(global);
        }
    }
    (mapping, decisions)
}

/// Where to switch from one chunk's text to the next: a moment in the overlap
/// that neither chunk spends inside a segment, nearest the middle, so no
/// sentence is cut or kept twice. Falls back to the middle of the overlap.
fn cut_point(before: &[NativeOutputSegment], after: &[NativeOutputSegment], overlap: ChunkSpan) -> i64 {
    let middle = (overlap.start_ms + overlap.end_ms) / 2;
    let inside = |at: i64, segments: &[NativeOutputSegment]| {
        segments.iter().any(|segment| segment.start_ms < at && at < segment.end_ms)
    };
    before
        .iter()
        .chain(after)
        .flat_map(|segment| [segment.start_ms, segment.end_ms])
        .filter(|at| *at > overlap.start_ms && *at < overlap.end_ms)
        .filter(|at| !inside(*at, before) && !inside(*at, after))
        .min_by_key(|at| (at - middle).abs())
        .unwrap_or(middle)
}

/// Picks up to [`VOICE_SAMPLE_MS`] of a speaker's clearest speech in a chunk:
/// the longest segments, without their edges.
pub fn voice_sample_ranges(segments: &[NativeOutputSegment], label: &str) -> Vec<(i64, i64)> {
    let mut own: Vec<(i64, i64)> = segments
        .iter()
        .filter(|segment| segment.speaker.as_deref() == Some(label))
        .map(|segment| (segment.start_ms + VOICE_EDGE_TRIM_MS, segment.end_ms - VOICE_EDGE_TRIM_MS))
        .filter(|(start, end)| end - start >= VOICE_MIN_SEGMENT_MS)
        .collect();
    own.sort_by_key(|(start, end)| std::cmp::Reverse(end - start));
    let mut total = 0;
    let mut ranges = Vec::new();
    for (start, end) in own {
        if total >= VOICE_SAMPLE_MS {
            break;
        }
        let end = end.min(start + VOICE_SAMPLE_MS - total);
        total += end - start;
        ranges.push((start, end));
    }
    ranges
}

/// Computes voice embeddings with the speaker package's embedding model.
pub struct VoiceEmbedder {
    extractor: sherpa_onnx::SpeakerEmbeddingExtractor,
}

impl VoiceEmbedder {
    pub fn load(package_path: &Path) -> Option<Self> {
        let model = package_path.join("embedding.onnx");
        if !model.is_file() {
            return None;
        }
        let config = sherpa_onnx::SpeakerEmbeddingExtractorConfig {
            model: Some(model.to_string_lossy().into_owned()),
            num_threads: std::thread::available_parallelism().map(|n| n.get().min(4) as i32).unwrap_or(1),
            debug: false,
            provider: Some("cpu".to_string()),
        };
        sherpa_onnx::SpeakerEmbeddingExtractor::create(&config).map(|extractor| Self { extractor })
    }

    /// The embedding of `ranges` (absolute milliseconds) of `samples`.
    pub fn embed(&self, samples: &[f32], sample_rate_hz: u32, ranges: &[(i64, i64)]) -> Option<Vec<f32>> {
        let rate = sample_rate_hz as i64;
        let mut audio = Vec::new();
        for &(start, end) in ranges {
            let from = ((start * rate / 1_000) as usize).min(samples.len());
            let to = ((end * rate / 1_000) as usize).min(samples.len());
            audio.extend_from_slice(&samples[from..to]);
        }
        if audio.is_empty() {
            return None;
        }
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate_hz as i32, &audio);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            return None;
        }
        self.extractor.compute(&stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Prints how similar each speaker's voice is across 10-minute windows of
    /// a finished transcript, to check [`SAME_VOICE_SIMILARITY`] on real audio:
    /// `MURMELA_VOICE_AUDIO=… MURMELA_VOICE_TRANSCRIPT=…json cargo test --release --lib voice_similarity -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn voice_similarity_on_a_real_recording() {
        let audio = std::env::var("MURMELA_VOICE_AUDIO").expect("set MURMELA_VOICE_AUDIO");
        let transcript: crate::asr::TranscriptResult = serde_json::from_slice(
            &std::fs::read(std::env::var("MURMELA_VOICE_TRANSCRIPT").expect("set MURMELA_VOICE_TRANSCRIPT")).unwrap(),
        )
        .unwrap();
        let models_dir = std::path::PathBuf::from(std::env::var("HOME").unwrap())
            .join("Library/Application Support/com.murmela.app/models");
        let package = crate::model_downloads::installed_diarization_package_path(&models_dir).expect("no speaker package");
        let embedder = VoiceEmbedder::load(&package).unwrap();
        let prepared = crate::audio_preprocess::decode_audio_file(Path::new(&audio)).unwrap();
        let mut voices = Vec::new();
        for window in 0..6 {
            let (from, to) = (window * 600_000, (window + 1) * 600_000);
            let segments: Vec<NativeOutputSegment> = transcript
                .segments
                .iter()
                .filter(|s| s.start_ms >= from && s.end_ms <= to)
                .map(|s| NativeOutputSegment {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms,
                    text: s.text.clone(),
                    speaker: s.speaker_id.clone(),
                    language_code: None,
                })
                .collect();
            for speaker in &transcript.speakers {
                let ranges = voice_sample_ranges(&segments, &speaker.speaker_id);
                if let Some(voice) = embedder.embed(&prepared.samples, prepared.sample_rate_hz, &ranges) {
                    voices.push((format!("{}@{}", speaker.speaker_id, window * 10), voice));
                }
            }
        }
        for (a, voice_a) in &voices {
            let row: Vec<String> = voices.iter().map(|(_, voice_b)| format!("{:5.2}", cosine(voice_a, voice_b))).collect();
            eprintln!("{a:>14} {}", row.join(" "));
        }
    }

    fn segment(start_ms: i64, end_ms: i64, speaker: &str, text: &str) -> NativeOutputSegment {
        NativeOutputSegment {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: Some(speaker.into()),
            language_code: None,
        }
    }

    fn chunk(start_ms: i64, end_ms: i64, segments: Vec<NativeOutputSegment>) -> ChunkTranscript {
        ChunkTranscript {
            span: ChunkSpan { start_ms, end_ms },
            result: NativeWorkerResult { text: String::new(), segments, truncated: false, warning: None },
        }
    }

    #[test]
    fn chunks_cover_the_recording_with_overlap_and_cut_where_it_is_quiet() {
        let quiet_at = 9 * 60_000 + 40_000;
        let chunks = plan_chunks(40 * 60_000, 10 * 60_000, |at| if at == quiet_at { 0.0 } else { 1.0 });
        assert_eq!(chunks[0], ChunkSpan { start_ms: 0, end_ms: quiet_at });
        assert_eq!(chunks.last().unwrap().end_ms, 40 * 60_000);
        for pair in chunks.windows(2) {
            assert_eq!(pair[1].start_ms, pair[0].end_ms - OVERLAP_MS);
            assert!(pair[0].end_ms - pair[0].start_ms <= 10 * 60_000);
        }
        assert_eq!(plan_chunks(9 * 60_000, 10 * 60_000, |_| 0.0).len(), 1);
    }

    #[test]
    fn speakers_are_matched_through_the_overlap_and_text_is_kept_once() {
        // Chunk A: 0–100 s, chunk B: 40–160 s (overlap 40–100 s).
        // B numbers the same two people the other way round.
        let a = chunk(0, 100_000, vec![
            segment(0, 30_000, "0", "Hola."),
            segment(30_000, 55_000, "1", "Buenas."),
            segment(55_000, 72_000, "0", "Qué tal."),
            segment(72_000, 98_000, "1", "Bien."),
        ]);
        let b = chunk(40_000, 160_000, vec![
            segment(0, 15_000, "A", "Buenas."),
            segment(15_000, 32_000, "B", "Qué tal."),
            segment(32_000, 58_000, "A", "Bien."),
            segment(58_000, 120_000, "B", "Me alegro."),
        ]);
        let (result, report) = stitch(vec![a, b], &[]);
        let lines: Vec<_> = result
            .segments
            .iter()
            .map(|s| (s.text.as_str(), s.speaker.as_deref().unwrap()))
            .collect();
        assert_eq!(lines, vec![("Hola.", "0"), ("Buenas.", "1"), ("Qué tal.", "0"), ("Bien.", "1"), ("Me alegro.", "0")]);
        assert_eq!(report.speakers, 2);
        assert!(report.matches[0].iter().all(|(_, _, how)| *how == "overlap"));
    }

    #[test]
    fn a_speaker_silent_in_the_overlap_is_recognised_by_voice() {
        let a = chunk(0, 100_000, vec![
            segment(0, 30_000, "0", "Primero."),
            segment(40_000, 98_000, "1", "Largo."),
        ]);
        // Speaker "0" of A speaks again only after the overlap, as B's "Y".
        let b = chunk(40_000, 160_000, vec![
            segment(0, 58_000, "X", "Largo."),
            segment(70_000, 110_000, "Y", "Vuelvo."),
        ]);
        let voices = vec![
            HashMap::from([("0".to_string(), vec![1.0, 0.0, 0.1]), ("1".to_string(), vec![0.0, 1.0, 0.0])]),
            HashMap::from([("X".to_string(), vec![0.05, 1.0, 0.0]), ("Y".to_string(), vec![0.9, 0.1, 0.1])]),
        ];
        let (result, report) = stitch(vec![a, b], &voices);
        assert_eq!(result.segments.last().unwrap().speaker.as_deref(), Some("0"));
        assert_eq!(report.speakers, 2);
        assert!(report.matches[0].contains(&("Y".to_string(), 0, "voice")));

        // Without voices the same speaker becomes a new one rather than a wrong match.
        let a = chunk(0, 100_000, vec![segment(0, 30_000, "0", "Primero."), segment(40_000, 98_000, "1", "Largo.")]);
        let b = chunk(40_000, 160_000, vec![segment(0, 58_000, "X", "Largo."), segment(70_000, 110_000, "Y", "Vuelvo.")]);
        let (result, _) = stitch(vec![a, b], &[]);
        assert_eq!(result.segments.last().unwrap().speaker.as_deref(), Some("2"));
    }

    #[test]
    fn a_placeholder_reaching_past_the_cut_is_kept_once() {
        // Every moment in the overlap (40–100 s) falls inside some segment, so
        // the cut falls back to the middle and both placeholders survive it.
        let mut a = chunk(0, 100_000, vec![
            segment(0, 45_000, "0", "Hola."),
            segment(45_000, 90_000, "", "[Speech]"),
            segment(92_000, 100_000, "0", "Y."),
        ]);
        a.result.segments[1].speaker = None;
        let mut b = chunk(40_000, 160_000, vec![
            segment(0, 22_000, "Z", "Hola."),
            segment(22_000, 56_000, "", "[Speech]"),
            segment(60_000, 90_000, "Z", "Sigue."),
        ]);
        b.result.segments[1].speaker = None;
        let (result, _) = stitch(vec![a, b], &[]);
        let texts: Vec<_> = result.segments.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(texts, vec!["Hola.", "[Speech]", "Sigue."]);
    }

    #[test]
    fn different_voices_are_not_merged() {
        let a = chunk(0, 100_000, vec![segment(0, 30_000, "0", "Uno.")]);
        let b = chunk(40_000, 160_000, vec![segment(70_000, 110_000, "Y", "Otra persona.")]);
        let voices = vec![
            HashMap::from([("0".to_string(), vec![1.0, 0.0])]),
            HashMap::from([("Y".to_string(), vec![0.0, 1.0])]),
        ];
        let (result, _) = stitch(vec![a, b], &voices);
        assert_eq!(result.segments.last().unwrap().speaker.as_deref(), Some("1"));
    }

    #[test]
    fn voice_samples_prefer_long_segments_without_edges() {
        let segments = vec![
            segment(0, 500, "0", "Sí."),
            segment(1_000, 21_000, "0", "Largo."),
            segment(30_000, 33_000, "1", "Otro."),
        ];
        assert_eq!(voice_sample_ranges(&segments, "0"), vec![(1_150, 20_850)]);
    }
}
