//! Records one audio channel of a call straight to disk as 16 kHz mono PCM.
//!
//! Calls run for hours, so unlike dictation nothing is kept in memory: each
//! block from the device is mixed down, resampled and written as it arrives.
//! Both channels follow the wall clock from the moment recording started.
//! Gaps (a headset reconnecting, a device sending nothing while the call is
//! silent) are filled with silence and runaway clocks are trimmed, so a
//! passage at 12:03 on the microphone and 12:03 on the call's sound happened
//! at the same moment.

use std::any::Any;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use hound::{SampleFormat as WavFormat, WavSpec, WavWriter};

pub const RATE_HZ: u32 = 16_000;
/// How far a channel may run ahead of or behind the wall clock before it is corrected.
const CLOCK_SLACK_SAMPLES: u64 = RATE_HZ as u64 / 2;
const RECONNECT_INTERVAL: Duration = Duration::from_secs(1);

/// Audio as a device delivered it.
pub struct Block {
    pub at: Instant,
    pub samples: Vec<f32>,
    pub rate: u32,
    pub channels: u16,
}

enum Message {
    Block(Block),
    Finish(Instant),
}

/// Where a capture sends its blocks, its level and its errors.
#[derive(Clone)]
pub struct BlockSink {
    sender: Sender<Message>,
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
}

impl BlockSink {
    pub fn send(&self, samples: Vec<f32>, rate: u32, channels: u16) {
        let mut peak = 0.0_f32;
        let mut sum = 0.0_f32;
        for sample in &samples {
            let magnitude = sample.abs().min(1.0);
            peak = peak.max(magnitude);
            sum += magnitude * magnitude;
        }
        let rms = if samples.is_empty() {
            0.0
        } else {
            (sum / samples.len() as f32).sqrt()
        };
        let envelope = (rms * 5.4).max(peak * 1.9).clamp(0.0, 1.0);
        let previous = f32::from_bits(self.level.load(Ordering::Relaxed));
        let next = if envelope >= previous {
            previous * 0.2 + envelope * 0.8
        } else {
            previous * 0.7 + envelope * 0.3
        };
        self.level.store(next.to_bits(), Ordering::Relaxed);
        let _ = self.sender.send(Message::Block(Block {
            at: Instant::now(),
            samples,
            rate,
            channels,
        }));
    }

    pub fn fail(&self, message: impl Into<String>) {
        if let Ok(mut error) = self.error.lock() {
            *error = Some(message.into());
        }
    }
}

/// Something that can deliver audio blocks: a microphone, the system's sound, a test signal.
pub trait AudioSource: Send + 'static {
    /// Starts delivering blocks; delivery stops when the returned value is dropped.
    fn open(&mut self, sink: BlockSink) -> Result<Box<dyn Any>>;
    /// Shown while recording, e.g. the microphone's name.
    fn describe(&self) -> String;
}

#[derive(Debug, Clone)]
pub struct ChannelSummary {
    pub path: PathBuf,
    pub duration_ms: i64,
    pub peak: f32,
    /// The last device error, if the channel ever lost its device.
    pub error: Option<String>,
}

impl ChannelSummary {
    /// True when the channel holds nothing but digital silence or near it.
    pub fn is_silent(&self) -> bool {
        self.peak < 0.003
    }
}

/// One recording channel: a source thread that keeps the device open (and reopens
/// it after errors) and a writer thread that turns blocks into the WAV file.
pub struct ChannelRecorder {
    sender: Sender<Message>,
    stop: Arc<AtomicBool>,
    level: Arc<AtomicU32>,
    error: Arc<Mutex<Option<String>>>,
    label: Arc<Mutex<String>>,
    source_thread: Option<JoinHandle<()>>,
    writer_thread: Option<JoinHandle<Result<ChannelSummary>>>,
}

