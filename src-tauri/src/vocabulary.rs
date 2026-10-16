use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use strsim::jaro_winkler;
use unicode_normalization::UnicodeNormalization;
use uuid::Uuid;

use crate::app_state::AppState;
use crate::storage::open_connection_by_path;
use crate::asr::{TranscriptResult, TranscriptSegment};

const SINGLE_TOKEN_THRESHOLD: f64 = 0.965;
const MULTI_TOKEN_THRESHOLD: f64 = 0.94;
const MIN_FUZZY_LEAD: f64 = 0.04;
const MAX_TOKEN_SPAN: usize = 3;
pub const DICTIONARY_PROMPT_MAX_CHARS: usize = 1_500;
const DICTIONARY_PROMPT_PREFIX: &str =
    "Preserve these exact spellings only when spoken; never add them otherwise: ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VocabularyPrompt {
    pub text: String,
    pub terms: Vec<String>,
    pub included_count: usize,
    pub truncated_count: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    ExactOnly,
    ExactAndFuzzy,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyAlias {
    pub id: String,
    pub alias: String,
    pub normalized_alias: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VocabularyTerm {
    pub id: String,
    pub canonical: String,
    pub normalized_canonical: String,
    pub match_mode: MatchMode,
    pub is_builtin: bool,
    pub created_at: String,
    pub updated_at: String,
    pub aliases: Vec<VocabularyAlias>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateVocabularyTermInput {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub match_mode: MatchMode,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateVocabularyTermInput {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub match_mode: MatchMode,
}

struct PreparedTermInput {
    canonical: String,
    normalized_canonical: String,
    match_mode: MatchMode,
    aliases: Vec<(String, String)>,
}

struct MatchCandidate {
    canonical: String,
    normalized: String,
    source: MatchCandidateSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchCandidateSource {
    Canonical,
    Alias,
}

#[derive(Default)]
struct VocabularyMatcher {
    exact: HashMap<(usize, String), String>,
    fuzzy: HashMap<usize, Vec<MatchCandidate>>,
}

#[derive(Clone)]
struct TokenPart {
    raw: String,
    trailing_ws: String,
}

struct ParsedToken<'a> {
    prefix: &'a str,
    core: &'a str,
    suffix: &'a str,
}

#[derive(Debug, Clone, PartialEq)]
struct CorrectionDecision {
    original_span: String,
    replacement: String,
    strategy: CorrectionStrategy,
    score: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CorrectionStrategy {
    ExactCanonical,
    ExactAlias,
    FuzzyAlias,
}

pub fn seed_builtin_terms(state: &AppState) -> Result<()> {
    let connection = open_connection(state)?;
    let seeds = builtin_term_specs();
    for seed in seeds {
        connection.execute(
            "INSERT OR IGNORE INTO custom_vocabulary_terms (id, canonical, normalized_canonical, match_mode, is_builtin, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6)",
            params![
                seed.id,
                seed.canonical,
                normalize_for_match(seed.canonical),
                to_match_mode(MatchMode::ExactOnly),
                seed.created_at,
                seed.created_at,
            ],
        )?;
        connection.execute(
            "UPDATE custom_vocabulary_terms
             SET match_mode = ?1, updated_at = ?2
             WHERE id = ?3",
            params![
                to_match_mode(MatchMode::ExactOnly),
                seed.created_at,
                seed.id
            ],
        )?;

        for alias in seed.aliases {
            connection.execute(
                "INSERT OR IGNORE INTO custom_vocabulary_aliases (id, term_id, alias, normalized_alias)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    alias_id(seed.id, alias),
                    seed.id,
                    alias,
                    normalize_for_match(alias),
                ],
            )?;
        }
    }
    Ok(())
}

pub fn list_vocabulary_terms(state: &AppState) -> Result<Vec<VocabularyTerm>> {
    list_vocabulary_terms_from_db_path(&state.db_path)
}

pub fn list_vocabulary_terms_from_db_path(db_path: &Path) -> Result<Vec<VocabularyTerm>> {
    let connection = open_connection_by_path(db_path)?;
    let mut statement = connection.prepare(
        "SELECT id, canonical, normalized_canonical, match_mode, is_builtin, created_at, updated_at
         FROM custom_vocabulary_terms
         ORDER BY is_builtin DESC, canonical COLLATE NOCASE ASC",
    )?;
    let rows = statement.query_map([], map_vocabulary_term_row)?;
    let mut terms = rows.collect::<std::result::Result<Vec<_>, _>>()?;
    for term in &mut terms {
        term.aliases = list_aliases_for_term(&connection, &term.id)?;
    }
    Ok(terms)
}

pub fn build_asr_prompt_from_db_path(db_path: &Path) -> Result<Option<VocabularyPrompt>> {
    let terms = list_vocabulary_terms_from_db_path(db_path)?;
    Ok(build_asr_prompt(&terms))
}

pub(crate) fn build_asr_prompt(terms: &[VocabularyTerm]) -> Option<VocabularyPrompt> {
    let mut ordered = terms.to_vec();
    ordered.sort_by(|left, right| {
        prompt_rank(left)
            .cmp(&prompt_rank(right))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| {
                left.canonical
                    .to_lowercase()
                    .cmp(&right.canonical.to_lowercase())
            })
    });

    let mut seen = HashSet::new();
    let mut included = Vec::new();
    let mut text = DICTIONARY_PROMPT_PREFIX.to_string();
    let mut eligible_count = 0usize;
    for term in ordered {
        let canonical = term.canonical.nfkc().collect::<String>().trim().to_string();
        let normalized = normalize_for_match(&canonical);
        if canonical.is_empty() || normalized.is_empty() || !seen.insert(normalized) {
            continue;
        }
        eligible_count += 1;
        let encoded = serde_json::to_string(&canonical).ok()?;
        let separator = if included.is_empty() { "" } else { ", " };
        let candidate_chars =
            text.chars().count() + separator.chars().count() + encoded.chars().count();
        if candidate_chars > DICTIONARY_PROMPT_MAX_CHARS {
            continue;
        }
        text.push_str(separator);
        text.push_str(&encoded);
        included.push(canonical);
    }

    if included.is_empty() {
        return None;
    }
    Some(VocabularyPrompt {
        text,
        terms: included.clone(),
        included_count: included.len(),
        truncated_count: eligible_count.saturating_sub(included.len()),
    })
}

fn prompt_rank(term: &VocabularyTerm) -> u8 {
    if term.is_builtin {
        1
    } else {
        0
    }
}

pub fn create_vocabulary_term(
    state: &AppState,
    input: CreateVocabularyTermInput,
) -> Result<VocabularyTerm> {
    let prepared = prepare_term_input(input.canonical, input.aliases, input.match_mode)?;
    let connection = open_connection(state)?;
    ensure_no_conflicts(&connection, &prepared, None)?;

    let term_id = Uuid::new_v4().to_string();
    let created_at = Utc::now().to_rfc3339();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO custom_vocabulary_terms (id, canonical, normalized_canonical, match_mode, is_builtin, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6)",
        params![
            &term_id,
            &prepared.canonical,
            &prepared.normalized_canonical,
            to_match_mode(prepared.match_mode),
            &created_at,
            &created_at,
        ],
    )?;

    for (alias, normalized_alias) in &prepared.aliases {
        transaction.execute(
            "INSERT INTO custom_vocabulary_aliases (id, term_id, alias, normalized_alias)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::new_v4().to_string(),
                &term_id,
                alias,
                normalized_alias
            ],
        )?;
    }

    transaction.commit()?;
    get_vocabulary_term_by_id(&connection, &term_id)?
        .ok_or_else(|| anyhow!("created vocabulary term not found"))
}

