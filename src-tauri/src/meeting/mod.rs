//! Recording calls and meetings: the microphone and the call app's sound on one
//! timeline, transcribed locally with speakers.
//!
//! A recording lives in `<app data>/meetings/<id>/`:
//! - `microphone.wav` and `call-audio.wav`: the two channels, 16 kHz mono;
//! - `call.wav`: both mixed, for playback in Review;
//! - `call.json`: the sidecar with the title and what the browser helper reported.
//!
//! When stopped, the recording becomes an ordinary file transcription job for
//! `call.wav`. The job finds the sidecar next to it and transcribes the two
//! channels separately (see `file_jobs`), so a retry from the Files queue still
//! knows it is a call. Recordings made "in the room" have only the microphone
//! and are transcribed like any file, with speaker identification when enabled.

pub mod bridge;
pub mod macos_tap;
pub mod recorder;
pub mod sources;
pub mod speakers;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use recorder::{ChannelRecorder, ChannelSummary};
use speakers::{SpeakerEvent, SpeakerEventKind};

pub const LOCAL_FILE: &str = "microphone.wav";
pub const REMOTE_FILE: &str = "call-audio.wav";
pub const MIX_FILE: &str = "call.wav";
pub const SIDECAR_FILE: &str = "call.json";
/// The longest audio Murmela transcribes.
pub const MAX_CALL_MS: i64 = crate::audio_preprocess::MAX_AUDIO_DURATION_MS;
const MAX_EVENTS: usize = 200_000;
const MAX_EVENTS_PER_REPORT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallMode {
    /// A call on this computer: the microphone and the call app's sound.
    Call,
    /// Everyone in the same room: the microphone only.
    Room,
}

/// What `call.json` holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallSidecar {
    pub version: u32,
    pub title: String,
    pub mode: CallMode,
    pub started_at: String,
    pub duration_ms: i64,
    /// False when the call app's channel held only silence (e.g. permission refused).
    pub remote_heard: bool,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub self_name: Option<String>,
    #[serde(default)]
    pub events: Vec<SpeakerEvent>,
}

impl CallSidecar {
    pub fn local_path(&self, dir: &Path) -> PathBuf {
        dir.join(LOCAL_FILE)
    }

    /// The call app's channel, when this was a call and it carried sound.
    pub fn remote_path(&self, dir: &Path) -> Option<PathBuf> {
        (self.mode == CallMode::Call && self.remote_heard).then(|| dir.join(REMOTE_FILE))
    }
}

/// The sidecar of a call recording, if `source` is a call's `call.wav` inside `meetings_dir`.
pub fn sidecar_for(meetings_dir: &Path, source: &Path) -> Option<(PathBuf, CallSidecar)> {
    let dir = source.parent()?;
    let root = meetings_dir.canonicalize().ok()?;
    if dir.canonicalize().ok()?.parent()? != root || source.file_name()? != MIX_FILE {
        return None;
    }
    let text = std::fs::read_to_string(dir.join(SIDECAR_FILE)).ok()?;
    let sidecar: CallSidecar = serde_json::from_str(&text).ok()?;
    Some((dir.to_path_buf(), sidecar))
}

