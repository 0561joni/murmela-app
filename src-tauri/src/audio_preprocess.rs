use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, Context, Result};
use hound::{SampleFormat, WavSpec, WavWriter};
use sha2::{Digest, Sha256};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::default::{get_codecs, get_probe};
use uuid::Uuid;

pub const TARGET_SAMPLE_RATE_HZ: u32 = 16_000;
pub const TARGET_CHANNELS: u16 = 1;
pub const MAX_AUDIO_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Videos are larger because the picture is skipped; only the audio track is decoded.
pub const MAX_VIDEO_FILE_BYTES: u64 = 20 * 1024 * 1024 * 1024;
pub const MAX_AUDIO_DURATION_MS: i64 = 6 * 60 * 60 * 1000;

/// Audio file extensions accepted for file transcription.
pub const AUDIO_FILE_EXTENSIONS: &[&str] = &["wav", "mp3", "m4a", "opus"];
/// Video containers whose first audio track is extracted and transcribed.
pub const VIDEO_FILE_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v", "mkv", "webm", "avi"];
/// Video containers that macOS (AVFoundation / WebKit) opens natively.
const APPLE_VIDEO_FILE_EXTENSIONS: &[&str] = &["mp4", "mov", "m4v"];

fn lowercase_extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
}

pub fn is_video_path(path: &Path) -> bool {
    lowercase_extension(path).is_some_and(|ext| VIDEO_FILE_EXTENSIONS.contains(&ext.as_str()))
}

pub fn is_supported_media_path(path: &Path) -> bool {
    lowercase_extension(path).is_some_and(|ext| {
        AUDIO_FILE_EXTENSIONS.contains(&ext.as_str())
            || VIDEO_FILE_EXTENSIONS.contains(&ext.as_str())
    })
}

/// True when the macOS web view can play the original file directly.
pub fn is_natively_playable_path(path: &Path) -> bool {
    !is_video_path(path)
        || lowercase_extension(path)
            .is_some_and(|ext| APPLE_VIDEO_FILE_EXTENSIONS.contains(&ext.as_str()))
}

#[derive(Debug, Clone)]
pub struct PreparedAudio {
    pub sample_rate_hz: u32,
    pub channels: u16,
    pub samples: Vec<f32>,
}

pub fn decode_audio_file(path: &Path) -> Result<PreparedAudio> {
    validate_audio_file_size(path)?;
    if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("wav"))
    {
        let prepared = decode_wav_file(path)?;
        validate_audio_duration(prepared_duration_ms(&prepared))?;
        return Ok(prepared);
    }

    let prepared = match decode_audio_file_native(path) {
        Ok(prepared) => Ok(prepared),
        Err(native_error) => decode_audio_file_with_fallback(path).with_context(|| {
            format!(
                "native decode failed for {}: {}",
                path.display(),
                native_error
            )
        }),
    }?;
    validate_audio_duration(prepared_duration_ms(&prepared))?;
    Ok(prepared)
}

fn decode_audio_file_native(path: &Path) -> Result<PreparedAudio> {
    let file = File::open(path)
        .with_context(|| format!("failed to open audio file {}", path.display()))?;
    let media_source = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        hint.with_extension(extension);
    }

    let probed = get_probe()
        .format(
            &hint,
            media_source,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .context("failed to probe audio format")?;
    let mut format = probed.format;
    let track = select_audio_track(format.tracks())
        .ok_or_else(|| anyhow!("No supported audio track was found"))?;
    let track_id = track.id;

    let mut decoder = get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("failed to create audio decoder")?;
    // AAC inside MP4/MOV often reports no channel layout until the first packet
    // is decoded, so the normalizer is created from the first decoded buffer.
    let mut normalizer: Option<StreamingNormalizer> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(SymphoniaError::ResetRequired) => {
                return Err(anyhow!("Audio stream reset is not supported for this file"));
            }
            Err(error) => return Err(error).context("failed to read audio packet"),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = decoder
            .decode(&packet)
            .with_context(|| format!("failed to decode {}", path.display()))?;

        let spec = *decoded.spec();
        let duration = decoded.capacity() as u64;
        let mut sample_buffer = SampleBuffer::<f32>::new(duration, spec);
        sample_buffer.copy_interleaved_ref(decoded);
        if normalizer.is_none() {
            normalizer = Some(StreamingNormalizer::new(
                spec.rate,
                spec.channels.count() as u16,
            )?);
        }
        if let Some(normalizer) = normalizer.as_mut() {
            normalizer.push(sample_buffer.samples())?;
        }
    }

    normalizer
        .ok_or_else(|| anyhow!("The audio track contains no decodable audio"))?
        .finish()
}