pub fn update_vocabulary_term(
    state: &AppState,
    term_id: &str,
    input: UpdateVocabularyTermInput,
) -> Result<VocabularyTerm> {
    let prepared = prepare_term_input(input.canonical, input.aliases, input.match_mode)?;
    let connection = open_connection(state)?;
    let existing = get_vocabulary_term_by_id(&connection, term_id)?
        .ok_or_else(|| anyhow!("VOCABULARY_NOT_FOUND: term {term_id} does not exist"))?;
    ensure_no_conflicts(&connection, &prepared, Some(term_id))?;

    let updated_at = Utc::now().to_rfc3339();
    let transaction = connection.unchecked_transaction()?;
    transaction.execute(
        "UPDATE custom_vocabulary_terms
         SET canonical = ?1, normalized_canonical = ?2, match_mode = ?3, updated_at = ?4
         WHERE id = ?5",
        params![
            &prepared.canonical,
            &prepared.normalized_canonical,
            to_match_mode(prepared.match_mode),
            &updated_at,
            term_id,
        ],
    )?;
    transaction.execute(
        "DELETE FROM custom_vocabulary_aliases WHERE term_id = ?1",
        [term_id],
    )?;
    for (alias, normalized_alias) in &prepared.aliases {
        transaction.execute(
            "INSERT INTO custom_vocabulary_aliases (id, term_id, alias, normalized_alias)
             VALUES (?1, ?2, ?3, ?4)",
            params![Uuid::new_v4().to_string(), term_id, alias, normalized_alias],
        )?;
    }
    transaction.commit()?;

    let updated = get_vocabulary_term_by_id(&connection, term_id)?
        .ok_or_else(|| anyhow!("updated vocabulary term not found"))?;
    Ok(VocabularyTerm {
        is_builtin: existing.is_builtin,
        ..updated
    })
}

pub fn delete_vocabulary_term(state: &AppState, term_id: &str) -> Result<()> {
    let connection = open_connection(state)?;
    connection.execute(
        "DELETE FROM custom_vocabulary_terms WHERE id = ?1",
        [term_id],
    )?;
    Ok(())
}

