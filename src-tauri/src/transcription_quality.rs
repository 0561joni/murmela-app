use std::collections::{HashSet, VecDeque};

use strsim::normalized_levenshtein;

#[derive(Debug, Clone)]
struct ObservedSegment {
    start_ms: i64,
    end_ms: i64,
    normalized: String,
}

#[derive(Debug, Default)]
pub struct LiveRepetitionDetector {
    recent: VecDeque<ObservedSegment>,
}

impl LiveRepetitionDetector {
    pub fn observe(&mut self, start_ms: i64, end_ms: i64, text: &str) -> Option<String> {
        let normalized = normalize_text(text);
        if normalized.is_empty() {
            return None;
        }

        if let Some(reason) = low_diversity_reason(&normalized) {
            return Some(reason);
        }

        self.recent.push_back(ObservedSegment {
            start_ms,
            end_ms,
            normalized,
        });
        while self.recent.len() > 8 {
            self.recent.pop_front();
        }

        repeated_run_reason(&self.recent)
    }
}

pub fn repetition_reason<'a>(
    segments: impl IntoIterator<Item = (i64, i64, &'a str)>,
) -> Option<String> {
    let mut detector = LiveRepetitionDetector::default();
    for (start_ms, end_ms, text) in segments {
        if let Some(reason) = detector.observe(start_ms, end_ms, text) {
            return Some(reason);
        }
    }
    None
}

pub fn normalize_text(text: &str) -> String {
    let mut normalized = String::new();
    let mut pending_space = false;
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() {
            if pending_space && !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push(character);
            pending_space = false;
        } else {
            pending_space = true;
        }
    }
    normalized
}

fn repeated_run_reason(recent: &VecDeque<ObservedSegment>) -> Option<String> {
    let last = recent.back()?;
    let meaningful =
        last.normalized.chars().count() >= 20 || last.normalized.split_whitespace().count() >= 4;
    let required = if meaningful { 3 } else { 8 };
    if recent.len() < required {
        return None;
    }

    let run = recent.iter().rev().take(required).collect::<Vec<_>>();
    let first = run.first()?;
    let all_exact = run
        .iter()
        .all(|segment| segment.normalized == first.normalized);
    let all_near = meaningful
        && run
            .iter()
            .all(|segment| normalized_levenshtein(&segment.normalized, &first.normalized) >= 0.92);
    let span_ms = run
        .iter()
        .map(|segment| segment.end_ms)
        .max()
        .unwrap_or(last.end_ms)
        - run
            .iter()
            .map(|segment| segment.start_ms)
            .min()
            .unwrap_or(last.start_ms);

    if all_exact || (all_near && span_ms >= 4_000) {
        Some(format!(
            "decoder repeated substantially identical text {required} times"
        ))
    } else {
        None
    }
}

fn low_diversity_reason(normalized: &str) -> Option<String> {
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 24 {
        return None;
    }

    let trigrams = tokens.windows(3).collect::<Vec<_>>();
    let unique = trigrams
        .iter()
        .map(|trigram| format!("{}\u{1f}{}\u{1f}{}", trigram[0], trigram[1], trigram[2]))
        .collect::<HashSet<_>>()
        .len();
    let diversity = unique as f32 / trigrams.len().max(1) as f32;
    if diversity < 0.22 {
        Some("decoder produced abnormally repetitive token patterns".to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_three_repeated_sentences() {
        let reason = repetition_reason([
            (0, 4_000, "This is the sentence that became stuck."),
            (4_000, 8_000, "This is the sentence that became stuck."),
            (8_000, 12_000, "This is the sentence that became stuck."),
        ]);
        assert!(reason.is_some());
    }

    #[test]
    fn does_not_reject_normal_short_repetition() {
        let reason =
            repetition_reason([(0, 500, "yes"), (500, 1_000, "yes"), (1_000, 1_500, "yes")]);
        assert!(reason.is_none());
    }

    #[test]
    fn detects_repetition_inside_one_large_segment() {
        let repeated = "we will review the document tomorrow ".repeat(12);
        let reason = repetition_reason([(0, 20_000, repeated.as_str())]);
        assert!(reason.is_some());
    }

    #[test]
    fn normalization_ignores_case_punctuation_and_spacing() {
        assert_eq!(normalize_text(" Hello,   WORLD! "), "hello world");
    }
}