/// Video containers list picture/subtitle tracks too (often first), so pick the
/// first track that carries a codec Symphonia can actually decode.
fn select_audio_track(
    tracks: &[symphonia::core::formats::Track],
) -> Option<&symphonia::core::formats::Track> {
    let codecs = get_codecs();
    tracks.iter().find(|track| {
        track.codec_params.codec != CODEC_TYPE_NULL
            && track.codec_params.sample_rate.is_some()
            && codecs.get_codec(track.codec_params.codec).is_some()
    })
}

fn decode_audio_file_with_fallback(path: &Path) -> Result<PreparedAudio> {
    if is_video_path(path) {
        return decode_video_audio_with_fallback(path);
    }

    if let Ok(prepared) = decode_audio_file_with_afconvert(path) {
        return Ok(prepared);
    }

    decode_audio_file_with_ffmpeg(path)
}

fn decode_video_audio_with_fallback(path: &Path) -> Result<PreparedAudio> {
    let avfoundation_error = if is_natively_playable_path(path) {
        match decode_video_audio_with_avconvert(path) {
            Ok(prepared) => return Ok(prepared),
            Err(error) => Some(error),
        }
    } else {
        None
    };

    if find_command_binary("ffmpeg").is_none() {
        if let Some(error) = avfoundation_error {
            return Err(error);
        }
        let extension = lowercase_extension(path)
            .unwrap_or_default()
            .to_ascii_uppercase();
        return Err(anyhow!(
            "{extension} videos need ffmpeg to extract the audio. Install it with `brew install ffmpeg`, or convert the video to MP4/MOV."
        ));
    }
    decode_audio_file_with_ffmpeg(path)
}

/// Uses macOS AVFoundation (`/usr/bin/avconvert`) to export only the audio
/// track of an MP4/MOV/M4V into a temporary M4A, then decodes that natively.
fn decode_video_audio_with_avconvert(path: &Path) -> Result<PreparedAudio> {
    let avconvert = find_command_binary("avconvert")
        .ok_or_else(|| anyhow!("macOS avconvert is unavailable"))?;
    let temp_m4a = transcoded_temp_path(path, "avconvert", "m4a");
    let output = Command::new(&avconvert)
        .arg("--source")
        .arg(path)
        .arg("--preset")
        .arg("PresetAppleM4A")
        .arg("--output")
        .arg(&temp_m4a)
        .arg("--replace")
        .output()
        .with_context(|| format!("failed to launch avconvert for {}", path.display()))?;

    if !output.status.success() || !temp_m4a.is_file() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        cleanup_temp_file(&temp_m4a);
        return Err(anyhow!(
            "Could not extract audio from {} (the video may have no audio track): {} {}",
            path.display(),
            stderr,
            stdout
        ));
    }

    let decoded = decode_audio_file_native(&temp_m4a)
        .or_else(|_| decode_audio_file_with_afconvert(&temp_m4a));
    cleanup_temp_file(&temp_m4a);
    decoded
}

