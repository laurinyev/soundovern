//! Oboe (AAudio / OpenSL ES) output driven directly by the Rust engine.
//!
//! The device callback pulls PCM from [`Engine::render`], so audio never crosses into Kotlin.
//! The stream asks for LowLatency + Exclusive (MMAP where the device supports it), starts with a
//! 2-burst buffer, grows it one burst at a time on xruns, and is reopened if the route changes
//! (headphones plugged in, Bluetooth connected, device disconnected...).

use oboe::{
    AudioOutputCallback, AudioOutputStreamSafe, AudioStream, AudioStreamAsync, AudioStreamBase, AudioStreamBuilder,
    AudioStreamSafe, ContentType, DataCallbackResult, Error, Output, PerformanceMode, SampleRateConversionQuality,
    SharingMode, Stereo, Usage,
};
use soundovern_core::client::Engine;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Default)]
pub struct Info {
    latency_ms_bits: AtomicU32,
    pub buffer_frames: AtomicI32,
    pub burst_frames: AtomicI32,
    pub xruns: AtomicI32,
    pub exclusive: AtomicBool,
    pub reopens: AtomicI32,
    restart: AtomicBool,
}

impl Info {
    pub fn latency_ms(&self) -> f32 {
        f32::from_bits(self.latency_ms_bits.load(Ordering::Relaxed))
    }
}

struct Callback {
    engine: Arc<Engine>,
    info: Arc<Info>,
    scratch: Vec<f32>,
    calls: u32,
    last_xruns: i32,
}

impl AudioOutputCallback for Callback {
    type FrameType = (f32, Stereo);

    fn on_audio_ready(&mut self, stream: &mut dyn AudioOutputStreamSafe, frames: &mut [(f32, f32)]) -> DataCallbackResult {
        let n = frames.len();
        self.scratch.resize(n * 2, 0.0); // allocation only when the callback size grows
        self.engine.render(&mut self.scratch);
        for (f, s) in frames.iter_mut().zip(self.scratch.chunks_exact(2)) {
            *f = (s[0], s[1]);
        }
        // Tuning, ~every 50 callbacks: grow the device buffer by one burst after new xruns.
        self.calls = self.calls.wrapping_add(1);
        if self.calls % 50 == 0 {
            if let Ok(x) = stream.get_xrun_count() {
                self.info.xruns.store(x, Ordering::Relaxed);
                if x > self.last_xruns {
                    self.last_xruns = x;
                    let burst = stream.get_frames_per_burst().max(1);
                    let cap = stream.get_buffer_capacity_in_frames();
                    let cur = stream.get_buffer_size_in_frames();
                    if cur < cap {
                        let _ = stream.set_buffer_size_in_frames((cur + burst).min(cap));
                    }
                }
            }
            self.info.buffer_frames.store(stream.get_buffer_size_in_frames(), Ordering::Relaxed);
        }
        DataCallbackResult::Continue
    }

    fn on_error_after_close(&mut self, _s: &mut dyn AudioOutputStreamSafe, err: Error) {
        tracing_log(&format!("oboe stream closed by system: {err:?}; will reopen"));
        self.info.restart.store(true, Ordering::Relaxed);
    }
}

fn tracing_log(msg: &str) {
    // The core crate logs through `tracing`, which has no subscriber on Android; use stderr (-> logcat).
    eprintln!("soundovern: {msg}");
}

type Stream = AudioStreamAsync<Output, Callback>;

fn open(engine: &Arc<Engine>, info: &Arc<Info>) -> Result<Stream, Error> {
    let cb = Callback { engine: engine.clone(), info: info.clone(), scratch: Vec::new(), calls: 0, last_xruns: 0 };
    let mut stream = AudioStreamBuilder::default()
        .set_performance_mode(PerformanceMode::LowLatency)
        .set_sharing_mode(SharingMode::Exclusive) // falls back to Shared if the device can't
        .set_usage(Usage::Media)
        .set_content_type(ContentType::Music)
        .set_format::<f32>()
        .set_channel_count::<Stereo>()
        .set_sample_rate(engine.params().sample_rate as i32)
        .set_sample_rate_conversion_quality(SampleRateConversionQuality::Medium)
        .set_callback(cb)
        .open_stream()?;
    let burst = stream.get_frames_per_burst().max(1);
    let cap = stream.get_buffer_capacity_in_frames();
    let initial = (burst * 2).min(cap.max(burst));
    let _ = stream.set_buffer_size_in_frames(initial);
    info.burst_frames.store(burst, Ordering::Relaxed);
    info.buffer_frames.store(stream.get_buffer_size_in_frames(), Ordering::Relaxed);
    info.exclusive.store(stream.get_sharing_mode() == SharingMode::Exclusive, Ordering::Relaxed);
    info.restart.store(false, Ordering::Relaxed);
    stream.start()?;
    tracing_log(&format!(
        "oboe open: burst={burst} buffer={} cap={cap} sharing={:?} perf={:?} rate={}",
        stream.get_buffer_size_in_frames(),
        stream.get_sharing_mode(),
        stream.get_performance_mode(),
        stream.get_sample_rate()
    ));
    Ok(stream)
}

pub struct AudioOut {
    pub info: Arc<Info>,
    stop: Arc<AtomicBool>,
    supervisor: Option<std::thread::JoinHandle<()>>,
}

impl AudioOut {
    pub fn start(engine: Arc<Engine>) -> Result<AudioOut, String> {
        let info = Arc::new(Info::default());
        let first = open(&engine, &info).map_err(|e| format!("audio output: {e:?}"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let stream = Arc::new(Mutex::new(Some(first)));
        let (info2, stop2) = (info.clone(), stop.clone());
        let supervisor = std::thread::Builder::new()
            .name("sn-audio-sup".into())
            .spawn(move || {
                let mut tick = 0u32;
                while !stop2.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(250));
                    tick += 1;
                    let mut g = stream.lock().unwrap();
                    if info2.restart.load(Ordering::Relaxed) {
                        *g = None; // drop old stream first
                        match open(&engine, &info2) {
                            Ok(s) => {
                                info2.reopens.fetch_add(1, Ordering::Relaxed);
                                *g = Some(s);
                            }
                            Err(e) => tracing_log(&format!("reopen failed: {e:?}")),
                        }
                    } else if tick % 4 == 0 {
                        if let Some(s) = g.as_mut() {
                            if let Ok(l) = s.calculate_latency_millis() {
                                info2.latency_ms_bits.store((l as f32).to_bits(), Ordering::Relaxed);
                            }
                        }
                    }
                }
                *stream.lock().unwrap() = None; // closes the stream
            })
            .map_err(|e| e.to_string())?;
        Ok(AudioOut { info, stop, supervisor: Some(supervisor) })
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.supervisor.take() {
            let _ = h.join();
        }
    }
}
