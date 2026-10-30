//! Who said what in a recorded call.
//!
//! A call is recorded as two channels on one timeline: the microphone (this
//! Mac's user) and the sound of the call app (everyone else). That split is
//! certain, so every microphone passage belongs to "You" and every call-app
//! passage to the other side. Within the other side, speakers are told apart
//! by acoustic diarization (Pro), by the call's own active-speaker signal
//! from the browser helper, or by both. The browser signal also carries the
//! participants' display names.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::asr::{TranscriptResult, TranscriptSegment, TranscriptWarning};
use crate::diarization::{
    normalize_turns, DiarizationSource, DiarizationStatus, DiarizationTurn, RawDiarizationTurn,
    TranscriptSpeaker,
};
use crate::speaker_reconciliation::{
    reconcile_segment, SpeakerAttribution, DIARIZATION_POLICY_VERSION,
};

pub const LOCAL_SPEAKER_ID: &str = "speaker_me";
pub const REMOTE_SPEAKER_ID: &str = "speaker_remote";
pub const DEFAULT_LOCAL_NAME: &str = "You";
pub const DEFAULT_REMOTE_NAME: &str = "Other side";
/// Marks a transcript whose speakers come from the recorded channels.
pub const MEETING_DIARIZATION_MODEL_ID: &str = "meeting-channels-v1";

/// An active-speaker indicator turns on about when the voice starts and lingers a little after it.
const ACTIVE_LEAD_MS: i64 = 250;
const ACTIVE_TAIL_MS: i64 = 500;
/// A caption appears one to two seconds after the words were spoken.
const CAPTION_LEAD_MS: i64 = 2_500;
const CAPTION_TAIL_MS: i64 = 250;
/// The browser helper repeats "speaking" every few seconds; a span with no news is closed after this.
const OPEN_SPAN_GRACE_MS: i64 = 6_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerEventKind {
    /// The participant's tile shows them speaking (sent again while they keep speaking).
    Speaking,
    /// Their speaking indicator turned off.
    Stopped,
    /// A live caption attributed to them appeared.
    Caption,
}

/// One observation from the call app, on the recording's timeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerEvent {
    pub at_ms: i64,
    pub name: String,
    pub kind: SpeakerEventKind,
    #[serde(default)]
    pub is_self: bool,
}

/// A stretch of the recording during which the call app said `name` was speaking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakerSpan {
    pub start_ms: i64,
    pub end_ms: i64,
    pub name: String,
}

/// Turns the browser helper's events for other participants into speaking spans.
pub fn spans_from_events(events: &[SpeakerEvent], end_ms: i64) -> Vec<SpeakerSpan> {
    let mut sorted: Vec<&SpeakerEvent> = events.iter().filter(|event| !event.is_self).collect();
    sorted.sort_by_key(|event| event.at_ms);
    // name -> (started, last heard)
    let mut open: HashMap<String, (i64, i64)> = HashMap::new();
    let mut spans = Vec::new();
    let close = |spans: &mut Vec<SpeakerSpan>, name: &str, start: i64, end: i64| {
        let start = (start - ACTIVE_LEAD_MS).max(0);
        let end = (end + ACTIVE_TAIL_MS).min(end_ms.max(start));
        if end > start {
            spans.push(SpeakerSpan {
                start_ms: start,
                end_ms: end,
                name: name.to_string(),
            });
        }
    };
    for event in sorted {
        let name = clean_name(&event.name);
        if name.is_empty() {
            continue;
        }
        match event.kind {
            SpeakerEventKind::Speaking => match open.get_mut(&name) {
                Some((start, last)) if event.at_ms - *last <= OPEN_SPAN_GRACE_MS => {
                    *last = event.at_ms;
                }
                Some((start, last)) => {
                    let (start, last) = (*start, *last);
                    close(&mut spans, &name, start, last + OPEN_SPAN_GRACE_MS / 2);
                    open.insert(name, (event.at_ms, event.at_ms));
                }
                None => {
                    open.insert(name, (event.at_ms, event.at_ms));
                }
            },
            SpeakerEventKind::Stopped => {
                if let Some((start, _)) = open.remove(&name) {
                    close(&mut spans, &name, start, event.at_ms);
                }
            }
            SpeakerEventKind::Caption => {
                let start = (event.at_ms - CAPTION_LEAD_MS).max(0);
                let end = (event.at_ms + CAPTION_TAIL_MS).min(end_ms.max(start));
                if end > start {
                    spans.push(SpeakerSpan {
                        start_ms: start,
                        end_ms: end,
                        name,
                    });
                }
            }
        }
    }
    for (name, (start, last)) in open {
        close(
            &mut spans,
            &name,
            start,
            (last + OPEN_SPAN_GRACE_MS / 2).min(end_ms),
        );
    }
    spans.sort_by(|a, b| {
        a.start_ms
            .cmp(&b.start_ms)
            .then_with(|| a.name.cmp(&b.name))
    });
    spans
}

