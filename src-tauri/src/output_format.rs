//! Shared output formatting so every engine and export writes the same shapes:
//! clock times, language codes and transcript titles.

use chrono::{DateTime, Local};

/// `MM:SS` below one hour, `H:MM:SS` from one hour on. Negative values clamp to 0.
pub fn clock_ms(ms: i64) -> String {
    let total_seconds = ms.max(0) / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds / 60) % 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// One line of `timestamped_text`: `[MM:SS - MM:SS] lang: text`.
pub fn timestamped_line(start_ms: i64, end_ms: i64, language: &str, text: &str) -> String {
    format!(
        "[{} - {}] {}: {}",
        clock_ms(start_ms),
        clock_ms(end_ms),
        language,
        text
    )
}

const LANGUAGE_NAMES: &[(&str, &[&str])] = &[
    ("en", &["english", "englisch"]),
    ("de", &["german", "deutsch"]),
    ("es", &["spanish", "español", "espanol", "spanisch"]),
    ("fr", &["french", "français", "francais", "französisch"]),
    ("it", &["italian", "italiano"]),
    ("pt", &["portuguese", "português", "portugues"]),
    ("nl", &["dutch", "nederlands"]),
    ("zh", &["chinese", "mandarin"]),
    ("yue", &["cantonese"]),
    ("ja", &["japanese"]),
    ("ko", &["korean"]),
    ("ru", &["russian"]),
    ("ar", &["arabic"]),
    ("tr", &["turkish"]),
    ("pl", &["polish"]),
    ("sv", &["swedish"]),
    ("da", &["danish"]),
    ("fi", &["finnish"]),
    ("cs", &["czech"]),
    ("el", &["greek"]),
    ("ro", &["romanian"]),
    ("hu", &["hungarian"]),
    ("hi", &["hindi"]),
    ("id", &["indonesian"]),
    ("ms", &["malay"]),
    ("th", &["thai"]),
    ("vi", &["vietnamese"]),
    ("fa", &["persian"]),
    ("fil", &["filipino"]),
    ("mk", &["macedonian"]),
];

/// Lower-case ISO 639 code for any engine's language value; `und` when unknown.
/// `"English"`, `"EN"`, `"en-US"` and `"en_us"` all become `"en"`.
pub fn normalize_language_code(value: Option<&str>) -> String {
    let Some(raw) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return "und".into();
    };
    let lower = raw.to_lowercase();
    if matches!(lower.as_str(), "und" | "unknown" | "auto" | "none" | "null") {
        return "und".into();
    }
    if let Some((code, _)) = LANGUAGE_NAMES
        .iter()
        .find(|(code, names)| lower == *code || names.contains(&lower.as_str()))
    {
        return (*code).into();
    }
    let base = lower.split(['-', '_']).next().unwrap_or_default();
    if (2..=3).contains(&base.len()) && base.chars().all(|ch| ch.is_ascii_lowercase()) {
        base.into()
    } else {
        "und".into()
    }
}

const AUDIO_EXTENSIONS: &[&str] = &[
    "wav", "mp3", "m4a", "opus", "aac", "mp4", "flac", "ogg", "mov", "m4v", "mkv", "webm", "avi",
];

/// Library title for an imported file: the file name without its audio/video extension.
pub fn file_display_title(original_name: &str) -> String {
    let name = original_name.trim();
    if let Some((stem, extension)) = name.rsplit_once('.') {
        if !stem.trim().is_empty()
            && AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str())
        {
            return stem.trim().to_string();
        }
    }
    name.to_string()
}

/// Title for a dictation without text, in local time and German date order.
pub fn dictation_fallback_title(now: DateTime<Local>) -> String {
    format!("Quick dictate {}", now.format("%d.%m.%Y %H:%M"))
}

/// First `max_chars` characters, with a single "…" when shortened.
pub fn truncate_title(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut title: String = trimmed.chars().take(max_chars).collect();
    title = title.trim_end().to_string();
    title.push('…');
    title
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn clock_uses_one_format_below_and_above_an_hour() {
        assert_eq!(clock_ms(-5), "00:00");
        assert_eq!(clock_ms(65_999), "01:05");
        assert_eq!(clock_ms(4_503_000), "1:15:03");
    }

    #[test]
    fn language_codes_are_normalized_for_every_engine() {
        for (input, expected) in [
            (None, "und"),
            (Some(""), "und"),
            (Some("unknown"), "und"),
            (Some("English"), "en"),
            (Some("EN"), "en"),
            (Some("en-US"), "en"),
            (Some("es_AR"), "es"),
            (Some("Deutsch"), "de"),
            (Some("yue"), "yue"),
            (Some("Klingon language"), "und"),
        ] {
            assert_eq!(normalize_language_code(input), expected, "{input:?}");
        }
    }

    #[test]
    fn file_titles_drop_only_audio_extensions() {
        assert_eq!(file_display_title("English-Spanish.m4a"), "English-Spanish");
        assert_eq!(file_display_title("Meeting 2026.05.WAV"), "Meeting 2026.05");
        assert_eq!(file_display_title("Team call.MOV"), "Team call");
        assert_eq!(file_display_title("talk.webm"), "talk");
        assert_eq!(file_display_title("notes.v2"), "notes.v2");
        assert_eq!(file_display_title(".m4a"), ".m4a");
    }

    #[test]
    fn fallback_titles_use_local_german_order_and_one_ellipsis() {
        let now = Local.with_ymd_and_hms(2026, 9, 25, 23, 4, 0).unwrap();
        assert_eq!(
            dictation_fallback_title(now),
            "Quick dictate 25.09.2026 23:04"
        );
        assert_eq!(truncate_title("short", 72), "short");
        assert_eq!(truncate_title("abcdef", 3), "abc…");
    }
}
