use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use sherpa_onnx::{
    FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
    OfflineSpeakerSegmentationModelConfig, OfflineSpeakerSegmentationPyannoteModelConfig,
    SpeakerEmbeddingExtractorConfig,
};

use crate::audio_preprocess;
use crate::diarization::{RawDiarizationTurn, DIARIZATION_MODEL_SPEC_V2};

pub const WORKER_ARG: &str = "--diarize-worker";

#[derive(Debug)]
pub struct DiarizationWorkerCanceled;

impl std::fmt::Display for DiarizationWorkerCanceled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("diarization canceled")
    }
}

impl std::error::Error for DiarizationWorkerCanceled {}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerRequest {
    pub job_id: String,
    pub audio_path: PathBuf,
    pub package_path: PathBuf,
    pub exact_speaker_count: Option<i32>,
    pub spec_version: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerOutput {
    Heartbeat,
    Result { turns: Vec<RawDiarizationTurn> },
    Error { message: String },
}

pub fn run_stdio_worker() -> i32 {
    match run_stdio_worker_inner() {
        Ok(()) => 0,
        Err(error) => {
            let _ = emit_output(&WorkerOutput::Error {
                message: error.to_string(),
            });
            1
        }
    }
}

fn run_stdio_worker_inner() -> Result<()> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("failed to read diarization worker request")?;
    let request: WorkerRequest =
        serde_json::from_str(&input).context("failed to parse diarization worker request")?;
    if request.spec_version != DIARIZATION_MODEL_SPEC_V2.manifest_version {
        return Err(anyhow!("Unsupported diarization manifest version."));
    }
    crate::diarization::validate_speaker_count_hint(request.exact_speaker_count)
        .map_err(anyhow::Error::msg)?;

    let (finish_tx, finish_rx) = std::sync::mpsc::channel();
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let heartbeat = spawn_heartbeat(finish_rx, Arc::clone(&stdout));
    let result = diarize(&request);
    let _ = finish_tx.send(());
    let _ = heartbeat.join();
    emit_output_to(&stdout, &WorkerOutput::Result { turns: result? })
}

fn diarize(request: &WorkerRequest) -> Result<Vec<RawDiarizationTurn>> {
    let segmentation_path = request.package_path.join("segmentation.onnx");
    let embedding_path = request.package_path.join("embedding.onnx");
    if !segmentation_path.is_file() || !embedding_path.is_file() {
        return Err(anyhow!(
            "Diarization package is incomplete; segmentation.onnx and embedding.onnx are required."
        ));
    }
    let threads = thread::available_parallelism()
        .map(|value| value.get().min(4) as i32)
        .unwrap_or(1);
    let config = OfflineSpeakerDiarizationConfig {
        segmentation: OfflineSpeakerSegmentationModelConfig {
            pyannote: OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(segmentation_path.to_string_lossy().into_owned()),
            },
            num_threads: threads,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        embedding: SpeakerEmbeddingExtractorConfig {
            model: Some(embedding_path.to_string_lossy().into_owned()),
            num_threads: threads,
            debug: false,
            provider: Some("cpu".to_string()),
        },
        clustering: FastClusteringConfig {
            num_clusters: request.exact_speaker_count.unwrap_or(-1),
            threshold: DIARIZATION_MODEL_SPEC_V2.clustering_threshold,
        },
        min_duration_on: DIARIZATION_MODEL_SPEC_V2.min_duration_on_seconds,
        min_duration_off: DIARIZATION_MODEL_SPEC_V2.min_duration_off_seconds,
    };
    let diarizer = OfflineSpeakerDiarization::create(&config)
        .ok_or_else(|| anyhow!("sherpa-onnx could not initialize the diarization models."))?;
    let prepared = audio_preprocess::decode_audio_file(&request.audio_path)?;
    if diarizer.sample_rate() != prepared.sample_rate_hz as i32 {
        return Err(anyhow!(
            "Diarization model expects {} Hz audio, but preprocessing produced {} Hz.",
            diarizer.sample_rate(),
            prepared.sample_rate_hz
        ));
    }
    let result = diarizer
        .process(&prepared.samples)
        .ok_or_else(|| anyhow!("sherpa-onnx diarization failed."))?;
    let segments = result.sort_by_start_time();
    let intervals = segments
        .iter()
        .map(|segment| {
            (
                (segment.start * 1000.0).round() as i64,
                (segment.end * 1000.0).round() as i64,
                segment.speaker,
            )
        })
        .filter(|(start, end, _)| end > start)
        .collect::<Vec<_>>();
    Ok(atomic_turns(&intervals))
}

