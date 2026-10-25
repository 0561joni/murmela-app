//! Ducks the Mac's output volume while dictating, then restores it.

use std::process::Command;
use std::sync::mpsc::{self, Sender, SyncSender};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};

const DUCK_FACTOR: f32 = 0.30;

#[derive(Debug, Clone)]
pub struct VolumeSnapshot {
    volume: f32,
    muted: bool,
}

enum VolumeJob {
    Duck,
    Restore,
    Flush(SyncSender<()>),
}

/// Ducks and restores on its own thread, in order, so neither `osascript` run
/// (about 140 ms each) delays opening the microphone or delivering the text.
pub struct VolumeDucker {
    jobs: Mutex<Sender<VolumeJob>>,
}

impl VolumeDucker {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel::<VolumeJob>();
        std::thread::Builder::new()
            .name("murmela-volume".into())
            .spawn(move || {
                let mut snapshot: Option<VolumeSnapshot> = None;
                for job in rx {
                    match job {
                        VolumeJob::Duck if snapshot.is_none() => {
                            match duck_to_30_percent_of_current() {
                                Ok(taken) => snapshot = taken,
                                Err(error) => eprintln!("[volume] ducking failed: {error:?}"),
                            }
                        }
                        VolumeJob::Duck => {}
                        VolumeJob::Restore => {
                            if let Some(taken) = snapshot.take() {
                                if let Err(error) = restore(taken) {
                                    eprintln!("[volume] restore failed: {error:?}");
                                }
                            }
                        }
                        VolumeJob::Flush(done) => {
                            let _ = done.send(());
                        }
                    }
                }
            })
            .expect("volume thread");
        Self {
            jobs: Mutex::new(tx),
        }
    }

    fn send(&self, job: VolumeJob) {
        if let Ok(jobs) = self.jobs.lock() {
            let _ = jobs.send(job);
        }
    }

    pub fn duck(&self) {
        self.send(VolumeJob::Duck);
    }

    pub fn restore(&self) {
        self.send(VolumeJob::Restore);
    }

    /// Waits until queued ducks and restores have run (used when quitting).
    pub fn flush(&self, timeout: Duration) {
        let (tx, rx) = mpsc::sync_channel(1);
        self.send(VolumeJob::Flush(tx));
        let _ = rx.recv_timeout(timeout);
    }
}

impl Default for VolumeDucker {
    fn default() -> Self {
        Self::new()
    }
}

/// Lowers the output volume to 30% of its current level while dictating.
/// Returns `None` when the output is muted or silent already. Reading and
/// setting share one `osascript` run.
fn duck_to_30_percent_of_current() -> Result<Option<VolumeSnapshot>> {
    let output = run_osascript(&[
        "set s to get volume settings",
        &format!(
            "if not output muted of s and output volume of s > 0 then set volume output volume (((output volume of s) * {DUCK_FACTOR}) as integer)"
        ),
        "return s",
    ])?;
    let snapshot = parse_volume_settings(&output)?;
    if snapshot.muted || snapshot.volume <= 0.0 {
        return Ok(None);
    }
    Ok(Some(snapshot))
}

fn restore(snapshot: VolumeSnapshot) -> Result<()> {
    let volume_percent = (snapshot.volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    run_osascript(&[
        &format!("set volume output volume {volume_percent}"),
        if snapshot.muted {
            "set volume with output muted"
        } else {
            "set volume without output muted"
        },
    ])?;
    Ok(())
}

fn run_osascript(lines: &[&str]) -> Result<String> {
    let mut command = Command::new("osascript");
    for line in lines {
        command.args(["-e", line]);
    }
    let output = command
        .output()
        .with_context(|| format!("failed to run osascript: {}", lines.join("; ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(anyhow!(
            "osascript failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn parse_volume_settings(output: &str) -> Result<VolumeSnapshot> {
    let mut volume = None;
    let mut muted = None;

    for part in output.split(',') {
        let part = part.trim();
        if let Some(raw) = part.strip_prefix("output volume:") {
            let parsed = raw
                .trim()
                .parse::<f32>()
                .context("failed to parse macOS output volume")?;
            volume = Some((parsed / 100.0).clamp(0.0, 1.0));
        } else if let Some(raw) = part.strip_prefix("output muted:") {
            let raw = raw.trim();
            muted = Some(match raw {
                "true" => true,
                "false" => false,
                _ => return Err(anyhow!("failed to parse macOS output mute state: {raw}")),
            });
        }
    }

    Ok(VolumeSnapshot {
        volume: volume.ok_or_else(|| anyhow!("macOS output volume missing"))?,
        muted: muted.ok_or_else(|| anyhow!("macOS output mute state missing"))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_volume_settings() {
        let parsed = parse_volume_settings(
            "output volume:72, input volume:50, alert volume:100, output muted:false",
        )
        .expect("settings should parse");

        assert!((parsed.volume - 0.72).abs() < f32::EPSILON);
        assert!(!parsed.muted);
    }

    #[test]
    fn duck_factor_renders_as_an_applescript_number() {
        assert_eq!(format!("{DUCK_FACTOR}"), "0.3");
    }
}
