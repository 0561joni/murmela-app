use strsim::normalized_levenshtein;

use crate::asr::TranscriptSegment;
use crate::transcription_quality::normalize_text;

pub fn stitch_segments(mut segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    segments.sort_by_key(|segment| (segment.start_ms, segment.end_ms));
    let mut stitched: Vec<TranscriptSegment> = Vec::with_capacity(segments.len());

    for candidate in segments {
        if let Some(previous) = stitched.last_mut() {
            if is_duplicate_overlap(previous, &candidate) {
                if confidence(&candidate) > confidence(previous) {
                    *previous = candidate;
                }
                continue;
            }
        }
        stitched.push(candidate);
    }

    for (order, segment) in stitched.iter_mut().enumerate() {
        segment.segment_order = order as i32;
    }
    stitched
}

fn is_duplicate_overlap(left: &TranscriptSegment, right: &TranscriptSegment) -> bool {
    let overlap_ms = left.end_ms.min(right.end_ms) - left.start_ms.max(right.start_ms);
    if overlap_ms < -250 {
        return false;
    }

    let left_text = normalize_text(&left.text);
    let right_text = normalize_text(&right.text);
    if left_text.is_empty() || right_text.is_empty() {
        return false;
    }
    if left_text == right_text {
        return true;
    }
    if left_text.chars().count() >= 12
        && (left_text.contains(&right_text) || right_text.contains(&left_text))
    {
        return true;
    }
    normalized_levenshtein(&left_text, &right_text) >= 0.86
}

fn confidence(segment: &TranscriptSegment) -> f32 {
    segment.confidence.unwrap_or(0.0)
}

/// Longest fragment (in characters) that may move across a boundary. Anything
/// longer is a run-on without punctuation; keeping the original split is safer
/// than creating a very long paragraph.
const MAX_SENTENCE_MOVE_CHARS: usize = 320;

/// Move segment boundaries onto sentence ends.
///
/// Chunked engines (Qwen3-ASR decodes ~25 s windows cut at the quietest
/// point) return one segment per window, so a boundary often lands inside a
/// sentence and the transcript shows the sentence as two paragraphs. For each
/// boundary that does not end a sentence, the shorter of "the rest of the
/// sentence at the start of the next segment" and "the unfinished sentence at
/// the end of the previous segment" is moved across. Words repeated by the
/// chunk overlap are dropped, and a period the decoder added only because its
/// window ended is removed when the next window continues in lowercase.
/// Timings of moved text are interpolated by character position.
pub fn align_segments_to_sentences(segments: Vec<TranscriptSegment>) -> Vec<TranscriptSegment> {
    let mut segments: Vec<TranscriptSegment> = segments
        .into_iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .collect();
    let lowercase_words = mid_sentence_word_forms(&segments);

    // Index of the segment before the current boundary; it can absorb several
    // following segments when a sentence spans more than one window.
    let mut previous: Option<usize> = None;
    for index in 0..segments.len() {
        if let Some(previous_index) = previous {
            let (before, after) = segments.split_at_mut(index);
            align_boundary(&mut before[previous_index], &mut after[0], &lowercase_words);
            if !before[previous_index].text.trim().is_empty() && after[0].text.trim().is_empty() {
                continue;
            }
        }
        previous = Some(index);
    }

    let mut aligned: Vec<TranscriptSegment> = segments
        .into_iter()
        .filter(|segment| !segment.text.trim().is_empty())
        .collect();
    for (order, segment) in aligned.iter_mut().enumerate() {
        segment.segment_order = order as i32;
    }
    aligned
}

fn align_boundary(
    left: &mut TranscriptSegment,
    right: &mut TranscriptSegment,
    lowercase_words: &LowercaseWords,
) {
    if is_gap(left) || is_gap(right) || left.text.trim().is_empty() {
        return;
    }
    if right.start_ms < left.end_ms {
        drop_overlap_repeat(left, right);
    }
    if right.text.trim().is_empty() {
        return;
    }
    if ends_sentence(&left.text) {
        if !is_spurious_period(&left.text, &right.text) {
            return;
        }
        let trimmed = left.text.trim_end();
        left.text = trimmed[..trimmed.len() - 1].to_string();
    }
    move_boundary_to_sentence_end(left, right, lowercase_words);
}

