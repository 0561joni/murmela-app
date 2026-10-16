use super::*;

/// A scripted stand-in for `murmela-fluid-worker`. It logs every request type
/// to `log`, answers in protocol order, and grows the preview by one word per
/// audio frame. `mode` changes one behaviour per test.
const FAKE_WORKER: &str = r#"#!/usr/bin/env python3
import json, re, sys
MODE, LOG = sys.argv[0].rsplit("/", 1)[0] + "/mode", sys.argv[0].rsplit("/", 1)[0] + "/log"
mode = open(MODE).read().strip()
words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"]
frames, chunk = 0, 0
for line in sys.stdin:
    request = json.loads(line)
    kind = request["type"]
    with open(LOG, "a") as log:
        log.write(kind + "\n")
    reply = dict(version=1, sessionId=request["sessionId"], sequence=request["sequence"] + 1,
                 code="", rssBytes=1, peakRssBytes=1)
    if mode == "stale":
        stale = dict(reply, sessionId="abandoned", type="final", finalText="old text")
        print(json.dumps(stale), flush=True)
    if mode == "unordered" and kind == "audio":
        reply["sequence"] += 1
    if kind == "load":
        chunk = int(re.search(r"(\d+)ms$", request["nemotronPath"]).group(1))
        reply.update(type="ready", loadMs=5, chunkMs=chunk)
    elif kind == "warmup":
        reply.update(type="warm", warmupMs=1)
    elif kind == "ping":
        reply.update(type="pong", loaded=True)
    elif kind == "unload":
        reply.update(type="unloaded")
    elif kind == "start":
        frames = 0
        reply.update(type="started", language="auto")
    elif kind == "audio":
        frames += 1
        ack = request["startSample"] + len(request["samples"])
        committed = " ".join(words[:frames - 1])
        if mode == "rewrite" and frames == 3:
            committed = "rewritten"
        reply.update(type="progress", committedText=committed, tentativeText=" " + words[frames - 1],
                     ackSample=ack, streamError="")
    elif kind == "finish":
        final_error = "FLUID_FINAL_FAILED" if mode == "final_fails" else ""
        reply.update(type="final", streamText="alpha beta stream", language="de-DE",
                     finalText="" if final_error else "Alpha, beta final.", streamError="",
                     finalError=final_error, ackSample=request["expectedSamples"],
                     timings=dict(audioMs=1, flushMs=1, finalMs=1))
    elif kind in ("cancel", "reset"):
        reply.update(type="canceled")
    print(json.dumps(reply), flush=True)
"#;

