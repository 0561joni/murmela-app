//! One progress bar for a file transcription. The worker says which step it
//! is in; each step gets a share of the bar sized by how long it usually takes
//! on this file, and a step that reports no progress of its own (starting the
//! worker and loading the model, finding pauses, matching speakers, and the
//! models that only say "done") moves on that estimate. So the bar starts
//! moving at once instead of sitting near 0 % until the decoder speaks up.

use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::transcription_policy::{DIRECT_FILE_MAX_MS, MAX_CHUNK_MS};

/// What a transcription worker is doing, in the order it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    LoadingModel = 0,
    FindingPauses = 1,
    Transcribing = 2,
    MatchingSpeakers = 3,
}

impl Step {
    const ALL: [Step; 4] = [
        Step::LoadingModel,
        Step::FindingPauses,
        Step::Transcribing,
        Step::MatchingSpeakers,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Step::LoadingModel => "Loading the model…",
            Step::FindingPauses => "Finding pauses…",
            Step::Transcribing => "Transcribing…",
            Step::MatchingSpeakers => "Matching speakers…",
        }
    }
}

/// The step of the transcription running in this process. A worker process
/// transcribes one file at a time, so one slot is enough.
static CURRENT_STEP: AtomicU8 = AtomicU8::new(Step::LoadingModel as u8);

pub fn set_step(step: Step) {
    CURRENT_STEP.store(step as u8, Ordering::Relaxed);
}

pub fn current_step() -> Step {
    Step::ALL
        .get(CURRENT_STEP.load(Ordering::Relaxed) as usize)
        .copied()
        .unwrap_or(Step::LoadingModel)
}

/// Share of the bar for preparing the audio, before the worker starts.
pub const PREPARING_SHARE: f32 = 2.0;

// Measured on an M3 Pro (18 GB) with the release build, October 2026.
/// Starting a worker process and reading the audio.
const WORKER_START_SECONDS: f32 = 1.0;
/// whisper.cpp and Qwen3-ASR map their model files into memory (about
/// 4.5 s for Qwen3-ASR's 4.4 GB).
const MAPPED_LOAD_BYTES_PER_SECOND: f32 = 1.0e9;
/// VibeVoice starts a Python runtime and loads its 9 GB model with MLX,
/// which maps the weights lazily (about 3 s).
const PYTHON_START_SECONDS: f32 = 1.5;
const MLX_LOAD_BYTES_PER_SECOND: f32 = 3.0e9;
/// Silero VAD over the whole recording.
const PAUSE_SECONDS_PER_AUDIO_MINUTE: f32 = 0.3;
/// Loading the voice model and embedding each chunk's speakers.
const MATCHING_SECONDS: f32 = 3.0;
const MATCHING_SECONDS_PER_AUDIO_MINUTE: f32 = 0.1;

/// Audio milliseconds per wall-clock millisecond before this Mac has
/// transcribed a file with the model (measured on an M3 Pro).
fn default_speed(engine: &str) -> f32 {
    match engine {
        "whisper.cpp" => 6.0,
        "qwen3_asr_c" => 2.5,
        "vibevoice-mlx" => 1.7,
        _ => 2.0,
    }
}

/// How long each step of one worker run is expected to take, and where it
/// sits on the bar.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Where each step starts on the 0–100 bar; the last one ends at 100.
    starts: [f32; 4],
    expected: [Duration; 4],
    /// The model reports how far it got while transcribing.
    reports_progress: bool,
}