pub fn correct_transcript_result(
    db_path: &Path,
    transcript: TranscriptResult,
) -> Result<TranscriptResult> {
    let terms = list_vocabulary_terms_from_db_path(db_path)?;
    correct_transcript_with_terms(&terms, transcript)
}

pub(crate) fn correct_transcript_with_terms(
    terms: &[VocabularyTerm],
    transcript: TranscriptResult,
) -> Result<TranscriptResult> {
    let mut segments = Vec::with_capacity(transcript.segments.len());
    let mut _correction_decisions = Vec::new();
    let matcher = if terms.is_empty() {
        None
    } else {
        Some(build_matcher(&terms))
    };

    // Whisper segments often split mid-sentence; only capitalize a segment's
    // first word when the previous segment ended a sentence.
    let mut sentence_start = true;
    for segment in transcript.segments {
        let (corrected_text, decisions) = matcher
            .as_ref()
            .map(|matcher| correct_segment_text_with_decisions(&segment.text, matcher))
            .unwrap_or_else(|| (segment.text.clone(), Vec::new()));
        let postprocessed_text = clean_punctuation_text_from(&corrected_text, sentence_start);
        if !postprocessed_text.is_empty() {
            sentence_start = ends_sentence(&postprocessed_text);
        }
        _correction_decisions.extend(decisions);
        segments.push(TranscriptSegment {
            text: postprocessed_text,
            ..segment
        });
    }

    let (plain_text, full_text, timestamped_text) = rebuild_transcript_text(&segments);

    Ok(TranscriptResult {
        plain_text,
        full_text,
        timestamped_text,
        segments,
        ..transcript
    })
}

fn rebuild_transcript_text(segments: &[TranscriptSegment]) -> (String, String, String) {
    let plain_text = clean_punctuation_text(
        &segments
            .iter()
            .map(|segment| segment.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    );
    let timestamped_text = segments
        .iter()
        .map(|segment| {
            format!(
                "[{} - {}] {}: {}",
                crate::output_format::clock_ms(segment.start_ms),
                crate::output_format::clock_ms(segment.end_ms),
                segment.language_code,
                segment.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    (plain_text.clone(), plain_text, timestamped_text)
}

fn ends_sentence(text: &str) -> bool {
    text.trim_end()
        .trim_end_matches(|ch: char| is_quote_mark(ch) || matches!(ch, ')' | ']' | '’' | '\''))
        .ends_with(['.', '!', '?', '…'])
}

fn clean_punctuation_text(input: &str) -> String {
    clean_punctuation_text_from(input, true)
}

fn clean_punctuation_text_from(input: &str, sentence_start: bool) -> String {
    let mut output = String::with_capacity(input.len());
    let mut pending_space = false;
    let mut chars = input.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch.is_whitespace() {
            pending_space = !output.is_empty();
            continue;
        }

        if is_closing_punctuation(ch) {
            // Keep "10:30", "3.5", "1,5", "example.com" and "https://…" intact:
            // punctuation directly followed by a letter or digit is part of a
            // number, time, URL or abbreviation, so its spacing is preserved.
            let embedded = matches!(ch, '.' | ',' | ':')
                && output.chars().last().is_some_and(|prev| !prev.is_whitespace())
                && chars
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || *next == '/');
            if embedded && !pending_space {
                output.push(ch);
                continue;
            }
            trim_trailing_spaces(&mut output);
            output.push(ch);
            // `„Komm!“` — a quote right after the punctuation closes it.
            pending_space = !chars.peek().is_some_and(|next| is_quote_mark(*next));
            continue;
        }

        if is_spacing_preserved_punctuation(ch) {
            // Quotes and dashes keep the spacing the transcript already has:
            // `sagte „Hallo“ und`, `He said "hi" and`, `this - not that`,
            // `state-of-the-art`.
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            output.push(ch);
            pending_space = false;
            continue;
        }

        if is_opening_punctuation(ch) {
            if pending_space && !output.is_empty() {
                output.push(' ');
            }
            output.push(ch);
            pending_space = false;
            continue;
        }

        if is_connector_punctuation(ch) {
            trim_trailing_spaces(&mut output);
            output.push(ch);
            while matches!(chars.peek(), Some(next) if next.is_whitespace()) {
                chars.next();
            }
            pending_space = false;
            continue;
        }

        if pending_space && !output.is_empty() && !ends_with_spacing_suppressed_char(&output) {
            output.push(' ');
        }

        output.push(ch);
        pending_space = false;
    }

    capitalize_sentences(output.trim(), sentence_start)
}

fn trim_trailing_spaces(output: &mut String) {
    while output.ends_with(' ') {
        output.pop();
    }
}

fn ends_with_spacing_suppressed_char(output: &str) -> bool {
    matches!(
        output.chars().last(),
        Some(' ' | '(' | '[' | '{' | '‘')
    )
}

fn is_closing_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '.' | ',' | '!' | '?' | ';' | ':' | ')' | ']' | '}' | '%' | '…'
    )
}