struct Fake {
    dir: PathBuf,
}
impl Fake {
    fn new(mode: &str) -> Option<Self> {
        use std::os::unix::fs::PermissionsExt;
        if Command::new("python3").arg("-V").output().is_err() {
            eprintln!("python3 unavailable; skipping live-pair fake-worker test");
            return None;
        }
        let dir = std::env::temp_dir().join(format!("live-pair-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("worker");
        std::fs::write(&script, FAKE_WORKER).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let fake = Self { dir };
        fake.set_mode(mode);
        Some(fake)
    }
    fn set_mode(&self, mode: &str) {
        std::fs::write(self.dir.join("mode"), mode).unwrap();
    }
    fn config(&self, chunk_ms: u32) -> Config {
        Config {
            helper: self.dir.join("worker"),
            nemotron: self.dir.join(format!("nemotron/latin/{chunk_ms}ms")),
            parakeet: self.dir.join("parakeet"),
            chunk_ms,
            keep_loaded: true,
        }
    }
    fn requests(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn count(&self, kind: &str) -> usize {
        self.requests().iter().filter(|line| *line == kind).count()
    }
}
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn pair() -> (LivePair, Arc<Mutex<Vec<LivePairState>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let record = seen.clone();
    let pair = LivePair::unsupervised(move |status| lock(&record).push(status.state));
    (pair, seen)
}

fn session(samples: usize) -> LiveSession {
    let (_tx, rx) = mpsc::channel();
    let session = LiveSession {
        id: "dictation".into(),
        recording_id: "audio".into(),
        state: Default::default(),
        result: Arc::new(Mutex::new(rx)),
        cancelled: Default::default(),
        armed: Arc::new(AtomicBool::new(true)),
    };
    {
        let mut state = lock(&session.state);
        state.samples = vec![0.01; samples];
        state.finished = true;
    }
    session
}

type Overlay = Arc<Mutex<Vec<(StreamingState, Option<StreamingText>)>>>;
type Outcome = Result<(String, Option<String>, Option<String>)>;
fn run(pair: &LivePair, config: &Config, live: &LiveSession) -> (Outcome, Overlay) {
    let overlay: Overlay = Default::default();
    let record = overlay.clone();
    let ui = move |state: StreamingState, text: Option<StreamingText>, _lag: u64| {
        lock(&record).push((state, text));
    };
    let mut lease = None;
    let mut sequence = 0;
    let result = stream(live, pair, config, &ui, "auto", &mut lease, &mut sequence);
    if let Some(lease) = lease.as_mut() {
        lease.cancel(&live.id, sequence);
    }
    (result, overlay)
}

fn child_pid(pair: &LivePair) -> Option<u32> {
    lock(&pair.inner.slot)
        .process
        .as_ref()
        .map(|process| process.child.id())
}

#[test]
fn protocol_skips_stale_sessions_and_rejects_unordered_responses() {
    let Some(fake) = Fake::new("stale") else { return };
    let mut process = Process::launch(&fake.config(560).helper).unwrap();
    // A response for an abandoned session arrives first and is ignored.
    let ready = process
        .exchange(
            "load",
            "ctl",
            0,
            json!({"nemotronPath": "/m/560ms", "parakeetPath": "/p"}),
            Duration::from_secs(5),
            &|| Ok(()),
        )
        .unwrap();
    assert_eq!((ready.kind.as_str(), ready.chunk_ms), ("ready", 560));

    fake.set_mode("unordered");
    let mut process = Process::launch(&fake.config(560).helper).unwrap();
    process
        .exchange("start", "s", 0, json!({}), Duration::from_secs(5), &|| Ok(()))
        .unwrap();
    let error = process
        .exchange(
            "audio",
            "s",
            1,
            json!({"startSample": 0, "samples": [0.0]}),
            Duration::from_secs(5),
            &|| Ok(()),
        )
        .unwrap_err();
    assert!(error.to_string().starts_with("LIVE_PAIR_PROTOCOL"), "{error}");
    assert!(is_fatal(&error));
}

#[test]
fn error_codes_are_sanitised_and_wire_errors_surface() {
    assert_eq!(wire_code("FLUID_FINAL_FAILED"), "FLUID_FINAL_FAILED");
    assert_eq!(wire_code("private transcript"), "LIVE_PAIR_RUNTIME_ERROR");
    assert_eq!(wire_code(""), "LIVE_PAIR_RUNTIME_ERROR");
}

#[test]
fn a_session_streams_a_growing_preview_and_pastes_the_final_text() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, _) = pair();
    let config = fake.config(560);
    let live = session(8960 * 3 + 100);
    let (result, overlay) = run(&pair, &config, &live);
    let (text, language, warning) = result.unwrap();
    assert_eq!(text, "Alpha, beta final.");
    assert_eq!(language.as_deref(), Some("de-DE"));
    assert!(warning.is_none());
    assert_eq!(lock(&live.state).processed, 8960 * 3 + 100);
    let overlay = lock(&overlay);
    let previews: Vec<_> = overlay
        .iter()
        .filter_map(|(_, text)| text.as_ref().map(|text| text.committed.clone()))
        .collect();
    assert_eq!(previews, ["", "alpha", "alpha beta", "alpha beta gamma"]);
    assert_eq!(overlay.last().unwrap().0, StreamingState::Finalizing);
    // Loaded once on demand, then only session requests.
    assert_eq!(
        fake.requests(),
        ["load", "warmup", "start", "audio", "audio", "audio", "audio", "finish"]
    );
    // The models stay resident for the next press: no second load.
    let (result, _) = run(&pair, &config, &session(100));
    result.unwrap();
    assert_eq!(fake.count("load"), 1);
    assert_eq!(pair.status().state, LivePairState::Ready);
}

#[test]
fn finish_falls_back_to_stream_text_when_the_final_pass_fails() {
    let Some(fake) = Fake::new("final_fails") else { return };
    let (pair, _) = pair();
    let (result, _) = run(&pair, &fake.config(560), &session(9000));
    let (text, _, warning) = result.unwrap();
    assert_eq!(text, "alpha beta stream");
    let warning = warning.unwrap();
    assert!(warning.starts_with("FLUID_FINAL_FAILED"), "{warning}");
    let transcript = transcript("id", text, 600, None, Some(warning));
    assert_eq!(transcript.warnings.len(), 1);
    assert_eq!(transcript.model_name, MODEL_NAME);
}

#[test]
fn a_rewritten_preview_prefix_is_rejected() {
    let Some(fake) = Fake::new("rewrite") else { return };
    let (pair, _) = pair();
    let (result, _) = run(&pair, &fake.config(560), &session(8960 * 4));
    let error = result.unwrap_err();
    assert!(error.to_string().starts_with("LIVE_PAIR_PROTOCOL"), "{error}");
    // The unfinished session is dropped in the helper, which stays usable.
    assert_eq!(fake.requests().last().map(String::as_str), Some("cancel"));
    let (result, _) = run(&pair, &fake.config(560), &session(100));
    result.unwrap();
    assert_eq!(fake.count("load"), 1);
}

#[test]
fn cancel_before_paste_discards_the_result_and_drops_the_session() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, _) = pair();
    let live = session(8960 * 2);
    live.cancel();
    let (result, _) = run(&pair, &fake.config(560), &live);
    assert!(canceled(&result.unwrap_err()));
    // Canceled before the lease: nothing reached the helper.
    assert_eq!(fake.count("finish"), 0);

    // A completed session canceled afterwards never reaches the caller.
    let (tx, rx) = mpsc::channel();
    let live = LiveSession {
        result: Arc::new(Mutex::new(rx)),
        ..session(10)
    };
    tx.send(Ok(Completion {
        text: "late".into(),
        language: None,
        warning: None,
        _work: crate::shutdown::begin_work(true).unwrap(),
    }))
    .unwrap();
    live.cancel();
    assert!(live.finish("audio", 10).is_err());
    assert!(live.finish("another-recording", 10).is_err());
}