impl Plan {
    pub fn new(
        engine: &str,
        model_size_bytes: i64,
        audio_ms: i64,
        audio_ms_per_wall_ms: Option<f64>,
    ) -> Self {
        let audio_minutes = audio_ms.max(0) as f32 / 60_000.0;
        let model_bytes = model_size_bytes.max(0) as f32;
        let loading = WORKER_START_SECONDS
            + match engine {
                "whisper.cpp" | "qwen3_asr_c" => model_bytes / MAPPED_LOAD_BYTES_PER_SECOND,
                "vibevoice-mlx" => PYTHON_START_SECONDS + model_bytes / MLX_LOAD_BYTES_PER_SECOND,
                // MOSS loads its model inside the decoder run.
                _ => PYTHON_START_SECONDS,
            };
        let finds_pauses = match engine {
            "whisper.cpp" => audio_ms > DIRECT_FILE_MAX_MS,
            "qwen3_asr_c" => audio_ms > MAX_CHUNK_MS,
            _ => false,
        };
        let pauses = if finds_pauses {
            audio_minutes * PAUSE_SECONDS_PER_AUDIO_MINUTE
        } else {
            0.0
        };
        let speed = audio_ms_per_wall_ms
            .filter(|speed| speed.is_finite() && *speed > 0.0)
            .map(|speed| speed as f32)
            .unwrap_or_else(|| default_speed(engine));
        let transcribing = (audio_ms.max(1) as f32 / 1_000.0 / speed).max(1.0);
        let chunked = engine == "vibevoice-mlx"
            && audio_ms
                > crate::native_chunks::chunk_length_ms(crate::memory_pressure::installed_bytes());
        let matching = if chunked {
            MATCHING_SECONDS + audio_minutes * MATCHING_SECONDS_PER_AUDIO_MINUTE
        } else {
            0.0
        };
        Self::from_seconds(
            [loading, pauses, transcribing, matching],
            matches!(engine, "whisper.cpp" | "qwen3_asr_c"),
        )
    }

    fn from_seconds(seconds: [f32; 4], reports_progress: bool) -> Self {
        let total = seconds.iter().sum::<f32>().max(f32::EPSILON);
        let mut starts = [0.0; 4];
        for index in 1..4 {
            starts[index] = starts[index - 1] + seconds[index - 1] / total * 100.0;
        }
        Self {
            starts,
            expected: seconds.map(Duration::from_secs_f32),
            reports_progress,
        }
    }

    /// Where the bar stands `elapsed` into `step`, given the model's own
    /// progress through the run (0–100, below 0 before it reported any).
    fn percent(&self, step: Step, elapsed: Duration, reported: i32) -> f32 {
        let index = step as usize;
        let start = self.starts[index];
        let end = self.starts.get(index + 1).copied().unwrap_or(100.0);
        let expected = self.expected[index].as_secs_f32().max(0.1);
        let estimated = start + (end - start) * eased(elapsed.as_secs_f32() / expected);
        if step != Step::Transcribing {
            return estimated;
        }
        let reported = start + (end - start) * reported.clamp(0, 100) as f32 / 100.0;
        if self.reports_progress {
            reported
        } else {
            estimated.max(reported)
        }
    }
}

/// Linear while a step takes as long as expected, then slowing down, so an
/// estimate that was too short creeps on instead of reaching the end early.
fn eased(fraction: f32) -> f32 {
    const KNEE: f32 = 0.8;
    let fraction = fraction.max(0.0);
    if fraction <= KNEE {
        fraction
    } else {
        let rest = 1.0 - KNEE;
        1.0 - rest * rest / (rest + fraction - KNEE)
    }
}

/// Follows one worker run and gives the bar's position (0–100), which only
/// ever moves forward.
#[derive(Debug, Clone)]
pub struct Tracker {
    plan: Plan,
    step: Step,
    step_started: Instant,
    shown: f32,
}

