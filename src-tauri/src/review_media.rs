//! Token-scoped loopback media with streamed HTTP range responses. Audio never
//! crosses IPC as a base64 blob and the endpoint cannot address arbitrary paths.
use crate::audio_files::SelectedSourceFile;
use crate::review::{ReviewRef, ReviewStore};
use anyhow::{anyhow, bail, Result};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Asset {
    path: PathBuf,
    mime: String,
    temporary: bool,
}
impl Drop for Asset {
    fn drop(&mut self) {
        if self.temporary {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
#[derive(Clone)]
pub struct MediaStore {
    assets: Arc<Mutex<HashMap<String, Arc<Asset>>>>,
    port: Option<u16>,
    temp_dir: PathBuf,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewAudio {
    pub url: String,
    pub token: String,
    pub duration_ms: Option<i64>,
}
impl MediaStore {
    pub fn new(temp_dir: PathBuf) -> Self {
        let assets = Arc::new(Mutex::new(HashMap::new()));
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0));
        let port = listener
            .as_ref()
            .ok()
            .and_then(|l| l.local_addr().ok())
            .map(|a| a.port());
        if let Ok(listener) = listener {
            let assets = Arc::downgrade(&assets);
            let active = Arc::new(AtomicUsize::new(0));
            std::thread::spawn(move || {
                for socket in listener.incoming() {
                    let Some(assets) = assets.upgrade() else {
                        break;
                    };
                    let Ok(socket) = socket else {
                        continue;
                    };
                    if active.fetch_add(1, Ordering::SeqCst) >= 8 {
                        active.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let active = active.clone();
                    std::thread::spawn(move || {
                        let _ = serve(socket, &assets);
                        active.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            });
        }
        Self {
            assets,
            port,
            temp_dir,
        }
    }
    pub fn resolve(
        &self,
        store: &ReviewStore,
        reference: &ReviewRef,
        replacement_path: Option<String>,
        fallback: bool,
    ) -> Result<ReviewAudio> {
        let port = self
            .port
            .ok_or_else(|| anyhow!("AUDIO_UNAVAILABLE: Local audio playback could not start."))?;
        let source = validated_source(store, reference, replacement_path)?;
        // MKV/WebM/AVI cannot be played by the web view; serve extracted audio instead.
        let fallback = fallback
            || !crate::audio_preprocess::is_natively_playable_path(Path::new(&source.file_path));
        let asset = if fallback {
            let prepared =
                crate::audio_preprocess::decode_audio_file(Path::new(&source.file_path))?;
            let asset = Arc::new(Asset {
                path: self
                    .temp_dir
                    .join(format!("review-playback-{}.wav", uuid::Uuid::new_v4())),
                mime: "audio/wav".into(),
                temporary: true,
            });
            crate::audio_preprocess::write_wav(&asset.path, &prepared)?;
            asset
        } else {
            Arc::new(Asset {
                path: PathBuf::from(&source.file_path),
                mime: source.mime_type.clone(),
                temporary: false,
            })
        };
        let token = uuid::Uuid::new_v4().to_string();
        self.assets
            .lock()
            .map_err(|_| anyhow!("Audio playback unavailable"))?
            .insert(token.clone(), asset);
        Ok(ReviewAudio {
            url: format!("http://127.0.0.1:{port}/audio/{token}"),
            token,
            duration_ms: source.duration_ms,
        })
    }
    pub fn release(&self, token: &str) {
        if let Ok(mut assets) = self.assets.lock() {
            assets.remove(token);
        }
    }
    pub fn clear(&self) {
        if let Ok(mut assets) = self.assets.lock() {
            assets.clear();
        }
    }
}

pub fn validated_source(
    store: &ReviewStore,
    reference: &ReviewRef,
    replacement_path: Option<String>,
) -> Result<SelectedSourceFile> {
    let mut stored = store.source(reference)?;
    let path = replacement_path.as_deref().unwrap_or(&stored.file_path);
    if !Path::new(path).is_file() {
        bail!("SOURCE_FILE_REQUIRED: Locate the original audio file to listen or identify speakers again.");
    }
    let expected = stored.sha256.as_ref().ok_or_else(|| anyhow!("SOURCE_UNVERIFIABLE: This older transcript has no audio fingerprint. Reading and speaker corrections are available, but audio cannot be safely relinked."))?;
    if crate::audio_preprocess::sha256_file(Path::new(path))? != *expected {
        bail!("SOURCE_FILE_MISMATCH: This audio does not match the original recording. Choose the original file.");
    }
    if let Some(path) = replacement_path {
        stored.file_path = std::fs::canonicalize(path)?.to_string_lossy().into_owned();
        store.relink(reference, stored.clone())?;
    }
    Ok(stored)
}

fn byte_range(header: Option<&str>, length: u64) -> Result<Option<(u64, u64)>> {
    let Some(value) = header else {
        return Ok(None);
    };
    let value = value
        .strip_prefix("bytes=")
        .ok_or_else(|| anyhow!("Invalid range"))?;
    if value.contains(',') || length == 0 {
        bail!("Invalid range");
    }
    let (start, end) = value
        .split_once('-')
        .ok_or_else(|| anyhow!("Invalid range"))?;
    let range = if start.is_empty() {
        let suffix = end.parse::<u64>()?;
        if suffix == 0 {
            bail!("Invalid range");
        }
        (length.saturating_sub(suffix), length - 1)
    } else {
        let start = start.parse::<u64>()?;
        let end = if end.is_empty() {
            length - 1
        } else {
            end.parse::<u64>()?.min(length - 1)
        };
        (start, end)
    };
    if range.0 >= length || range.1 < range.0 {
        bail!("Invalid range");
    }
    Ok(Some(range))
}

fn serve(mut socket: TcpStream, assets: &Mutex<HashMap<String, Arc<Asset>>>) -> Result<()> {
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(socket.try_clone()?);
    let mut request = String::new();
    reader.by_ref().take(8192).read_line(&mut request)?;
    let mut parts = request.split_whitespace();
    let method = parts.next().unwrap_or("");
    let uri = parts.next().unwrap_or("");
    if !matches!(method, "GET" | "HEAD") {
        write!(
            socket,
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    let mut range = None;
    let mut origin = None;
    let mut bytes = request.len();
    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(8192).read_line(&mut line)?;
        bytes += read;
        if bytes > 16384 {
            return Ok(());
        }
        if read == 0 || line == "\r\n" {
            break;
        }
        if let Some((key, value)) = line.trim().split_once(':') {
            if key.eq_ignore_ascii_case("range") {
                range = Some(value.trim().to_owned());
            }
            if key.eq_ignore_ascii_case("origin") {
                origin = Some(value.trim().to_owned());
            }
        }
    }
    let allowed = [
        "tauri://localhost",
        "http://tauri.localhost",
        "https://tauri.localhost",
        "http://localhost:1420",
        "http://127.0.0.1:1420",
    ];
    if origin
        .as_ref()
        .is_some_and(|value| !allowed.contains(&value.as_str()))
    {
        write!(
            socket,
            "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    }
    let token = uri.strip_prefix("/audio/").unwrap_or("");
    let asset = assets
        .lock()
        .map_err(|_| anyhow!("Audio state unavailable"))?
        .get(token)
        .cloned();
    let Some(asset) = asset else {
        write!(
            socket,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )?;
        return Ok(());
    };
    let mut file = std::fs::File::open(&asset.path)?;
    let length = file.metadata()?.len();
    let range = match byte_range(range.as_deref(), length) {
        Ok(range) => range,
        Err(_) => {
            write!(socket,"HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{length}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            return Ok(());
        }
    };
    let (start, end) = range.unwrap_or((0, length.saturating_sub(1)));
    let count = if length == 0 { 0 } else { end - start + 1 };
    let status = if range.is_some() {
        "206 Partial Content"
    } else {
        "200 OK"
    };
    write!(socket,"HTTP/1.1 {status}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\nContent-Length: {count}\r\nCache-Control: no-store\r\nConnection: close\r\n",asset.mime)?;
    if let Some(origin) = origin {
        write!(
            socket,
            "Access-Control-Allow-Origin: {origin}\r\nVary: Origin\r\n"
        )?;
    }
    if range.is_some() {
        write!(socket, "Content-Range: bytes {start}-{end}/{length}\r\n")?;
    }
    write!(socket, "\r\n")?;
    if method != "HEAD" {
        file.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut file.take(count), &mut socket)?;
    }
    Ok(())
}

pub fn cleanup_stale_audio(temp_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(temp_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("review-prepared-") || name.starts_with("review-playback-") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires local ffmpeg for WAV/MP3/M4A/OPUS fixtures"]
    fn format_playback_assets_decode_and_stream_with_fallback_cleanup() {
        let root = std::env::temp_dir().join(format!("review-codecs-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let store = ReviewStore::new(root.join("unused.sqlite"));
        let media = MediaStore::new(root.clone());
        for (extension, codec) in [
            ("wav", "pcm_s16le"),
            ("mp3", "libmp3lame"),
            ("m4a", "aac"),
            ("opus", "libopus"),
        ] {
            let path = root.join(format!("tone.{extension}"));
            let output = std::process::Command::new("ffmpeg")
                .args([
                    "-v",
                    "error",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:duration=2",
                    "-ar",
                    "48000",
                    "-ac",
                    "2",
                    "-c:a",
                    codec,
                ])
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            let prepared =
                crate::audio_preprocess::prepare_job_audio(&path.to_string_lossy(), &root).unwrap();
            assert!(
                (prepared.duration_ms - 2000).abs() < 150,
                "{extension}: {}",
                prepared.duration_ms
            );
            let mut source = crate::audio_files::selected_source_file_from_path(path).unwrap();
            source.sha256 = Some(prepared.sha256.clone());
            source.duration_ms = Some(prepared.duration_ms);
            let reference = store
                .create_session(extension, source, crate::review::fixture_result())
                .unwrap();
            for fallback in [false, true] {
                let asset = media.resolve(&store, &reference, None, fallback).unwrap();
                let resolved = media
                    .assets
                    .lock()
                    .unwrap()
                    .get(&asset.token)
                    .unwrap()
                    .clone();
                assert!(resolved.path.is_file());
                assert_eq!(resolved.temporary, fallback);
                let asset_path = resolved.path.clone();
                drop(resolved);
                media.release(&asset.token);
                assert_eq!(asset_path.exists(), !fallback);
            }
            println!("codec {extension}: decoded, fingerprinted, original and WAV fallback resolved, temporary assets released");
        }
        media.clear();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn accepts_valid_full_open_and_suffix_ranges_and_rejects_invalid_ranges() {
        assert_eq!(byte_range(None, 100).unwrap(), None);
        for (header, expected) in [
            ("bytes=0-9", (0, 9)),
            ("bytes=90-", (90, 99)),
            ("bytes=-10", (90, 99)),
            ("bytes=0-999", (0, 99)),
        ] {
            assert_eq!(byte_range(Some(header), 100).unwrap(), Some(expected));
        }
        for header in [
            "bytes=100-",
            "bytes=3-2",
            "bytes=-0",
            "bytes=0-1,3-4",
            "other=0-10",
            "bytes=x-y",
        ] {
            assert!(byte_range(Some(header), 100).is_err(), "{header}");
        }
        assert!(byte_range(Some("bytes=0-"), 0).is_err());
    }
    #[test]
    fn tokens_scope_audio_and_http_streams_correct_partial_bytes() {
        let path = std::env::temp_dir().join(format!("review-http-{}.wav", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"0123456789").unwrap();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let assets = Arc::new(Mutex::new(HashMap::from([(
            "token".into(),
            Arc::new(Asset {
                path: path.clone(),
                mime: "audio/wav".into(),
                temporary: true,
            }),
        )])));
        let server = std::thread::spawn(move || {
            for _ in 0..5 {
                let (socket, _) = listener.accept().unwrap();
                serve(socket, &assets).unwrap();
            }
        });
        let request = |path: &str, headers: &str, method: &str| {
            let mut socket = TcpStream::connect(address).unwrap();
            write!(
                socket,
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\n{headers}\r\n"
            )
            .unwrap();
            let mut output = String::new();
            socket.read_to_string(&mut output).unwrap();
            output
        };
        let partial = request("/audio/token", "Range: bytes=2-5\r\n", "GET");
        assert!(partial.starts_with("HTTP/1.1 206"));
        assert!(partial.ends_with("2345"));
        assert!(partial.contains("Content-Range: bytes 2-5/10"));
        let head = request("/audio/token", "", "HEAD");
        assert!(head.contains("Content-Length: 10"));
        assert!(head.ends_with("\r\n\r\n"));
        assert!(request("/audio/token", "Range: bytes=100-\r\n", "GET").starts_with("HTTP/1.1 416"));
        assert!(request("/audio/../../secret", "", "GET").starts_with("HTTP/1.1 404"));
        assert!(request(
            "/audio/token",
            "Origin: https://unrelated.example\r\n",
            "GET"
        )
        .starts_with("HTTP/1.1 403"));
        server.join().unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn verifies_relinked_source_and_preserves_old_path_on_mismatch() {
        let root = std::env::temp_dir().join(format!("review-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("original.wav");
        let other = root.join("wrong.wav");
        std::fs::write(&path, b"original audio").unwrap();
        std::fs::write(&other, b"different audio").unwrap();
        let store = ReviewStore::new(root.join("unused"));
        let reference = store
            .create_session(
                "job",
                SelectedSourceFile {
                    file_path: root.join("moved.wav").to_string_lossy().into(),
                    original_name: "original.wav".into(),
                    mime_type: "audio/wav".into(),
                    size_bytes: 14,
                    duration_ms: Some(1000),
                    sha256: Some(crate::audio_preprocess::sha256_file(&path).unwrap()),
                },
                crate::review::fixture_result(),
            )
            .unwrap();
        assert!(validated_source(&store, &reference, None)
            .unwrap_err()
            .to_string()
            .starts_with("SOURCE_FILE_REQUIRED"));
        assert!(
            validated_source(&store, &reference, Some(other.to_string_lossy().into()))
                .unwrap_err()
                .to_string()
                .starts_with("SOURCE_FILE_MISMATCH")
        );
        assert!(store
            .source(&reference)
            .unwrap()
            .file_path
            .ends_with("moved.wav"));
        assert!(validated_source(&store, &reference, Some(path.to_string_lossy().into())).is_ok());
        assert!(validated_source(&store, &reference, None).is_ok());
        let mut legacy = store.source(&reference).unwrap();
        legacy.sha256 = None;
        let legacy_ref = store
            .create_session("legacy", legacy, crate::review::fixture_result())
            .unwrap();
        assert!(validated_source(&store, &legacy_ref, None)
            .unwrap_err()
            .to_string()
            .starts_with("SOURCE_UNVERIFIABLE"));
        assert_eq!(
            store.get(&legacy_ref).unwrap().detail.summary.plain_text,
            "First. Second."
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
