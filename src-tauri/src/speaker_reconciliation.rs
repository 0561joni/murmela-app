use crate::diarization::DiarizationTurn;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiarizationPolicyV1 {
    pub dominant_voiced_ratio: f64,
    pub dominant_segment_ratio: f64,
    pub maximum_overlap_ratio: f64,
    pub maximum_secondary_ratio: f64,
    pub likely_voiced_ratio: f64,
    pub likely_segment_ratio: f64,
    pub likely_secondary_ratio: f64,
}
pub const DIARIZATION_POLICY_V1: DiarizationPolicyV1 = DiarizationPolicyV1 {
    dominant_voiced_ratio: 0.80,
    dominant_segment_ratio: 0.60,
    maximum_overlap_ratio: 0.15,
    maximum_secondary_ratio: 0.20,
    likely_voiced_ratio: 0.65,
    likely_segment_ratio: 0.50,
    likely_secondary_ratio: 0.30,
};
pub const DIARIZATION_POLICY_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeakerAttribution {
    None,
    Assigned,
    Likely,
    Uncertain,
    Overlap,
}

impl Default for SpeakerAttribution {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SegmentAttribution {
    pub attribution: SpeakerAttribution,
    pub speaker_id: Option<String>,
    pub speaker_ids: Vec<String>,
    /// Temporal reconciliation confidence; never an acoustic probability.
    pub confidence: Option<f32>,
}

fn intersection(a_start: i64, a_end: i64, b_start: i64, b_end: i64) -> i64 {
    (a_end.min(b_end) - a_start.max(b_start)).max(0)
}

pub fn reconcile_segment(
    start_ms: i64,
    end_ms: i64,
    turns: &[DiarizationTurn],
) -> SegmentAttribution {
    let duration = (end_ms - start_ms).max(0);
    if duration == 0 {
        return none();
    }
    let mut coverage = HashMap::<String, i64>::new();
    let mut voiced = 0i64;
    let mut overlap = 0i64;
    let mut candidates = HashSet::new();
    for turn in turns {
        let span = intersection(start_ms, end_ms, turn.start_ms, turn.end_ms);
        if span == 0 {
            continue;
        }
        voiced += span;
        if turn.is_overlap || turn.speaker_ids.len() > 1 {
            overlap += span;
        }
        for id in &turn.speaker_ids {
            *coverage.entry(id.clone()).or_default() += span;
            candidates.insert(id.clone());
        }
    }
    if voiced == 0 {
        return none();
    }
    let mut ranked: Vec<_> = coverage.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let (dominant, dominant_ms) = &ranked[0];
    let second_ms = ranked.get(1).map(|item| item.1).unwrap_or(0);
    let policy = DIARIZATION_POLICY_V1;
    if overlap as f64 / duration as f64 >= policy.maximum_overlap_ratio {
        let mut ids: Vec<_> = candidates.into_iter().collect();
        ids.sort();
        return SegmentAttribution {
            attribution: SpeakerAttribution::Overlap,
            speaker_id: None,
            speaker_ids: ids,
            confidence: None,
        };
    }
    let passes = *dominant_ms as f64 / voiced as f64 >= policy.dominant_voiced_ratio
        && *dominant_ms as f64 / duration as f64 >= policy.dominant_segment_ratio
        && second_ms as f64 / duration as f64 <= policy.maximum_secondary_ratio;
    if passes {
        SegmentAttribution {
            attribution: SpeakerAttribution::Assigned,
            speaker_id: Some(dominant.clone()),
            speaker_ids: vec![dominant.clone()],
            confidence: Some((*dominant_ms as f64 / voiced as f64) as f32),
        }
    } else if *dominant_ms as f64 / voiced as f64 >= policy.likely_voiced_ratio
        && *dominant_ms as f64 / duration as f64 >= policy.likely_segment_ratio
        && second_ms as f64 / duration as f64 <= policy.likely_secondary_ratio
    {
        let mut ids: Vec<_> = candidates.into_iter().collect();
        ids.sort();
        SegmentAttribution {
            attribution: SpeakerAttribution::Likely,
            speaker_id: Some(dominant.clone()),
            speaker_ids: ids,
            confidence: Some((*dominant_ms as f64 / voiced as f64) as f32),
        }
    } else {
        let mut ids: Vec<_> = candidates.into_iter().collect();
        ids.sort();
        SegmentAttribution {
            attribution: SpeakerAttribution::Uncertain,
            speaker_id: None,
            speaker_ids: ids,
            confidence: None,
        }
    }
}
fn none() -> SegmentAttribution {
    SegmentAttribution {
        attribution: SpeakerAttribution::None,
        speaker_id: None,
        speaker_ids: vec![],
        confidence: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn turn(start: i64, end: i64, ids: &[&str]) -> DiarizationTurn {
        DiarizationTurn {
            id: "x".into(),
            start_ms: start,
            end_ms: end,
            speaker_ids: ids.iter().map(|x| x.to_string()).collect(),
            confidence: None,
            is_overlap: ids.len() > 1,
            is_uncertain: false,
            turn_order: 0,
        }
    }
    #[test]
    fn exact_boundaries_do_not_intersect() {
        assert_eq!(
            reconcile_segment(100, 200, &[turn(0, 100, &["speaker_0"])]).attribution,
            SpeakerAttribution::None
        );
    }
    #[test]
    fn dominant_speaker_is_assigned() {
        assert_eq!(
            reconcile_segment(0, 1000, &[turn(0, 900, &["speaker_0"])])
                .speaker_id
                .as_deref(),
            Some("speaker_0")
        );
    }
    #[test]
    fn weak_coverage_is_uncertain() {
        assert_eq!(
            reconcile_segment(0, 1000, &[turn(0, 490, &["speaker_0"])]).attribution,
            SpeakerAttribution::Uncertain
        );
    }
    #[test]
    fn clear_majority_is_likely() {
        let x = reconcile_segment(
            0,
            1000,
            &[turn(0, 600, &["speaker_0"]), turn(600, 900, &["speaker_1"])],
        );
        assert_eq!(x.attribution, SpeakerAttribution::Likely);
        assert_eq!(x.speaker_id.as_deref(), Some("speaker_0"));
    }
    #[test]
    fn overlap_has_no_primary() {
        let x = reconcile_segment(
            0,
            1000,
            &[
                turn(0, 200, &["speaker_0", "speaker_1"]),
                turn(200, 1000, &["speaker_0"]),
            ],
        );
        assert_eq!(x.attribution, SpeakerAttribution::Overlap);
        assert!(x.speaker_id.is_none());
    }
    #[test]
    fn silence_is_unattributed() {
        assert_eq!(
            reconcile_segment(0, 1000, &[]).attribution,
            SpeakerAttribution::None
        );
    }
}