#[test]
fn a_crashed_helper_is_restarted_with_backoff() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, seen) = pair();
    let config = fake.config(560);
    pair.configure(Some(config.clone()));
    let start = Instant::now();
    tick(&pair.inner, false, start);
    assert_eq!(pair.status().state, LivePairState::Ready);
    let first = child_pid(&pair).unwrap();
    unsafe { libc::kill(first as i32, libc::SIGKILL) };
    std::thread::sleep(Duration::from_millis(200));
    tick(&pair.inner, false, start);
    let second = child_pid(&pair).unwrap();
    assert_ne!(first, second);
    assert_eq!(fake.count("load"), 2);
    assert_eq!(pair.status().state, LivePairState::Ready);

    // A helper that cannot start backs off 1 s, 5 s, 30 s, then gives up.
    let broken = Config {
        helper: fake.dir.join("missing-helper"),
        ..config
    };
    pair.configure(Some(broken));
    let mut now = Instant::now();
    for delay in [0, 1, 5, 30] {
        now += Duration::from_secs(delay);
        tick(&pair.inner, false, now);
        // Retrying before the delay elapses does nothing.
        tick(&pair.inner, false, now + Duration::from_millis(500));
    }
    assert_eq!(lock(&pair.inner.control).failures, 4);
    assert!(lock(&pair.inner.control).gave_up);
    assert_eq!(pair.status().state, LivePairState::Error);
    tick(&pair.inner, false, now + Duration::from_secs(3600));
    assert_eq!(lock(&pair.inner.control).failures, 4);
    assert!(lock(&seen).contains(&LivePairState::Preparing));
}