/// True for a recording Murmela made of a call or meeting (its `call.wav` with a sidecar).
pub fn is_recording_path(source: &Path) -> bool {
    source.file_name().is_some_and(|name| name == MIX_FILE)
        && source
            .parent()
            .is_some_and(|dir| dir.join(SIDECAR_FILE).is_file())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelStatus {
    pub label: String,
    pub level: f32,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserStatus {
    pub platform: String,
    pub meeting_title: Option<String>,
    pub participants: Vec<String>,
    pub speaking: Vec<String>,
    pub ms_since_last_report: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallStatus {
    pub recording: bool,
    pub call_id: Option<String>,
    pub mode: Option<CallMode>,
    pub title: Option<String>,
    pub elapsed_ms: i64,
    pub limit_ms: i64,
    pub microphone: Option<ChannelStatus>,
    pub system: Option<ChannelStatus>,
    pub browser: Option<BrowserStatus>,
    /// Port of the local endpoint the browser helper reports to, while recording.
    pub bridge_port: Option<u16>,
}

/// What the browser helper sends while a call runs.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserReport {
    pub platform: String,
    #[serde(default)]
    pub meeting_title: Option<String>,
    #[serde(default)]
    pub self_name: Option<String>,
    #[serde(default)]
    pub participants: Vec<String>,
    #[serde(default)]
    pub events: Vec<BrowserEvent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserEvent {
    /// Milliseconds since the Unix epoch, from the browser's clock.
    pub t: i64,
    pub name: String,
    pub kind: SpeakerEventKind,
    #[serde(default)]
    pub is_self: bool,
}

struct BrowserState {
    platform: String,
    meeting_title: Option<String>,
    self_name: Option<String>,
    participants: Vec<String>,
    speaking: Vec<String>,
    last_report: Instant,
}

struct ActiveCall {
    id: String,
    dir: PathBuf,
    mode: CallMode,
    title: String,
    started: Instant,
    started_epoch_ms: i64,
    started_at: String,
    microphone: ChannelRecorder,
    system: Option<ChannelRecorder>,
    events: Vec<SpeakerEvent>,
    browser: Option<BrowserState>,
    bridge: Option<bridge::Bridge>,
}

/// A finished recording, ready to be transcribed.
#[derive(Debug, Clone)]
pub struct FinishedCall {
    pub source_file: crate::audio_files::SelectedSourceFile,
}

#[derive(Clone)]
pub struct MeetingController {
    meetings_dir: PathBuf,
    active: Arc<Mutex<Option<ActiveCall>>>,
}

impl MeetingController {
    pub fn new(meetings_dir: PathBuf) -> Self {
        Self {
            meetings_dir,
            active: Arc::new(Mutex::new(None)),
        }
    }

    pub fn meetings_dir(&self) -> &Path {
        &self.meetings_dir
    }

    pub fn start(
        &self,
        mode: CallMode,
        title: Option<String>,
        preferred_input: Option<String>,
    ) -> Result<CallStatus> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| anyhow!("Call state unavailable"))?;
        if active.is_some() {
            return Err(anyhow!(
                "CALL_ALREADY_RECORDING: A call is already being recorded."
            ));
        }
        let id = Uuid::new_v4().to_string();
        let dir = self.meetings_dir.join(&id);
        std::fs::create_dir_all(&dir)?;
        let started = Instant::now();
        let now = chrono::Local::now();
        let title = title
            .map(|title| speakers::clean_name(&title))
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| {
                let kind = if mode == CallMode::Call {
                    "Call"
                } else {
                    "Meeting"
                };
                format!("{kind} {}", now.format("%Y-%m-%d %H.%M"))
            });
        let microphone = match ChannelRecorder::start(
            Box::new(sources::MicrophoneSource::new(preferred_input)),
            &dir.join(LOCAL_FILE),
            started,
        ) {
            Ok(recorder) => recorder,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                return Err(error.context("The microphone could not start"));
            }
        };
        let system = if mode == CallMode::Call {
            match ChannelRecorder::start(
                Box::new(sources::SystemSource),
                &dir.join(REMOTE_FILE),
                started,
            ) {
                Ok(recorder) => Some(recorder),
                Err(error) => {
                    let _ = microphone.finish(Instant::now());
                    let _ = std::fs::remove_dir_all(&dir);
                    return Err(error);
                }
            }
        } else {
            None
        };
        let bridge = if mode == CallMode::Call {
            match bridge::Bridge::start(self.clone()) {
                Ok(bridge) => Some(bridge),
                Err(error) => {
                    // Names are a bonus; the recording works without the browser helper.
                    eprintln!("[call] browser helper endpoint unavailable: {error:#}");
                    None
                }
            }
        } else {
            None
        };
        *active = Some(ActiveCall {
            id,
            dir,
            mode,
            title,
            started,
            started_epoch_ms: chrono::Utc::now().timestamp_millis(),
            started_at: now.to_rfc3339(),
            microphone,
            system,
            events: Vec::new(),
            browser: None,
            bridge,
        });
        Ok(status_of(active.as_ref()))
    }

    pub fn status(&self) -> CallStatus {
        self.active
            .lock()
            .map(|active| status_of(active.as_ref()))
            .unwrap_or_else(|_| status_of(None))
    }

    pub fn is_recording(&self) -> bool {
        self.active
            .lock()
            .map(|active| active.is_some())
            .unwrap_or(false)
    }

    /// Stops recording and writes the files and sidecar for transcription.
    pub fn stop(&self) -> Result<FinishedCall> {
        let call = self
            .active
            .lock()
            .map_err(|_| anyhow!("Call state unavailable"))?
            .take()
            .ok_or_else(|| anyhow!("No call is being recorded."))?;
        let ended = Instant::now();
        drop(call.bridge);
        let local = call.microphone.finish(ended)?;
        let remote = match call.system {
            Some(system) => Some(system.finish(ended)?),
            None => None,
        };
        finish_files(
            &call.dir,
            &call.title,
            call.mode,
            &call.started_at,
            &local,
            remote.as_ref(),
            call.events,
            call.browser,
        )
    }

    /// Stops and deletes the recording.
    pub fn discard(&self) -> Result<()> {
        let Some(call) = self
            .active
            .lock()
            .map_err(|_| anyhow!("Call state unavailable"))?
            .take()
        else {
            return Ok(());
        };
        drop(call.bridge);
        let ended = Instant::now();
        let _ = call.microphone.finish(ended);
        if let Some(system) = call.system {
            let _ = system.finish(ended);
        }
        std::fs::remove_dir_all(&call.dir)
            .with_context(|| format!("could not delete {}", call.dir.display()))
    }

    /// Takes a report from the browser helper. Returns false when nothing is being recorded.
    pub fn receive_browser_report(&self, report: BrowserReport) -> bool {
        let Ok(mut guard) = self.active.lock() else {
            return false;
        };
        let Some(call) = guard.as_mut() else {
            return false;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        let platform: String = report
            .platform
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(20)
            .collect();
        let browser = call.browser.get_or_insert_with(|| BrowserState {
            platform: platform.clone(),
            meeting_title: None,
            self_name: None,
            participants: Vec::new(),
            speaking: Vec::new(),
            last_report: Instant::now(),
        });
        browser.platform = platform;
        browser.last_report = Instant::now();
        if let Some(title) = report
            .meeting_title
            .map(|title| speakers::clean_name(&title))
            .filter(|t| !t.is_empty())
        {
            browser.meeting_title = Some(title);
        }
        if let Some(name) = report
            .self_name
            .map(|name| speakers::clean_name(&name))
            .filter(|n| !n.is_empty())
        {
            browser.self_name = Some(name);
        }
        if !report.participants.is_empty() {
            browser.participants = report
                .participants
                .iter()
                .map(|name| speakers::clean_name(name))
                .filter(|name| !name.is_empty())
                .take(100)
                .collect();
        }
        for event in report.events.into_iter().take(MAX_EVENTS_PER_REPORT) {
            let name = speakers::clean_name(&event.name);
            // Only events from this recording's time, allowing for a few seconds of clock skew.
            if name.is_empty()
                || event.t < call.started_epoch_ms - 5_000
                || event.t > now_ms + 5_000
            {
                continue;
            }
            match event.kind {
                SpeakerEventKind::Speaking if !browser.speaking.contains(&name) => {
                    browser.speaking.push(name.clone())
                }
                SpeakerEventKind::Stopped => browser.speaking.retain(|speaking| speaking != &name),
                _ => {}
            }
            if call.events.len() < MAX_EVENTS {
                call.events.push(SpeakerEvent {
                    at_ms: (event.t - call.started_epoch_ms).max(0),
                    name,
                    kind: event.kind,
                    is_self: event.is_self,
                });
            }
        }
        true
    }
}