fn move_boundary_to_sentence_end(
    left: &mut TranscriptSegment,
    right: &mut TranscriptSegment,
    lowercase_words: &LowercaseWords,
) {
    let left_text = left.text.trim().to_string();
    let right_text = right.text.trim().to_string();
    let left_chars = left_text.chars().count();
    let right_chars = right_text.chars().count();

    // Option A: pull the rest of the sentence from the start of `right`.
    let head_end = first_sentence_end(&right_text).unwrap_or(right_text.len());
    let head_chars = right_text[..head_end].chars().count();
    // Option B: push the unfinished sentence at the end of `left` forward.
    let tail_start = last_sentence_end(&left_text).unwrap_or(0);
    let tail_chars = left_chars - left_text[..tail_start].chars().count();

    if head_chars.min(tail_chars) > MAX_SENTENCE_MOVE_CHARS {
        return;
    }
    let boundary_ms;
    if head_chars <= tail_chars {
        let head = right_text[..head_end].trim();
        let rest = right_text[head_end..].trim();
        let head = continue_sentence(head, lowercase_words);
        boundary_ms = interpolate(right, head_chars, right_chars);
        left.text = join_text(&left_text, &head);
        right.text = rest.to_string();
    } else {
        let tail = left_text[tail_start..].trim();
        let kept = left_text[..tail_start].trim();
        let continued = continue_sentence(&right_text, lowercase_words);
        boundary_ms = interpolate(left, left_chars - tail_chars, left_chars);
        right.text = join_text(tail, &continued);
        left.text = kept.to_string();
    }
    if left.text.is_empty() {
        right.start_ms = right.start_ms.min(left.start_ms);
    } else if right.text.is_empty() {
        left.end_ms = left.end_ms.max(right.end_ms);
    } else {
        let boundary_ms = boundary_ms.clamp(left.start_ms, right.end_ms);
        left.end_ms = boundary_ms.max(left.start_ms);
        right.start_ms = boundary_ms.min(right.end_ms);
    }
}

fn interpolate(segment: &TranscriptSegment, chars: usize, total_chars: usize) -> i64 {
    if total_chars == 0 {
        return segment.start_ms;
    }
    let duration = (segment.end_ms - segment.start_ms).max(0);
    segment.start_ms + duration * chars as i64 / total_chars as i64
}

fn is_gap(segment: &TranscriptSegment) -> bool {
    segment.language_code == "und" && segment.text.trim_start().starts_with('[')
}

const SENTENCE_TERMINATORS: [char; 7] = ['.', '!', '?', '…', '。', '！', '？'];

fn is_closing_mark(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '”' | '’' | '»' | ')' | ']' | '」' | '』')
}

fn is_wide_terminator(ch: char) -> bool {
    matches!(ch, '。' | '！' | '？')
}

const ABBREVIATIONS: [&str; 22] = [
    "mr", "mrs", "ms", "dr", "prof", "st", "vs", "etc", "eg", "ie", "zb", "bzw", "usw", "ca", "nr",
    "sr", "sra", "srta", "dra", "mme", "mlle", "no",
];

/// Byte offset just past a sentence terminator (and any closing quotes).
fn sentence_end_at(text: &str, index: usize, ch: char) -> Option<usize> {
    let mut end = index + ch.len_utf8();
    let rest = &text[end..];
    for next in rest.chars() {
        if is_closing_mark(next) {
            end += next.len_utf8();
        } else {
            break;
        }
    }
    let following = text[end..].chars().next();
    let boundary = match following {
        None => true,
        Some(next) if next.is_whitespace() => true,
        // "?!" / "..." continue the same terminator run.
        Some(next) if SENTENCE_TERMINATORS.contains(&next) => false,
        Some(_) => is_wide_terminator(ch),
    };
    if !boundary {
        return None;
    }
    if ch == '.' && is_abbreviation(&text[..index]) {
        return None;
    }
    Some(end)
}