impl ChannelRecorder {
    /// Opens the source and starts writing to `path`. Fails if the source cannot open at all.
    pub fn start(mut source: Box<dyn AudioSource>, path: &Path, started: Instant) -> Result<Self> {
        let (sender, receiver) = mpsc::channel();
        let level = Arc::new(AtomicU32::new(0));
        let error = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = BlockSink {
            sender: sender.clone(),
            level: level.clone(),
            error: error.clone(),
        };
        let label = Arc::new(Mutex::new(source.describe()));
        let writer = ChannelWriter::create(path, started)?;
        let writer_thread = thread::Builder::new()
            .name("murmela-call-writer".into())
            .spawn(move || run_writer(writer, receiver))?;

        // Open once here so a missing permission or device fails the start, not silently later.
        let (opened_tx, opened_rx) = mpsc::channel::<Result<(), String>>();
        let thread_stop = stop.clone();
        let thread_label = label.clone();
        let source_thread = thread::Builder::new()
            .name("murmela-call-source".into())
            .spawn(move || {
                let mut guard = match source.open(sink.clone()) {
                    Ok(guard) => {
                        let _ = opened_tx.send(Ok(()));
                        Some(guard)
                    }
                    Err(error) => {
                        let _ = opened_tx.send(Err(format!("{error:#}")));
                        return;
                    }
                };
                let mut last_attempt = Instant::now();
                while !thread_stop.load(Ordering::SeqCst) {
                    thread::sleep(Duration::from_millis(100));
                    let failed = sink
                        .error
                        .lock()
                        .map(|error| error.is_some())
                        .unwrap_or(false);
                    if failed && guard.is_some() {
                        // The device went away (unplugged, switched): drop it and reconnect.
                        guard = None;
                    }
                    if guard.is_none() && last_attempt.elapsed() >= RECONNECT_INTERVAL {
                        last_attempt = Instant::now();
                        if let Ok(next) = source.open(sink.clone()) {
                            if let Ok(mut error) = sink.error.lock() {
                                *error = None;
                            }
                            if let Ok(mut label) = thread_label.lock() {
                                *label = source.describe();
                            }
                            guard = Some(next);
                        }
                    }
                }
                drop(guard);
            })?;
        match opened_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => {}
            Ok(Err(message)) => {
                let _ = sender.send(Message::Finish(started));
                let _ = writer_thread.join();
                let _ = std::fs::remove_file(path);
                return Err(anyhow!(message));
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                let _ = sender.send(Message::Finish(started));
                return Err(anyhow!("The audio device did not start in time."));
            }
        }
        Ok(Self {
            sender,
            stop,
            level,
            error,
            label,
            source_thread: Some(source_thread),
            writer_thread: Some(writer_thread),
        })
    }

    pub fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }

    pub fn error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|error| error.clone())
    }

    pub fn label(&self) -> String {
        self.label
            .lock()
            .map(|label| label.clone())
            .unwrap_or_default()
    }

    /// Stops the device and writes the file up to `ended`.
    pub fn finish(mut self, ended: Instant) -> Result<ChannelSummary> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.source_thread.take() {
            let _ = thread.join();
        }
        let _ = self.sender.send(Message::Finish(ended));
        let mut summary = self
            .writer_thread
            .take()
            .ok_or_else(|| anyhow!("The recording was already finished."))?
            .join()
            .map_err(|_| anyhow!("The recording writer stopped unexpectedly."))??;
        summary.error = self.error();
        Ok(summary)
    }
}

impl Drop for ChannelRecorder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Message::Finish(Instant::now()));
    }
}

fn run_writer(mut writer: ChannelWriter, receiver: Receiver<Message>) -> Result<ChannelSummary> {
    loop {
        match receiver.recv() {
            Ok(Message::Block(block)) => writer.push(&block)?,
            Ok(Message::Finish(ended)) => return writer.finish(ended),
            Err(_) => return writer.finish(Instant::now()),
        }
    }
}

/// Mixes, resamples and aligns blocks, and writes them as 16-bit 16 kHz mono.
pub struct ChannelWriter {
    path: PathBuf,
    started: Instant,
    wav: WavWriter<BufWriter<File>>,
    resampler: Option<Resampler>,
    written: u64,
    peak: f32,
}