/// The name the call app shows for this Mac's user, if the browser helper saw it.
pub fn self_name(events: &[SpeakerEvent]) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for event in events.iter().filter(|event| event.is_self) {
        let name = clean_name(&event.name);
        if !name.is_empty() {
            *counts.entry(name).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0)))
        .map(|(name, _)| name)
}

pub fn clean_name(name: &str) -> String {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(80).collect()
}

pub struct AssembleInput {
    pub job_id: String,
    pub model_name: String,
    /// The microphone channel's transcript.
    pub local: Option<TranscriptResult>,
    /// The call app channel's transcript.
    pub remote: Option<TranscriptResult>,
    /// Acoustic diarization of the call app channel, when it ran.
    pub remote_turns: Option<Vec<RawDiarizationTurn>>,
    /// Who the call app said was speaking, without this Mac's user.
    pub spans: Vec<SpeakerSpan>,
    pub local_name: Option<String>,
    /// Pro: tell the other participants apart instead of grouping them as one.
    pub separate_remote_speakers: bool,
}

/// Merges both channels into one transcript with speakers.
pub fn assemble(input: AssembleInput) -> TranscriptResult {
    let mut warnings: Vec<TranscriptWarning> = Vec::new();
    let mut local_segments = Vec::new();
    let mut remote_segments = Vec::new();
    if let Some(local) = input.local {
        warnings.extend(local.warnings);
        local_segments = local.segments;
    }
    if let Some(remote) = input.remote {
        warnings.extend(remote.warnings);
        remote_segments = remote.segments;
    }
    let local_segments = drop_bleed(drop_echo(local_segments, &remote_segments), &remote_segments);
    let (spans, inferred_self) = split_self_spans(input.spans, &local_segments, &remote_segments);

    let mut speakers: Vec<TranscriptSpeaker> = Vec::new();
    let mut turns: Vec<DiarizationTurn> = Vec::new();

    let local_name = input
        .local_name
        .or(inferred_self)
        .map(|name| clean_name(&name))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| DEFAULT_LOCAL_NAME.to_string());
    if !local_segments.is_empty() {
        speakers.push(TranscriptSpeaker {
            speaker_id: LOCAL_SPEAKER_ID.to_string(),
            display_name: local_name,
            speaker_order: 0,
        });
    }
    let mut segments: Vec<TranscriptSegment> = local_segments
        .into_iter()
        .map(|mut segment| {
            assign(&mut segment, LOCAL_SPEAKER_ID, 1.0);
            turns.push(turn_for(&segment, LOCAL_SPEAKER_ID));
            segment
        })
        .collect();

    let acoustic = input
        .remote_turns
        .filter(|_| input.separate_remote_speakers)
        .map(normalize_turns)
        .filter(|turns| !turns.is_empty());
    let span_names = distinct_names(&spans);
    let remote = if let Some(acoustic) = acoustic {
        attribute_with_diarization(remote_segments, acoustic, &spans)
    } else if input.separate_remote_speakers && span_names.len() >= 2 {
        attribute_with_spans(remote_segments, &spans)
    } else {
        attribute_as_one(remote_segments, &spans)
    };
    let mut order = speakers.len() as i32;
    for mut speaker in remote.speakers {
        if remote
            .segments
            .iter()
            .any(|segment| mentions(segment, &speaker.speaker_id))
            || remote
                .turns
                .iter()
                .any(|turn| turn.speaker_ids.contains(&speaker.speaker_id))
        {
            speaker.speaker_order = order;
            order += 1;
            speakers.push(speaker);
        }
    }
    turns.extend(remote.turns);
    segments.extend(remote.segments);

    segments.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.end_ms.cmp(&b.end_ms)));
    turns.sort_by(|a, b| a.start_ms.cmp(&b.start_ms).then(a.end_ms.cmp(&b.end_ms)));
    for (index, turn) in turns.iter_mut().enumerate() {
        turn.id = format!("turn_{index}");
        turn.turn_order = index as i32;
    }
    let uncertain = segments
        .iter()
        .any(|segment| !matches!(segment.speaker_attribution, SpeakerAttribution::Assigned));
    // Each channel listed its own unclear stretches; list the ones left once.
    let covered: Vec<(i64, i64)> = warnings
        .iter()
        .filter(|warning| !crate::native_sounds::is_unclear_warning(warning))
        .map(|warning| (warning.start_ms, warning.end_ms))
        .collect();
    warnings.retain(|warning| !crate::native_sounds::is_unclear_warning(warning));
    let unclear: Vec<(i64, i64)> = segments
        .iter()
        .filter(|segment| crate::native_sounds::is_unclear_mark(&segment.text))
        .map(|segment| (segment.start_ms, segment.end_ms))
        .filter(|&(start, end)| !covered.iter().any(|&(from, to)| start >= from && end <= to))
        .collect();
    warnings.extend(crate::native_sounds::unclear_warning(&unclear));
    let mut result = crate::asr::build_transcript_result_named(
        input.job_id,
        &input.model_name,
        segments,
        warnings,
    );
    result.speakers = speakers;
    result.diarization_turns = turns;
    result.diarization_source = DiarizationSource::PostProcess;
    result.diarization_model_id = Some(MEETING_DIARIZATION_MODEL_ID.to_string());
    result.diarization_policy_version = Some(DIARIZATION_POLICY_VERSION);
    result.diarization_status = if result.segments.is_empty() {
        DiarizationStatus::NotEnoughSpeech
    } else if uncertain {
        DiarizationStatus::CompletedWithUncertainty
    } else {
        DiarizationStatus::Completed
    };
    result
}

