use serde::{Deserialize, Serialize};

pub const MOSS_MODEL_ID: &str = "moss-transcribe-diarize-0.9b-f16";
pub const MOSS_MODEL_NAME: &str = "MOSS Transcribe + Diarize 0.9B F16";
pub const MOSS_MODEL_DIR: &str = "moss-transcribe-diarize-0.9b-f16";
pub const MOSS_MODEL_REVISION: &str = "54e4bbd17da3f84adf1c1bcf7791b9b9266f741e";
pub const MOSS_MODEL_SIZE: i64 = 1_833_647_104;

pub const VIBEVOICE_MODEL_ID: &str = "vibevoice-asr-8bit-mlx";
pub const VIBEVOICE_MODEL_NAME: &str = "VibeVoice-ASR 8-bit MLX";
pub const VIBEVOICE_MODEL_DIR: &str = "vibevoice-asr-8bit-mlx";
pub const VIBEVOICE_MODEL_REVISION: &str = "725c72e54d6ef875472c27fbc50fab470a960940";
pub const VIBEVOICE_MODEL_SIZE: i64 = 9_521_624_407;
/// VibeVoice peaked at about 14 GB in the internal benchmark, so it is
/// offered only on Macs with at least 18 GB of installed memory.
pub const VIBEVOICE_MIN_MEMORY_BYTES: u64 = 18 * 1024 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelUseContext {
    ShortcutDictation,
    QuickDictate,
    FileTranscription,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelLanguageControl {
    AutomaticAndFixed,
    AutomaticOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    pub supported_contexts: Vec<ModelUseContext>,
    pub native_diarization: bool,
    pub timestamped_segments: bool,
    pub context_support: bool,
    pub language_control: ModelLanguageControl,
    pub maximum_audio_duration_ms: Option<i64>,
    #[serde(default)]
    pub streaming_transcription: bool,
}

impl ModelCapabilities {
    pub fn standard_asr() -> Self {
        Self {
            supported_contexts: vec![
                ModelUseContext::ShortcutDictation,
                ModelUseContext::QuickDictate,
                ModelUseContext::FileTranscription,
            ],
            native_diarization: false,
            timestamped_segments: true,
            context_support: true,
            language_control: ModelLanguageControl::AutomaticAndFixed,
            maximum_audio_duration_ms: None,
            streaming_transcription: false,
        }
    }

    pub fn moss() -> Self {
        Self {
            native_diarization: true,
            language_control: ModelLanguageControl::AutomaticOnly,
            maximum_audio_duration_ms: Some(90 * 60 * 1_000),
            ..Self::standard_asr()
        }
    }

    pub fn vibevoice() -> Self {
        Self {
            supported_contexts: vec![ModelUseContext::FileTranscription],
            native_diarization: true,
            timestamped_segments: true,
            context_support: true,
            language_control: ModelLanguageControl::AutomaticOnly,
            maximum_audio_duration_ms: Some(60 * 60 * 1_000),
            streaming_transcription: false,
        }
    }

    pub fn package_only() -> Self {
        Self {
            supported_contexts: Vec::new(),
            native_diarization: false,
            timestamped_segments: false,
            context_support: false,
            language_control: ModelLanguageControl::AutomaticOnly,
            maximum_audio_duration_ms: None,
            streaming_transcription: false,
        }
    }
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self::standard_asr()
    }
}

pub fn capabilities_for_model(model_id: &str, engine: &str) -> ModelCapabilities {
    match model_id {
        crate::live_pair::MODEL_ID => ModelCapabilities {
            supported_contexts: vec![ModelUseContext::ShortcutDictation],
            timestamped_segments: false,
            streaming_transcription: true,
            maximum_audio_duration_ms: Some(300_000),
            ..ModelCapabilities::standard_asr()
        },
        MOSS_MODEL_ID => ModelCapabilities::moss(),
        VIBEVOICE_MODEL_ID => ModelCapabilities::vibevoice(),
        _ if engine == "sherpa-onnx"
            || engine == "whisper.cpp-vad"
            || engine == "llama.cpp-translation" =>
        {
            ModelCapabilities::package_only()
        }
        _ => ModelCapabilities::standard_asr(),
    }
}

/// Apple Silicon on macOS 14 or newer, which the CoreML live-pair helper needs.
pub fn apple_silicon_supported() -> bool {
    cfg!(target_arch = "aarch64") && macos_version().is_some_and(|(major, _)| major >= 14)
}

/// Why VibeVoice cannot run on this Mac, or `None` when it can (worker aside).
pub fn vibevoice_unavailable_reason() -> Option<&'static str> {
    if !apple_silicon_supported() {
        return Some("Requires Apple Silicon and macOS 14 or newer.");
    }
    if !vibevoice_memory_sufficient(crate::memory_pressure::installed_bytes()) {
        return Some("Needs a Mac with at least 18 GB of memory.");
    }
    None
}

pub fn vibevoice_memory_sufficient(installed: Option<u64>) -> bool {
    installed.is_some_and(|bytes| bytes >= VIBEVOICE_MIN_MEMORY_BYTES)
}

/// The running macOS version as (major, minor), read once from `sw_vers`.
pub fn macos_version() -> Option<(u32, u32)> {
    static VERSION: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *VERSION.get_or_init(|| {
        let output = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let text = String::from_utf8(output.stdout).ok()?;
        let mut parts = text.trim().split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().and_then(|part| part.parse().ok()).unwrap_or(0);
        Some((major, minor))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_models_publish_their_context_and_duration_limits() {
        let moss = capabilities_for_model(MOSS_MODEL_ID, "moss-transcribe-cpp");
        assert_eq!(moss.supported_contexts.len(), 3);
        assert!(moss.native_diarization);
        assert_eq!(moss.maximum_audio_duration_ms, Some(5_400_000));

        let vibe = capabilities_for_model(VIBEVOICE_MODEL_ID, "vibevoice-mlx");
        assert_eq!(vibe.supported_contexts, [ModelUseContext::FileTranscription]);
        assert_eq!(vibe.maximum_audio_duration_ms, Some(3_600_000));
    }

    #[test]
    fn vibevoice_needs_eighteen_gigabytes_of_installed_memory() {
        assert!(!vibevoice_memory_sufficient(None));
        assert!(!vibevoice_memory_sufficient(Some(16 * 1024 * 1024 * 1024)));
        assert!(vibevoice_memory_sufficient(Some(18 * 1024 * 1024 * 1024)));
        assert!(vibevoice_memory_sufficient(Some(36 * 1024 * 1024 * 1024)));
    }

    #[test]
    fn streaming_is_opt_in_and_live_pair_cannot_enter_other_contexts() {
        let mut legacy = serde_json::to_value(ModelCapabilities::standard_asr()).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("streamingTranscription");
        let legacy: ModelCapabilities = serde_json::from_value(legacy).unwrap();
        assert!(!legacy.streaming_transcription);
        let live = capabilities_for_model(crate::live_pair::MODEL_ID, crate::live_pair::ENGINE);
        assert_eq!(
            live.supported_contexts,
            [ModelUseContext::ShortcutDictation]
        );
        assert!(live.streaming_transcription && live.context_support);
        assert!(!live.native_diarization && !live.timestamped_segments);
        assert_eq!(
            live.language_control,
            ModelLanguageControl::AutomaticAndFixed
        );
        assert_eq!(live.maximum_audio_duration_ms, Some(300_000));
    }
}