fn is_abbreviation(before_period: &str) -> bool {
    let word: String = before_period
        .chars()
        .rev()
        .take_while(|ch| !ch.is_whitespace())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    let single_letter = word.chars().count() == 1 && word.chars().all(char::is_alphabetic);
    single_letter || ABBREVIATIONS.contains(&word.as_str())
}

fn sentence_ends(text: &str) -> Vec<usize> {
    text.char_indices()
        .filter(|(_, ch)| SENTENCE_TERMINATORS.contains(ch))
        .filter_map(|(index, ch)| sentence_end_at(text, index, ch))
        .collect()
}

fn first_sentence_end(text: &str) -> Option<usize> {
    sentence_ends(text).into_iter().next()
}

/// Start of the unfinished trailing sentence, if the text contains an earlier
/// sentence end.
fn last_sentence_end(text: &str) -> Option<usize> {
    sentence_ends(text)
        .into_iter()
        .filter(|end| !text[*end..].trim().is_empty())
        .last()
}

pub(crate) fn ends_sentence(text: &str) -> bool {
    let trimmed = text.trim_end();
    let Some((index, ch)) = trimmed
        .char_indices()
        .rev()
        .find(|(_, ch)| !is_closing_mark(*ch))
    else {
        return false;
    };
    SENTENCE_TERMINATORS.contains(&ch) && (ch != '.' || !is_abbreviation(&trimmed[..index]))
}

/// "…went to the." + "store and…" — the decoder closed its window with a
/// period although the speaker continued.
fn is_spurious_period(left: &str, right: &str) -> bool {
    let left = left.trim_end();
    if !left.ends_with('.') || left.ends_with("..") {
        return false;
    }
    right
        .trim_start()
        .chars()
        .find(|ch| ch.is_alphabetic() || ch.is_numeric())
        .is_some_and(char::is_lowercase)
}

/// Word forms seen in the middle of a sentence, used to undo capitalization
/// the decoder applied only because a new window started.
struct LowercaseWords {
    lowercase: std::collections::HashSet<String>,
    capitalized: std::collections::HashSet<String>,
}

fn mid_sentence_word_forms(segments: &[TranscriptSegment]) -> LowercaseWords {
    let mut words = LowercaseWords {
        lowercase: Default::default(),
        capitalized: Default::default(),
    };
    for segment in segments {
        let mut sentence_start = true;
        for raw in segment.text.split_whitespace() {
            let word = bare_word(raw);
            if !word.is_empty() && !sentence_start {
                if word.chars().next().is_some_and(char::is_lowercase) {
                    words.lowercase.insert(word.to_string());
                } else if word.chars().next().is_some_and(char::is_uppercase) {
                    words.capitalized.insert(word.to_string());
                }
            }
            sentence_start = ends_sentence(raw);
        }
    }
    words
}

fn bare_word(raw: &str) -> &str {
    raw.trim_matches(|ch: char| !ch.is_alphanumeric())
}

/// Lowercase the first word of `text` when it continues a sentence and the
/// transcript shows that word is normally lowercase ("The" → "the", but not
/// "I", names, acronyms or German nouns, which appear capitalized mid-sentence).
fn continue_sentence(text: &str, words: &LowercaseWords) -> String {
    let Some(raw) = text.split_whitespace().next() else {
        return text.to_string();
    };
    let word = bare_word(raw);
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return text.to_string();
    };
    let rest_is_lower = chars.clone().all(|ch| !ch.is_uppercase());
    if !first.is_uppercase() || !rest_is_lower || word.chars().count() < 2 {
        return text.to_string();
    }
    let lowered: String = first.to_lowercase().chain(chars).collect();
    if words.capitalized.contains(word) || !words.lowercase.contains(&lowered) {
        return text.to_string();
    }
    let offset = text.find(word).unwrap_or(0);
    format!(
        "{}{}{}",
        &text[..offset],
        lowered,
        &text[offset + word.len()..]
    )
}