fn is_opening_punctuation(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '‘')
}

fn is_connector_punctuation(ch: char) -> bool {
    matches!(ch, '\'' | '’' | '/')
}

fn is_quote_mark(ch: char) -> bool {
    matches!(ch, '"' | '“' | '”' | '„' | '«' | '»')
}

fn is_spacing_preserved_punctuation(ch: char) -> bool {
    is_quote_mark(ch) || matches!(ch, '-' | '–' | '—')
}

fn capitalize_sentences(input: &str, capitalize_first: bool) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut output = String::with_capacity(input.len());
    let mut capitalize_next = capitalize_first;

    for (index, &ch) in chars.iter().enumerate() {
        if capitalize_next && ch.is_alphabetic() {
            capitalize_next = false;
            // Keep deliberate mixed-case spellings ("iPhone", "eBay", "macOS").
            let word_has_inner_capital = chars[index + 1..]
                .iter()
                .take_while(|next| next.is_alphanumeric())
                .any(|next| next.is_uppercase());
            if word_has_inner_capital {
                output.push(ch);
            } else {
                output.extend(ch.to_uppercase());
            }
            continue;
        }

        output.push(ch);

        // Only a terminator followed by whitespace ends a sentence, so
        // "e.g.", "3.5" and "example.com" do not trigger capitals.
        if matches!(ch, '.' | '!' | '?')
            && chars.get(index + 1).is_none_or(|next| next.is_whitespace())
        {
            capitalize_next = true;
        } else if !ch.is_whitespace() && !matches!(ch, '"' | '”' | '“' | '„' | '«' | '\'' | '’') {
            capitalize_next = false;
        }
    }

    output
}

fn prepare_term_input(
    canonical: String,
    aliases: Vec<String>,
    match_mode: MatchMode,
) -> Result<PreparedTermInput> {
    let canonical = canonical.trim().to_string();
    if canonical.is_empty() {
        return Err(anyhow!(
            "VOCABULARY_INVALID: canonical term must not be empty"
        ));
    }
    let normalized_canonical = normalize_for_match(&canonical);
    if normalized_canonical.is_empty() {
        return Err(anyhow!(
            "VOCABULARY_INVALID: canonical term must contain letters or numbers"
        ));
    }

    let mut seen = HashSet::new();
    let mut prepared_aliases = Vec::new();
    for alias in aliases {
        let alias = alias.trim().to_string();
        if alias.is_empty() {
            continue;
        }
        let normalized_alias = normalize_for_match(&alias);
        if normalized_alias.is_empty() || normalized_alias == normalized_canonical {
            continue;
        }
        if seen.insert(normalized_alias.clone()) {
            prepared_aliases.push((alias, normalized_alias));
        }
    }

    Ok(PreparedTermInput {
        canonical,
        normalized_canonical,
        match_mode,
        aliases: prepared_aliases,
    })
}

