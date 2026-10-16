//! Real Tauri controllers + SQLite + subprocess protocol; deterministic inference.
//! Does not initialize AppState or open the user's database. The model directory
//! is read only for the normal installed-package admission check.
//! MURMELA_SMOKE_MODELS=/path/to/models cargo run --example review_workflow_smoke
use anyhow::{bail, Result};
use murmela_lib::{
    asr, audio_files, diarization, diarization_worker, file_jobs, review, review_jobs,
    transcription_worker,
};
use std::{
    io::Read,
    path::PathBuf,
    sync::{atomic::AtomicI32, Arc},
    time::{Duration, Instant},
};

struct FixtureEngine;
impl asr::TranscriptionEngine for FixtureEngine {
    fn list_models(&self) -> Result<Vec<asr::InstalledModel>> {
        Ok(vec![])
    }
    fn refresh_from_disk(&self) -> Result<Vec<asr::InstalledModel>> {
        self.list_models()
    }
    fn transcribe_file(
        &self,
        _: asr::FileTranscriptionRequest,
        _: Option<Arc<AtomicI32>>,
    ) -> Result<asr::TranscriptResult> {
        bail!("The subprocess supplies fixture inference")
    }
}
fn result(id: &str) -> asr::TranscriptResult {
    serde_json::from_value(serde_json::json!({"jobId":id,"modelName":"Workflow fixture","fullText":"First. Second.","plainText":"First. Second.","timestampedText":"First. Second.","detectedLanguages":["en"],"qualityStatus":"clean","recoveredRegionCount":0,"warnings":[],"diarizationStatus":"not_requested","diarizationSource":"none","diarizationModelId":null,"diarizationWarning":null,"diarizationPolicyVersion":null,"diarizationClusteringThreshold":null,"diarizationSpeakerCountHint":null,"speakers":[],"diarizationTurns":[],"segments":[
        {"id":format!("{id}:0"),"startMs":0,"endMs":1000,"text":"First.","languageCode":"en","segmentOrder":0,"confidence":null,"speakerId":null,"speakerIds":null,"speakerAttribution":"none","speakerConfidence":null},
        {"id":format!("{id}:1"),"startMs":1000,"endMs":2000,"text":"Second.","languageCode":"en","segmentOrder":1,"confidence":null,"speakerId":null,"speakerIds":null,"speakerAttribution":"none","speakerConfidence":null}
    ]})).unwrap()
}
fn worker() -> bool {
    let args: Vec<_> = std::env::args().collect();
    if args.iter().any(|s| s == transcription_worker::WORKER_ARG) {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw).unwrap();
        let request: transcription_worker::WorkerRequest = serde_json::from_str(&raw).unwrap();
        println!(
            "{}",
            serde_json::to_string(&transcription_worker::WorkerOutput::Result {
                result: result(&request.request.file_path)
            })
            .unwrap()
        );
        return true;
    }
    if args.iter().any(|s| s == diarization_worker::WORKER_ARG) {
        let mut raw = String::new();
        std::io::stdin().read_to_string(&mut raw).unwrap();
        let request: diarization_worker::WorkerRequest = serde_json::from_str(&raw).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        let output = if request.job_id.contains("failed") {
            diarization_worker::WorkerOutput::Error {
                message: "Fixture inference failure".into(),
            }
        } else {
            diarization_worker::WorkerOutput::Result {
                turns: if request.job_id.contains("empty") {
                    vec![]
                } else {
                    vec![
                        diarization::RawDiarizationTurn {
                            start_ms: 0,
                            end_ms: 1000,
                            cluster_ids: vec![0],
                            confidence: None,
                        },
                        diarization::RawDiarizationTurn {
                            start_ms: 1000,
                            end_ms: 2000,
                            cluster_ids: vec![1],
                            confidence: None,
                        },
                    ]
                },
            }
        };
        println!("{}", serde_json::to_string(&output).unwrap());
        return true;
    }
    false
}
fn wait<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(result) = check() {
            return result;
        }
        assert!(Instant::now() < deadline, "Workflow timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn workflow(app: tauri::AppHandle) -> Result<()> {
    let root = std::env::temp_dir().join(format!("review-workflow-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("temp"))?;
    let db = root.join("review.sqlite");
    murmela_lib::storage::initialize_database(&db)?;
    let conn = rusqlite::Connection::open(&db)?;
    conn.execute_batch("UPDATE settings SET insert_behavior='clipboard_only', file_diarization_enabled=1, sounds_enabled=0 WHERE id=1;")?;
    let models = PathBuf::from(std::env::var("MURMELA_SMOKE_MODELS")?);
    assert!(
        murmela_lib::model_downloads::installed_diarization_package_path(&models).is_some(),
        "Set MURMELA_SMOKE_MODELS to an installed speaker package's models directory"
    );
    let store = review::ReviewStore::new(db.clone());
    let queue = review_jobs::ProcessingQueue::default();
    let files = file_jobs::FileTranscriptionController::new(
        app.clone(),
        Arc::new(FixtureEngine),
        models.clone(),
        db.clone(),
        store.clone(),
        queue.clone(),
    );
    let retries =
        review_jobs::ReviewJobController::new(app, store.clone(), models, root.join("temp"), queue);
    let audio = root.join("original.wav");
    let mut wav = hound::WavWriter::create(
        &audio,
        hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )?;
    for i in 0..32000 {
        wav.write_sample(
            ((i as f64 / 16000. * 440. * std::f64::consts::TAU).sin() * 10000.) as i16,
        )?;
    }
    wav.finalize()?;
    let source = audio_files::selected_source_file_from_path(audio)?;
    let request = audio_files::FileTranscriptionRequest {
        job_id: "initial-cancel".into(),
        source_file: source.clone(),
        speaker_count_hint: Some(2),
    };
    files.start(request.clone());
    files.start(request);
    assert_eq!(files.statuses().len(), 1);
    let early = wait(|| {
        files.statuses().into_iter().find(|s| {
            matches!(s.stage, file_jobs::FileTranscriptionJobStage::Diarizing)
                && s.review_ref.is_some()
        })
    });
    assert!(early.result.is_none(), "Events must not contain full text");
    let reference = early.review_ref.unwrap();
    let document = store.get(&reference)?;
    assert_eq!(document.detail.summary.plain_text, "First. Second.");
    assert!(retries
        .start(reference.clone(), Some(2), false, None)
        .is_err());
    let assigned = store.edit(
        &reference,
        document.revision,
        review::ReviewEdit::Assign {
            segment_ids: vec![document.detail.segments[0].id.clone()],
            speaker_ids: vec![],
            new_speaker_name: Some("Maya".into()),
        },
    )?;
    let manual_id = assigned.detail.segments[0].speaker_id.clone().unwrap();
    let start = Instant::now();
    files.cancel("initial-cancel")?;
    assert!(start.elapsed() < Duration::from_secs(1));
    wait(|| {
        files
            .statuses()
            .into_iter()
            .find(|s| matches!(s.stage, file_jobs::FileTranscriptionJobStage::Completed))
    });
    let kept = store.get(&reference)?;
    assert_eq!(
        kept.detail.summary.diarization_status,
        diarization::DiarizationStatus::Canceled
    );
    assert_eq!(
        kept.detail.segments[0].speaker_id.as_deref(),
        Some(manual_id.as_str())
    );
    let count: i64 = conn.query_row("SELECT count(*) FROM transcripts", [], |r| r.get(0))?;
    assert_eq!(count, 1);
    println!("early text, duplicate file start, corrections during initial inference, stop preserving text: passed");
    retries.start(
        reference.clone(),
        Some(2),
        false,
        Some("successful-retry".into()),
    )?;
    assert!(retries
        .start(reference.clone(), Some(2), false, None)
        .is_err());
    let current = store.get(&reference)?;
    store.edit(
        &reference,
        current.revision,
        review::ReviewEdit::Rename {
            speaker_id: manual_id.clone(),
            name: "Maya Chen".into(),
        },
    )?;
    let finished = wait(|| {
        retries
            .statuses()
            .into_iter()
            .find(|j| j.job_id == "successful-retry" && !j.active())
    });
    assert_eq!(finished.stage, "completed", "{:?}", finished.error);
    let current = store.get(&reference)?;
    assert_eq!(
        current.detail.segments[0].speaker_id.as_deref(),
        Some(manual_id.as_str())
    );
    assert!(current
        .detail
        .speakers
        .iter()
        .any(|s| s.display_name == "Maya Chen"));
    println!("duplicate retry prevention and edits during successful rerun: passed");
    for id in ["empty-retry", "failed-retry", "canceled-retry"] {
        let before = store.get(&reference)?;
        retries.start(reference.clone(), Some(2), true, Some(id.into()))?;
        if id == "canceled-retry" {
            let start = Instant::now();
            retries.cancel(id)?;
            println!(
                "retry cancellation acknowledgement_ms={:.3}",
                start.elapsed().as_secs_f64() * 1000.
            );
            assert!(start.elapsed() < Duration::from_secs(1));
        }
        wait(|| {
            retries
                .statuses()
                .into_iter()
                .find(|j| j.job_id == id && !j.active())
        });
        let after = store.get(&reference)?;
        assert_eq!(serde_json::to_value(before)?, serde_json::to_value(after)?);
    }
    println!(
        "empty, failed and canceled retries preserve results and ignore requested reset: passed"
    );
    conn.execute("UPDATE settings SET save_history=0", [])?;
    files.start(audio_files::FileTranscriptionRequest {
        job_id: "session-empty".into(),
        source_file: source,
        speaker_count_hint: Some(2),
    });
    let session = wait(|| {
        files.statuses().into_iter().find(|s| {
            s.job_id == "session-empty"
                && matches!(s.stage, file_jobs::FileTranscriptionJobStage::Completed)
        })
    });
    let session_ref = session.review_ref.unwrap();
    assert!(matches!(session_ref, review::ReviewRef::Session { .. }));
    let empty = store.get(&session_ref)?;
    assert!(empty.detail.speakers.is_empty());
    assert!(empty
        .detail
        .segments
        .iter()
        .all(|s| s.speaker_id.is_none() && s.speaker_ids.is_none()));
    files.dismiss("session-empty")?;
    assert!(store.get(&session_ref).is_err());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM transcripts", [], |r| r
            .get::<_, i64>(0))?,
        1
    );
    println!("unsaved session, initial no-speech consistency and dismissal: passed");
    drop(conn);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
fn main() {
    if worker() {
        return;
    }
    let mut context = tauri::generate_context!();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .setup(|app| {
            let app = app.handle().clone();
            std::thread::spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    workflow(app.clone())
                }));
                let success = matches!(outcome, Ok(Ok(())));
                if !success {
                    eprintln!("Workflow smoke failed: {outcome:?}");
                }
                app.exit(if success { 0 } else { 1 });
            });
            Ok(())
        })
        .run(context)
        .expect("Tauri runtime");
}