#[test]
fn critical_memory_pressure_unloads_and_the_next_press_reloads() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, _) = pair();
    let config = fake.config(560);
    pair.configure(Some(config.clone()));
    let now = Instant::now();
    tick(&pair.inner, false, now);
    // Warning keeps the models resident.
    pair.memory_pressure(0x2);
    tick(&pair.inner, false, now);
    assert_eq!(fake.count("unload"), 0);
    pair.memory_pressure(0x4);
    tick(&pair.inner, false, now);
    assert_eq!(fake.count("unload"), 1);
    assert_eq!(pair.status().state, LivePairState::Unloaded);
    // The process stays; only the models were freed.
    assert!(child_pid(&pair).is_some());
    let (result, _) = run(&pair, &config, &session(100));
    result.unwrap();
    assert_eq!(fake.count("load"), 2);
    assert_eq!(pair.status().state, LivePairState::Ready);

    // Without a press, models return 60 s after pressure is normal again.
    pair.memory_pressure(0x4);
    tick(&pair.inner, false, now);
    pair.memory_pressure(0x1);
    let cleared = Instant::now();
    tick(&pair.inner, false, cleared + Duration::from_secs(30));
    assert_eq!(fake.count("load"), 2);
    tick(&pair.inner, false, cleared + Duration::from_secs(61));
    tick(&pair.inner, false, cleared + Duration::from_secs(62));
    assert_eq!(fake.count("load"), 3);
}

#[test]
fn setting_changes_reload_and_deselection_stops_the_helper() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, _) = pair();
    pair.configure(Some(fake.config(560)));
    let now = Instant::now();
    tick(&pair.inner, false, now);
    let pid = child_pid(&pair).unwrap();
    // Same models: nothing reloads.
    pair.configure(Some(fake.config(560)));
    tick(&pair.inner, false, now);
    assert_eq!(fake.count("load"), 1);
    // Another chunk size loads other models into the same helper.
    pair.configure(Some(fake.config(1120)));
    tick(&pair.inner, false, now);
    assert_eq!(fake.count("load"), 2);
    assert_eq!(child_pid(&pair), Some(pid));
    // Keep-loaded off: the idle helper exits after a minute.
    pair.configure(Some(Config {
        keep_loaded: false,
        ..fake.config(1120)
    }));
    tick(&pair.inner, false, Instant::now());
    assert!(child_pid(&pair).is_some());
    tick(&pair.inner, false, Instant::now() + IDLE_EXIT + Duration::from_secs(1));
    assert!(child_pid(&pair).is_none());
    // Deselecting the live pair stops it.
    pair.configure(Some(fake.config(560)));
    tick(&pair.inner, false, Instant::now());
    assert!(child_pid(&pair).is_some());
    pair.configure(None);
    tick(&pair.inner, false, Instant::now());
    assert!(child_pid(&pair).is_none());
    assert_eq!(pair.status().state, LivePairState::Off);
}

#[test]
fn wake_from_sleep_pings_and_a_busy_helper_is_left_alone() {
    let Some(fake) = Fake::new("normal") else { return };
    let (pair, _) = pair();
    pair.configure(Some(fake.config(560)));
    tick(&pair.inner, false, Instant::now());
    tick(&pair.inner, true, Instant::now());
    assert_eq!(fake.count("ping"), 1);
    lock(&pair.inner.slot).in_session = true;
    pair.memory_pressure(0x4);
    tick(&pair.inner, true, Instant::now());
    assert_eq!((fake.count("ping"), fake.count("unload")), (1, 0));
    lock(&pair.inner.slot).in_session = false;
}