fn atomic_turns(intervals: &[(i64, i64, i32)]) -> Vec<RawDiarizationTurn> {
    // End/start events at the same instant are applied together. Reference
    // counts preserve a speaker with intersecting intervals of its own.
    let mut events = Vec::with_capacity(intervals.len() * 2);
    for &(start, end, speaker) in intervals {
        if end > start {
            events.push((start, speaker, 1i32));
            events.push((end, speaker, -1));
        }
    }
    events.sort_unstable();
    let mut active = std::collections::BTreeMap::<i32, i32>::new();
    let mut turns: Vec<RawDiarizationTurn> = Vec::new();
    let mut index = 0;
    while index < events.len() {
        let start_ms = events[index].0;
        while index < events.len() && events[index].0 == start_ms {
            let (_, speaker, delta) = events[index];
            *active.entry(speaker).or_default() += delta;
            if active[&speaker] == 0 {
                active.remove(&speaker);
            }
            index += 1;
        }
        if index == events.len() || active.is_empty() {
            continue;
        }
        let end_ms = events[index].0;
        let cluster_ids: Vec<_> = active.keys().copied().collect();
        if let Some(previous) = turns.last_mut() {
            if previous.end_ms == start_ms && previous.cluster_ids == cluster_ids {
                previous.end_ms = end_ms;
                continue;
            }
        }
        turns.push(RawDiarizationTurn {
            start_ms,
            end_ms,
            cluster_ids,
            confidence: None,
        });
    }
    turns
}

fn spawn_heartbeat(
    finished: std::sync::mpsc::Receiver<()>,
    stdout: Arc<Mutex<io::Stdout>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || loop {
        let _ = emit_output_to(&stdout, &WorkerOutput::Heartbeat);
        if finished.recv_timeout(Duration::from_secs(2))
            != Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        {
            break;
        }
    })
}

fn emit_output(output: &WorkerOutput) -> Result<()> {
    emit_output_to(&Arc::new(Mutex::new(io::stdout())), output)
}

fn emit_output_to(stdout: &Arc<Mutex<io::Stdout>>, output: &WorkerOutput) -> Result<()> {
    let mut stdout = stdout
        .lock()
        .map_err(|_| anyhow!("diarization worker stdout lock poisoned"))?;
    serde_json::to_writer(&mut *stdout, output)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub fn parse_worker_output_line(line: &str) -> Result<WorkerOutput> {
    serde_json::from_str(line.trim()).context("failed to parse diarization worker output")
}

pub fn read_worker_output_lines<R: io::Read + Send + 'static>(
    reader: R,
    sender: std::sync::mpsc::Sender<Result<WorkerOutput, String>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in io::BufReader::new(reader).lines() {
            let output = line.map_err(|error| error.to_string()).and_then(|line| {
                parse_worker_output_line(&line).map_err(|error| error.to_string())
            });
            if sender.send(output).is_err() {
                break;
            }
        }
    })
}

pub fn run_subprocess_worker<F>(
    request: &WorkerRequest,
    cancelled: Option<&AtomicBool>,
    mut on_activity: F,
) -> Result<Vec<RawDiarizationTurn>>
where
    F: FnMut(),
{
    let executable = std::env::current_exe().context("failed to locate app executable")?;
    let mut command = Command::new(executable);
    crate::managed_process::isolate(&mut command);
    let child = command
        .arg(WORKER_ARG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to start isolated diarization worker")?;
    let mut child = crate::managed_process::ManagedChild::new(child);
    if let Some(mut stdin) = child.stdin.take() {
        serde_json::to_writer(&mut stdin, request)?;
        stdin.write_all(b"\n")?;
    }
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("diarization worker stdout was unavailable"))?;
    let (sender, receiver) = std::sync::mpsc::channel();
    let reader = read_worker_output_lines(stdout, sender);
    let mut last_output = Instant::now();
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::SeqCst)) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err(DiarizationWorkerCanceled.into());
        }
        match receiver.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(WorkerOutput::Heartbeat)) => {
                record_worker_activity(&mut last_output, &mut on_activity)
            }
            Ok(Ok(WorkerOutput::Result { turns })) => {
                let status = child.wait()?;
                let _ = reader.join();
                if status.success() {
                    return Ok(turns);
                }
                return Err(anyhow!("Diarization worker exited unsuccessfully."));
            }
            Ok(Ok(WorkerOutput::Error { message })) => {
                let _ = child.wait();
                let _ = reader.join();
                return Err(anyhow!(message));
            }
            Ok(Err(message)) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err(anyhow!(message));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if last_output.elapsed() > Duration::from_secs(10 * 60) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = reader.join();
                    return Err(anyhow!("Diarization worker stopped responding."));
                }
                if let Some(status) = child.try_wait()? {
                    let _ = reader.join();
                    return Err(anyhow!("Diarization worker exited with {status}."));
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let status = child.wait()?;
                let _ = reader.join();
                return Err(anyhow!(
                    "Diarization worker closed its output with {status}."
                ));
            }
        }
    }
}