fn ensure_no_conflicts(
    connection: &Connection,
    prepared: &PreparedTermInput,
    current_term_id: Option<&str>,
) -> Result<()> {
    let excluded = current_term_id.unwrap_or("");
    let mut candidates = prepared
        .aliases
        .iter()
        .map(|(_, normalized)| normalized.clone())
        .collect::<Vec<_>>();
    candidates.push(prepared.normalized_canonical.clone());

    for candidate in candidates {
        let canonical_conflict: Option<String> = connection
            .query_row(
                "SELECT canonical FROM custom_vocabulary_terms
                 WHERE normalized_canonical = ?1 AND id != ?2
                 LIMIT 1",
                params![&candidate, excluded],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(conflict) = canonical_conflict {
            return Err(anyhow!(
                "VOCABULARY_CONFLICT: '{}' conflicts with existing term '{}'",
                candidate,
                conflict
            ));
        }

        let alias_conflict: Option<String> = connection
            .query_row(
                "SELECT alias FROM custom_vocabulary_aliases
                 WHERE normalized_alias = ?1 AND term_id != ?2
                 LIMIT 1",
                params![&candidate, excluded],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(conflict) = alias_conflict {
            return Err(anyhow!(
                "VOCABULARY_CONFLICT: '{}' conflicts with existing alias '{}'",
                candidate,
                conflict
            ));
        }
    }

    Ok(())
}

fn build_matcher(terms: &[VocabularyTerm]) -> VocabularyMatcher {
    let mut matcher = VocabularyMatcher::default();
    for term in terms {
        let exact_variants = std::iter::once((
            term.normalized_canonical.clone(),
            MatchCandidateSource::Canonical,
        ))
        .chain(
            term.aliases
                .iter()
                .map(|alias| (alias.normalized_alias.clone(), MatchCandidateSource::Alias)),
        )
        .collect::<Vec<_>>();

        for (normalized, source) in exact_variants {
            if normalized.is_empty() {
                continue;
            }
            let token_count = normalized.split_whitespace().count();
            matcher
                .exact
                .entry((token_count, normalized.clone()))
                .or_insert_with(|| term.canonical.clone());

            if source == MatchCandidateSource::Alias && term.match_mode == MatchMode::ExactAndFuzzy
            {
                matcher
                    .fuzzy
                    .entry(token_count)
                    .or_default()
                    .push(MatchCandidate {
                        canonical: term.canonical.clone(),
                        normalized,
                        source,
                    });
            }
        }
    }
    matcher
}

fn correct_segment_text_with_decisions(
    input: &str,
    matcher: &VocabularyMatcher,
) -> (String, Vec<CorrectionDecision>) {
    let tokens = tokenize_with_whitespace(input);
    if tokens.is_empty() {
        return (input.to_string(), Vec::new());
    }

    let mut output = String::new();
    let mut decisions = Vec::new();
    let mut index = 0;
    while index < tokens.len() {
        let mut replacement: Option<(usize, String, CorrectionStrategy, Option<f64>)> = None;

        for span_len in (1..=MAX_TOKEN_SPAN).rev() {
            if index + span_len > tokens.len() {
                continue;
            }
            let normalized_span = normalize_span(&tokens[index..index + span_len]);
            if normalized_span.is_empty() {
                continue;
            }

            if let Some(canonical) = matcher.exact.get(&(span_len, normalized_span.clone())) {
                let strategy = if normalized_span == normalize_for_match(canonical) {
                    CorrectionStrategy::ExactCanonical
                } else {
                    CorrectionStrategy::ExactAlias
                };
                replacement = Some((span_len, canonical.clone(), strategy, None));
                break;
            }

            if let Some((canonical, score)) = fuzzy_match(&normalized_span, span_len, matcher) {
                replacement = Some((
                    span_len,
                    canonical,
                    CorrectionStrategy::FuzzyAlias,
                    Some(score),
                ));
                break;
            }
        }

        if let Some((span_len, canonical, strategy, score)) = replacement {
            let first = parse_token_parts(&tokens[index].raw);
            let last = parse_token_parts(&tokens[index + span_len - 1].raw);
            output.push_str(first.prefix);
            output.push_str(&canonical);
            output.push_str(last.suffix);
            output.push_str(&tokens[index + span_len - 1].trailing_ws);
            decisions.push(CorrectionDecision {
                original_span: tokens[index..index + span_len]
                    .iter()
                    .map(|token| token.raw.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
                replacement: canonical,
                strategy,
                score,
            });
            index += span_len;
        } else {
            output.push_str(&tokens[index].raw);
            output.push_str(&tokens[index].trailing_ws);
            index += 1;
        }
    }

    (output, decisions)
}

fn fuzzy_match(
    normalized_span: &str,
    token_count: usize,
    matcher: &VocabularyMatcher,
) -> Option<(String, f64)> {
    let candidates = matcher.fuzzy.get(&token_count)?;
    if is_ambiguous_common_span(normalized_span) {
        return None;
    }
    let threshold = if token_count == 1 {
        SINGLE_TOKEN_THRESHOLD
    } else {
        MULTI_TOKEN_THRESHOLD
    };
    let span_len = normalized_span.chars().count() as i64;

    let mut best: Option<(&MatchCandidate, f64)> = None;
    let mut second_best: Option<f64> = None;
    for candidate in candidates {
        if candidate.source != MatchCandidateSource::Alias {
            continue;
        }
        let candidate_len = candidate.normalized.chars().count() as i64;
        if (candidate_len - span_len).abs() > 2 && candidate_len.max(span_len) > 0 {
            continue;
        }
        let score = jaro_winkler(normalized_span, &candidate.normalized);
        if score < threshold {
            continue;
        }
        match best {
            Some((_, best_score)) if score > best_score => {
                second_best = Some(best_score);
                best = Some((candidate, score));
            }
            Some((_, best_score))
                if second_best.is_none_or(|value| score > value) && score <= best_score =>
            {
                second_best = Some(score);
            }
            None => best = Some((candidate, score)),
            _ => {}
        }
    }

    let (candidate, score) = best?;
    if second_best.is_some_and(|value| (score - value) < MIN_FUZZY_LEAD) {
        return None;
    }

    Some((candidate.canonical.clone(), score))
}

fn tokenize_with_whitespace(input: &str) -> Vec<TokenPart> {
    let mut tokens = Vec::new();
    let mut chars = input.char_indices().peekable();
    while let Some((start, ch)) = chars.peek().copied() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        let token_start = start;
        let mut token_end = input.len();
        while let Some((idx, value)) = chars.peek().copied() {
            if value.is_whitespace() {
                token_end = idx;
                break;
            }
            chars.next();
        }

        let mut ws_end = token_end;
        while let Some((idx, value)) = chars.peek().copied() {
            if !value.is_whitespace() {
                break;
            }
            chars.next();
            ws_end = idx + value.len_utf8();
        }

        tokens.push(TokenPart {
            raw: input[token_start..token_end].to_string(),
            trailing_ws: input[token_end..ws_end].to_string(),
        });
    }
    tokens
}

fn normalize_span(tokens: &[TokenPart]) -> String {
    tokens
        .iter()
        .filter_map(|token| {
            let parsed = parse_token_parts(&token.raw);
            let normalized = normalize_for_match(parsed.core);
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_token_parts(raw: &str) -> ParsedToken<'_> {
    let mut start = 0;
    let mut end = raw.len();
    let indices = raw.char_indices().collect::<Vec<_>>();

    for (idx, ch) in &indices {
        if ch.is_alphanumeric() {
            start = *idx;
            break;
        }
        start = idx + ch.len_utf8();
    }

    for (idx, ch) in indices.iter().rev() {
        if ch.is_alphanumeric() {
            end = idx + ch.len_utf8();
            break;
        }
        end = *idx;
    }

    if start >= end || start >= raw.len() {
        return ParsedToken {
            prefix: "",
            core: raw,
            suffix: "",
        };
    }

    ParsedToken {
        prefix: &raw[..start],
        core: &raw[start..end],
        suffix: &raw[end..],
    }
}

pub fn normalize_for_match(input: &str) -> String {
    input
        .split_whitespace()
        .map(|part| {
            part.chars()
                .filter(|ch| ch.is_alphanumeric())
                .flat_map(|ch| ch.to_lowercase())
                .collect::<String>()
        })
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_ambiguous_common_span(normalized_span: &str) -> bool {
    let tokens = normalized_span.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return true;
    }

    if tokens.len() == 1 {
        return tokens[0].chars().count() <= 4 || is_common_function_word(tokens[0]);
    }

    tokens.iter().all(|token| is_common_function_word(token))
}

fn is_common_function_word(token: &str) -> bool {
    matches!(
        token,
        "a" | "an"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "but"
            | "by"
            | "for"
            | "from"
            | "he"
            | "her"
            | "him"
            | "his"
            | "i"
            | "if"
            | "in"
            | "into"
            | "is"
            | "it"
            | "its"
            | "me"
            | "my"
            | "not"
            | "of"
            | "on"
            | "or"
            | "our"
            | "she"
            | "that"
            | "the"
            | "their"
            | "them"
            | "there"
            | "they"
            | "this"
            | "to"
            | "too"
            | "up"
            | "us"
            | "use"
            | "we"
            | "you"
            | "your"
    )
}

fn list_aliases_for_term(connection: &Connection, term_id: &str) -> Result<Vec<VocabularyAlias>> {
    let mut statement = connection.prepare(
        "SELECT id, alias, normalized_alias
         FROM custom_vocabulary_aliases
         WHERE term_id = ?1
         ORDER BY alias COLLATE NOCASE ASC",
    )?;
    let rows = statement.query_map([term_id], |row| {
        Ok(VocabularyAlias {
            id: row.get("id")?,
            alias: row.get("alias")?,
            normalized_alias: row.get("normalized_alias")?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .context("failed to read vocabulary aliases")
}

fn get_vocabulary_term_by_id(
    connection: &Connection,
    term_id: &str,
) -> Result<Option<VocabularyTerm>> {
    let term = connection
        .query_row(
            "SELECT id, canonical, normalized_canonical, match_mode, is_builtin, created_at, updated_at
             FROM custom_vocabulary_terms
             WHERE id = ?1",
            [term_id],
            map_vocabulary_term_row,
        )
        .optional()?;

    if let Some(mut term) = term {
        term.aliases = list_aliases_for_term(connection, term_id)?;
        Ok(Some(term))
    } else {
        Ok(None)
    }
}

fn map_vocabulary_term_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<VocabularyTerm> {
    Ok(VocabularyTerm {
        id: row.get("id")?,
        canonical: row.get("canonical")?,
        normalized_canonical: row.get("normalized_canonical")?,
        match_mode: parse_match_mode(row.get("match_mode")?)?,
        is_builtin: row.get("is_builtin")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        aliases: Vec::new(),
    })
}

fn parse_match_mode(value: String) -> rusqlite::Result<MatchMode> {
    match value.as_str() {
        "exact_only" => Ok(MatchMode::ExactOnly),
        "exact_and_fuzzy" => Ok(MatchMode::ExactAndFuzzy),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn to_match_mode(value: MatchMode) -> &'static str {
    match value {
        MatchMode::ExactOnly => "exact_only",
        MatchMode::ExactAndFuzzy => "exact_and_fuzzy",
    }
}

fn open_connection(state: &AppState) -> Result<Connection> {
    open_connection_by_path(&state.db_path)
}

fn alias_id(term_id: &str, alias: &str) -> String {
    format!("{term_id}-{}", normalize_for_match(alias).replace(' ', "-"))
}

struct BuiltinSeedSpec<'a> {
    id: &'a str,
    canonical: &'a str,
    aliases: &'a [&'a str],
    created_at: &'a str,
}

fn builtin_term_specs() -> &'static [BuiltinSeedSpec<'static>] {
    &[
        BuiltinSeedSpec {
            id: "builtin-linkedin",
            canonical: "LinkedIn",
            aliases: &["linked in", "linken"],
            created_at: "2026-03-14T00:00:00Z",
        },
        BuiltinSeedSpec {
            id: "builtin-github",
            canonical: "GitHub",
            aliases: &["git hub"],
            created_at: "2026-03-14T00:00:00Z",
        },
        BuiltinSeedSpec {
            id: "builtin-openai",
            canonical: "OpenAI",
            aliases: &["open ai"],
            created_at: "2026-03-14T00:00:00Z",
        },
        BuiltinSeedSpec {
            id: "builtin-chatgpt",
            canonical: "ChatGPT",
            aliases: &["chat gpt"],
            created_at: "2026-03-14T00:00:00Z",
        },
        BuiltinSeedSpec {
            id: "builtin-whatsapp",
            canonical: "WhatsApp",
            aliases: &[],
            created_at: "2026-03-14T00:00:00Z",
        },
        BuiltinSeedSpec {
            id: "builtin-youtube",
            canonical: "YouTube",
            aliases: &["you tube"],
            created_at: "2026-03-14T00:00:00Z",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_term(
        canonical: &str,
        aliases: &[&str],
        match_mode: MatchMode,
        is_builtin: bool,
    ) -> VocabularyTerm {
        VocabularyTerm {
            id: canonical.to_string(),
            canonical: canonical.to_string(),
            normalized_canonical: normalize_for_match(canonical),
            match_mode,
            is_builtin,
            created_at: "2026-03-30T00:00:00Z".to_string(),
            updated_at: "2026-03-30T00:00:00Z".to_string(),
            aliases: aliases
                .iter()
                .map(|alias| VocabularyAlias {
                    id: format!("{canonical}-{alias}"),
                    alias: (*alias).to_string(),
                    normalized_alias: normalize_for_match(alias),
                })
                .collect(),
        }
    }

    #[test]
    fn exact_builtin_alias_still_corrects() {
        let matcher = build_matcher(&[make_term(
            "YouTube",
            &["you tube"],
            MatchMode::ExactOnly,
            true,
        )]);
        let (corrected, decisions) = correct_segment_text_with_decisions("you tube", &matcher);
        assert_eq!(corrected, "YouTube");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].strategy, CorrectionStrategy::ExactAlias);
    }

    #[test]
    fn builtin_term_does_not_fuzzy_match_common_phrase() {
        let matcher = build_matcher(&[make_term(
            "YouTube",
            &["you tube"],
            MatchMode::ExactOnly,
            true,
        )]);
        let (corrected, decisions) =
            correct_segment_text_with_decisions("I want you to use it", &matcher);
        assert_eq!(corrected, "I want you to use it");
        assert!(decisions.is_empty());
    }

    #[test]
    fn custom_alias_can_fuzzy_match_when_enabled() {
        let matcher = build_matcher(&[make_term(
            "CloudOpus",
            &["cloud oppus"],
            MatchMode::ExactAndFuzzy,
            false,
        )]);
        let (corrected, decisions) = correct_segment_text_with_decisions("cloud opus", &matcher);
        assert_eq!(corrected, "CloudOpus");
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].strategy, CorrectionStrategy::FuzzyAlias);
        assert!(decisions[0].score.is_some());
    }

    #[test]
    fn custom_term_in_exact_only_mode_skips_fuzzy_match() {
        let matcher = build_matcher(&[make_term(
            "Obsidian",
            &["obsidyen"],
            MatchMode::ExactOnly,
            false,
        )]);
        let (corrected, decisions) = correct_segment_text_with_decisions("obsidion", &matcher);
        assert_eq!(corrected, "obsidion");
        assert!(decisions.is_empty());
    }

    #[test]
    fn ambiguous_common_words_do_not_fuzzy_match() {
        let matcher = build_matcher(&[make_term(
            "YouTube",
            &["you toob"],
            MatchMode::ExactAndFuzzy,
            false,
        )]);
        let (corrected, decisions) =
            correct_segment_text_with_decisions("I want you to use only cloud opus 4.6", &matcher);
        assert_eq!(corrected, "I want you to use only cloud opus 4.6");
        assert!(decisions.is_empty());
    }

    #[test]
    fn asr_prompt_contains_canonical_spellings_but_not_aliases() {
        let term = make_term(
            "CloudOpus",
            &["cloud oppus"],
            MatchMode::ExactAndFuzzy,
            false,
        );
        let prompt = build_asr_prompt(&[term]).expect("prompt");
        assert!(prompt.text.contains("\"CloudOpus\""));
        assert!(!prompt.text.contains("cloud oppus"));
        assert_eq!(prompt.terms, vec!["CloudOpus"]);
    }

    #[test]
    fn recent_custom_terms_are_prioritized_and_builtins_are_last() {
        let mut recent = make_term("Élodie", &[], MatchMode::ExactOnly, false);
        recent.updated_at = "2026-04-01T00:00:00Z".to_string();
        let mut older = make_term("Murmela", &[], MatchMode::ExactOnly, false);
        older.updated_at = "2026-03-01T00:00:00Z".to_string();
        let builtin = make_term("OpenAI", &[], MatchMode::ExactOnly, true);
        let prompt = build_asr_prompt(&[builtin, older, recent]).expect("prompt");
        assert_eq!(prompt.terms, vec!["Élodie", "Murmela", "OpenAI"]);
    }

    #[test]
    fn asr_prompt_never_exceeds_character_budget() {
        let terms = (0..100)
            .map(|index| {
                make_term(
                    &format!("VeryLongDictionaryTermNumber{index:03}"),
                    &[],
                    MatchMode::ExactOnly,
                    false,
                )
            })
            .collect::<Vec<_>>();
        let prompt = build_asr_prompt(&terms).expect("prompt");
        assert!(prompt.text.chars().count() <= DICTIONARY_PROMPT_MAX_CHARS);
        assert!(prompt.truncated_count > 0);
        assert_eq!(prompt.included_count + prompt.truncated_count, 100);
    }

    #[test]
    fn punctuation_cleanup_preserves_numbers_times_and_urls() {
        for (input, expected) in [
            ("Das Meeting ist um 10:30 Uhr.", "Das Meeting ist um 10:30 Uhr."),
            ("Der Wert ist 3.5 Prozent, also 1,5 Punkte.", "Der Wert ist 3.5 Prozent, also 1,5 Punkte."),
            ("Das kostet 1.000.000 Euro.", "Das kostet 1.000.000 Euro."),
            ("Visit example.com for details.", "Visit example.com for details."),
            ("Siehe https://example.com/a dazu.", "Siehe https://example.com/a dazu."),
            ("Hallo , wie geht es ?", "Hallo, wie geht es?"),
        ] {
            assert_eq!(clean_punctuation_text(input), expected, "input: {input}");
        }
    }

    #[test]
    fn punctuation_cleanup_preserves_quote_and_dash_spacing() {
        for (input, expected) in [
            ("He said \"hello\" and left.", "He said \"hello\" and left."),
            ("Er sagte „Hallo“ und ging.", "Er sagte „Hallo“ und ging."),
            ("Er rief: „Komm!“ Dann ging er.", "Er rief: „Komm!“ Dann ging er."),
            ("We need this - not that.", "We need this - not that."),
            ("A state-of-the-art tool.", "A state-of-the-art tool."),
            ("Es ist 2024–2026 geplant.", "Es ist 2024–2026 geplant."),
        ] {
            assert_eq!(clean_punctuation_text(input), expected, "input: {input}");
        }
    }

    #[test]
    fn segments_split_mid_sentence_are_not_capitalized() {
        let segment = |text: &str, order: i32| TranscriptSegment {
            id: format!("s{order}"),
            start_ms: order as i64 * 1_000,
            end_ms: order as i64 * 1_000 + 900,
            text: text.into(),
            language_code: "en".into(),
            segment_order: order,
            confidence: None,
            speaker_id: None,
            speaker_ids: None,
            speaker_attribution: crate::speaker_reconciliation::SpeakerAttribution::None,
            speaker_confidence: None,
        };
        let segments = vec![
            segment("so we went to the", 0),
            segment("store and then", 1),
            segment("we bought milk.", 2),
            segment("then we left.", 3),
        ];
        let texts: Vec<String> = {
            let mut start = true;
            segments
                .iter()
                .map(|s| {
                    let cleaned = clean_punctuation_text_from(&s.text, start);
                    start = ends_sentence(&cleaned);
                    cleaned
                })
                .collect()
        };
        assert_eq!(texts, ["So we went to the", "store and then", "we bought milk.", "Then we left."]);
    }

    #[test]
    fn mixed_case_brand_names_keep_their_spelling_at_sentence_start() {
        assert_eq!(clean_punctuation_text("iPhone und eBay sind Marken."), "iPhone und eBay sind Marken.");
        assert_eq!(clean_punctuation_text("gut. macOS ist da."), "Gut. macOS ist da.");
        assert_eq!(clean_punctuation_text("hello world."), "Hello world.");
    }
}