#[test]
fn language_mapping_and_setting_defaults() {
    use crate::settings::LanguageMode;
    let mut settings: crate::settings::AppSettings = serde_json::from_value(json!({
        "defaultMode": "quick_dictate", "shortcut": "CmdOrCtrl+Shift+Space",
        "translationEnabled": false, "translationCycleShortcut": "CmdOrCtrl+Shift+Right",
        "translationModelId": "", "shortcutMode": "push_to_talk", "languageMode": "auto",
        "fixedLanguage": null, "preferredInputDevice": null, "insertBehavior": "paste",
        "launchAtLoginEnabled": false, "gpuEnabled": true,
        "shortcutDictationModelProfile": "fast", "shortcutDictationSelectedModelId": MODEL_ID,
        "quickDictateModelProfile": "fast", "quickDictateSelectedModelId": null,
        "fileTranscribeModelProfile": "fast", "fileTranscribeSelectedModelId": null,
        "saveHistory": true, "soundsEnabled": true, "volumeDuckingEnabled": true,
        "fileDiarizationEnabled": false
    }))
    .unwrap();
    // Older stored settings deserialize with the live-pair defaults.
    assert_eq!(settings.live_pair_chunk_ms, 560);
    assert!(settings.live_pair_keep_loaded);
    settings.language_mode = LanguageMode::Auto;
    assert_eq!(source_language(&settings).unwrap(), "auto");
    settings.language_mode = LanguageMode::Fixed;
    settings.fixed_language = Some("German".into());
    assert_eq!(source_language(&settings).unwrap(), "de");
    settings.fixed_language = Some("ja".into());
    assert!(source_language(&settings).is_err());
}

#[test]
fn artifacts_match_the_pinned_manifest() {
    let manifest: Value =
        serde_json::from_str(include_str!("../../../workers/fluid/manifest.json")).unwrap();
    assert_eq!(manifest["defaultChunkMs"], 560);
    assert!(artifacts().iter().all(|artifact| artifact.sha256.len() == 64
        && artifact.url.contains("/resolve/")
        && (artifact.path.starts_with("nemotron/latin/") || artifact.path.starts_with("parakeet/"))));
    for chunk in crate::settings::LIVE_PAIR_CHUNKS_MS {
        assert!(artifacts()
            .iter()
            .any(|artifact| artifact.path == format!("nemotron/latin/{chunk}ms/metadata.json")));
    }
    assert!(artifacts()
        .iter()
        .any(|artifact| artifact.path == "parakeet/Encoder_v2.mlmodelc/weights/weight.bin"));
    assert_eq!(total_bytes(), artifacts().iter().map(|a| a.bytes).sum::<i64>());
}

#[test]
#[ignore = "Requires the pinned live-pair models, the built helper and the Apple Neural Engine"]
fn real_worker_cold_and_warm() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let models = std::env::var_os("MURMELA_FLUID_MODELS")
        .map(PathBuf::from)
        .unwrap_or_else(|| root.join("target/fluid-models"));
    let config = Config {
        helper: root.join("bundle/fluid/murmela-fluid-worker"),
        nemotron: models.join("nemotron/latin/560ms"),
        parakeet: models.join("parakeet"),
        chunk_ms: 560,
        keep_loaded: true,
    };
    let audio = crate::audio_preprocess::decode_audio_file(
        &root.join("target/speech-fixtures/en-01.wav"),
    )
    .unwrap();
    let (pair, _) = pair();
    let mut pid = None;
    for label in ["cold", "warm"] {
        let live = session(0);
        lock(&live.state).samples = audio.samples.clone();
        let began = Instant::now();
        let (result, overlay) = run(&pair, &config, &live);
        let (text, _, warning) = result.unwrap();
        eprintln!("{label}: {:?} in {:?}", text, began.elapsed());
        assert!(warning.is_none());
        assert!(text.to_lowercase().contains("invoice"), "{text}");
        assert!(lock(&overlay)
            .iter()
            .any(|(_, preview)| preview.as_ref().is_some_and(|p| !p.committed.is_empty())));
        let current = child_pid(&pair);
        assert!(pid.is_none() || pid == current);
        pid = current;
    }
}