impl Tracker {
    pub fn new(plan: Plan, now: Instant) -> Self {
        Self {
            plan,
            step: Step::LoadingModel,
            step_started: now,
            shown: 0.0,
        }
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// A step the worker reports out of order (a whisper retry finding pauses
    /// again) is ignored.
    pub fn update(&mut self, step: Option<Step>, reported: i32, now: Instant) -> f32 {
        if let Some(step) = step.filter(|step| *step > self.step) {
            self.step = step;
            self.step_started = now;
        }
        let elapsed = now.saturating_duration_since(self.step_started);
        self.shown = self
            .shown
            .max(self.plan.percent(self.step, elapsed, reported));
        self.shown
    }
}

/// How far the audio preparation has got after `elapsed`, on the
/// `PREPARING_SHARE` of the bar.
pub fn preparing_percent(file_size_bytes: i64, elapsed: Duration) -> f32 {
    // Decoding and resampling: measured 0.6–2.7 s for files of a few MB to
    // an hour-long video.
    let expected = 0.5 + file_size_bytes.max(0) as f32 / 100e6;
    PREPARING_SHARE * eased(elapsed.as_secs_f32() / expected)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eased_is_linear_then_never_reaches_the_end() {
        assert_eq!(eased(0.0), 0.0);
        assert!((eased(0.5) - 0.5).abs() < 1e-6);
        assert!((eased(0.8) - 0.8).abs() < 1e-6);
        assert!(eased(1.0) > 0.85 && eased(1.0) < 0.95);
        assert!(eased(3.0) > eased(2.0));
        assert!(eased(100.0) < 1.0);
    }

    #[test]
    fn shares_follow_the_expected_seconds() {
        let plan = Plan::from_seconds([10.0, 0.0, 90.0, 0.0], false);
        assert_eq!(plan.starts, [0.0, 10.0, 10.0, 100.0]);
    }

    #[test]
    fn a_model_without_progress_moves_on_the_estimate() {
        let start = Instant::now();
        let mut tracker = Tracker::new(Plan::from_seconds([10.0, 0.0, 90.0, 0.0], false), start);
        let loading = tracker.update(None, -1, start + Duration::from_secs(5));
        assert!((loading - 5.0).abs() < 0.01, "{loading}");
        tracker.update(Some(Step::Transcribing), 1, start + Duration::from_secs(10));
        let halfway = tracker.update(None, 1, start + Duration::from_secs(55));
        assert!((halfway - 55.0).abs() < 0.01, "{halfway}");
        // A finished chunk the estimate had not reached yet counts.
        let chunk = tracker.update(None, 80, start + Duration::from_secs(56));
        assert!((chunk - 82.0).abs() < 0.01, "{chunk}");
    }

    #[test]
    fn reported_progress_drives_transcribing_and_the_bar_never_goes_back() {
        let start = Instant::now();
        let mut tracker = Tracker::new(Plan::from_seconds([10.0, 10.0, 80.0, 0.0], true), start);
        tracker.update(Some(Step::FindingPauses), -1, start + Duration::from_secs(10));
        let pauses = tracker.update(None, -1, start + Duration::from_secs(15));
        assert!((pauses - 15.0).abs() < 0.01, "{pauses}");
        // Pauses took longer than expected: transcribing starts where the bar is.
        let slow = tracker.update(None, -1, start + Duration::from_secs(60));
        assert!(slow < 20.0);
        let transcribing = tracker.update(Some(Step::Transcribing), 0, start + Duration::from_secs(61));
        assert!(transcribing >= slow);
        assert!((tracker.update(None, 50, start + Duration::from_secs(80)) - 60.0).abs() < 0.01);
        // A retry that finds pauses again does not move the bar back.
        assert!((tracker.update(Some(Step::FindingPauses), 10, start + Duration::from_secs(81)) - 60.0).abs() < 0.01);
        assert_eq!(tracker.step(), Step::Transcribing);
    }

    #[test]
    fn long_whisper_files_find_pauses_and_vibevoice_starts_python() {
        let whisper = Plan::new("whisper.cpp", 574_041_195, 600_000, Some(4.4));
        assert!(whisper.starts[2] > whisper.starts[1]);
        assert!(whisper.expected[0] < Duration::from_secs(3));
        let short = Plan::new("whisper.cpp", 574_041_195, 30_000, None);
        assert_eq!(short.starts[1], short.starts[2]);
        let vibevoice = Plan::new("vibevoice-mlx", 8_900_000_000, 120_000, Some(1.71));
        assert!(vibevoice.expected[0] > Duration::from_secs(4));
        assert!(!vibevoice.reports_progress);
    }

    #[test]
    fn preparing_stays_within_its_share() {
        assert_eq!(preparing_percent(1_000_000, Duration::ZERO), 0.0);
        assert!(preparing_percent(1_000_000, Duration::from_secs(60)) < PREPARING_SHARE);
    }
}