struct RemoteAttribution {
    segments: Vec<TranscriptSegment>,
    speakers: Vec<TranscriptSpeaker>,
    turns: Vec<DiarizationTurn>,
}

/// Everyone on the other side as one speaker, named when the call app showed exactly one other person.
fn attribute_as_one(segments: Vec<TranscriptSegment>, spans: &[SpeakerSpan]) -> RemoteAttribution {
    let names = distinct_names(spans);
    let spoken_ms: i64 = spans.iter().map(|span| span.end_ms - span.start_ms).sum();
    let display_name = if names.len() == 1 && spoken_ms >= 3_000 {
        names[0].clone()
    } else {
        DEFAULT_REMOTE_NAME.to_string()
    };
    let mut turns = Vec::new();
    let segments = segments
        .into_iter()
        .map(|mut segment| {
            assign(&mut segment, REMOTE_SPEAKER_ID, 1.0);
            turns.push(turn_for(&segment, REMOTE_SPEAKER_ID));
            segment
        })
        .collect();
    RemoteAttribution {
        segments,
        speakers: vec![TranscriptSpeaker {
            speaker_id: REMOTE_SPEAKER_ID.to_string(),
            display_name,
            speaker_order: 0,
        }],
        turns,
    }
}

/// Speakers straight from the call app's active-speaker signal, one per name.
fn attribute_with_spans(
    segments: Vec<TranscriptSegment>,
    spans: &[SpeakerSpan],
) -> RemoteAttribution {
    let names = distinct_names(spans);
    let ids: HashMap<&str, String> = names
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), format!("speaker_app_{index}")))
        .collect();
    let mut turns = atomic_turns(spans, &ids);
    let mut speakers: Vec<TranscriptSpeaker> = names
        .iter()
        .map(|name| TranscriptSpeaker {
            speaker_id: ids[name.as_str()].clone(),
            display_name: name.clone(),
            speaker_order: 0,
        })
        .collect();
    let mut needs_fallback = false;
    let segments = segments
        .into_iter()
        .map(|mut segment| {
            let attribution = reconcile_segment(segment.start_ms, segment.end_ms, &turns);
            if attribution.attribution == SpeakerAttribution::None {
                // Nobody was shown speaking: it is still certainly the other side.
                assign(&mut segment, REMOTE_SPEAKER_ID, 1.0);
                needs_fallback = true;
            } else {
                apply(&mut segment, attribution);
            }
            segment
        })
        .collect::<Vec<_>>();
    if needs_fallback {
        speakers.push(TranscriptSpeaker {
            speaker_id: REMOTE_SPEAKER_ID.to_string(),
            display_name: DEFAULT_REMOTE_NAME.to_string(),
            speaker_order: 0,
        });
        for segment in segments
            .iter()
            .filter(|segment| mentions(segment, REMOTE_SPEAKER_ID))
        {
            turns.push(turn_for(segment, REMOTE_SPEAKER_ID));
        }
    }
    RemoteAttribution {
        segments,
        speakers,
        turns,
    }
}

