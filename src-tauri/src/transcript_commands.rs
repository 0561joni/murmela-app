use std::collections::{HashMap, HashSet};
use std::fs;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::speaker_reconciliation::SpeakerAttribution;
use crate::storage::TranscriptDetail;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptCopyVariant {
    SpeakerAware,
    Plain,
    Translation,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptExportFormat {
    Txt,
    Md,
    Srt,
    Vtt,
    Json,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptExportResult {
    pub path: Option<String>,
}

pub fn copy(
    app: &AppHandle,
    detail: &TranscriptDetail,
    variant: TranscriptCopyVariant,
) -> Result<()> {
    let text = match variant {
        TranscriptCopyVariant::SpeakerAware => format_speaker_text(detail, false),
        TranscriptCopyVariant::Plain => detail.summary.plain_text.clone(),
        TranscriptCopyVariant::Translation => translation_text(detail)?.to_owned(),
    };
    app.clipboard().write_text(text).map_err(anyhow::Error::msg)
}

pub fn export_variant_blocking(
    app: &AppHandle,
    window: &WebviewWindow,
    detail: &TranscriptDetail,
    format: TranscriptExportFormat,
    variant: Option<TranscriptCopyVariant>,
) -> Result<TranscriptExportResult> {
    if unsafe { libc::pthread_main_np() } != 0 {
        anyhow::bail!("Export must run outside the macOS main thread.");
    }

    let (extension, label) = match format {
        TranscriptExportFormat::Txt => ("txt", "Plain text"),
        TranscriptExportFormat::Md => ("md", "Markdown"),
        TranscriptExportFormat::Srt => ("srt", "SubRip subtitles"),
        TranscriptExportFormat::Vtt => ("vtt", "WebVTT subtitles"),
        TranscriptExportFormat::Json => ("json", "JSON"),
    };
    let file_name = format!(
        "{}.{}",
        safe_file_stem(&crate::output_format::file_display_title(&detail.summary.title)),
        extension
    );
    let selected = app
        .dialog()
        .file()
        .set_parent(window)
        .set_title("Export transcript")
        .set_file_name(file_name)
        .add_filter(label, &[extension])
        .blocking_save_file();
    let path = match selected {
        Some(FilePath::Path(path)) => path,
        Some(FilePath::Url(_)) => {
            anyhow::bail!("The selected export destination is not a local file.")
        }
        None => return Ok(TranscriptExportResult { path: None }),
    };
    let contents = if matches!(variant, Some(TranscriptCopyVariant::Translation)) {
        match format {
            TranscriptExportFormat::Txt | TranscriptExportFormat::Md => {
                translation_text(detail)?.to_owned()
            }
            TranscriptExportFormat::Json => format_transcript(detail, format)?,
            _ => anyhow::bail!("Subtitles use the timed original transcript."),
        }
    } else {
        format_transcript(detail, format)?
    };
    fs::write(&path, contents)
        .with_context(|| format!("failed to export transcript to {}", path.display()))?;
    Ok(TranscriptExportResult {
        path: Some(path.to_string_lossy().into_owned()),
    })
}

fn translation_text(detail: &TranscriptDetail) -> Result<&str> {
    detail
        .translation
        .as_ref()
        .filter(|t| t.status == "completed")
        .and_then(|t| t.output_text.as_deref())
        .ok_or_else(|| anyhow::anyhow!("No completed translation is available."))
}

fn format_transcript(detail: &TranscriptDetail, format: TranscriptExportFormat) -> Result<String> {
    Ok(match format {
        TranscriptExportFormat::Txt => format_speaker_text(detail, false),
        TranscriptExportFormat::Md => format_speaker_text(detail, true),
        TranscriptExportFormat::Srt => format_subtitles(detail, false),
        TranscriptExportFormat::Vtt => format!("WEBVTT\n\n{}", format_subtitles(detail, true)),
        TranscriptExportFormat::Json => serde_json::to_string_pretty(&json_export(detail))?,
    })
}

/// Version of the JSON export layout. Bump it when a field changes meaning or
/// is removed; adding fields is backwards compatible.
pub const JSON_EXPORT_SCHEMA_VERSION: u32 = 1;

/// Stable, documented JSON export instead of the app's internal structures.
fn json_export(detail: &TranscriptDetail) -> serde_json::Value {
    let names = speaker_names(detail);
    let manual: HashSet<_> = detail
        .manual_segment_ids
        .iter()
        .map(String::as_str)
        .collect();
    let summary = &detail.summary;
    let segments: Vec<serde_json::Value> = detail
        .segments
        .iter()
        .map(|segment| {
            serde_json::json!({
                "startMs": segment.start_ms,
                "endMs": segment.end_ms,
                "start": crate::output_format::clock_ms(segment.start_ms),
                "end": crate::output_format::clock_ms(segment.end_ms),
                "speaker": segment_label(
                    segment.speaker_attribution,
                    segment.speaker_id.as_deref(),
                    segment.speaker_ids.as_deref(),
                    &names,
                    manual.contains(segment.id.as_str()),
                ),
                "language": crate::output_format::normalize_language_code(Some(&segment.language_code)),
                "text": segment.text.trim(),
            })
        })
        .collect();
    let speakers: Vec<serde_json::Value> = detail
        .speakers
        .iter()
        .map(|speaker| serde_json::json!({"id": speaker.speaker_id, "name": speaker.display_name}))
        .collect();
    let translation = detail.translation.as_ref().map(|translation| {
        serde_json::json!({
            "targetLanguage": translation.target_language,
            "status": translation.status,
            "text": translation.output_text,
            "model": translation.model_id,
            "promptVersion": translation.prompt_version,
        })
    });
    serde_json::json!({
        "schemaVersion": JSON_EXPORT_SCHEMA_VERSION,
        "app": "Murmela",
        "transcript": {
            "id": summary.id,
            "title": summary.title,
            "createdAt": summary.created_at,
            "source": summary.source_type,
            "durationMs": summary.duration_ms,
            "languages": summary
                .detected_languages
                .iter()
                .map(|language| crate::output_format::normalize_language_code(Some(language)))
                .collect::<Vec<_>>(),
            "model": summary.model_name,
            "text": summary.plain_text,
        },
        "speakers": speakers,
        "segments": segments,
        "translation": translation,
    })
}

fn format_speaker_text(detail: &TranscriptDetail, markdown: bool) -> String {
    let names = speaker_names(detail);
    let manual: HashSet<_> = detail
        .manual_segment_ids
        .iter()
        .map(String::as_str)
        .collect();
    let mut groups: Vec<(String, String, Vec<String>)> = Vec::new();
    for segment in &detail.segments {
        let label = segment_label(
            segment.speaker_attribution,
            segment.speaker_id.as_deref(),
            segment.speaker_ids.as_deref(),
            &names,
            manual.contains(segment.id.as_str()),
        );
        let identity = format!(
            "{:?}|{:?}|{:?}|{}",
            segment.speaker_attribution, segment.speaker_id, segment.speaker_ids, label
        );
        if let Some((previous, _, texts)) = groups.last_mut() {
            if *previous == identity {
                texts.push(segment.text.trim().to_string());
                continue;
            }
        }
        groups.push((identity, label, vec![segment.text.trim().to_string()]));
    }
    if groups.is_empty() {
        return detail.summary.plain_text.clone();
    }
    groups
        .into_iter()
        .map(|(_, label, texts)| {
            let text = texts.join(" ");
            if label.is_empty() {
                text
            } else if markdown {
                format!("**{label}:** {text}")
            } else {
                format!("{label}: {text}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn format_subtitles(detail: &TranscriptDetail, webvtt: bool) -> String {
    let names = speaker_names(detail);
    let manual: HashSet<_> = detail
        .manual_segment_ids
        .iter()
        .map(String::as_str)
        .collect();
    detail
        .segments
        .iter()
        .enumerate()
        .map(|(index, segment)| {
            let label = segment_label(
                segment.speaker_attribution,
                segment.speaker_id.as_deref(),
                segment.speaker_ids.as_deref(),
                &names,
                manual.contains(segment.id.as_str()),
            );
            let timing = format!(
                "{} --> {}",
                subtitle_time(segment.start_ms, webvtt),
                subtitle_time(segment.end_ms, webvtt)
            );
            let text = if label.is_empty() {
                segment.text.trim().to_string()
            } else {
                format!("{label}: {}", segment.text.trim())
            };
            if webvtt {
                format!("{timing}\n{text}")
            } else {
                format!("{}\n{timing}\n{text}", index + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn speaker_names(detail: &TranscriptDetail) -> HashMap<&str, &str> {
    detail
        .speakers
        .iter()
        .map(|speaker| (speaker.speaker_id.as_str(), speaker.display_name.as_str()))
        .collect()
}

fn segment_label(
    attribution: SpeakerAttribution,
    speaker_id: Option<&str>,
    speaker_ids: Option<&[String]>,
    names: &HashMap<&str, &str>,
    manual: bool,
) -> String {
    let name = |id: &str| {
        names
            .get(id)
            .copied()
            .unwrap_or("Unknown speaker")
            .to_string()
    };
    match attribution {
        SpeakerAttribution::Assigned => speaker_id
            .map(name)
            .unwrap_or_else(|| "Unknown speaker".into()),
        SpeakerAttribution::Likely => speaker_id
            .map(|id| format!("{} (likely)", name(id)))
            .unwrap_or_else(|| "Likely speaker".into()),
        SpeakerAttribution::Overlap => {
            let label = speaker_ids
                .unwrap_or_default()
                .iter()
                .map(|id| name(id))
                .collect::<Vec<_>>()
                .join(" + ");
            if label.is_empty() {
                "Overlapping speakers".into()
            } else {
                label
            }
        }
        SpeakerAttribution::Uncertain => {
            let candidates = speaker_ids
                .unwrap_or_default()
                .iter()
                .map(|id| name(id))
                .collect::<Vec<_>>()
                .join(" / ");
            if candidates.is_empty() {
                "Uncertain speaker".into()
            } else {
                format!("Uncertain: {candidates}")
            }
        }
        SpeakerAttribution::None => {
            if names.is_empty() && !manual {
                String::new()
            } else {
                "Unknown speaker".into()
            }
        }
    }
}

fn subtitle_time(ms: i64, webvtt: bool) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1_000) % 60;
    let millis = ms % 1_000;
    let separator = if webvtt { '.' } else { ',' };
    format!("{hours:02}:{minutes:02}:{seconds:02}{separator}{millis:03}")
}

fn safe_file_stem(title: &str) -> String {
    const MAX_STEM_BYTES: usize = 180;
    let mut stem = String::new();
    for character in title.chars().map(|character| {
        if character.is_alphanumeric() || matches!(character, ' ' | '-' | '_') {
            character
        } else {
            '_'
        }
    }) {
        if stem.len() + character.len_utf8() > MAX_STEM_BYTES {
            break;
        }
        stem.push(character);
    }
    let stem = stem.trim().to_string();
    if stem.is_empty() {
        "transcript".into()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn detail() -> TranscriptDetail {
        let store =
            crate::review::ReviewStore::new(std::env::temp_dir().join("unused-export-test.sqlite"));
        let source = crate::audio_files::SelectedSourceFile {
            file_path: "/original.wav".into(),
            original_name: "original.wav".into(),
            mime_type: "audio/wav".into(),
            size_bytes: 10,
            duration_ms: Some(20000),
            sha256: None,
        };
        let reference = store
            .create_session("export", source, crate::review::fixture_result())
            .unwrap();
        store.get(&reference).unwrap().detail
    }

    #[test]
    fn json_export_uses_the_versioned_schema() {
        let detail = detail();
        let value: serde_json::Value =
            serde_json::from_str(&format_transcript(&detail, TranscriptExportFormat::Json).unwrap())
                .unwrap();
        assert_eq!(value["schemaVersion"], JSON_EXPORT_SCHEMA_VERSION);
        assert_eq!(value["transcript"]["id"], detail.summary.id.as_str());
        assert_eq!(value["segments"].as_array().unwrap().len(), detail.segments.len());
        assert!(value["segments"][0]["start"].is_string());
        assert!(value.get("diarizationTurns").is_none());
    }

    #[test]
    fn every_export_uses_effective_names_without_changing_passage_text_or_times() {
        let mut detail = detail();
        detail.speakers[0].display_name = "Maya".into();
        detail.speakers[1].display_name = "Leo".into();
        detail.segments[0].speaker_attribution = SpeakerAttribution::Likely;
        for format in [
            TranscriptExportFormat::Txt,
            TranscriptExportFormat::Md,
            TranscriptExportFormat::Srt,
            TranscriptExportFormat::Vtt,
        ] {
            let output = format_transcript(&detail, format).unwrap();
            assert!(output.contains("Maya (likely)"));
            assert!(output.contains("Leo"));
            assert!(output.contains("First."));
            assert!(output.contains("Second."));
        }
        let srt = format_transcript(&detail, TranscriptExportFormat::Srt).unwrap();
        assert!(srt.contains("00:00:00,000 --> 00:00:10,000"));
        let json = format_transcript(&detail, TranscriptExportFormat::Json).unwrap();
        let exported: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(exported["segments"][0]["text"], detail.segments[0].text.trim());
        assert_eq!(exported["segments"][1]["startMs"], 10000);
        assert_eq!(exported["speakers"][0]["name"], "Maya");
        assert_eq!(exported["segments"][0]["speaker"], "Maya (likely)");
    }

    #[test]
    fn disabled_identification_does_not_add_unknown_prefixes() {
        let mut detail = detail();
        detail.speakers.clear();
        detail.diarization_turns.clear();
        for s in &mut detail.segments {
            s.speaker_id = None;
            s.speaker_ids = None;
            s.speaker_attribution = SpeakerAttribution::None;
        }
        for format in [
            TranscriptExportFormat::Txt,
            TranscriptExportFormat::Md,
            TranscriptExportFormat::Srt,
            TranscriptExportFormat::Vtt,
        ] {
            let output = format_transcript(&detail, format).unwrap();
            assert!(!output.contains("Unknown speaker"));
            assert!(output.contains("First."));
        }
        detail.manual_segment_ids = vec![detail.segments[0].id.clone()];
        assert_eq!(
            format_speaker_text(&detail, false),
            "Unknown speaker: First.\n\nSecond."
        );
    }

    #[test]
    fn identical_display_names_do_not_merge_distinct_speaker_passages() {
        let mut detail = detail();
        for s in &mut detail.speakers {
            s.display_name = "Alex".into();
        }
        assert_eq!(
            format_speaker_text(&detail, false),
            "Alex: First.\n\nAlex: Second."
        );
    }

    #[test]
    fn formats_subtitle_timestamps() {
        assert_eq!(subtitle_time(3_723_004, false), "01:02:03,004");
        assert_eq!(subtitle_time(3_723_004, true), "01:02:03.004");
    }

    #[test]
    fn export_file_stems_are_sanitized_and_byte_bounded() {
        assert_eq!(
            safe_file_stem("Meeting: Team / Status"),
            "Meeting_ Team _ Status"
        );
        let multibyte_title = "会".repeat(200);
        let stem = safe_file_stem(&multibyte_title);
        assert_eq!(stem.len(), 180);
        assert_eq!(stem.chars().count(), 60);
    }
}