impl ChannelWriter {
    pub fn create(path: &Path, started: Instant) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let wav = WavWriter::create(
            path,
            WavSpec {
                channels: 1,
                sample_rate: RATE_HZ,
                bits_per_sample: 16,
                sample_format: WavFormat::Int,
            },
        )
        .with_context(|| format!("could not create {}", path.display()))?;
        Ok(Self {
            path: path.to_path_buf(),
            started,
            wav,
            resampler: None,
            written: 0,
            peak: 0.0,
        })
    }

    pub fn push(&mut self, block: &Block) -> Result<()> {
        if block.rate == 0 || block.channels == 0 {
            return Ok(());
        }
        let resampler = match &mut self.resampler {
            Some(resampler)
                if resampler.rate == block.rate && resampler.channels == block.channels =>
            {
                resampler
            }
            slot => slot.insert(Resampler::new(block.rate, block.channels)),
        };
        let mut output = Vec::new();
        resampler.push(&block.samples, &mut output);
        let expected = samples_since(self.started, block.at);
        let after = self.written + output.len() as u64;
        if expected > after + CLOCK_SLACK_SAMPLES {
            // The device delivered nothing for a while: keep the timeline with silence.
            self.write_silence(expected - after)?;
        } else if after > expected + CLOCK_SLACK_SAMPLES {
            let excess = ((after - expected) as usize).min(output.len());
            output.drain(..excess);
        }
        self.write(&output)
    }

    pub fn finish(mut self, ended: Instant) -> Result<ChannelSummary> {
        let expected = samples_since(self.started, ended);
        if expected > self.written {
            self.write_silence(expected - self.written)?;
        }
        self.wav.finalize()?;
        Ok(ChannelSummary {
            path: self.path,
            duration_ms: (self.written * 1000 / RATE_HZ as u64) as i64,
            peak: self.peak,
            error: None,
        })
    }

    fn write_silence(&mut self, count: u64) -> Result<()> {
        let zeros = vec![0.0_f32; 4_096];
        let mut left = count;
        while left > 0 {
            let take = left.min(zeros.len() as u64) as usize;
            self.write(&zeros[..take])?;
            left -= take as u64;
        }
        Ok(())
    }

    fn write(&mut self, samples: &[f32]) -> Result<()> {
        for sample in samples {
            let clamped = sample.clamp(-1.0, 1.0);
            self.peak = self.peak.max(clamped.abs());
            self.wav
                .write_sample((clamped * i16::MAX as f32).round() as i16)?;
        }
        self.written += samples.len() as u64;
        Ok(())
    }
}

fn samples_since(started: Instant, at: Instant) -> u64 {
    (at.saturating_duration_since(started).as_secs_f64() * RATE_HZ as f64) as u64
}

/// Streaming mix-down and conversion to 16 kHz. Higher rates are averaged over each
/// output period, which also filters what would otherwise alias into the speech band;
/// lower rates (Bluetooth headset microphones at 8 kHz) are interpolated.
struct Resampler {
    rate: u32,
    channels: u16,
    frames: u64,
    produced: u64,
    sum: f64,
    count: u32,
    previous: f32,
}

impl Resampler {
    fn new(rate: u32, channels: u16) -> Self {
        Self {
            rate,
            channels,
            frames: 0,
            produced: 0,
            sum: 0.0,
            count: 0,
            previous: 0.0,
        }
    }

    fn push(&mut self, interleaved: &[f32], output: &mut Vec<f32>) {
        let channels = self.channels as usize;
        for frame in interleaved.chunks_exact(channels) {
            let value = frame.iter().sum::<f32>() / channels as f32;
            if self.rate >= RATE_HZ {
                // Output sample n covers input frames [n*rate/16k, (n+1)*rate/16k).
                let bin = self.frames * RATE_HZ as u64 / self.rate as u64;
                if bin > self.produced && self.count > 0 {
                    output.push((self.sum / self.count as f64) as f32);
                    self.produced += 1;
                    self.sum = 0.0;
                    self.count = 0;
                }
                self.sum += value as f64;
                self.count += 1;
            } else {
                // Emit every output sample whose position falls between the previous frame and this one.
                loop {
                    let position = self.produced as f64 * self.rate as f64 / RATE_HZ as f64;
                    if position > self.frames as f64 {
                        break;
                    }
                    let fraction = (position - (self.frames as f64 - 1.0)).clamp(0.0, 1.0) as f32;
                    let sample = if self.frames == 0 {
                        value
                    } else {
                        self.previous + (value - self.previous) * fraction
                    };
                    output.push(sample);
                    self.produced += 1;
                }
                self.previous = value;
            }
            self.frames += 1;
        }
    }
}