/// Acoustic clusters, named by the call app where its signal agrees, and corrected by it
/// where diarization was unsure or heard two different named people as one voice.
fn attribute_with_diarization(
    segments: Vec<TranscriptSegment>,
    turns: Vec<DiarizationTurn>,
    spans: &[SpeakerSpan],
) -> RemoteAttribution {
    let mut segments: Vec<TranscriptSegment> = segments
        .into_iter()
        .map(|mut segment| {
            let attribution = reconcile_segment(segment.start_ms, segment.end_ms, &turns);
            if attribution.attribution == SpeakerAttribution::None {
                assign(&mut segment, REMOTE_SPEAKER_ID, 1.0);
            } else {
                apply(&mut segment, attribution);
            }
            segment
        })
        .collect();
    let mut speakers = crate::diarization::speakers_from_turns(&turns);
    let names = name_clusters(&segments, spans);
    for speaker in &mut speakers {
        if let Some(name) = names.get(&speaker.speaker_id) {
            speaker.display_name = name.clone();
        }
    }
    let mut extra_turns = Vec::new();
    for segment in &mut segments {
        let Some((name, coverage)) = dominant_span_name(segment, spans) else {
            continue;
        };
        if coverage < 0.8 {
            continue;
        }
        let current = segment
            .speaker_id
            .as_deref()
            .and_then(|id| speakers.iter().find(|speaker| speaker.speaker_id == id));
        let current_is_named =
            current.is_some_and(|speaker| names.contains_key(&speaker.speaker_id));
        let agrees = current.is_some_and(|speaker| speaker.display_name == name);
        let unsure = segment.speaker_attribution != SpeakerAttribution::Assigned
            || segment.speaker_id.as_deref() == Some(REMOTE_SPEAKER_ID);
        if agrees || !(unsure || current_is_named) {
            continue;
        }
        let target = match speakers.iter().find(|speaker| speaker.display_name == name) {
            Some(speaker) => speaker.speaker_id.clone(),
            None => {
                let id = format!("speaker_app_{}", speakers.len());
                speakers.push(TranscriptSpeaker {
                    speaker_id: id.clone(),
                    display_name: name.clone(),
                    speaker_order: 0,
                });
                id
            }
        };
        assign(segment, &target, coverage as f32);
        extra_turns.push(turn_for(segment, &target));
    }
    if segments
        .iter()
        .any(|segment| mentions(segment, REMOTE_SPEAKER_ID))
    {
        speakers.push(TranscriptSpeaker {
            speaker_id: REMOTE_SPEAKER_ID.to_string(),
            display_name: DEFAULT_REMOTE_NAME.to_string(),
            speaker_order: 0,
        });
        for segment in segments
            .iter()
            .filter(|segment| mentions(segment, REMOTE_SPEAKER_ID))
        {
            extra_turns.push(turn_for(segment, REMOTE_SPEAKER_ID));
        }
    }
    let mut turns = turns;
    turns.extend(extra_turns);
    RemoteAttribution {
        segments,
        speakers,
        turns,
    }
}

/// Finds this computer's user among the names the call app reported: their spans
/// line up with the microphone, not with the call's sound. Teams and Zoom do not mark
/// "you", so without this the user's own name would compete for the other side's voices.
/// Returns the other participants' spans and the user's name, if one stood out.
pub fn split_self_spans(
    spans: Vec<SpeakerSpan>,
    local: &[TranscriptSegment],
    remote: &[TranscriptSegment],
) -> (Vec<SpeakerSpan>, Option<String>) {
    let overlap = |span: &SpeakerSpan, segments: &[TranscriptSegment]| -> i64 {
        segments
            .iter()
            .map(|segment| {
                intersection(span.start_ms, span.end_ms, segment.start_ms, segment.end_ms)
            })
            .sum()
    };
    let mut evidence: BTreeMap<String, (i64, i64)> = BTreeMap::new();
    for span in &spans {
        let entry = evidence.entry(span.name.clone()).or_default();
        entry.0 += overlap(span, local);
        entry.1 += overlap(span, remote);
    }
    let own = evidence
        .into_iter()
        .filter(|(_, (mine, theirs))| *mine >= 1_500 && *mine >= 2 * *theirs)
        .max_by_key(|(_, (mine, _))| *mine)
        .map(|(name, _)| name);
    let spans = spans
        .into_iter()
        .filter(|span| Some(&span.name) != own.as_ref())
        .collect();
    (spans, own)
}

/// Gives each acoustic cluster the name the call app showed most while it spoke,
/// if that name clearly dominates. Each name goes to at most one cluster.
pub fn name_clusters(
    segments: &[TranscriptSegment],
    spans: &[SpeakerSpan],
) -> HashMap<String, String> {
    let mut evidence: BTreeMap<String, BTreeMap<String, i64>> = BTreeMap::new();
    for segment in segments {
        if !matches!(
            segment.speaker_attribution,
            SpeakerAttribution::Assigned | SpeakerAttribution::Likely
        ) {
            continue;
        }
        let Some(cluster) = segment.speaker_id.as_ref() else {
            continue;
        };
        for span in spans {
            let overlap =
                intersection(segment.start_ms, segment.end_ms, span.start_ms, span.end_ms);
            if overlap > 0 {
                *evidence
                    .entry(cluster.clone())
                    .or_default()
                    .entry(span.name.clone())
                    .or_default() += overlap;
            }
        }
    }
    let mut candidates = Vec::new();
    for (cluster, names) in &evidence {
        let total: i64 = names.values().sum();
        for (name, ms) in names {
            candidates.push((
                cluster.clone(),
                name.clone(),
                *ms,
                *ms as f64 / total.max(1) as f64,
            ));
        }
    }
    candidates.sort_by(|a, b| {
        b.2.cmp(&a.2)
            .then_with(|| a.0.cmp(&b.0))
            .then_with(|| a.1.cmp(&b.1))
    });
    let mut named = HashMap::new();
    let mut used = HashSet::new();
    for (cluster, name, ms, share) in candidates {
        if ms < 2_000 || share < 0.5 || named.contains_key(&cluster) || used.contains(&name) {
            continue;
        }
        used.insert(name.clone());
        named.insert(cluster, name);
    }
    named
}