fn status_of(call: Option<&ActiveCall>) -> CallStatus {
    let Some(call) = call else {
        return CallStatus {
            recording: false,
            call_id: None,
            mode: None,
            title: None,
            elapsed_ms: 0,
            limit_ms: MAX_CALL_MS,
            microphone: None,
            system: None,
            browser: None,
            bridge_port: None,
        };
    };
    let channel = |recorder: &ChannelRecorder| ChannelStatus {
        label: recorder.label(),
        level: recorder.level(),
        error: recorder.error(),
    };
    CallStatus {
        recording: true,
        call_id: Some(call.id.clone()),
        mode: Some(call.mode),
        title: Some(call.title.clone()),
        elapsed_ms: call.started.elapsed().as_millis() as i64,
        limit_ms: MAX_CALL_MS,
        microphone: Some(channel(&call.microphone)),
        system: call.system.as_ref().map(channel),
        browser: call.browser.as_ref().map(|browser| BrowserStatus {
            platform: browser.platform.clone(),
            meeting_title: browser.meeting_title.clone(),
            participants: browser.participants.clone(),
            speaking: browser.speaking.clone(),
            ms_since_last_report: browser.last_report.elapsed().as_millis() as i64,
        }),
        bridge_port: call.bridge.as_ref().map(|bridge| bridge.port()),
    }
}