fn decode_audio_file_with_afconvert(path: &Path) -> Result<PreparedAudio> {
    let temp_wav = transcoded_temp_wav_path(path, "afconvert");
    let output = Command::new("afconvert")
        .arg(path)
        .arg("-f")
        .arg("WAVE")
        .arg("-d")
        .arg("LEI16@16000")
        .arg("-c")
        .arg("1")
        .arg(&temp_wav)
        .output()
        .with_context(|| format!("failed to launch afconvert for {}", path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        cleanup_temp_file(&temp_wav);
        return Err(anyhow!(
            "afconvert failed for {}: {}{}",
            path.display(),
            stderr,
            if stdout.is_empty() {
                "".to_string()
            } else {
                format!(" {}", stdout)
            }
        ));
    }

    let decoded = decode_wav_file(&temp_wav);
    cleanup_temp_file(&temp_wav);
    decoded
}

fn decode_audio_file_with_ffmpeg(path: &Path) -> Result<PreparedAudio> {
    let temp_wav = transcoded_temp_wav_path(path, "ffmpeg");
    let ffmpeg_binary = find_command_binary("ffmpeg").ok_or_else(|| {
        anyhow!("No fallback audio decoder was found. Install ffmpeg or use WAV/MP3 input.")
    })?;
    let output = Command::new(&ffmpeg_binary)
        .arg("-nostdin")
        .arg("-y")
        .arg("-i")
        .arg(path)
        .arg("-map")
        .arg("0:a:0")
        .arg("-vn")
        .arg("-sn")
        .arg("-dn")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-f")
        .arg("wav")
        .arg(&temp_wav)
        .output()
        .with_context(|| {
            format!(
                "failed to launch {} for {}",
                ffmpeg_binary.display(),
                path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        cleanup_temp_file(&temp_wav);
        if stderr.contains("matches no streams") {
            return Err(anyhow!(
                "{} has no audio track to transcribe.",
                path.display()
            ));
        }
        return Err(anyhow!("ffmpeg failed for {}: {}", path.display(), stderr));
    }

    let decoded = decode_wav_file(&temp_wav);
    cleanup_temp_file(&temp_wav);
    decoded
}

fn transcoded_temp_wav_path(path: &Path, tool_name: &str) -> std::path::PathBuf {
    transcoded_temp_path(path, tool_name, "wav")
}

fn transcoded_temp_path(path: &Path, tool_name: &str, extension: &str) -> std::path::PathBuf {
    let file_stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("transcode");
    std::env::temp_dir().join(format!(
        "{file_stem}-{tool_name}-{}-{}.{extension}",
        std::process::id(),
        Uuid::new_v4()
    ))
}

fn cleanup_temp_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn find_command_binary(command: &str) -> Option<PathBuf> {
    let direct = PathBuf::from(command);
    if direct.is_absolute() && direct.is_file() {
        return Some(direct);
    }

    let mut candidates = Vec::new();
    if let Some(path_env) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&path_env) {
            candidates.push(entry.join(command));
        }
    }

    // Apps started from Finder do not inherit the shell's PATH.
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/opt/local/bin", "/usr/bin"] {
        candidates.push(PathBuf::from(dir).join(command));
    }

    candidates.into_iter().find(|candidate| candidate.is_file())
}

pub fn decode_wav_file(path: &Path) -> Result<PreparedAudio> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to read wav file {}", path.display()))?;
    let spec = reader.spec();
    let mut normalizer = StreamingNormalizer::new(spec.sample_rate, spec.channels)?;
    let mut chunk = Vec::with_capacity(8192);
    let mut push = |sample: f32| -> Result<()> {
        chunk.push(sample);
        if chunk.len() == 8192 {
            normalizer.push(&chunk)?;
            chunk.clear();
        }
        Ok(())
    };
    match spec.sample_format {
        SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                push(sample.context("failed to read float wav samples")?)?;
            }
        }
        SampleFormat::Int => {
            let scale = (1_i64 << spec.bits_per_sample.saturating_sub(1) as u32) as f32;
            for sample in reader.samples::<i32>() {
                push(sample.context("failed to read pcm wav samples")? as f32 / scale)?;
            }
        }
    }
    normalizer.push(&chunk)?;
    normalizer.finish()
}

pub fn normalize_audio(samples: &[f32], sample_rate_hz: u32, channels: u16) -> PreparedAudio {
    let mono = mix_to_mono(samples, channels);
    let resampled = if sample_rate_hz == TARGET_SAMPLE_RATE_HZ {
        mono
    } else {
        resample_linear(&mono, sample_rate_hz, TARGET_SAMPLE_RATE_HZ)
    };
    PreparedAudio {
        sample_rate_hz: TARGET_SAMPLE_RATE_HZ,
        channels: TARGET_CHANNELS,
        samples: resampled,
    }
}

pub fn write_wav(path: &Path, audio: &PreparedAudio) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let spec = WavSpec {
        channels: audio.channels,
        sample_rate: audio.sample_rate_hz,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut writer = WavWriter::create(path, spec)
        .with_context(|| format!("failed to create wav {}", path.display()))?;
    for sample in &audio.samples {
        let clamped = sample.clamp(-1.0, 1.0);
        writer.write_sample((clamped * i16::MAX as f32).round() as i16)?;
    }
    writer.finalize()?;
    Ok(())
}

