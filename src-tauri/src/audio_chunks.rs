use crate::transcription_policy::{
    CHUNK_OVERLAP_MS, MAX_CHUNK_MS, MIN_CHUNK_MS, SPLIT_ANALYSIS_FRAME_MS, SPLIT_ANALYSIS_STEP_MS,
    TARGET_CHUNK_MS,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioChunk {
    pub start_sample: usize,
    pub end_sample: usize,
}

impl AudioChunk {
    pub fn start_ms(self, sample_rate_hz: u32) -> i64 {
        samples_to_ms(self.start_sample, sample_rate_hz)
    }

    pub fn end_ms(self, sample_rate_hz: u32) -> i64 {
        samples_to_ms(self.end_sample, sample_rate_hz)
    }

    pub fn duration_ms(self, sample_rate_hz: u32) -> i64 {
        self.end_ms(sample_rate_hz) - self.start_ms(sample_rate_hz)
    }
}

/// Divide long audio into decoder-bounded windows. The split search is deliberately
/// conservative: it only chooses a low-energy point, and never removes audio. A
/// small overlap protects words that straddle a forced boundary.
pub fn plan_audio_chunks(samples: &[f32], sample_rate_hz: u32) -> Vec<AudioChunk> {
    plan_audio_chunks_with_splits(samples, sample_rate_hz, &[])
}

pub fn plan_audio_chunks_with_splits(
    samples: &[f32],
    sample_rate_hz: u32,
    preferred_splits: &[usize],
) -> Vec<AudioChunk> {
    if samples.is_empty() || sample_rate_hz == 0 {
        return Vec::new();
    }

    let max_samples = ms_to_samples(MAX_CHUNK_MS, sample_rate_hz);
    if samples.len() <= max_samples {
        return vec![AudioChunk {
            start_sample: 0,
            end_sample: samples.len(),
        }];
    }

    let target_samples = ms_to_samples(TARGET_CHUNK_MS, sample_rate_hz);
    let min_samples = ms_to_samples(MIN_CHUNK_MS, sample_rate_hz);
    let overlap_samples = ms_to_samples(CHUNK_OVERLAP_MS, sample_rate_hz);
    let mut chunks = Vec::new();
    let mut start = 0usize;

    while start < samples.len() {
        let remaining = samples.len() - start;
        if remaining <= max_samples {
            chunks.push(AudioChunk {
                start_sample: start,
                end_sample: samples.len(),
            });
            break;
        }

        let earliest = start.saturating_add(min_samples);
        let latest = start.saturating_add(max_samples).min(samples.len());
        let desired = start.saturating_add(target_samples).min(latest);
        let split = preferred_splits
            .iter()
            .copied()
            .filter(|split| *split >= earliest && *split <= latest)
            .min_by_key(|split| split.abs_diff(desired))
            .unwrap_or_else(|| quietest_split(samples, earliest, latest, desired, sample_rate_hz))
            .clamp(earliest, latest);

        chunks.push(AudioChunk {
            start_sample: start,
            end_sample: split,
        });

        let next_start = split.saturating_sub(overlap_samples);
        start = if next_start > start {
            next_start
        } else {
            split
        };
    }

    chunks
}

pub fn split_chunk_near_middle(
    samples: &[f32],
    chunk: AudioChunk,
    sample_rate_hz: u32,
) -> Option<(AudioChunk, AudioChunk)> {
    if chunk.end_sample <= chunk.start_sample || sample_rate_hz == 0 {
        return None;
    }

    let duration = chunk.end_sample - chunk.start_sample;
    let minimum_half = ms_to_samples(4_000, sample_rate_hz);
    if duration < minimum_half.saturating_mul(2) {
        return None;
    }

    let middle = chunk.start_sample + duration / 2;
    let radius = ms_to_samples(3_000, sample_rate_hz);
    let earliest = middle
        .saturating_sub(radius)
        .max(chunk.start_sample + minimum_half);
    let latest = middle
        .saturating_add(radius)
        .min(chunk.end_sample.saturating_sub(minimum_half));
    if earliest >= latest {
        return None;
    }

    let split = quietest_split(samples, earliest, latest, middle, sample_rate_hz);
    let overlap = ms_to_samples(CHUNK_OVERLAP_MS, sample_rate_hz) / 2;
    Some((
        AudioChunk {
            start_sample: chunk.start_sample,
            end_sample: split.saturating_add(overlap).min(chunk.end_sample),
        },
        AudioChunk {
            start_sample: split.saturating_sub(overlap).max(chunk.start_sample),
            end_sample: chunk.end_sample,
        },
    ))
}

fn quietest_split(
    samples: &[f32],
    earliest: usize,
    latest: usize,
    desired: usize,
    sample_rate_hz: u32,
) -> usize {
    let frame = ms_to_samples(SPLIT_ANALYSIS_FRAME_MS, sample_rate_hz).max(1);
    let step = ms_to_samples(SPLIT_ANALYSIS_STEP_MS, sample_rate_hz).max(1);
    let mut best = desired.clamp(earliest, latest);
    let mut best_score = f64::INFINITY;
    let mut cursor = earliest;

    while cursor <= latest {
        let half = frame / 2;
        let frame_start = cursor.saturating_sub(half);
        let frame_end = cursor.saturating_add(half).min(samples.len());
        if frame_end > frame_start {
            let energy = samples[frame_start..frame_end]
                .iter()
                .map(|sample| sample.abs() as f64)
                .sum::<f64>()
                / (frame_end - frame_start) as f64;
            let distance_penalty = (cursor.abs_diff(desired) as f64)
                / ((latest.saturating_sub(earliest)).max(1) as f64)
                * 0.002;
            let score = energy + distance_penalty;
            if score < best_score {
                best_score = score;
                best = cursor;
            }
        }

        match cursor.checked_add(step) {
            Some(next) if next > cursor => cursor = next,
            _ => break,
        }
    }

    best
}

fn ms_to_samples(ms: i64, sample_rate_hz: u32) -> usize {
    ((ms.max(0) as u128 * sample_rate_hz as u128) / 1000) as usize
}

fn samples_to_ms(samples: usize, sample_rate_hz: u32) -> i64 {
    ((samples as u128 * 1000) / sample_rate_hz.max(1) as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_audio_stays_in_one_chunk() {
        let samples = vec![0.1; 16_000 * 10];
        assert_eq!(
            plan_audio_chunks(&samples, 16_000),
            vec![AudioChunk {
                start_sample: 0,
                end_sample: samples.len()
            }]
        );
    }

    #[test]
    fn long_audio_is_fully_covered_by_bounded_overlapping_chunks() {
        let samples = vec![0.1; 16_000 * 80];
        let chunks = plan_audio_chunks(&samples, 16_000);
        assert!(chunks.len() >= 3);
        assert_eq!(chunks.first().unwrap().start_sample, 0);
        assert_eq!(chunks.last().unwrap().end_sample, samples.len());
        for pair in chunks.windows(2) {
            assert!(pair[0].end_sample >= pair[1].start_sample);
            assert!(pair[0].duration_ms(16_000) <= MAX_CHUNK_MS);
        }
    }

    #[test]
    fn split_retry_keeps_both_sides_and_overlap() {
        let samples = vec![0.1; 16_000 * 20];
        let original = AudioChunk {
            start_sample: 0,
            end_sample: samples.len(),
        };
        let (left, right) = split_chunk_near_middle(&samples, original, 16_000).unwrap();
        assert_eq!(left.start_sample, original.start_sample);
        assert_eq!(right.end_sample, original.end_sample);
        assert!(left.end_sample > right.start_sample);
    }
}