fn record_worker_activity<F>(last_output: &mut Instant, on_activity: &mut F)
where
    F: FnMut(),
{
    *last_output = Instant::now();
    on_activity();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference_turns(intervals: &[(i64, i64, i32)]) -> Vec<RawDiarizationTurn> {
        let mut boundaries: Vec<_> = intervals.iter().flat_map(|(s, e, _)| [*s, *e]).collect();
        boundaries.sort();
        boundaries.dedup();
        let mut result: Vec<RawDiarizationTurn> = Vec::new();
        for pair in boundaries.windows(2) {
            let (start, end) = (pair[0], pair[1]);
            let mut ids: Vec<_> = intervals
                .iter()
                .filter(|(s, e, _)| *s < end && *e > start)
                .map(|(_, _, id)| *id)
                .collect();
            ids.sort();
            ids.dedup();
            if ids.is_empty() {
                continue;
            }
            if let Some(last) = result.last_mut() {
                if last.end_ms == start && last.cluster_ids == ids {
                    last.end_ms = end;
                    continue;
                }
            }
            result.push(RawDiarizationTurn {
                start_ms: start,
                end_ms: end,
                cluster_ids: ids,
                confidence: None,
            });
        }
        result
    }

    #[test]
    fn sweep_matches_full_scan_with_nested_intervals_ties_gaps_and_overlaps() {
        let mut seed = 29u64;
        for count in [1, 2, 10, 100, 1000] {
            let spans: Vec<_> = (0..count)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let start = (seed % 1000) as i64 * 10;
                    let duration = (seed / 1000 % 100 + 1) as i64 * 10;
                    (start, start + duration, (seed % 8) as i32)
                })
                .collect();
            assert_eq!(
                serde_json::to_value(atomic_turns(&spans)).unwrap(),
                serde_json::to_value(reference_turns(&spans)).unwrap()
            );
        }
    }

    #[test]
    #[ignore = "synthetic performance comparison; run with --ignored --nocapture"]
    fn benchmark_interval_sweep() {
        for minutes in [30, 60, 120] {
            for speakers in [2, 4, 8] {
                let spans: Vec<_> = (0..minutes * 60 * 4)
                    .map(|i| {
                        let start = i * 250;
                        (start, start + 350, (i / 8 % speakers) as i32)
                    })
                    .collect();
                let start = Instant::now();
                let reference = reference_turns(&spans);
                let baseline = start.elapsed();
                let start = Instant::now();
                let sweep = atomic_turns(&spans);
                let optimized = start.elapsed();
                assert_eq!(
                    serde_json::to_value(&reference).unwrap(),
                    serde_json::to_value(&sweep).unwrap()
                );
                println!("intervals minutes={minutes} speakers={speakers} count={} baseline_ms={:.3} sweep_ms={:.3}",spans.len(),baseline.as_secs_f64()*1000.,optimized.as_secs_f64()*1000.);
            }
        }
    }

    #[test]
    fn converts_overlapping_segments_to_atomic_turns() {
        let turns = atomic_turns(&[(0, 1_000, 0), (500, 1_500, 1)]);
        assert_eq!(turns.len(), 3);
        assert_eq!(turns[1].cluster_ids, vec![0, 1]);
        assert_eq!((turns[1].start_ms, turns[1].end_ms), (500, 1_000));
    }

    #[test]
    fn heartbeat_refreshes_liveness_and_notifies_the_caller() {
        let mut last_output = Instant::now() - Duration::from_secs(30);
        let mut activity_count = 0;

        record_worker_activity(&mut last_output, &mut || activity_count += 1);

        assert_eq!(activity_count, 1);
        assert!(last_output.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn cancellation_has_a_typed_error() {
        let error = anyhow::Error::from(DiarizationWorkerCanceled);
        assert!(error.is::<DiarizationWorkerCanceled>());
    }
}
