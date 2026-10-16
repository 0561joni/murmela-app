//! Runtime checks for local translation output (the automated acceptance
//! checks, plus form-of-address consistency).
//! A failed check triggers one stricter retry; if that also fails, dictation
//! pastes the original text with a warning.

/// Reasons a translation should not be pasted. Empty means it passed.
pub fn check_translation(source: &str, output: &str, target: &str) -> Vec<&'static str> {
    let mut issues = Vec::new();
    let output_trimmed = output.trim();
    if output_trimmed.is_empty() {
        issues.push("empty");
        return issues;
    }
    if has_preface(output_trimmed) {
        issues.push("preface");
    }
    if number_signature(source) != number_signature(output) {
        issues.push("numbers_changed");
    }
    if links(source) != links(output) {
        issues.push("links_changed");
    }
    if paragraph_count(source) != paragraph_count(output) {
        issues.push("paragraphs_changed");
    }
    match target {
        "fr" if mixes_french_address(source, output) => issues.push("mixed_address"),
        "es-AR" if uses_tuteo(output) => issues.push("tuteo"),
        _ => {}
    }
    issues
}

/// Instruction sent with the retry, naming what the first attempt got wrong.
pub fn retry_instruction(issues: &[&str]) -> String {
    let mut rules = vec!["Output only the translation, with no introduction, notes or quotation marks."];
    for issue in issues {
        rules.push(match *issue {
            "numbers_changed" => "Copy every number exactly; only the decimal separator may follow the target language.",
            "links_changed" => "Copy every URL and email address exactly.",
            "paragraphs_changed" => "Keep exactly the same paragraph breaks as the source.",
            "mixed_address" => "Use one form of address throughout: tu unless the source is formal or plural.",
            "tuteo" => "Use voseo (vos) for informal address; never tú or vosotros.",
            _ => continue,
        });
    }
    rules.dedup();
    rules.join(" ")
}