/// Drops microphone passages that only repeat the call's sound, as happens when
/// the call plays through loudspeakers instead of headphones.
/// Unclear speech on the microphone while the other side is talking is the
/// other side heard through the microphone, not this Mac's user.
pub fn drop_bleed(
    local: Vec<TranscriptSegment>,
    remote: &[TranscriptSegment],
) -> Vec<TranscriptSegment> {
    local
        .into_iter()
        .filter(|segment| {
            if !crate::native_sounds::is_unclear_mark(&segment.text) {
                return true;
            }
            let length = (segment.end_ms - segment.start_ms).max(1);
            let talking: i64 = remote
                .iter()
                .filter(|other| !crate::native_sounds::is_unclear_mark(&other.text))
                .map(|other| {
                    (other.end_ms.min(segment.end_ms) - other.start_ms.max(segment.start_ms)).max(0)
                })
                .sum();
            talking * 2 < length
        })
        .collect()
}

pub fn drop_echo(
    local: Vec<TranscriptSegment>,
    remote: &[TranscriptSegment],
) -> Vec<TranscriptSegment> {
    const WINDOW_MS: i64 = 1_500;
    local
        .into_iter()
        .filter(|segment| {
            let words = tokens(&segment.text);
            if words.is_empty() {
                return false;
            }
            let nearby: Vec<&TranscriptSegment> = remote
                .iter()
                .filter(|other| {
                    other.start_ms <= segment.end_ms + WINDOW_MS
                        && other.end_ms >= segment.start_ms - WINDOW_MS
                })
                .collect();
            if nearby.is_empty() {
                return true;
            }
            let heard: HashSet<String> = nearby
                .iter()
                .flat_map(|other| tokens(&other.text))
                .collect();
            if words.len() <= 2 {
                // Too short to judge by overlap: only an exact repeat at the same moment is echo.
                let tight = nearby.iter().any(|other| {
                    other.start_ms <= segment.end_ms + 800
                        && other.end_ms >= segment.start_ms - 800
                        && tokens(&other.text) == words
                });
                return !tight;
            }
            let repeated = words.iter().filter(|word| heard.contains(*word)).count();
            (repeated as f64 / words.len() as f64) < 0.6
        })
        .collect()
}