fn mix_to_mono(samples: &[f32], channels: u16) -> Vec<f32> {
    if channels <= 1 {
        return samples.to_vec();
    }
    let channels = channels as usize;
    samples
        .chunks(channels)
        .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
        .collect()
}

fn resample_linear(samples: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if samples.is_empty() || from_rate == 0 || from_rate == to_rate {
        return samples.to_vec();
    }
    let ratio = to_rate as f64 / from_rate as f64;
    let target_len = ((samples.len() as f64) * ratio).round() as usize;
    if target_len <= 1 {
        return samples.to_vec();
    }

    let mut output = Vec::with_capacity(target_len);
    for index in 0..target_len {
        let source_position = index as f64 / ratio;
        let left_index = source_position.floor() as usize;
        let right_index = (left_index + 1).min(samples.len().saturating_sub(1));
        let fraction = (source_position - left_index as f64) as f32;
        let left = samples[left_index];
        let right = samples[right_index];
        output.push(left + (right - left) * fraction);
    }
    output
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn validate_audio_file_size(path: &Path) -> Result<()> {
    let metadata =
        std::fs::metadata(path).with_context(|| format!("failed to read {}", path.display()))?;
    let (limit, message_prefix, limit_label) = if is_video_path(path) {
        (MAX_VIDEO_FILE_BYTES, "Video", "video files up to 20 GB")
    } else {
        (MAX_AUDIO_FILE_BYTES, "Audio", "files up to 2.00 GB")
    };
    if metadata.len() > limit {
        return Err(anyhow!(
            "{message_prefix} file is too large: {:.2} GB. Murmela currently supports {limit_label}.",
            metadata.len() as f64 / 1_000_000_000.0
        ));
    }
    Ok(())
}

fn validate_audio_duration(duration_ms: i64) -> Result<()> {
    if duration_ms > MAX_AUDIO_DURATION_MS {
        return Err(anyhow!(
            "Audio file is too long: {:.1} hours. Murmela currently supports files up to 6 hours.",
            duration_ms as f64 / 3_600_000.0
        ));
    }
    Ok(())
}

fn prepared_duration_ms(prepared: &PreparedAudio) -> i64 {
    if prepared.samples.is_empty() {
        0
    } else {
        (((prepared.samples.len() as f64 / prepared.sample_rate_hz as f64) * 1000.0).round() as i64)
            .max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_extensions_are_classified() {
        assert!(is_video_path(Path::new("/tmp/Call.MOV")));
        assert!(is_supported_media_path(Path::new("/tmp/talk.webm")));
        assert!(!is_video_path(Path::new("/tmp/voice.m4a")));
        assert!(is_natively_playable_path(Path::new("/tmp/clip.mp4")));
        assert!(!is_natively_playable_path(Path::new("/tmp/talk.mkv")));
        assert!(is_natively_playable_path(Path::new("/tmp/voice.opus")));
    }

    #[test]
    #[ignore = "requires local ffmpeg to create video fixtures"]
    fn video_files_decode_only_their_audio_track() {
        let dir = temp_path("video-fixtures");
        std::fs::create_dir_all(&dir).unwrap();
        // Video track first, audio second: the decoder must skip the picture.
        for (name, video_codec, audio_codec) in [
            ("clip.mp4", "libx264", "aac"),
            ("clip.mov", "libx264", "aac"),
            ("clip.m4v", "libx264", "aac"),
            ("clip.mkv", "libx264", "aac"),
            ("clip.webm", "libvpx", "libopus"),
            ("clip.avi", "mpeg4", "libmp3lame"),
        ] {
            let path = dir.join(name);
            let output = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
                .arg("testsrc=size=320x240:rate=25:duration=3")
                .args([
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=3:sample_rate=48000",
                ])
                .args([
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:v",
                    video_codec,
                    "-c:a",
                    audio_codec,
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let audio =
                decode_audio_file(&path).unwrap_or_else(|error| panic!("{name}: {error:#}"));
            let seconds = audio.samples.len() as f64 / audio.sample_rate_hz as f64;
            assert!((seconds - 3.0).abs() < 0.15, "{name}: {seconds}s");
            assert!(
                audio.samples.iter().any(|sample| sample.abs() > 0.05),
                "{name} is silent"
            );
        }
        let silent = dir.join("no-audio.mkv");
        let output = Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "testsrc=duration=1",
                "-c:v",
                "libx264",
            ])
            .arg(&silent)
            .output()
            .unwrap();
        assert!(output.status.success());
        let error = decode_audio_file(&silent).unwrap_err();
        assert!(format!("{error:#}").contains("no audio track"), "{error:#}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("murmela-audio-test-{name}-{}", Uuid::new_v4()))
    }

    #[test]
    fn streaming_sha256_matches_known_digest() {
        let path = temp_path("sha256");
        std::fs::write(&path, b"abc").expect("write fixture");
        let digest = sha256_file(&path).expect("hash fixture");
        let _ = std::fs::remove_file(path);
        assert_eq!(
            digest,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn transcoded_temp_paths_are_unique_for_same_source() {
        let source = Path::new("/tmp/same-name.m4a");
        let first = transcoded_temp_wav_path(source, "ffmpeg");
        let second = transcoded_temp_wav_path(source, "ffmpeg");
        assert_ne!(first, second);
    }

    #[test]
    fn wav_decode_mixes_stereo_float_wav_to_mono() {
        let path = temp_path("stereo-float").with_extension("wav");
        let spec = WavSpec {
            channels: 2,
            sample_rate: TARGET_SAMPLE_RATE_HZ,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let mut writer = WavWriter::create(&path, spec).expect("create wav");
        for sample in [0.2_f32, 0.6_f32, -0.4_f32, 0.0_f32] {
            writer.write_sample(sample).expect("write sample");
        }
        writer.finalize().expect("finalize wav");

        let prepared = decode_wav_file(&path).expect("decode wav");
        let _ = std::fs::remove_file(path);

        assert_eq!(prepared.sample_rate_hz, TARGET_SAMPLE_RATE_HZ);
        assert_eq!(prepared.channels, TARGET_CHANNELS);
        assert_eq!(prepared.samples.len(), 2);
        assert!((prepared.samples[0] - 0.4).abs() < 0.0001);
        assert!((prepared.samples[1] - -0.2).abs() < 0.0001);
    }
}

/// Lossless normalized float WAV shared by inference workers. Playback uses a
/// separate PCM16 conversion only when the original codec is unsupported.
pub struct PreparedJobAudio {
    pub path: PathBuf,
    pub duration_ms: i64,
    pub sha256: String,
}
impl Drop for PreparedJobAudio {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
pub fn prepare_job_audio(source: &str, temp_dir: &Path) -> Result<PreparedJobAudio> {
    std::fs::create_dir_all(temp_dir)?;
    let before = std::fs::metadata(source)?;
    let prepared = decode_audio_file(Path::new(source))?;
    let duration_ms = prepared_duration_ms(&prepared);
    let sha256 = sha256_file(Path::new(source))?;
    let after = std::fs::metadata(source)?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(anyhow!("The source audio changed during preparation. Try again after the file finishes saving."));
    }
    let asset = PreparedJobAudio {
        path: temp_dir.join(format!("review-prepared-{}.wav", Uuid::new_v4())),
        duration_ms,
        sha256,
    };
    let mut writer = WavWriter::create(
        &asset.path,
        WavSpec {
            channels: prepared.channels,
            sample_rate: prepared.sample_rate_hz,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        },
    )?;
    for sample in &prepared.samples {
        writer.write_sample(*sample)?;
    }
    writer.finalize()?;
    Ok(asset)
}

// Normalize packet-by-packet: only the final 16 kHz mono buffer is retained.
// A carried frame and sample keep interpolation identical across packet edges.
pub(crate) struct StreamingNormalizer {
    rate: u32,
    channels: usize,
    ratio: f64,
    frames: usize,
    sum: f32,
    channel_count: usize,
    previous: f32,
    samples: Vec<f32>,
    tiny: Vec<f32>,
}
impl StreamingNormalizer {
    pub(crate) fn new(rate: u32, channels: u16) -> Result<Self> {
        if rate == 0 || channels == 0 {
            return Err(anyhow!("Invalid audio sample rate or channel count."));
        }
        Ok(Self {
            rate,
            channels: channels as usize,
            ratio: TARGET_SAMPLE_RATE_HZ as f64 / rate as f64,
            frames: 0,
            sum: 0.0,
            channel_count: 0,
            previous: 0.0,
            samples: Vec::new(),
            tiny: Vec::new(),
        })
    }
    pub(crate) fn push(&mut self, interleaved: &[f32]) -> Result<()> {
        for sample in interleaved {
            self.sum += *sample;
            self.channel_count += 1;
            if self.channel_count == self.channels {
                self.frame()?;
            }
        }
        Ok(())
    }
    fn frame(&mut self) -> Result<()> {
        let sample = self.sum / self.channel_count as f32;
        self.sum = 0.0;
        self.channel_count = 0;
        if (self.frames as u64) * 1000 / self.rate as u64 > MAX_AUDIO_DURATION_MS as u64 {
            return Err(anyhow!("Audio exceeds the six-hour duration limit."));
        }
        if (self.frames as f64 * self.ratio).round() <= 1.0 {
            self.tiny.push(sample);
        } else {
            self.tiny.clear();
        }
        let index = self.frames;
        loop {
            let position = self.samples.len() as f64 / self.ratio;
            if position > index as f64 {
                break;
            }
            let value = if position.floor() as usize == index || index == 0 {
                sample
            } else {
                let fraction = (position - position.floor()) as f32;
                self.previous + (sample - self.previous) * fraction
            };
            self.samples.push(value);
        }
        self.previous = sample;
        self.frames += 1;
        Ok(())
    }
    /// The final sample can still change when output length is rounded on
    /// finish. Publish only this immutable prefix during microphone capture.
    pub(crate) fn stable_samples(&self) -> &[f32] {
        if (self.frames as f64 * self.ratio).round() <= 1.0 {
            return &[];
        }
        &self.samples[..self.samples.len().saturating_sub(1)]
    }
    pub(crate) fn finish(mut self) -> Result<PreparedAudio> {
        if self.channel_count > 0 {
            self.frame()?;
        }
        let target = (self.frames as f64 * self.ratio).round() as usize;
        if target <= 1 {
            self.samples = self.tiny;
        } else {
            self.samples.resize(target, self.previous);
        }
        Ok(PreparedAudio {
            sample_rate_hz: TARGET_SAMPLE_RATE_HZ,
            channels: TARGET_CHANNELS,
            samples: self.samples,
        })
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    #[test]
    fn packet_normalization_matches_whole_buffer_for_rates_channels_and_boundaries() {
        for rate in [8000, 16000, 22050, 44100, 48000, 96000] {
            for channels in [1, 2, 6] {
                for frames in [0, 1, 2, 5, 101, 4097] {
                    let samples: Vec<f32> = (0..frames * channels as usize)
                        .map(|i| (i as f32 * 0.13).sin())
                        .collect();
                    let expected = normalize_audio(&samples, rate, channels);
                    for chunk in [1, 13, 256, 4096] {
                        let mut streaming = StreamingNormalizer::new(rate, channels).unwrap();
                        for part in samples.chunks(chunk) {
                            streaming.push(part).unwrap();
                            let stable = streaming.stable_samples();
                            assert!(stable.len() <= expected.samples.len());
                            assert!(stable
                                .iter()
                                .zip(&expected.samples)
                                .all(|(a, b)| (a - b).abs() < 1e-6));
                        }
                        let actual = streaming.finish().unwrap();
                        assert_eq!(
                            actual.samples.len(),
                            expected.samples.len(),
                            "rate={rate} channels={channels} frames={frames}"
                        );
                        for (a, b) in actual.samples.iter().zip(&expected.samples) {
                            assert!(
                                (a - b).abs() < 1e-6,
                                "{a} != {b} rate={rate} channels={channels} frames={frames}"
                            );
                        }
                    }
                }
            }
        }
    }
    #[test]
    fn prepared_float_audio_roundtrips_losslessly_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("review-prepare-test-{}", Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        let source = dir.join("source.wav");
        write_wav(
            &source,
            &PreparedAudio {
                sample_rate_hz: 16000,
                channels: 1,
                samples: (0..2000).map(|i| (i as f32 / 10.0).sin()).collect(),
            },
        )
        .unwrap();
        let expected = decode_audio_file(&source).unwrap();
        let prepared = prepare_job_audio(source.to_str().unwrap(), &dir).unwrap();
        let target = prepared.path.clone();
        assert_eq!(
            decode_audio_file(&target).unwrap().samples,
            expected.samples
        );
        assert_eq!(prepared.sha256, sha256_file(&source).unwrap());
        drop(prepared);
        assert!(!target.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
