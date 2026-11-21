//! PipeWire capture of the default sink monitor (all desktop audio).
//!
//! A stream with `stream.capture.sink=true` and no explicit target is linked by the session
//! manager to the default sink's monitor and re-linked automatically on default-sink changes.

use anyhow::Result;
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::format_utils;
use spa::pod::Pod;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub struct CaptureConfig {
    pub rate: u32,
    pub channels: u32,
    pub quantum: u32,
    pub target: Option<String>,
}

#[derive(Default)]
pub struct CaptureStats {
    pub overruns: AtomicU64,
    pub samples: AtomicU64,
    pub restarts: AtomicU64,
}

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    prod: rtrb::Producer<f32>,
    stats: Arc<CaptureStats>,
}

/// Run capture forever (reconnecting with backoff). Blocks the calling thread.
/// Each (re)connection creates a fresh ring buffer and sends its consumer on `tx`.
pub fn run(cfg: CaptureConfig, tx: std::sync::mpsc::Sender<rtrb::Consumer<f32>>, stats: Arc<CaptureStats>, stop: Arc<AtomicBool>) {
    pw::init();
    let mut backoff = Duration::from_millis(250);
    while !stop.load(Ordering::Relaxed) {
        let started = std::time::Instant::now();
        let (prod, cons) = rtrb::RingBuffer::new(cfg.rate as usize * cfg.channels as usize / 2); // 500 ms
        if tx.send(cons).is_err() {
            return;
        }
        if let Err(e) = run_once(&cfg, prod, stats.clone(), stop.clone()) {
            tracing::error!("pipewire capture failed: {e:#}");
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        stats.restarts.fetch_add(1, Ordering::Relaxed);
        if started.elapsed() > Duration::from_secs(10) {
            backoff = Duration::from_millis(250);
        }
        tracing::warn!("capture stopped; retrying in {backoff:?}");
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(5));
    }
}

fn run_once(cfg: &CaptureConfig, prod: rtrb::Producer<f32>, stats: Arc<CaptureStats>, stop: Arc<AtomicBool>) -> Result<()> {
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let mut props = properties! {
        *pw::keys::MEDIA_TYPE => "Audio",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Music",
        *pw::keys::NODE_NAME => "soundovern-capture",
        *pw::keys::NODE_LATENCY => format!("{}/{}", cfg.quantum, cfg.rate),
        *pw::keys::STREAM_CAPTURE_SINK => "true",
    };
    if let Some(t) = &cfg.target {
        props.insert("target.object", t.clone());
    }

    let stream = pw::stream::StreamBox::new(&core, "soundovern-capture", props)?;
    let data = UserData { format: Default::default(), prod, stats };

    let ml_state = mainloop.clone();
    let ml_core = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, seq, res, msg| {
            tracing::error!("pipewire core error id={id} seq={seq} res={res}: {msg}");
            if id == 0 {
                ml_core.quit(); // connection to the daemon is gone
            }
        })
        .register();

    let _listener = stream
        .add_local_listener_with_user_data(data)
        .state_changed(move |_, _, old, new| {
            tracing::info!("capture stream state {old:?} -> {new:?}");
            if let pw::stream::StreamState::Error(_) = new {
                ml_state.quit();
            }
        })
        .param_changed(|_, ud, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((mt, ms)) = format_utils::parse_format(param) else { return };
            if mt != MediaType::Audio || ms != MediaSubtype::Raw {
                return;
            }
            if ud.format.parse(param).is_ok() {
                tracing::info!("capture format: {} Hz, {} ch", ud.format.rate(), ud.format.channels());
            }
        })
        .process(|stream, ud| {
            let Some(mut buffer) = stream.dequeue_buffer() else { return };
            let datas = buffer.datas_mut();
            let Some(d) = datas.first_mut() else { return };
            let size = d.chunk().size() as usize;
            let offset = d.chunk().offset() as usize;
            let Some(bytes) = d.data() else { return };
            let end = (offset + size).min(bytes.len());
            let bytes = &bytes[offset.min(end)..end];
            let n = bytes.len() / 4;
            // push as many samples as fit; drop the rest (never block the RT thread)
            let free = ud.prod.slots();
            let take = n.min(free);
            if take < n {
                ud.stats.overruns.fetch_add(1, Ordering::Relaxed);
            }
            if let Ok(mut chunk) = ud.prod.write_chunk_uninit(take) {
                let (a, b) = chunk.as_mut_slices();
                for (i, slot) in a.iter_mut().chain(b.iter_mut()).enumerate() {
                    let v = f32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap());
                    slot.write(v);
                }
                unsafe { chunk.commit_all() };
                ud.stats.samples.fetch_add(take as u64, Ordering::Relaxed);
            }
        })
        .register()?;

    let mut info = spa::param::audio::AudioInfoRaw::new();
    info.set_format(spa::param::audio::AudioFormat::F32LE);
    info.set_rate(cfg.rate);
    info.set_channels(cfg.channels);
    let obj = pw::spa::pod::Object {
        type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: pw::spa::param::ParamType::EnumFormat.as_raw(),
        properties: info.into(),
    };
    let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow::anyhow!("pod serialize: {e:?}"))?
    .0
    .into_inner();
    let mut params = [Pod::from_bytes(&values).ok_or_else(|| anyhow::anyhow!("bad pod"))?];

    stream.connect(
        spa::utils::Direction::Input,
        None,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // Periodically check the stop flag so shutdown is prompt.
    let ml_timer = mainloop.clone();
    let stop2 = stop.clone();
    let timer = mainloop.loop_().add_timer(move |_| {
        if stop2.load(Ordering::Relaxed) {
            ml_timer.quit();
        }
    });
    timer.update_timer(Some(Duration::from_millis(200)), Some(Duration::from_millis(200))).into_result()?;

    mainloop.run();
    Ok(())
}