pub fn describe(issues: &[&str]) -> String {
    issues
        .iter()
        .map(|issue| match *issue {
            "empty" => "empty output",
            "preface" => "added introduction",
            "numbers_changed" => "numbers changed",
            "links_changed" => "links changed",
            "paragraphs_changed" => "paragraphs changed",
            "mixed_address" => "mixed tu/vous",
            "tuteo" => "tú instead of vos",
            _ => "unexpected output",
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn has_preface(output: &str) -> bool {
    let lower = output.to_lowercase();
    [
        "translation:",
        "here is the translation",
        "traduction :",
        "traduction:",
        "voici la traduction",
        "traducción:",
        "acá está la traducción",
        "aquí está la traducción",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

/// Digits of every number, ignoring grouping/decimal separators, which
/// legitimately change between languages ("3.5" → "3,5", "1.000" → "1 000").
fn number_signature(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut numbers = Vec::new();
    let mut current = String::new();
    for (index, &ch) in chars.iter().enumerate() {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }
        let joins_digits = matches!(ch, '.' | ',' | ' ' | '\u{a0}' | '\u{202f}' | '\'')
            && !current.is_empty()
            && chars.get(index + 1).is_some_and(|next| next.is_ascii_digit());
        if joins_digits {
            continue;
        }
        if !current.is_empty() {
            numbers.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        numbers.push(current);
    }
    numbers.sort();
    numbers
}

fn links(text: &str) -> Vec<String> {
    let mut found: Vec<String> = text
        .split_whitespace()
        .map(|word| word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')', '"', '»', '“', '”']))
        .filter(|word| {
            word.starts_with("http://")
                || word.starts_with("https://")
                || word.starts_with("www.")
                || (word.contains('@') && word.contains('.'))
        })
        .map(str::to_string)
        .collect();
    found.sort();
    found
}

fn paragraph_count(text: &str) -> usize {
    text.trim()
        .split("\n\n")
        .filter(|paragraph| !paragraph.trim().is_empty())
        .count()
        .max(1)
}

fn words_of(sentence: &str) -> Vec<String> {
    sentence
        .split(|ch: char| !ch.is_alphabetic() && ch != '\'' && ch != '’')
        .flat_map(|word| word.split(['\'', '’']))
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// A sentence that addresses the reader with both tu and vous. Deliberately
/// loose: only subject/object pronouns count (not ton/ta/tes/votre/vos), and
/// sources that address a group or use formal German/English cues are skipped,
/// because "tu" for one person and "vous" for a group can both be correct.
fn mixes_french_address(source: &str, output: &str) -> bool {
    const TU: &[&str] = &["tu", "toi", "te"];
    const VOUS: &[&str] = &["vous", "veuillez"];
    const GROUP_OR_FORMAL: &[&str] = &[
        "ihr", "euch", "euer", "eure", "sie", "ihnen", "everyone", "everybody", "guys", "folks",
        "team", "all",
    ];
    let source_words = words_of(source);
    if source_words
        .iter()
        .any(|word| GROUP_OR_FORMAL.contains(&word.as_str()))
    {
        return false;
    }
    output.split(['.', '!', '?', '\n']).any(|sentence| {
        let words = words_of(sentence);
        words.iter().any(|w| TU.contains(&w.as_str()))
            && words.iter().any(|w| VOUS.contains(&w.as_str()))
    })
}

/// Peninsular/tuteo forms that Argentinian output must not use.
fn uses_tuteo(text: &str) -> bool {
    const FORMS: &[&str] = &["tú", "vosotros", "vosotras", "tienes", "puedes", "quieres", "sabes", "eres"];
    words_of(text).iter().any(|word| FORMS.contains(&word.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_faithful_translations_with_localized_numbers() {
        assert!(check_translation(
            "Der Preis beträgt 3.5 Prozent von 1.000.000 Euro.",
            "Le prix est de 3,5 pour cent de 1 000 000 euros.",
            "fr"
        )
        .is_empty());
        assert!(check_translation(
            "Kannst du mir morgen die Datei schicken?",
            "¿Me podés mandar el archivo mañana?",
            "es-AR"
        )
        .is_empty());
    }

    #[test]
    fn flags_the_problems_seen_in_the_benchmark_corpus() {
        assert_eq!(
            check_translation(
                "Please let us know if you can attend the appointment.",
                "Veuillez nous faire savoir si tu pourras assister au rendez-vous.",
                "fr"
            ),
            ["mixed_address"]
        );
        assert_eq!(
            check_translation("Das kostet 125 Euro.", "Traduction : Cela coûte 125 euros.", "fr"),
            ["preface"]
        );
        assert_eq!(
            check_translation("Siehe https://example.com/a", "Mirá https://example.com", "es-AR"),
            ["links_changed"]
        );
        assert_eq!(
            check_translation("Es kostet 125 Euro.", "Cuesta 152 euros.", "es-AR"),
            ["numbers_changed"]
        );
        assert_eq!(check_translation("Hast du Zeit?", "¿Tienes tiempo?", "es-AR"), ["tuteo"]);
        assert_eq!(
            check_translation("Eins.\n\nZwei.", "Un. Deux.", "fr"),
            ["paragraphs_changed"]
        );
    }

    #[test]
    fn french_address_check_allows_group_and_formal_sources() {
        // One person informally plus a group: correct French, not flagged.
        assert!(check_translation(
            "Kannst du ihnen sagen, dass ihr morgen kommt?",
            "Peux-tu leur dire que vous venez demain ?",
            "fr"
        )
        .is_empty());
        assert!(check_translation(
            "Tell everyone you will send the report.",
            "Dis à tout le monde que tu enverras le rapport, merci à vous.",
            "fr"
        )
        .is_empty());
        // Possessives alone no longer count.
        assert!(check_translation("Your choice.", "Ton choix, comme vous voulez.", "fr").is_empty());
    }

    #[test]
    fn retry_instruction_names_each_failed_rule_once() {
        let text = retry_instruction(&["numbers_changed", "mixed_address"]);
        assert!(text.contains("number"));
        assert!(text.contains("one form of address"));
    }
}