#[allow(clippy::too_many_arguments)]
fn finish_files(
    dir: &Path,
    title: &str,
    mode: CallMode,
    started_at: &str,
    local: &ChannelSummary,
    remote: Option<&ChannelSummary>,
    events: Vec<SpeakerEvent>,
    browser: Option<BrowserState>,
) -> Result<FinishedCall> {
    let mix = dir.join(MIX_FILE);
    let duration_ms = match remote {
        Some(remote) => recorder::mix_channels(&local.path, &remote.path, &mix)?,
        None => {
            std::fs::copy(&local.path, &mix)?;
            local.duration_ms
        }
    };
    if local.is_silent() && remote.is_none_or(|remote| remote.is_silent()) {
        return Err(anyhow!(
            "CALL_SILENT: Nothing was heard during the recording. Check the microphone and that the call's sound plays on this computer. The recording was kept in {}.",
            dir.display()
        ));
    }
    let sidecar = CallSidecar {
        version: 1,
        title: title.to_string(),
        mode,
        started_at: started_at.to_string(),
        duration_ms,
        remote_heard: remote.is_some_and(|remote| !remote.is_silent()),
        platform: browser.as_ref().map(|browser| browser.platform.clone()),
        self_name: speakers::self_name(&events)
            .or_else(|| browser.and_then(|browser| browser.self_name)),
        events,
    };
    std::fs::write(dir.join(SIDECAR_FILE), serde_json::to_vec_pretty(&sidecar)?)?;
    let mut source_file = crate::audio_files::selected_source_file_from_path(mix)?;
    source_file.original_name = format!("{title}.wav");
    source_file.duration_ms = Some(duration_ms);
    Ok(FinishedCall { source_file })
}

/// A recording that has not been transcribed yet: a call whose transcription failed,
/// or one that was still running when Murmela quit or stopped unexpectedly.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRecording {
    pub id: String,
    pub title: String,
    pub started_at: String,
    pub duration_ms: i64,
    pub mode: CallMode,
    /// Recovered from a recording that never stopped properly.
    pub interrupted: bool,
}

