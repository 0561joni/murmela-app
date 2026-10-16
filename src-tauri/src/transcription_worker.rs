use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::asr::{
    discover_installed_models, FileTranscriptionRequest, LocalTranscriptionEngine,
    TranscriptResult, TranscriptionEngine,
};

pub const WORKER_ARG: &str = "--transcribe-worker";
/// Same protocol, but one request per stdin line and the process stays alive
/// (with its loaded model) until stdin closes. Used for warm dictation reuse.
pub const PERSISTENT_WORKER_ARG: &str = "--transcribe-worker-persistent";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerRequest {
    pub models_dir: PathBuf,
    pub request: FileTranscriptionRequest,
    /// Only load the request's model (persistent worker) and answer `Ready`.
    /// The audio file is not read.
    #[serde(default)]
    pub preload: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerOutput {
    Progress { progress_percent: i32 },
    Heartbeat { progress_percent: i32 },
    Result { result: TranscriptResult },
    Ready,
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
        .context("failed to read transcription worker request")?;
    let request: WorkerRequest =
        serde_json::from_str(&input).context("failed to parse transcription worker request")?;

    let models = discover_installed_models(&request.models_dir)?;
    let engine = LocalTranscriptionEngine::new(request.models_dir, models);
    let progress = Arc::new(AtomicI32::new(-1));
    let finished = Arc::new(AtomicBool::new(false));
    let stdout = Arc::new(Mutex::new(io::stdout()));

    let progress_thread = spawn_progress_emitter(
        Arc::clone(&progress),
        Arc::clone(&finished),
        Arc::clone(&stdout),
    );

    let result = engine.transcribe_file(request.request, Some(progress));
    finished.store(true, Ordering::SeqCst);
    let _ = progress_thread.join();

    let result = result?;
    emit_output_to(&stdout, &WorkerOutput::Result { result })
}

pub fn run_persistent_stdio_worker() -> i32 {
    let stdout = Arc::new(Mutex::new(io::stdout()));
    let mut engine: Option<(PathBuf, LocalTranscriptionEngine)> = None;
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(line) => line,
            Err(_) => return 1,
        };
        if line.trim().is_empty() {
            continue;
        }
        let outcome = handle_persistent_request(&line, &mut engine, &stdout);
        if let Err(error) = outcome {
            if emit_output_to(
                &stdout,
                &WorkerOutput::Error {
                    message: error.to_string(),
                },
            )
            .is_err()
            {
                return 1;
            }
        }
    }
    0
}

fn handle_persistent_request(
    line: &str,
    engine: &mut Option<(PathBuf, LocalTranscriptionEngine)>,
    stdout: &Arc<Mutex<io::Stdout>>,
) -> Result<()> {
    let request: WorkerRequest =
        serde_json::from_str(line).context("failed to parse transcription worker request")?;
    if engine
        .as_ref()
        .is_none_or(|(models_dir, _)| *models_dir != request.models_dir)
    {
        *engine = None;
        let models = discover_installed_models(&request.models_dir)?;
        *engine = Some((
            request.models_dir.clone(),
            LocalTranscriptionEngine::new(request.models_dir.clone(), models),
        ));
    }
    let (_, engine) = engine.as_ref().expect("engine initialized");
    // Pick up newly installed models without dropping the warm context of the
    // model that is already loaded.
    if let Some(model_id) = request.request.selected_model_id.as_deref() {
        if !engine.list_models()?.iter().any(|model| model.id == model_id) {
            engine.refresh_from_disk()?;
        }
    }

    if request.preload {
        engine.preload(&request.request)?;
        return emit_output_to(stdout, &WorkerOutput::Ready);
    }

    let progress = Arc::new(AtomicI32::new(-1));
    let finished = Arc::new(AtomicBool::new(false));
    let progress_thread = spawn_progress_emitter(
        Arc::clone(&progress),
        Arc::clone(&finished),
        Arc::clone(stdout),
    );
    let result = engine.transcribe_file(request.request, Some(progress));
    finished.store(true, Ordering::SeqCst);
    let _ = progress_thread.join();
    let result = result?;
    emit_output_to(stdout, &WorkerOutput::Result { result })
}

fn spawn_progress_emitter(
    progress: Arc<AtomicI32>,
    finished: Arc<AtomicBool>,
    stdout: Arc<Mutex<io::Stdout>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_progress = -1;
        let mut ticks = 0_u32;
        while !finished.load(Ordering::SeqCst) {
            let current = progress.load(Ordering::Relaxed);
            if current >= 0 && current != last_progress {
                let _ = emit_output_to(
                    &stdout,
                    &WorkerOutput::Progress {
                        progress_percent: current,
                    },
                );
                last_progress = current;
            }
            if ticks % 4 == 0 {
                let _ = emit_output_to(
                    &stdout,
                    &WorkerOutput::Heartbeat {
                        progress_percent: current,
                    },
                );
            }
            ticks = ticks.wrapping_add(1);
            thread::sleep(Duration::from_millis(500));
        }
    })
}

fn emit_output(output: &WorkerOutput) -> Result<()> {
    let stdout = Arc::new(Mutex::new(io::stdout()));
    emit_output_to(&stdout, output)
}

fn emit_output_to(stdout: &Arc<Mutex<io::Stdout>>, output: &WorkerOutput) -> Result<()> {
    let mut stdout = stdout
        .lock()
        .map_err(|_| anyhow!("transcription worker stdout lock poisoned"))?;
    serde_json::to_writer(&mut *stdout, output)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}

pub fn parse_worker_output_line(line: &str) -> Result<WorkerOutput> {
    serde_json::from_str(line.trim()).context("failed to parse transcription worker output")
}

pub fn read_worker_output_lines<R: io::Read + Send + 'static>(
    reader: R,
    sender: std::sync::mpsc::Sender<Result<WorkerOutput, String>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let reader = io::BufReader::new(reader);
        for line in reader.lines() {
            let output = match line {
                Ok(line) => parse_worker_output_line(&line).map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            if sender.send(output).is_err() {
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress_output_line() {
        let output = parse_worker_output_line(r#"{"type":"progress","progress_percent":42}"#)
            .expect("progress output");
        match output {
            WorkerOutput::Progress { progress_percent } => assert_eq!(progress_percent, 42),
            _ => panic!("expected progress"),
        }
    }

    #[test]
    fn rejects_malformed_output_line() {
        let output = parse_worker_output_line("not-json");
        assert!(output.is_err());
    }

    #[test]
    fn preload_flag_defaults_off_and_ready_round_trips() {
        let request: WorkerRequest = serde_json::from_value(serde_json::json!({
            "modelsDir": "/tmp/models",
            "request": {
                "profile": "balanced",
                "selectedModelId": null,
                "languageMode": "auto",
                "fixedLanguage": null,
                "timestamps": false,
                "preferGpu": true,
                "filePath": ""
            }
        }))
        .expect("request without preload");
        assert!(!request.preload);
        let line = serde_json::to_string(&WorkerOutput::Ready).unwrap();
        assert!(matches!(
            parse_worker_output_line(&line).unwrap(),
            WorkerOutput::Ready
        ));
    }

    #[test]
    fn parses_heartbeat_output_line() {
        let output = parse_worker_output_line(r#"{"type":"heartbeat","progress_percent":17}"#)
            .expect("heartbeat output");
        match output {
            WorkerOutput::Heartbeat { progress_percent } => assert_eq!(progress_percent, 17),
            _ => panic!("expected heartbeat"),
        }
    }
}