/// Mixes the two finished channels into the file used for playback in Review.
pub fn mix_channels(local: &Path, remote: &Path, output: &Path) -> Result<i64> {
    let mut a = hound::WavReader::open(local)?;
    let mut b = hound::WavReader::open(remote)?;
    let mut writer = WavWriter::create(
        output,
        WavSpec {
            channels: 1,
            sample_rate: RATE_HZ,
            bits_per_sample: 16,
            sample_format: WavFormat::Int,
        },
    )?;
    let mut a = a.samples::<i16>();
    let mut b = b.samples::<i16>();
    let mut count = 0_i64;
    loop {
        let (left, right) = (a.next().transpose()?, b.next().transpose()?);
        if left.is_none() && right.is_none() {
            break;
        }
        let sum = left.unwrap_or(0) as i32 + right.unwrap_or(0) as i32;
        writer.write_sample(sum.clamp(i16::MIN as i32, i16::MAX as i32) as i16)?;
        count += 1;
    }
    writer.finalize()?;
    Ok(count * 1000 / RATE_HZ as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("murmela-call-{}-{name}.wav", uuid::Uuid::new_v4()))
    }

    fn read(path: &Path) -> Vec<i16> {
        hound::WavReader::open(path)
            .unwrap()
            .samples::<i16>()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn resampler_converts_common_device_rates_to_16k_mono() {
        for (rate, channels) in [
            (48_000, 2),
            (44_100, 1),
            (16_000, 1),
            (8_000, 1),
            (24_000, 2),
        ] {
            let mut resampler = Resampler::new(rate, channels);
            let mut output = Vec::new();
            let frames = rate as usize; // one second
            let block: Vec<f32> = (0..frames * channels as usize).map(|_| 0.5).collect();
            for chunk in block.chunks(480 * channels as usize) {
                resampler.push(chunk, &mut output);
            }
            assert!(
                (output.len() as i64 - 16_000).abs() <= 2,
                "{rate} Hz × {channels} gave {} samples",
                output.len()
            );
            assert!(output.iter().all(|sample| (sample - 0.5).abs() < 1e-4));
        }
    }

    #[test]
    fn writer_fills_gaps_and_pads_to_the_end_on_the_wall_clock() {
        let started = Instant::now();
        let path = temp_path("gap");
        let mut writer = ChannelWriter::create(&path, started).unwrap();
        // 1 s of audio arrives on time, then nothing for 2 s (e.g. a device that goes quiet).
        writer
            .push(&Block {
                at: started + Duration::from_secs(1),
                samples: vec![0.25; 48_000],
                rate: 48_000,
                channels: 1,
            })
            .unwrap();
        writer
            .push(&Block {
                at: started + Duration::from_secs(4),
                samples: vec![0.25; 48_000],
                rate: 48_000,
                channels: 1,
            })
            .unwrap();
        let summary = writer.finish(started + Duration::from_secs(5)).unwrap();
        let samples = read(&path);
        assert_eq!(samples.len(), 80_000);
        assert_eq!(summary.duration_ms, 5_000);
        // The second block sits at 3–4 s, after 2 s of inserted silence.
        assert!(samples[16_000..47_000].iter().all(|sample| *sample == 0));
        assert!(samples[48_500..63_000].iter().all(|sample| *sample > 8_000));
        assert!(!summary.is_silent());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn writer_trims_a_channel_that_runs_ahead_of_the_clock() {
        let started = Instant::now();
        let path = temp_path("ahead");
        let mut writer = ChannelWriter::create(&path, started).unwrap();
        // 3 s of audio claimed at the 1 s mark.
        writer
            .push(&Block {
                at: started + Duration::from_secs(1),
                samples: vec![0.1; 48_000],
                rate: 16_000,
                channels: 1,
            })
            .unwrap();
        let summary = writer.finish(started + Duration::from_secs(1)).unwrap();
        // Trimmed back to the clock, not merely to the edge of the slack.
        assert_eq!(read(&path).len() as u64, RATE_HZ as u64);
        assert_eq!(summary.duration_ms, 1_000);
        std::fs::remove_file(path).unwrap();
    }

    struct TestTone {
        opens: Arc<AtomicU32>,
        fail_first: bool,
    }

    struct Stopper(Arc<AtomicBool>);
    impl Drop for Stopper {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    impl AudioSource for TestTone {
        fn open(&mut self, sink: BlockSink) -> Result<Box<dyn Any>> {
            let attempt = self.opens.fetch_add(1, Ordering::SeqCst);
            let stopped = Arc::new(AtomicBool::new(false));
            let flag = stopped.clone();
            let fail = self.fail_first && attempt == 0;
            thread::spawn(move || {
                for index in 0.. {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    if fail && index == 3 {
                        sink.fail("device disconnected");
                        break;
                    }
                    sink.send(vec![0.3; 480], 48_000, 1);
                    thread::sleep(Duration::from_millis(10));
                }
            });
            Ok(Box::new(Stopper(stopped)))
        }
        fn describe(&self) -> String {
            "Test tone".into()
        }
    }

    #[test]
    fn recorder_reconnects_after_a_device_error_and_keeps_the_timeline() {
        let path = temp_path("reconnect");
        let opens = Arc::new(AtomicU32::new(0));
        let started = Instant::now();
        let recorder = ChannelRecorder::start(
            Box::new(TestTone {
                opens: opens.clone(),
                fail_first: true,
            }),
            &path,
            started,
        )
        .unwrap();
        assert_eq!(recorder.label(), "Test tone");
        thread::sleep(Duration::from_millis(1_800));
        assert!(recorder.level() > 0.0);
        let summary = recorder.finish(Instant::now()).unwrap();
        assert!(opens.load(Ordering::SeqCst) >= 2, "the source was reopened");
        assert!(
            summary.error.is_none(),
            "the error clears after reconnecting"
        );
        let elapsed = started.elapsed().as_millis() as i64;
        assert!(
            (summary.duration_ms - elapsed).abs() < 300,
            "{} vs {elapsed}",
            summary.duration_ms
        );
        std::fs::remove_file(path).unwrap();
    }

    struct Broken;
    impl AudioSource for Broken {
        fn open(&mut self, _: BlockSink) -> Result<Box<dyn Any>> {
            Err(anyhow!("permission denied"))
        }
        fn describe(&self) -> String {
            "Broken".into()
        }
    }

    #[test]
    fn a_source_that_cannot_open_fails_the_start_and_leaves_no_file() {
        let path = temp_path("broken");
        let error = ChannelRecorder::start(Box::new(Broken), &path, Instant::now())
            .err()
            .expect("start fails");
        assert!(error.to_string().contains("permission denied"));
        assert!(!path.exists());
    }

    #[test]
    fn mixing_adds_both_channels_and_keeps_the_longer_length() {
        let started = Instant::now();
        let (a, b, mixed) = (temp_path("a"), temp_path("b"), temp_path("mix"));
        let mut left = ChannelWriter::create(&a, started).unwrap();
        left.write(&[0.25; 100]).unwrap();
        left.finish(started).unwrap();
        let mut right = ChannelWriter::create(&b, started).unwrap();
        right.write(&[0.5; 160]).unwrap();
        right.finish(started).unwrap();
        mix_channels(&a, &b, &mixed).unwrap();
        let samples = read(&mixed);
        assert_eq!(samples.len(), 160);
        assert!((samples[0] as f32 / i16::MAX as f32 - 0.75).abs() < 0.001);
        assert!((samples[150] as f32 / i16::MAX as f32 - 0.5).abs() < 0.001);
        for path in [a, b, mixed] {
            std::fs::remove_file(path).unwrap();
        }
    }
}