fn join_text(left: &str, right: &str) -> String {
    let left = left.trim_end();
    let right = right.trim_start();
    if left.is_empty() {
        return right.to_string();
    }
    if right.is_empty() {
        return left.to_string();
    }
    let unspaced = |ch: char| matches!(ch as u32, 0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xFF00..=0xFFEF);
    let left_last = left.chars().last().unwrap_or(' ');
    let right_first = right.chars().next().unwrap_or(' ');
    if unspaced(left_last) || unspaced(right_first) {
        format!("{left}{right}")
    } else {
        format!("{left} {right}")
    }
}

/// Remove words at the start of `right` that repeat the end of `left`
/// because the two decoding windows overlapped.
fn drop_overlap_repeat(left: &TranscriptSegment, right: &mut TranscriptSegment) {
    let left_words: Vec<String> = left
        .text
        .split_whitespace()
        .map(|word| bare_word(word).to_lowercase())
        .collect();
    let right_raw: Vec<&str> = right.text.split_whitespace().collect();
    let right_words: Vec<String> = right_raw
        .iter()
        .map(|word| bare_word(word).to_lowercase())
        .collect();
    let longest = 4
        .min(left_words.len())
        .min(right_words.len().saturating_sub(1));
    for count in (1..=longest).rev() {
        let tail = &left_words[left_words.len() - count..];
        let head = &right_words[..count];
        if tail.iter().any(String::is_empty) || tail != head {
            continue;
        }
        // Single short words ("a", "I") repeat naturally; require real overlap.
        if count == 1 && tail[0].chars().count() < 3 {
            return;
        }
        right.text = right_raw[count..].join(" ");
        return;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(start_ms: i64, end_ms: i64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("{start_ms}:{end_ms}"),
            start_ms,
            end_ms,
            text: text.to_string(),
            language_code: "en".to_string(),
            segment_order: 0,
            confidence: Some(0.8),
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
            speaker_confidence: None,
        }
    }

    #[test]
    fn removes_text_duplicated_by_audio_overlap() {
        let stitched = stitch_segments(vec![
            segment(0, 5_000, "We should review the contract."),
            segment(4_500, 5_500, "We should review the contract."),
        ]);
        assert_eq!(stitched.len(), 1);
    }

    #[test]
    fn preserves_same_sentence_at_different_times() {
        let stitched = stitch_segments(vec![
            segment(0, 2_000, "Please confirm the proposal."),
            segment(10_000, 12_000, "Please confirm the proposal."),
        ]);
        assert_eq!(stitched.len(), 2);
    }

    fn texts(segments: &[TranscriptSegment]) -> Vec<&str> {
        segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    #[test]
    fn pulls_the_end_of_a_sentence_back_across_a_window_boundary() {
        let aligned = align_segments_to_sentences(vec![
            segment(
                0,
                25_000,
                "We started early. Then after lunch we all went over to the",
            ),
            segment(
                24_200,
                50_000,
                "store and bought milk. After that we drove home and cooked dinner together.",
            ),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "We started early. Then after lunch we all went over to the store and bought milk.",
                "After that we drove home and cooked dinner together."
            ]
        );
        assert!(aligned[0].end_ms > 24_200 && aligned[0].end_ms < 50_000);
        assert_eq!(aligned[0].end_ms, aligned[1].start_ms);
    }

    #[test]
    fn pushes_a_short_unfinished_sentence_forward_when_that_moves_less_text() {
        let aligned = align_segments_to_sentences(vec![
            segment(
                0,
                25_000,
                "The first quarter closed with strong margins across all plants. So the",
            ),
            segment(
                25_000,
                50_000,
                "Budget for next year needs a second look before we commit to anything",
            ),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "The first quarter closed with strong margins across all plants.",
                "So the Budget for next year needs a second look before we commit to anything"
            ]
        );
        assert!(aligned[0].end_ms < 25_000);
    }

    #[test]
    fn undoes_capitalization_caused_only_by_the_new_window() {
        let aligned = align_segments_to_sentences(vec![
            segment(
                0,
                25_000,
                "I think the plan works. We just need all of the exact",
            ),
            segment(
                25_000,
                50_000,
                "Numbers from finance. Can you send the numbers today? I will check them.",
            ),
        ]);
        assert_eq!(
            aligned[0].text,
            "I think the plan works. We just need all of the exact numbers from finance."
        );
    }

    #[test]
    fn keeps_names_and_german_nouns_capitalized() {
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, "Wir haben gestern mit der"),
            segment(
                25_000,
                50_000,
                "Firma gesprochen. Die Firma hat zugestimmt und alles ist gut.",
            ),
        ]);
        assert_eq!(
            aligned[0].text,
            "Wir haben gestern mit der Firma gesprochen."
        );
    }

    #[test]
    fn removes_a_period_the_decoder_added_at_the_window_end() {
        let aligned = align_segments_to_sentences(vec![
            segment(
                0,
                25_000,
                "Yesterday we visited the plant and talked to the.",
            ),
            segment(
                25_000,
                50_000,
                "operators about the dryer. They were happy with the results so far.",
            ),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "Yesterday we visited the plant and talked to the operators about the dryer.",
                "They were happy with the results so far."
            ]
        );
    }

    #[test]
    fn drops_words_repeated_by_the_chunk_overlap() {
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, "The evaluation shows a positive net present"),
            segment(
                24_200,
                50_000,
                "net present value over ten years. Next slide please.",
            ),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "The evaluation shows a positive net present value over ten years.",
                "Next slide please."
            ]
        );
    }

    #[test]
    fn leaves_sentence_aligned_segments_untouched() {
        let mut input = vec![
            segment(0, 5_000, "Hello there."),
            segment(5_000, 9_000, "How are you?"),
        ];
        input[1].segment_order = 1;
        let aligned = align_segments_to_sentences(input.clone());
        let shape = |segments: &[TranscriptSegment]| {
            segments
                .iter()
                .map(|s| (s.text.clone(), s.start_ms, s.end_ms, s.segment_order))
                .collect::<Vec<_>>()
        };
        assert_eq!(shape(&aligned), shape(&input));
    }

    #[test]
    fn a_sentence_spanning_three_windows_is_joined() {
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, "Okay. And then"),
            segment(25_000, 50_000, "we kept talking without any"),
            segment(50_000, 75_000, "pause at all. Done."),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "Okay.",
                "And then we kept talking without any pause at all.",
                "Done."
            ]
        );
        assert_eq!(aligned[1].start_ms, aligned[0].end_ms);
        assert_eq!(
            aligned.iter().map(|s| s.segment_order).collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn does_not_split_on_abbreviations_or_decimals() {
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, "We met Dr. Smith about the 3.5 million"),
            segment(25_000, 50_000, "euro budget. It was approved."),
        ]);
        assert_eq!(
            texts(&aligned),
            [
                "We met Dr. Smith about the 3.5 million euro budget.",
                "It was approved."
            ]
        );
    }

    #[test]
    fn gap_markers_are_hard_boundaries() {
        let mut gap = segment(25_000, 30_000, "[Unclear audio 00:25–00:30]");
        gap.language_code = "und".into();
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, "We were talking about the"),
            gap,
            segment(30_000, 50_000, "results. Fine."),
        ]);
        assert_eq!(aligned.len(), 3);
        assert_eq!(aligned[0].text, "We were talking about the");
    }

    #[test]
    fn very_long_run_on_windows_keep_their_split() {
        let long = "word ".repeat(100);
        let aligned = align_segments_to_sentences(vec![
            segment(0, 25_000, &format!("Start. {long}")),
            segment(25_000, 50_000, &format!("{long} end.")),
        ]);
        assert_eq!(aligned.len(), 2);
        assert!(aligned[0].text.trim_end().ends_with("word"));
    }

    #[test]
    fn chinese_text_is_joined_without_spaces() {
        let mut left = segment(0, 25_000, "我们昨天去了");
        let mut right = segment(25_000, 50_000, "工厂。然后回家了。");
        left.language_code = "zh".into();
        right.language_code = "zh".into();
        let aligned = align_segments_to_sentences(vec![left, right]);
        assert_eq!(texts(&aligned), ["我们昨天去了工厂。", "然后回家了。"]);
    }
}
