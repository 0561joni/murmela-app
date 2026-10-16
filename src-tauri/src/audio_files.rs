use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{Manager, Window};
use tauri_plugin_dialog::{DialogExt, FilePath};

use crate::audio_preprocess;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedSourceFile {
    pub file_path: String,
    pub original_name: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub duration_ms: Option<i64>,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTranscriptionRequest {
    pub job_id: String,
    pub source_file: SelectedSourceFile,
    pub speaker_count_hint: Option<i32>,
}

pub const UNSUPPORTED_DROP_MESSAGE: &str =
    "Drop audio (WAV, MP3, M4A, OPUS) or video (MP4, MOV, M4V, MKV, WEBM, AVI) files to transcribe.";

const SUPPORTED_MEDIA_EXTENSIONS: [&str; 10] = [
    "wav", "mp3", "m4a", "opus", "mp4", "mov", "m4v", "mkv", "webm", "avi",
];

pub async fn pick_audio_files(window: &Window) -> Result<Vec<SelectedSourceFile>> {
    let (tx, rx) = mpsc::channel();
    window
        .app_handle()
        .dialog()
        .file()
        .add_filter("Audio & video", &SUPPORTED_MEDIA_EXTENSIONS)
        .add_filter("Audio", audio_preprocess::AUDIO_FILE_EXTENSIONS)
        .add_filter("Video", audio_preprocess::VIDEO_FILE_EXTENSIONS)
        .pick_files(move |files| {
            let _ = tx.send(files);
        });
    let files = rx
        .recv()
        .map_err(|_| anyhow!("file picker did not return"))?;

    files
        .unwrap_or_default()
        .into_iter()
        .filter_map(file_path_to_pathbuf)
        .map(selected_source_file_from_path)
        .collect()
}

pub fn prepare_dropped_audio_files(paths: Vec<String>) -> Result<Vec<SelectedSourceFile>> {
    let mut seen_paths = HashSet::new();
    let mut selected_files = Vec::new();

    for raw_path in paths {
        let path = PathBuf::from(raw_path);
        if !is_supported_audio_path(&path) {
            continue;
        }

        let dedupe_key = path.to_string_lossy().to_string();
        if !seen_paths.insert(dedupe_key) {
            continue;
        }

        selected_files.push(selected_source_file_from_path(path)?);
    }

    if selected_files.is_empty() {
        return Err(anyhow!(UNSUPPORTED_DROP_MESSAGE));
    }

    Ok(selected_files)
}

fn file_path_to_pathbuf(file_path: FilePath) -> Option<PathBuf> {
    match file_path {
        FilePath::Path(path) => Some(path),
        _ => None,
    }
}

pub fn selected_source_file_from_path(path: PathBuf) -> Result<SelectedSourceFile> {
    audio_preprocess::validate_audio_file_size(&path)?;
    let metadata =
        fs::metadata(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let original_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("Unable to determine the file name"))?
        .to_string();

    // Decoding and fingerprinting happen in the queued preparation stage.
    Ok(SelectedSourceFile {
        file_path: path.display().to_string(),
        original_name,
        mime_type: mime_type_for_path(&path),
        size_bytes: metadata.len() as i64,
        duration_ms: None,
        sha256: None,
    })
}

fn is_supported_audio_path(path: &Path) -> bool {
    audio_preprocess::is_supported_media_path(path)
}

fn mime_type_for_path(path: &Path) -> String {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("mp3") => "audio/mpeg".to_string(),
        Some("m4a") => "audio/mp4".to_string(),
        Some("opus") => "audio/ogg".to_string(),
        Some("wav") => "audio/wav".to_string(),
        Some("mp4") => "video/mp4".to_string(),
        Some("m4v") => "video/x-m4v".to_string(),
        Some("mov") => "video/quicktime".to_string(),
        Some("mkv") => "video/x-matroska".to_string(),
        Some("webm") => "video/webm".to_string(),
        Some("avi") => "video/x-msvideo".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opus_paths_are_supported() {
        assert!(is_supported_audio_path(Path::new(
            "/tmp/voice-message.OPUS"
        )));
    }

    #[test]
    fn opus_paths_use_ogg_audio_mime_type() {
        assert_eq!(
            mime_type_for_path(Path::new("/tmp/voice-message.opus")),
            "audio/ogg"
        );
    }

    #[test]
    fn unsupported_drop_error_lists_all_supported_formats() {
        let error = prepare_dropped_audio_files(vec!["/tmp/document.pdf".to_string()])
            .expect_err("unsupported files should be rejected");

        assert_eq!(error.to_string(), UNSUPPORTED_DROP_MESSAGE);
    }

    #[test]
    fn video_paths_are_supported() {
        for name in [
            "/tmp/clip.mp4",
            "/tmp/Screen Recording.MOV",
            "/tmp/clip.m4v",
            "/tmp/talk.mkv",
            "/tmp/talk.webm",
            "/tmp/old.avi",
        ] {
            assert!(is_supported_audio_path(Path::new(name)), "{name}");
        }
        assert!(!is_supported_audio_path(Path::new("/tmp/slides.pdf")));
    }

    #[test]
    fn video_paths_use_video_mime_types() {
        assert_eq!(mime_type_for_path(Path::new("/tmp/a.mp4")), "video/mp4");
        assert_eq!(
            mime_type_for_path(Path::new("/tmp/a.MOV")),
            "video/quicktime"
        );
        assert_eq!(
            mime_type_for_path(Path::new("/tmp/a.mkv")),
            "video/x-matroska"
        );
        assert_eq!(mime_type_for_path(Path::new("/tmp/a.webm")), "video/webm");
    }

    #[test]
    fn picker_extensions_cover_audio_and_video_lists() {
        for extension in audio_preprocess::AUDIO_FILE_EXTENSIONS
            .iter()
            .chain(audio_preprocess::VIDEO_FILE_EXTENSIONS)
        {
            assert!(
                SUPPORTED_MEDIA_EXTENSIONS.contains(extension),
                "{extension}"
            );
        }
    }
}