fn tokens(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn distinct_names(spans: &[SpeakerSpan]) -> Vec<String> {
    let mut names = Vec::new();
    for span in spans {
        if !names.contains(&span.name) {
            names.push(span.name.clone());
        }
    }
    names
}

fn dominant_span_name(segment: &TranscriptSegment, spans: &[SpeakerSpan]) -> Option<(String, f64)> {
    let duration = (segment.end_ms - segment.start_ms).max(0);
    if duration == 0 {
        return None;
    }
    let mut coverage: HashMap<&str, i64> = HashMap::new();
    for span in spans {
        let overlap = intersection(segment.start_ms, segment.end_ms, span.start_ms, span.end_ms);
        if overlap > 0 {
            *coverage.entry(span.name.as_str()).or_default() += overlap;
        }
    }
    let mut ranked: Vec<_> = coverage.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let (name, top) = ranked.first()?;
    let second = ranked.get(1).map(|entry| entry.1).unwrap_or(0);
    // Two people shown speaking over the same passage: let diarization decide.
    if second as f64 / duration as f64 > 0.2 {
        return None;
    }
    Some((name.to_string(), (*top as f64 / duration as f64).min(1.0)))
}

/// Splits possibly overlapping spans into non-overlapping turns, as diarization produces.
fn atomic_turns(spans: &[SpeakerSpan], ids: &HashMap<&str, String>) -> Vec<DiarizationTurn> {
    let mut bounds: Vec<i64> = spans
        .iter()
        .flat_map(|span| [span.start_ms, span.end_ms])
        .collect();
    bounds.sort_unstable();
    bounds.dedup();
    let mut turns: Vec<DiarizationTurn> = Vec::new();
    for window in bounds.windows(2) {
        let (start, end) = (window[0], window[1]);
        let mut active: Vec<String> = spans
            .iter()
            .filter(|span| span.start_ms <= start && span.end_ms >= end)
            .filter_map(|span| ids.get(span.name.as_str()).cloned())
            .collect();
        active.sort();
        active.dedup();
        if active.is_empty() {
            continue;
        }
        if let Some(last) = turns.last_mut() {
            if last.end_ms == start && last.speaker_ids == active {
                last.end_ms = end;
                continue;
            }
        }
        turns.push(DiarizationTurn {
            id: String::new(),
            start_ms: start,
            end_ms: end,
            is_overlap: active.len() > 1,
            speaker_ids: active,
            confidence: None,
            is_uncertain: false,
            turn_order: 0,
        });
    }
    turns
}

fn assign(segment: &mut TranscriptSegment, speaker_id: &str, confidence: f32) {
    segment.speaker_id = Some(speaker_id.to_string());
    segment.speaker_ids = Some(vec![speaker_id.to_string()]);
    segment.speaker_attribution = SpeakerAttribution::Assigned;
    segment.speaker_confidence = Some(confidence);
}

fn apply(
    segment: &mut TranscriptSegment,
    attribution: crate::speaker_reconciliation::SegmentAttribution,
) {
    segment.speaker_id = attribution.speaker_id;
    segment.speaker_ids = (!attribution.speaker_ids.is_empty()).then_some(attribution.speaker_ids);
    segment.speaker_attribution = attribution.attribution;
    segment.speaker_confidence = attribution.confidence;
}

fn mentions(segment: &TranscriptSegment, speaker_id: &str) -> bool {
    segment.speaker_id.as_deref() == Some(speaker_id)
        || segment
            .speaker_ids
            .as_ref()
            .is_some_and(|ids| ids.iter().any(|id| id == speaker_id))
}

fn turn_for(segment: &TranscriptSegment, speaker_id: &str) -> DiarizationTurn {
    DiarizationTurn {
        id: String::new(),
        start_ms: segment.start_ms,
        end_ms: segment.end_ms,
        speaker_ids: vec![speaker_id.to_string()],
        confidence: Some(1.0),
        is_overlap: false,
        is_uncertain: false,
        turn_order: 0,
    }
}

fn intersection(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
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
            text: text.to_string(),
            language_code: "en".into(),
            segment_order: 0,
            confidence: None,
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: SpeakerAttribution::None,
            speaker_confidence: None,
        }
    }

    fn channel(segments: Vec<TranscriptSegment>) -> Option<TranscriptResult> {
        Some(crate::asr::build_transcript_result_named(
            "channel".into(),
            "Whisper",
            segments,
            vec![],
        ))
    }

    fn span(start_ms: i64, end_ms: i64, name: &str) -> SpeakerSpan {
        SpeakerSpan {
            start_ms,
            end_ms,
            name: name.into(),
        }
    }

    fn input(local: Vec<TranscriptSegment>, remote: Vec<TranscriptSegment>) -> AssembleInput {
        AssembleInput {
            job_id: "call".into(),
            model_name: "Whisper".into(),
            local: channel(local),
            remote: channel(remote),
            remote_turns: None,
            spans: vec![],
            local_name: None,
            separate_remote_speakers: false,
        }
    }

    fn names(result: &TranscriptResult) -> Vec<(String, String)> {
        result
            .segments
            .iter()
            .map(|segment| {
                let speaker = result
                    .speakers
                    .iter()
                    .find(|speaker| Some(&speaker.speaker_id) == segment.speaker_id.as_ref())
                    .map(|speaker| speaker.display_name.clone())
                    .unwrap_or_else(|| "?".into());
                (speaker, segment.text.clone())
            })
            .collect()
    }

    #[test]
    fn unclear_speech_on_the_microphone_while_the_other_side_talks_is_dropped() {
        let mut local = channel(vec![
            segment(0, 2_000, "Hello, thanks for joining."),
            // The other side, heard faintly through the microphone.
            segment(2_000, 9_000, "[Unclear audio 00:02–00:09]"),
            // Something unclear while nobody else talked stays.
            segment(20_000, 60_000, "[Unclear audio 00:20–01:00]"),
        ])
        .unwrap();
        local.warnings = crate::native_sounds::unclear_warning(&[(2_000, 9_000), (20_000, 60_000)])
            .into_iter()
            .collect();
        let mut result_input = input(vec![], vec![segment(2_500, 8_500, "Happy to be here today.")]);
        result_input.local = Some(local);
        let result = assemble(result_input);
        assert_eq!(
            names(&result),
            vec![
                ("You".to_string(), "Hello, thanks for joining.".to_string()),
                ("Other side".to_string(), "Happy to be here today.".to_string()),
                ("You".to_string(), "[Unclear audio 00:20–01:00]".to_string()),
            ]
        );
        assert_eq!(result.warnings.len(), 1);
        assert_eq!(
            result.warnings[0].reason,
            "Unclear speech in one place (00:20). Listen there to fill it in."
        );
        assert_eq!(result.quality_status, TranscriptQualityStatus::Partial);
    }

    #[test]
    fn channels_become_you_and_the_other_side_in_time_order() {
        let result = assemble(input(
            vec![
                segment(0, 2_000, "Hello, thanks for joining."),
                segment(6_000, 8_000, "Of course."),
            ],
            vec![segment(2_500, 5_500, "Happy to be here today.")],
        ));
        assert_eq!(
            names(&result),
            [
                ("You".into(), "Hello, thanks for joining.".into()),
                ("Other side".into(), "Happy to be here today.".into()),
                ("You".into(), "Of course.".into()),
            ]
        );
        assert_eq!(result.diarization_status, DiarizationStatus::Completed);
        assert_eq!(result.speakers.len(), 2);
        assert_eq!(result.speakers[0].speaker_id, LOCAL_SPEAKER_ID);
        assert_eq!(result.diarization_turns.len(), 3);
        assert!(result
            .segments
            .iter()
            .all(|s| s.speaker_attribution == SpeakerAttribution::Assigned));
        assert_eq!(
            result.plain_text,
            "Hello, thanks for joining. Happy to be here today. Of course."
        );
        assert_eq!(result.quality_status, TranscriptQualityStatus::Clean);
    }

    #[test]
    fn loudspeaker_echo_on_the_microphone_is_dropped() {
        let local = vec![
            // The microphone picked up the other side through the speakers.
            segment(2_700, 5_600, "happy to be here today"),
            // Real speech that happens to share a word.
            segment(9_000, 11_000, "Today we look at your contract."),
            // A short exact repeat at the same moment.
            segment(12_100, 12_600, "Okay."),
            // A short answer of our own while they talk about something else.
            segment(14_000, 14_400, "Right."),
        ];
        let remote = vec![
            segment(2_500, 5_500, "Happy to be here today."),
            segment(12_000, 12_500, "Okay."),
            segment(13_800, 15_000, "So the second point."),
        ];
        let kept: Vec<String> = drop_echo(local, &remote)
            .into_iter()
            .map(|s| s.text)
            .collect();
        assert_eq!(kept, ["Today we look at your contract.", "Right."]);
    }

    #[test]
    fn a_single_participant_seen_in_the_browser_names_the_other_side() {
        let mut call = input(
            vec![segment(0, 1_000, "Hi Anna.")],
            vec![segment(1_500, 6_000, "Hi, good to see you.")],
        );
        call.spans = vec![span(1_300, 6_200, "Anna Weber")];
        call.local_name = Some("  Ada   L ".into());
        let result = assemble(call);
        assert_eq!(
            names(&result),
            [
                ("Ada L".into(), "Hi Anna.".into()),
                ("Anna Weber".into(), "Hi, good to see you.".into())
            ]
        );
    }

    #[test]
    fn the_users_own_name_is_recognised_by_the_microphone_and_kept_off_the_other_side() {
        // Teams captions name everyone, the user included, and mark no one as "you".
        let mut call = input(
            vec![
                segment(0, 4_000, "Good morning, let's begin."),
                segment(9_000, 12_000, "Sounds good."),
            ],
            vec![segment(4_500, 8_500, "Morning! I sent the papers.")],
        );
        call.spans = vec![
            span(0, 4_200, "Jo Example"),
            span(4_400, 8_600, "Ada Lovelace"),
            span(8_900, 12_100, "Jo Example"),
        ];
        call.separate_remote_speakers = true;
        let result = assemble(call);
        assert_eq!(
            names(&result)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["Jo Example", "Ada Lovelace", "Jo Example"]
        );
    }

    #[test]
    fn without_pro_several_participants_stay_one_other_side() {
        let mut call = input(
            vec![],
            vec![
                segment(0, 3_000, "First."),
                segment(4_000, 7_000, "Second."),
            ],
        );
        call.spans = vec![span(0, 3_000, "Anna"), span(4_000, 7_000, "Ben")];
        let result = assemble(call);
        assert_eq!(result.speakers.len(), 1);
        assert_eq!(result.speakers[0].display_name, "Other side");
    }

    #[test]
    fn browser_spans_alone_separate_and_name_participants() {
        let mut call = input(
            vec![segment(8_000, 9_000, "Thanks both.")],
            vec![
                segment(0, 3_000, "I'll start."),
                segment(3_500, 7_000, "Then I'll add something."),
                segment(10_000, 12_000, "Nobody was shown speaking here."),
            ],
        );
        call.spans = vec![span(0, 3_200, "Anna"), span(3_300, 7_200, "Ben")];
        call.separate_remote_speakers = true;
        let result = assemble(call);
        assert_eq!(
            names(&result),
            [
                ("Anna".into(), "I'll start.".into()),
                ("Ben".into(), "Then I'll add something.".into()),
                ("You".into(), "Thanks both.".into()),
                (
                    "Other side".into(),
                    "Nobody was shown speaking here.".into()
                ),
            ]
        );
        let ids: HashSet<_> = result
            .speakers
            .iter()
            .map(|s| s.speaker_id.clone())
            .collect();
        for turn in &result.diarization_turns {
            assert!(
                turn.speaker_ids.iter().all(|id| ids.contains(id)),
                "turn uses an unknown speaker"
            );
        }
    }

    #[test]
    fn diarization_clusters_are_named_by_the_browser_and_corrected_where_unsure() {
        let remote = vec![
            segment(0, 4_000, "Cluster zero speaks."),
            segment(5_000, 9_000, "Cluster one speaks."),
            segment(10_000, 14_000, "Cluster zero again."),
            // Diarization lumped this into cluster zero, but the call showed Ben.
            segment(15_000, 18_000, "Actually this was Ben."),
        ];
        let raw = vec![
            RawDiarizationTurn {
                start_ms: 0,
                end_ms: 4_000,
                cluster_ids: vec![7],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 5_000,
                end_ms: 9_000,
                cluster_ids: vec![3],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 10_000,
                end_ms: 14_000,
                cluster_ids: vec![7],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 15_000,
                end_ms: 18_000,
                cluster_ids: vec![7],
                confidence: None,
            },
        ];
        let mut call = input(vec![], remote);
        call.remote_turns = Some(raw);
        call.separate_remote_speakers = true;
        call.spans = vec![
            span(0, 4_100, "Anna"),
            span(4_900, 9_100, "Ben"),
            span(9_900, 14_100, "Anna"),
            span(14_900, 18_100, "Ben"),
        ];
        let result = assemble(call);
        assert_eq!(
            names(&result)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["Anna", "Ben", "Anna", "Ben"]
        );
    }

    #[test]
    fn diarization_without_browser_keeps_numbered_speakers() {
        let raw = vec![
            RawDiarizationTurn {
                start_ms: 0,
                end_ms: 4_000,
                cluster_ids: vec![1],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 5_000,
                end_ms: 9_000,
                cluster_ids: vec![2],
                confidence: None,
            },
        ];
        let mut call = input(
            vec![segment(9_500, 10_000, "Thank you.")],
            vec![segment(0, 4_000, "A."), segment(5_000, 9_000, "B.")],
        );
        call.remote_turns = Some(raw);
        call.separate_remote_speakers = true;
        let result = assemble(call);
        assert_eq!(
            names(&result)
                .into_iter()
                .map(|(name, _)| name)
                .collect::<Vec<_>>(),
            ["Speaker 1", "Speaker 2", "You"]
        );
        assert_eq!(result.speakers[0].display_name, "You");
    }

    #[test]
    fn events_become_spans_with_heartbeats_stops_and_captions() {
        let events = vec![
            SpeakerEvent {
                at_ms: 1_000,
                name: "Anna".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: false,
            },
            SpeakerEvent {
                at_ms: 4_000,
                name: "Anna".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: false,
            },
            SpeakerEvent {
                at_ms: 6_000,
                name: "Anna".into(),
                kind: SpeakerEventKind::Stopped,
                is_self: false,
            },
            SpeakerEvent {
                at_ms: 2_000,
                name: "Me".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: true,
            },
            SpeakerEvent {
                at_ms: 20_000,
                name: "Ben".into(),
                kind: SpeakerEventKind::Caption,
                is_self: false,
            },
            // Speaking with no stop or heartbeat afterwards: closed after a grace period.
            SpeakerEvent {
                at_ms: 30_000,
                name: "Cara".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: false,
            },
        ];
        let spans = spans_from_events(&events, 100_000);
        assert_eq!(
            spans,
            [
                span(750, 6_500, "Anna"),
                span(17_500, 20_250, "Ben"),
                span(29_750, 33_500, "Cara"),
            ]
        );
        assert_eq!(self_name(&events).as_deref(), Some("Me"));
    }

    #[test]
    fn name_clusters_needs_clear_evidence_and_gives_each_name_once() {
        let mut a = segment(0, 10_000, "a");
        a.speaker_id = Some("speaker_0".into());
        a.speaker_attribution = SpeakerAttribution::Assigned;
        let mut b = segment(10_000, 20_000, "b");
        b.speaker_id = Some("speaker_1".into());
        b.speaker_attribution = SpeakerAttribution::Assigned;
        let spans = vec![
            span(0, 10_000, "Anna"),
            span(10_000, 13_000, "Anna"),
            span(13_000, 14_000, "Ben"),
        ];
        let named = name_clusters(&[a, b], &spans);
        assert_eq!(named.get("speaker_0").map(String::as_str), Some("Anna"));
        // Anna is taken and Ben's 1 s is too little evidence.
        assert_eq!(named.get("speaker_1"), None);
    }

    #[test]
    fn an_empty_call_is_not_enough_speech() {
        let result = assemble(input(vec![], vec![]));
        assert!(result.segments.is_empty());
        assert_eq!(
            result.diarization_status,
            DiarizationStatus::NotEnoughSpeech
        );
    }
}
