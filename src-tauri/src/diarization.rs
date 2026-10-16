use serde::{Deserialize, Serialize};

pub const DIARIZATION_MODEL_ID: &str = "sherpa-diarization-pyannote3-eres2net-voxceleb-v2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiarizationSource {
    None,
    NativeModel,
    PostProcess,
}

impl Default for DiarizationSource {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiarizationStatus {
    NotRequested,
    Pending,
    Running,
    Completed,
    CompletedWithUncertainty,
    Failed,
    Canceled,
    NotEnoughSpeech,
}

impl Default for DiarizationStatus {
    fn default() -> Self {
        Self::NotRequested
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSpeaker {
    pub speaker_id: String,
    pub display_name: String,
    pub speaker_order: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizationModelSpec {
    pub manifest_version: u32,
    pub window_shift_ratio: f32,
    pub min_duration_on_seconds: f32,
    pub min_duration_off_seconds: f32,
    pub clustering_threshold: f32,
    pub num_clusters: i32,
}

pub const DIARIZATION_MODEL_SPEC_V2: DiarizationModelSpec = DiarizationModelSpec {
    manifest_version: 2,
    window_shift_ratio: 0.1,
    min_duration_on_seconds: 0.3,
    min_duration_off_seconds: 0.5,
    // Sherpa documents 0.90 as a general baseline. The multilingual VoxCeleb
    // package was calibrated on the long private-use meeting fixture: 1.10
    // materially reduces fragmentation, while the sanity gate below still
    // rejects pathological results and offers an exact-count repair.
    clustering_threshold: 1.10,
    num_clusters: -1,
};

pub fn validate_speaker_count_hint(value: Option<i32>) -> Result<(), &'static str> {
    if value.is_some_and(|count| !(1..=20).contains(&count)) {
        Err("Known speaker count must be between 1 and 20.")
    } else {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDiarizationTurn {
    pub start_ms: i64,
    pub end_ms: i64,
    pub cluster_ids: Vec<i32>,
    pub confidence: Option<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiarizationDiagnostics {
    pub cluster_count: usize,
    pub turn_count: usize,
    pub short_cluster_count: usize,
}

pub fn aggregate_diagnostics(turns: &[RawDiarizationTurn]) -> DiarizationDiagnostics {
    use std::collections::HashMap;
    let mut durations = HashMap::<i32, i64>::new();
    for turn in turns {
        let duration = (turn.end_ms - turn.start_ms).max(0);
        for cluster in &turn.cluster_ids {
            *durations.entry(*cluster).or_default() += duration;
        }
    }
    DiarizationDiagnostics {
        cluster_count: durations.len(),
        turn_count: turns.len(),
        short_cluster_count: durations
            .values()
            .filter(|duration| **duration < 10_000)
            .count(),
    }
}

pub fn overclustering_warning(
    turns: &[RawDiarizationTurn],
    speaker_count_hint: Option<i32>,
) -> Option<String> {
    if speaker_count_hint.is_some() {
        return None;
    }
    let diagnostics = aggregate_diagnostics(turns);
    let count = diagnostics.cluster_count;
    let short = diagnostics.short_cluster_count;
    if count > 20 || (count >= 12 && short * 2 > count) {
        Some(format!(
            "Speaker identification found {count} possible voices ({short} very brief). The transcript was saved without speaker labels; retry with a known speaker count."
        ))
    } else {
        None
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiarizationTurn {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub speaker_ids: Vec<String>,
    pub confidence: Option<f32>,
    pub is_overlap: bool,
    pub is_uncertain: bool,
    pub turn_order: i32,
}

/// Replaces runtime cluster numbers with transcript-local IDs ordered by first appearance.
pub fn normalize_turns(mut turns: Vec<RawDiarizationTurn>) -> Vec<DiarizationTurn> {
    use std::collections::HashMap;
    turns.sort_by_key(|turn| (turn.start_ms, turn.end_ms));
    let mut speakers = HashMap::<i32, String>::new();
    let mut next = 0usize;
    turns
        .into_iter()
        .enumerate()
        .filter_map(|(index, turn)| {
            if turn.end_ms <= turn.start_ms || turn.cluster_ids.is_empty() {
                return None;
            }
            let mut ids = Vec::new();
            for cluster in turn.cluster_ids {
                let id = speakers.entry(cluster).or_insert_with(|| {
                    let id = format!("speaker_{next}");
                    next += 1;
                    id
                });
                if !ids.contains(id) {
                    ids.push(id.clone());
                }
            }
            Some(DiarizationTurn {
                id: format!("turn_{index}"),
                start_ms: turn.start_ms,
                end_ms: turn.end_ms,
                is_overlap: ids.len() > 1,
                speaker_ids: ids,
                confidence: turn.confidence,
                is_uncertain: false,
                turn_order: index as i32,
            })
        })
        .collect()
}

pub fn speakers_from_turns(turns: &[DiarizationTurn]) -> Vec<TranscriptSpeaker> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut speakers = Vec::new();
    for turn in turns {
        for speaker_id in &turn.speaker_ids {
            if seen.insert(speaker_id.clone()) {
                let speaker_order = speakers.len() as i32;
                speakers.push(TranscriptSpeaker {
                    speaker_id: speaker_id.clone(),
                    display_name: format!("Speaker {}", speaker_order + 1),
                    speaker_order,
                });
            }
        }
    }
    speakers
}

pub fn apply_turns_to_transcript(
    result: &mut crate::asr::TranscriptResult,
    raw_turns: Vec<RawDiarizationTurn>,
    speaker_count_hint: Option<i32>,
) {
    let turns = normalize_turns(raw_turns);
    if turns.is_empty() {
        result.speakers.clear();
        result.diarization_turns.clear();
        for segment in &mut result.segments {
            segment.speaker_id = None;
            segment.speaker_ids = None;
            segment.speaker_attribution = crate::speaker_reconciliation::SpeakerAttribution::None;
            segment.speaker_confidence = None;
        }
        result.diarization_source = DiarizationSource::PostProcess;
        result.diarization_status = DiarizationStatus::NotEnoughSpeech;
        result.diarization_model_id = Some(DIARIZATION_MODEL_ID.to_string());
        result.diarization_warning =
            Some("Not enough distinct speech was available to identify speakers.".to_string());
        result.diarization_policy_version =
            Some(crate::speaker_reconciliation::DIARIZATION_POLICY_VERSION);
        result.diarization_clustering_threshold = speaker_count_hint
            .is_none()
            .then_some(DIARIZATION_MODEL_SPEC_V2.clustering_threshold);
        result.diarization_speaker_count_hint = speaker_count_hint;
        return;
    }
    let speakers = speakers_from_turns(&turns);
    let mut maximum_end = i64::MIN;
    let prefix_end: Vec<_> = turns
        .iter()
        .map(|turn| {
            maximum_end = maximum_end.max(turn.end_ms);
            maximum_end
        })
        .collect();
    let mut uncertain = false;
    for segment in &mut result.segments {
        let begin = prefix_end.partition_point(|end| *end <= segment.start_ms);
        let end = turns
            .partition_point(|turn| turn.start_ms < segment.end_ms)
            .max(begin);
        let attribution = crate::speaker_reconciliation::reconcile_segment(
            segment.start_ms,
            segment.end_ms,
            &turns[begin..end],
        );
        segment.speaker_id = attribution.speaker_id;
        segment.speaker_ids =
            (!attribution.speaker_ids.is_empty()).then_some(attribution.speaker_ids);
        segment.speaker_attribution = attribution.attribution;
        segment.speaker_confidence = attribution.confidence;
        uncertain |= matches!(
            segment.speaker_attribution,
            crate::speaker_reconciliation::SpeakerAttribution::Uncertain
                | crate::speaker_reconciliation::SpeakerAttribution::Likely
                | crate::speaker_reconciliation::SpeakerAttribution::Overlap
        );
    }
    result.diarization_status = if uncertain {
        DiarizationStatus::CompletedWithUncertainty
    } else {
        DiarizationStatus::Completed
    };
    result.diarization_source = DiarizationSource::PostProcess;
    result.diarization_model_id = Some(DIARIZATION_MODEL_ID.to_string());
    result.diarization_warning = uncertain.then(|| {
        "Some transcript segments contain likely, overlapping, or uncertain speaker attribution."
            .to_string()
    });
    result.diarization_policy_version =
        Some(crate::speaker_reconciliation::DIARIZATION_POLICY_VERSION);
    result.diarization_clustering_threshold = speaker_count_hint
        .is_none()
        .then_some(DIARIZATION_MODEL_SPEC_V2.clustering_threshold);
    result.diarization_speaker_count_hint = speaker_count_hint;
    result.speakers = speakers;
    result.diarization_turns = turns;
}

pub fn mark_failure(result: &mut crate::asr::TranscriptResult, warning: impl Into<String>) {
    result.diarization_source = DiarizationSource::PostProcess;
    result.diarization_status = DiarizationStatus::Failed;
    result.diarization_model_id = Some(DIARIZATION_MODEL_ID.to_string());
    result.diarization_warning = Some(warning.into());
    result.diarization_policy_version =
        Some(crate::speaker_reconciliation::DIARIZATION_POLICY_VERSION);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maps_clusters_by_first_appearance() {
        let turns = normalize_turns(vec![
            RawDiarizationTurn {
                start_ms: 500,
                end_ms: 900,
                cluster_ids: vec![9],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 0,
                end_ms: 400,
                cluster_ids: vec![42],
                confidence: None,
            },
            RawDiarizationTurn {
                start_ms: 900,
                end_ms: 1200,
                cluster_ids: vec![42, 9],
                confidence: None,
            },
        ]);
        assert_eq!(turns[0].speaker_ids, ["speaker_0"]);
        assert_eq!(turns[1].speaker_ids, ["speaker_1"]);
        assert_eq!(turns[2].speaker_ids, ["speaker_0", "speaker_1"]);
        assert!(turns[2].is_overlap);
    }

    #[test]
    fn rejects_fragmented_automatic_clusters_but_not_a_hint() {
        let turns = (0..21)
            .map(|speaker| RawDiarizationTurn {
                start_ms: speaker * 1000,
                end_ms: speaker * 1000 + 500,
                cluster_ids: vec![speaker as i32],
                confidence: None,
            })
            .collect::<Vec<_>>();
        assert!(overclustering_warning(&turns, None).is_some());
        assert!(overclustering_warning(&turns, Some(7)).is_none());
    }

    #[test]
    fn automatic_threshold_is_conservative() {
        assert_eq!(DIARIZATION_MODEL_SPEC_V2.manifest_version, 2);
        assert_eq!(DIARIZATION_MODEL_SPEC_V2.clustering_threshold, 1.10);
        assert_eq!(DIARIZATION_MODEL_SPEC_V2.num_clusters, -1);
    }

    #[test]
    fn sanity_check_accepts_normal_two_seven_and_fifteen_speaker_results() {
        for count in [2, 7, 15] {
            let turns = (0..count)
                .map(|speaker| RawDiarizationTurn {
                    start_ms: speaker * 20_000,
                    end_ms: speaker * 20_000 + 15_000,
                    cluster_ids: vec![speaker as i32],
                    confidence: None,
                })
                .collect::<Vec<_>>();
            assert!(overclustering_warning(&turns, None).is_none());
        }
    }

    #[test]
    fn sanity_check_rejects_twelve_clusters_when_most_are_brief() {
        let turns = (0..12)
            .map(|speaker| RawDiarizationTurn {
                start_ms: speaker * 20_000,
                end_ms: speaker * 20_000 + if speaker < 7 { 5_000 } else { 15_000 },
                cluster_ids: vec![speaker as i32],
                confidence: None,
            })
            .collect::<Vec<_>>();
        assert!(overclustering_warning(&turns, None).is_some());
    }

    #[test]
    fn speaker_count_hint_accepts_only_one_through_twenty() {
        assert!(validate_speaker_count_hint(None).is_ok());
        assert!(validate_speaker_count_hint(Some(1)).is_ok());
        assert!(validate_speaker_count_hint(Some(20)).is_ok());
        assert!(validate_speaker_count_hint(Some(0)).is_err());
        assert!(validate_speaker_count_hint(Some(21)).is_err());
    }
}

#[cfg(test)]
mod reconciliation_regressions {
    use super::*;
    #[test]
    fn no_speech_clears_all_prior_references() {
        let mut result = crate::review::fixture_result();
        apply_turns_to_transcript(&mut result, vec![], None);
        assert!(result.speakers.is_empty());
        assert!(result.diarization_turns.is_empty());
        assert!(matches!(
            result.diarization_status,
            DiarizationStatus::NotEnoughSpeech
        ));
        for s in result.segments {
            assert!(s.speaker_id.is_none());
            assert!(s.speaker_ids.is_none());
            assert_eq!(
                s.speaker_attribution,
                crate::speaker_reconciliation::SpeakerAttribution::None
            );
        }
    }
    #[test]
    fn indexed_reconciliation_matches_full_scan_including_empty_and_inverted_segments() {
        let mut raw: Vec<_> = (0..200)
            .map(|i| RawDiarizationTurn {
                start_ms: i * 1000,
                end_ms: i * 1000 + 800,
                cluster_ids: vec![(i % 4) as i32],
                confidence: None,
            })
            .collect();
        raw.push(RawDiarizationTurn {
            start_ms: 0,
            end_ms: 10000,
            cluster_ids: vec![0],
            confidence: None,
        });
        let turns = normalize_turns(raw.clone());
        let mut result = crate::review::fixture_result();
        for (start, end) in [
            (0, 0),
            (5000, 3000),
            (150, 3400),
            (200000, 202000),
            (950, 1050),
            (-1000, 2300),
        ] {
            result.segments[0].start_ms = start;
            result.segments[0].end_ms = end;
            let expected = crate::speaker_reconciliation::reconcile_segment(start, end, &turns);
            apply_turns_to_transcript(&mut result, raw.clone(), None);
            let actual = &result.segments[0];
            assert_eq!(actual.speaker_attribution, expected.attribution);
            assert_eq!(actual.speaker_id, expected.speaker_id);
            assert_eq!(
                actual.speaker_ids.clone().unwrap_or_default(),
                expected.speaker_ids
            );
            assert_eq!(actual.speaker_confidence, expected.confidence);
        }
    }
}
