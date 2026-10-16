//! Ducks the Mac's output volume while dictating, then restores it.

use std::process::Command;

use anyhow::{anyhow, Context, Result};

const DUCK_FACTOR: f32 = 0.30;

#[derive(Debug, Clone)]
pub struct VolumeSnapshot {
    volume: f32,
    muted: bool,
}

fn ducked_volume(current: f32) -> f32 {
    (current.clamp(0.0, 1.0) * DUCK_FACTOR).clamp(0.0, 1.0)
}

/// Lowers the output volume to 30% of its current level while dictating.
/// Returns `None` when the output is muted or silent already.
pub fn duck_to_30_percent_of_current() -> Result<Option<VolumeSnapshot>> {
    let snapshot = read_volume_settings()?;
    if snapshot.muted || snapshot.volume <= 0.0 {
        return Ok(None);
    }

    let ducked_percent = (ducked_volume(snapshot.volume) * 100.0).round() as u8;
    run_osascript(&format!("set volume output volume {ducked_percent}"))?;
    Ok(Some(snapshot))
}

pub fn restore(snapshot: VolumeSnapshot) -> Result<()> {
    let volume_percent = (snapshot.volume.clamp(0.0, 1.0) * 100.0).round() as u8;
    run_osascript(&format!("set volume output volume {volume_percent}"))?;
    if snapshot.muted {
        run_osascript("set volume with output muted")?;
    } else {
        run_osascript("set volume without output muted")?;
    }
    Ok(())
}

fn read_volume_settings() -> Result<VolumeSnapshot> {
    let output = Command::new("osascript")
        .args(["-e", "get volume settings"])
        .output()
        .context("failed to query macOS output volume")?;

    if !output.status.success() {
        return Err(anyhow!(
            "macOS output volume query failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    parse_volume_settings(&String::from_utf8_lossy(&output.stdout))
}

fn run_osascript(script: &str) -> Result<()> {
    let output = Command::new("osascript")
        .args(["-e", script])
        .output()
        .with_context(|| format!("failed to run osascript: {script}"))?;

    if output.status.success() {
        Ok(())
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
    fn calculates_thirty_percent_of_current_volume() {
        assert!((ducked_volume(0.8) - 0.24).abs() < f32::EPSILON);
        assert!((ducked_volume(0.2) - 0.06).abs() < f32::EPSILON);
    }
}
