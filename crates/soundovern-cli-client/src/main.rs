//! Desktop test receiver using the shared engine. Plays through cpal, or renders to a WAV
//! file (`--wav`) for headless / feedback-free testing.
use anyhow::{Context, Result};
use clap::Parser;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use soundovern_core::client::{ClientConfig, Engine};
use std::time::{Duration, Instant};

#[derive(Parser)]
struct Opt {
    /// Server address, host or host:port
    server: String,
    /// Pairing PIN shown by the server
    #[arg(long)]
    pin: String,
    /// Minimum jitter buffer in ms
    #[arg(long, default_value_t = 20)]
    min_ms: u32,
    #[arg(long, default_value_t = 120)]
    max_ms: u32,
    /// Render to a WAV file instead of the sound card
    #[arg(long)]
    wav: Option<std::path::PathBuf>,
    /// With --wav: duration to record
    #[arg(long, default_value_t = 5)]
    seconds: u64,
    /// Fake a lossy network: drop this percentage of incoming media packets
    #[arg(long, default_value_t = 0.0)]
    sim_loss: f32,
}

fn write_wav(path: &std::path::Path, samples: &[f32], rate: u32, ch: u16) -> Result<()> {
    use std::io::Write;
    let dl = (samples.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + dl).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    for b in [16u32.to_le_bytes().as_slice(), &3u16.to_le_bytes(), &ch.to_le_bytes(), &rate.to_le_bytes(), &(rate * ch as u32 * 4).to_le_bytes(), &(ch * 4).to_le_bytes(), &32u16.to_le_bytes()] {
        f.write_all(b)?;
    }
    f.write_all(b"data")?;
    f.write_all(&dl.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let opt = Opt::parse();
    let mut cfg = ClientConfig::new(opt.server.clone());
    cfg.pin = opt.pin.clone();
    cfg.client_name = "cli-client".into();
    cfg.min_ms = opt.min_ms;
    cfg.max_ms = opt.max_ms;
    cfg.sim_loss_pct = opt.sim_loss;
    let eng = Engine::start(cfg).map_err(anyhow::Error::msg)?;
    let params = eng.params().clone();

    if let Some(path) = opt.wav {
        // Simulated sound card: pull one frame at real-time pace.
        let mut all = Vec::new();
        let mut block = vec![0f32; params.frame_samples() * 2];
        let mut next = Instant::now();
        let end = next + Duration::from_secs(opt.seconds);
        while Instant::now() < end {
            eng.render(&mut block);
            all.extend_from_slice(&block);
            next += Duration::from_millis(params.frame_ms as u64);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        write_wav(&path, &all, params.sample_rate, 2)?;
        let s = eng.stats().jitter;
        println!("wrote {} ({:.1}s). rx={} lost={} late={} underruns={} overflows={}", path.display(), all.len() as f32 / 2.0 / 48000.0, s.received, s.lost, s.late, s.underruns, s.overflows);
        eng.stop();
        return Ok(());
    }

    let device = cpal::default_host().default_output_device().context("no output device")?;
    let mut cfg: cpal::StreamConfig = device.default_output_config()?.into();
    cfg.sample_rate = params.sample_rate;
    let dev_ch = cfg.channels as usize;
    let mut stereo = Vec::<f32>::new();
    let e2 = eng.clone();
    let stream = device.build_output_stream(
        cfg,
        move |data: &mut [f32], _| {
            let frames = data.len() / dev_ch;
            stereo.resize(frames * 2, 0.0);
            e2.render(&mut stereo);
            for (i, f) in data.chunks_mut(dev_ch).enumerate() {
                for (c, s) in f.iter_mut().enumerate() {
                    *s = if c < 2 { stereo[i * 2 + c] } else { 0.0 };
                }
            }
        },
        |e| eprintln!("audio error: {e}"),
        None,
    )?;
    stream.play()?;
    tracing::info!("playing; Ctrl-C to quit");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
