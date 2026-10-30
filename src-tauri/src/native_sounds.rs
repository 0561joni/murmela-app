//! VibeVoice writes what it hears besides words as bracketed tags, each its
//! own segment without a speaker: "[Environmental Sounds]", "[Music]",
//! "[Unintelligible Speech]". In a 58-minute call they were a third of all
//! segments. Whisper suppresses such tokens, so for a transcript that reads
//! the same whichever engine wrote it, sounds are dropped and speech the
//! model could not make out becomes one "[Unclear audio a–b]" mark per
//! stretch, the mark Whisper and Qwen use for audio they could not decode.

use crate::asr::TranscriptWarning;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Speech,
    /// Noise, music, silence, laughter: nothing anyone said.
    Sound,
    /// Someone spoke, but the model wrote no words.
    Unclear,
}

/// Tag words that mean speech without words.
const UNCLEAR_WORDS: [&str; 6] = ["unintelligible", "inaudible", "unclear", "speech", "crosstalk", "mumbl"];

pub fn kind(text: &str) -> Kind {
    let mut rest = text.trim();
    if rest.is_empty() {
        return Kind::Speech;
    }
    let mut unclear = false;
    while !rest.is_empty() {
        let Some(tag) = rest.strip_prefix('[').and_then(|after| after.split_once(']')) else {
            return Kind::Speech;
        };
        let name = tag.0.to_lowercase();
        unclear |= UNCLEAR_WORDS.iter().any(|word| name.contains(word));
        rest = tag.1.trim_start_matches(|c: char| c.is_whitespace() || c == ',' || c == '.');
    }
    if unclear {
        Kind::Unclear
    } else {
        Kind::Sound
    }
}

pub fn unclear_text(start_ms: i64, end_ms: i64) -> String {
    format!(
        "[Unclear audio {}–{}]",
        crate::output_format::clock_ms(start_ms),
        crate::output_format::clock_ms(end_ms)
    )
}

pub fn is_unclear_mark(text: &str) -> bool {
    text.trim_start().starts_with("[Unclear audio ")
}

const UNCLEAR_REASON: &str = "Unclear speech";
const MAX_LISTED: usize = 4;
/// Less unclear speech than this is left to its marks in the text; it does
/// not make a whole transcript one that needs review.
const MIN_FLAGGED_UNCLEAR_MS: i64 = 30_000;

/// One warning for every stretch left unclear, so a long recording does not
/// list dozens of them; none when they add up to only a few seconds.
pub fn unclear_warning(stretches: &[(i64, i64)]) -> Option<TranscriptWarning> {
    let total: i64 = stretches.iter().map(|(start, end)| end - start).sum();
    if total < MIN_FLAGGED_UNCLEAR_MS {
        return None;
    }
    let first = stretches.first()?;
    let last = stretches.last()?;
    let mut places: Vec<String> = stretches
        .iter()
        .take(MAX_LISTED)
        .map(|(start, _)| crate::output_format::clock_ms(*start))
        .collect();
    if stretches.len() > MAX_LISTED {
        places.push("…".to_string());
    }
    let count = match stretches.len() {
        1 => "one place".to_string(),
        count => format!("{count} places"),
    };
    Some(TranscriptWarning {
        start_ms: first.0,
        end_ms: last.1,
        reason: format!("{UNCLEAR_REASON} in {count} ({}). Listen there to fill it in.", places.join(", ")),
        attempts: 1,
        outcome: "partially_recovered".to_string(),
    })
}

pub fn is_unclear_warning(warning: &TranscriptWarning) -> bool {
    warning.reason.starts_with(UNCLEAR_REASON)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_told_apart_from_speech() {
        assert_eq!(kind("[Environmental Sounds]"), Kind::Sound);
        assert_eq!(kind("[Music] [Noise]"), Kind::Sound);
        assert_eq!(kind("[Laughter]"), Kind::Sound);
        assert_eq!(kind("[Unintelligible Speech]"), Kind::Unclear);
        assert_eq!(kind("[Speech]"), Kind::Unclear);
        assert_eq!(kind("[Noise], [Unintelligible Speech]"), Kind::Unclear);
        assert_eq!(kind("Ah, pero eso es el mismo. [Laughter] Ah, no me acuerdo."), Kind::Speech);
        assert_eq!(kind("[Laughter] Sí."), Kind::Speech);
        assert_eq!(kind("Hola"), Kind::Speech);
    }

    #[test]
    fn unclear_stretches_make_one_warning() {
        assert!(unclear_warning(&[]).is_none());
        assert!(unclear_warning(&[(730_000, 751_000)]).is_none(), "a few seconds are only marked");
        let one = unclear_warning(&[(730_000, 770_000)]).unwrap();
        assert_eq!(one.reason, "Unclear speech in one place (12:10). Listen there to fill it in.");
        let many: Vec<_> = (0..6).map(|i| (i * 60_000, i * 60_000 + 5_000)).collect();
        let warning = unclear_warning(&many).unwrap();
        assert_eq!(warning.reason, "Unclear speech in 6 places (00:00, 01:00, 02:00, 03:00, …). Listen there to fill it in.");
        assert_eq!((warning.start_ms, warning.end_ms), (0, 305_000));
        assert!(is_unclear_warning(&warning));
        assert!(is_unclear_mark(&unclear_text(0, 5_000)));
    }
}