/// Recordings whose channels are still on disk, newest first. Interrupted ones are repaired first.
pub fn pending_recordings(meetings_dir: &Path) -> Vec<PendingRecording> {
    let Ok(entries) = std::fs::read_dir(meetings_dir) else {
        return Vec::new();
    };
    let mut pending: Vec<PendingRecording> = entries
        .flatten()
        .filter(|entry| entry.path().join(LOCAL_FILE).is_file())
        .filter_map(|entry| {
            let dir = entry.path();
            let id = dir.file_name()?.to_str()?.to_string();
            Uuid::parse_str(&id).ok()?;
            let interrupted = !dir.join(SIDECAR_FILE).is_file();
            if interrupted {
                recover_interrupted(&dir).ok()?;
            }
            let sidecar: CallSidecar =
                serde_json::from_slice(&std::fs::read(dir.join(SIDECAR_FILE)).ok()?).ok()?;
            Some(PendingRecording {
                id,
                title: sidecar.title,
                started_at: sidecar.started_at,
                duration_ms: sidecar.duration_ms,
                mode: sidecar.mode,
                interrupted,
            })
        })
        .collect();
    pending.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    pending
}

/// The directory of a pending recording, refusing anything but a recording id.
pub fn recording_dir(meetings_dir: &Path, id: &str) -> Result<PathBuf> {
    Uuid::parse_str(id).map_err(|_| anyhow!("Unknown recording."))?;
    let dir = meetings_dir.join(id);
    if !dir.join(SIDECAR_FILE).is_file() {
        return Err(anyhow!("This recording no longer exists."));
    }
    Ok(dir)
}

/// The file to transcribe for a pending recording.
pub fn pending_source(
    meetings_dir: &Path,
    id: &str,
) -> Result<crate::audio_files::SelectedSourceFile> {
    let dir = recording_dir(meetings_dir, id)?;
    let sidecar: CallSidecar = serde_json::from_slice(&std::fs::read(dir.join(SIDECAR_FILE))?)?;
    let mut source = crate::audio_files::selected_source_file_from_path(dir.join(MIX_FILE))?;
    source.original_name = format!("{}.wav", sidecar.title);
    source.duration_ms = Some(sidecar.duration_ms);
    Ok(source)
}

/// Rebuilds a recording that never stopped properly: repairs the WAV headers,
/// mixes the channels and writes a sidecar without browser names.
fn recover_interrupted(dir: &Path) -> Result<()> {
    let local = dir.join(LOCAL_FILE);
    let remote = dir.join(REMOTE_FILE);
    repair_wav_header(&local)?;
    let has_remote = remote.is_file() && repair_wav_header(&remote).is_ok();
    let mix = dir.join(MIX_FILE);
    let duration_ms = if has_remote {
        recorder::mix_channels(&local, &remote, &mix)?
    } else {
        std::fs::copy(&local, &mix)?;
        let reader = hound::WavReader::open(&local)?;
        reader.duration() as i64 * 1000 / recorder::RATE_HZ as i64
    };
    let peak = |path: &Path| -> f32 {
        hound::WavReader::open(path)
            .map(|mut reader| {
                reader
                    .samples::<i16>()
                    .filter_map(|sample| sample.ok())
                    .fold(0.0_f32, |peak, sample| {
                        peak.max((sample as f32 / i16::MAX as f32).abs())
                    })
            })
            .unwrap_or(0.0)
    };
    let modified: chrono::DateTime<chrono::Local> = std::fs::metadata(&local)?.modified()?.into();
    let sidecar = CallSidecar {
        version: 1,
        title: format!(
            "Interrupted recording {}",
            modified.format("%Y-%m-%d %H.%M")
        ),
        mode: if has_remote {
            CallMode::Call
        } else {
            CallMode::Room
        },
        started_at: (modified - chrono::Duration::milliseconds(duration_ms)).to_rfc3339(),
        duration_ms,
        remote_heard: has_remote && peak(&remote) >= 0.003,
        platform: None,
        self_name: None,
        events: Vec::new(),
    };
    std::fs::write(dir.join(SIDECAR_FILE), serde_json::to_vec_pretty(&sidecar)?)?;
    Ok(())
}

/// Sets the RIFF and data sizes of a WAV whose writer never finished, from the file's length.
fn repair_wav_header(path: &Path) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let length = file.metadata()?.len();
    let mut header = vec![0_u8; 4096.min(length as usize)];
    file.read_exact(&mut header)?;
    if header.len() < 12 || &header[0..4] != b"RIFF" || &header[8..12] != b"WAVE" {
        return Err(anyhow!("not a WAV file"));
    }
    let data_at = header
        .windows(4)
        .position(|window| window == b"data")
        .ok_or_else(|| anyhow!("WAV has no data chunk"))? as u64;
    let data_bytes = (length - data_at - 8) / 2 * 2;
    file.seek(SeekFrom::Start(4))?;
    file.write_all(&((data_at + data_bytes) as u32).to_le_bytes())?;
    file.seek(SeekFrom::Start(data_at + 4))?;
    file.write_all(&(data_bytes as u32).to_le_bytes())?;
    file.set_len(data_at + 8 + data_bytes)?;
    Ok(())
}

/// Deletes a recording and its files.
pub fn delete_recording(meetings_dir: &Path, id: &str) -> Result<()> {
    let dir = recording_dir(meetings_dir, id)?;
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

/// Deletes the channel files of a call once its transcript is saved; the mix stays for playback.
pub fn release_channels(dir: &Path) {
    let _ = std::fs::remove_file(dir.join(LOCAL_FILE));
    let _ = std::fs::remove_file(dir.join(REMOTE_FILE));
}

/// Removes recordings that no saved transcript uses, a day after they were made:
/// calls transcribed while history was off, and folders left by a failed start.
/// Recordings still waiting to be transcribed (their channels are on disk) are kept.
pub fn remove_unused_recordings(meetings_dir: &Path, db_path: &Path) -> Result<usize> {
    let Ok(entries) = std::fs::read_dir(meetings_dir) else {
        return Ok(0);
    };
    let connection = crate::storage::open_connection_by_path(db_path)?;
    let mut statement = connection.prepare("SELECT local_path FROM source_files")?;
    let used: std::collections::HashSet<PathBuf> = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|path| path.ok())
        .filter_map(|path| PathBuf::from(path).parent().map(Path::to_path_buf))
        .collect();
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age.as_secs() > 24 * 60 * 60);
        let waiting = path.join(LOCAL_FILE).is_file();
        if path.is_dir()
            && old
            && !waiting
            && !used.contains(&path)
            && std::fs::remove_dir_all(&path).is_ok()
        {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(dir: &Path, name: &str, value: f32, samples: usize) -> ChannelSummary {
        let started = Instant::now();
        let at = started + std::time::Duration::from_millis(samples as u64 * 1000 / 16_000);
        let mut writer = recorder::ChannelWriter::create(&dir.join(name), started).unwrap();
        writer
            .push(&recorder::Block {
                at,
                samples: vec![value; samples],
                rate: 16_000,
                channels: 1,
            })
            .unwrap();
        writer.finish(at).unwrap()
    }

    fn temp_meetings() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("murmela-meetings-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn finished_calls_write_a_mix_and_a_sidecar_the_file_job_recognises() {
        let meetings = temp_meetings();
        let dir = meetings.join("call-1");
        std::fs::create_dir_all(&dir).unwrap();
        let local = summary(&dir, LOCAL_FILE, 0.2, 16_000);
        let remote = summary(&dir, REMOTE_FILE, 0.3, 32_000);
        let events = vec![
            SpeakerEvent {
                at_ms: 100,
                name: "Ada".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: false,
            },
            SpeakerEvent {
                at_ms: 200,
                name: "Jo".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: true,
            },
        ];
        let finished = finish_files(
            &dir,
            "Client call",
            CallMode::Call,
            "2026-10-15T10:00:00+02:00",
            &local,
            Some(&remote),
            events,
            None,
        )
        .unwrap();
        assert_eq!(finished.source_file.original_name, "Client call.wav");
        assert_eq!(finished.source_file.duration_ms, Some(2_000));
        let (found_dir, sidecar) =
            sidecar_for(&meetings, Path::new(&finished.source_file.file_path)).unwrap();
        assert_eq!(sidecar.self_name.as_deref(), Some("Jo"));
        assert_eq!(found_dir, dir);
        assert!(sidecar.remote_heard);
        assert_eq!(sidecar.remote_path(&dir), Some(dir.join(REMOTE_FILE)));
        assert_eq!(sidecar.events.len(), 2);
        // A call.wav elsewhere is just a file.
        let elsewhere = temp_meetings();
        std::fs::copy(dir.join(MIX_FILE), elsewhere.join(MIX_FILE)).unwrap();
        std::fs::copy(dir.join(SIDECAR_FILE), elsewhere.join(SIDECAR_FILE)).unwrap();
        assert!(sidecar_for(&meetings, &elsewhere.join(MIX_FILE)).is_none());
        std::fs::remove_dir_all(meetings).unwrap();
        std::fs::remove_dir_all(elsewhere).unwrap();
    }

    #[test]
    fn a_silent_call_app_channel_is_left_out_and_total_silence_is_an_error() {
        let meetings = temp_meetings();
        let dir = meetings.join("call-2");
        std::fs::create_dir_all(&dir).unwrap();
        let local = summary(&dir, LOCAL_FILE, 0.2, 16_000);
        let remote = summary(&dir, REMOTE_FILE, 0.0, 16_000);
        let finished = finish_files(
            &dir,
            "T",
            CallMode::Call,
            "",
            &local,
            Some(&remote),
            vec![],
            None,
        )
        .unwrap();
        let (_, sidecar) =
            sidecar_for(&meetings, Path::new(&finished.source_file.file_path)).unwrap();
        assert!(!sidecar.remote_heard);
        assert_eq!(sidecar.remote_path(&dir), None);

        let quiet = summary(&dir, LOCAL_FILE, 0.0, 16_000);
        let error = finish_files(
            &dir,
            "T",
            CallMode::Call,
            "",
            &quiet,
            Some(&remote),
            vec![],
            None,
        )
        .unwrap_err();
        assert!(error.to_string().starts_with("CALL_SILENT"));
        std::fs::remove_dir_all(meetings).unwrap();
    }

    #[test]
    fn an_interrupted_recording_is_repaired_and_offered_for_transcription() {
        let meetings = temp_meetings();
        let id = Uuid::new_v4().to_string();
        let dir = meetings.join(&id);
        std::fs::create_dir_all(&dir).unwrap();
        for (name, value) in [(LOCAL_FILE, 0.2), (REMOTE_FILE, 0.4)] {
            summary(&dir, name, value, 32_000);
            // Simulate a crash: the header still claims an empty file.
            let mut bytes = std::fs::read(dir.join(name)).unwrap();
            let data_at = bytes.windows(4).position(|w| w == b"data").unwrap();
            bytes[4..8].copy_from_slice(&36u32.to_le_bytes());
            bytes[data_at + 4..data_at + 8].copy_from_slice(&0u32.to_le_bytes());
            std::fs::write(dir.join(name), bytes).unwrap();
        }
        let pending = pending_recordings(&meetings);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].interrupted);
        assert_eq!(pending[0].mode, CallMode::Call);
        assert_eq!(pending[0].duration_ms, 2_000);
        let source = pending_source(&meetings, &id).unwrap();
        let (_, sidecar) = sidecar_for(&meetings, Path::new(&source.file_path)).unwrap();
        assert!(sidecar.remote_heard);
        // Listed again, it is no longer interrupted.
        assert!(!pending_recordings(&meetings)[0].interrupted);
        assert!(pending_source(&meetings, "../etc").is_err());
        delete_recording(&meetings, &id).unwrap();
        assert!(pending_recordings(&meetings).is_empty());
        std::fs::remove_dir_all(meetings).unwrap();
    }

    #[test]
    fn unused_recordings_are_removed_but_waiting_ones_kept() {
        let meetings = temp_meetings();
        let db = meetings.join("db.sqlite");
        let connection = crate::storage::open_connection_by_path(&db).unwrap();
        connection
            .execute_batch("CREATE TABLE source_files (local_path TEXT);")
            .unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 24 * 60 * 60);
        let make = |name: &str, files: &[&str]| {
            let dir = meetings.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            for file in files {
                std::fs::write(dir.join(file), b"x").unwrap();
            }
            std::fs::File::open(&dir)
                .unwrap()
                .set_modified(old)
                .unwrap();
            dir
        };
        let saved = make("saved", &[MIX_FILE, SIDECAR_FILE]);
        let unused = make("unused", &[MIX_FILE, SIDECAR_FILE]);
        let waiting = make("waiting", &[LOCAL_FILE, SIDECAR_FILE]);
        connection
            .execute(
                "INSERT INTO source_files VALUES (?1)",
                [saved.join(MIX_FILE).to_string_lossy()],
            )
            .unwrap();
        assert_eq!(remove_unused_recordings(&meetings, &db).unwrap(), 1);
        assert!(saved.exists() && waiting.exists() && !unused.exists());
        std::fs::remove_dir_all(meetings).unwrap();
    }

    #[test]
    fn browser_reports_are_only_taken_while_recording_and_land_on_the_call_timeline() {
        let controller = MeetingController::new(temp_meetings());
        let report = || BrowserReport {
            platform: "meet<script>".into(),
            meeting_title: Some("Weekly".into()),
            self_name: None,
            participants: vec!["Ada".into()],
            events: vec![BrowserEvent {
                t: chrono::Utc::now().timestamp_millis(),
                name: " Ada ".into(),
                kind: SpeakerEventKind::Speaking,
                is_self: false,
            }],
        };
        assert!(!controller.receive_browser_report(report()));

        let dir = controller.meetings_dir.join("manual");
        std::fs::create_dir_all(&dir).unwrap();
        let started = Instant::now();
        struct Silence;
        impl recorder::AudioSource for Silence {
            fn open(&mut self, _: recorder::BlockSink) -> Result<Box<dyn std::any::Any>> {
                Ok(Box::new(()))
            }
            fn describe(&self) -> String {
                "Silence".into()
            }
        }
        let microphone =
            ChannelRecorder::start(Box::new(Silence), &dir.join(LOCAL_FILE), started).unwrap();
        *controller.active.lock().unwrap() = Some(ActiveCall {
            id: "manual".into(),
            dir: dir.clone(),
            mode: CallMode::Call,
            title: "Weekly".into(),
            started,
            started_epoch_ms: chrono::Utc::now().timestamp_millis() - 10_000,
            started_at: String::new(),
            microphone,
            system: None,
            events: vec![],
            browser: None,
            bridge: None,
        });
        assert!(controller.receive_browser_report(report()));
        let status = controller.status();
        let browser = status.browser.unwrap();
        assert_eq!(browser.platform, "meetscript");
        assert_eq!(browser.speaking, ["Ada"]);
        let events = controller
            .active
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .events
            .clone();
        assert_eq!(events.len(), 1);
        assert!((events[0].at_ms - 10_000).abs() < 1_000);
        controller.discard().unwrap();
        assert!(!dir.exists());
        std::fs::remove_dir_all(controller.meetings_dir).unwrap();
    }
}
