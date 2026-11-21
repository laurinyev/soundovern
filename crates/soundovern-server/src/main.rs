mod capture;
mod session;
mod silent;
mod stream;

use anyhow::{Context, Result};
use clap::Parser;
use soundovern_proto::*;
use std::net::{IpAddr, Ipv4Addr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "soundovern-server", about = "Stream all desktop audio (PipeWire) to LAN clients")]
struct Opt {
    /// Address to bind the control listener on
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    #[arg(long, default_value_t = DEFAULT_CONTROL_PORT)]
    port: u16,
    /// Name advertised to clients
    #[arg(long)]
    name: Option<String>,
    /// Pairing PIN (default: a random 6-digit PIN, created once and stored in ~/.config/soundovern/pin)
    #[arg(long)]
    pin: Option<String>,
    /// Capture a specific PipeWire node (name or id) instead of the default sink monitor
    #[arg(long)]
    target: Option<String>,
    #[arg(long, default_value_t = 128_000)]
    bitrate: u32,
    /// Opus frame size in ms (5, 10 or 20)
    #[arg(long, default_value_t = 10)]
    frame_ms: u8,
    #[arg(long)]
    no_fec: bool,
    #[arg(long)]
    no_mdns: bool,
    /// While a client is connected, don't play audio on this machine (route it to a null sink)
    #[arg(long)]
    silent_local: bool,
    /// Capture for N seconds, print levels, optionally write a WAV, then exit
    #[arg(long)]
    probe: Option<u64>,
    #[arg(long, requires = "probe")]
    wav: Option<std::path::PathBuf>,
}

fn hostname() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).unwrap_or_else(|_| "soundovern".into())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())).init();
    let opt = Opt::parse();
    let params = StreamParams { frame_ms: opt.frame_ms, bitrate: opt.bitrate, fec: !opt.no_fec, ..Default::default() };
    let stop = Arc::new(AtomicBool::new(false));
    let stats = Arc::new(capture::CaptureStats::default());
    let (tx, rx) = std::sync::mpsc::channel();

    let cap_cfg = capture::CaptureConfig {
        rate: params.sample_rate,
        channels: params.channels as u32,
        quantum: (params.frame_samples() as u32).max(128),
        target: opt.target.clone(),
    };
    {
        let (stats, stop) = (stats.clone(), stop.clone());
        std::thread::Builder::new().name("pw-capture".into()).spawn(move || capture::run(cap_cfg, tx, stats, stop))?;
    }

    if let Some(secs) = opt.probe {
        return probe(rx, secs, &params, opt.wav);
    }

    let name = opt.name.clone().unwrap_or_else(hostname);
    let pin = match &opt.pin {
        Some(p) => p.clone(),
        None => load_or_create_pin()?,
    };
    let shared = Arc::new(session::Shared::new(params.clone(), name.clone(), pin.clone()));

    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).context("udp bind")?;
    {
        let s = socket2::SockRef::from(&sock);
        let _ = s.set_tos_v4(0xB8); // DSCP EF: low-latency class (best effort, honoured by many APs)
        let _ = s.set_send_buffer_size(256 * 1024);
    }
    {
        let (sh, stop) = (shared.clone(), stop.clone());
        std::thread::Builder::new().name("encoder".into()).spawn(move || stream::run(rx, sock, sh, stop))?;
    }

    let listener = TcpListener::bind((opt.bind, opt.port)).with_context(|| format!("bind {}:{}", opt.bind, opt.port))?;
    tracing::info!("listening on {}:{} as '{name}' ({} kbps, {} ms frames)", opt.bind, opt.port, params.bitrate / 1000, params.frame_ms);
    if let Some(ip) = local_ip() {
        tracing::info!("connect clients to {ip}:{}", opt.port);
    }
    println!("\n  Pairing PIN: {pin}\n  (enter it in the app; audio and control are end-to-end encrypted with a key derived from it)\n");
    let _mdns = if opt.no_mdns { None } else { advertise(&name, opt.port) };

    if opt.silent_local {
        let silent = Arc::new(Mutex::new(silent::SilentLocal::default()));
        let s2 = silent.clone();
        ctrlc::set_handler(move || {
            s2.lock().unwrap().restore();
            std::process::exit(0);
        })?;
        let sh = shared.clone();
        std::thread::Builder::new().name("silent-local".into()).spawn(move || loop {
            let has_client = sh.active.lock().unwrap().is_some();
            let mut g = silent.lock().unwrap();
            if has_client && !g.active() {
                g.engage();
            } else if !has_client && g.active() {
                g.restore();
            }
            drop(g);
            std::thread::sleep(Duration::from_millis(300));
        })?;
    }

    // Periodic status line (every 5 s while a client is connected; at debug level when idle).
    {
        let (st, stats, sh) = (stop.clone(), stats.clone(), shared.clone());
        std::thread::Builder::new().name("status".into()).spawn(move || {
            let (mut last_pkts, mut last_bytes, mut last) = (0u64, 0u64, std::time::Instant::now());
            while !st.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_secs(5));
                let pkts = sh.metrics.sent_packets.load(Ordering::Relaxed);
                let bytes = sh.metrics.sent_bytes.load(Ordering::Relaxed);
                let dt = last.elapsed().as_secs_f64().max(0.001);
                let (pps, kbps) = ((pkts - last_pkts) as f64 / dt, (bytes - last_bytes) as f64 * 8.0 / dt / 1000.0);
                (last_pkts, last_bytes, last) = (pkts, bytes, std::time::Instant::now());
                let cap = format!(
                    "capture: {} overruns, {} restarts, send errors {}",
                    stats.overruns.load(Ordering::Relaxed),
                    stats.restarts.load(Ordering::Relaxed),
                    sh.metrics.send_errors.load(Ordering::Relaxed)
                );
                match sh.active.lock().unwrap().clone() {
                    Some(a) => {
                        let rep = match *sh.metrics.report.lock().unwrap() {
                            Some(r) => format!("client: rx {} lost {} late {} jitter {:.1} ms buf {:.0} ms", r.received, r.lost, r.late, r.jitter_ms, r.buffer_ms),
                            None => "client: no report yet".into(),
                        };
                        tracing::info!("[session {} '{}' {}] tx {pps:.0} pkt/s {kbps:.0} kbit/s | {rep} | {cap}", a.id, a.client_name, a.media_addr);
                    }
                    None => tracing::debug!("idle | {cap}"),
                }
            }
        })?;
    }
    session::serve(listener, shared);
    Ok(())
}

/// A random 6-digit PIN, generated once and kept in `$XDG_CONFIG_HOME/soundovern/pin` (mode 0600).
fn load_or_create_pin() -> Result<String> {
    use rand_core::{OsRng, RngCore};
    use std::os::unix::fs::OpenOptionsExt;
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))
        .context("cannot locate a config directory (set --pin)")?;
    let dir = base.join("soundovern");
    let file = dir.join("pin");
    if let Ok(s) = std::fs::read_to_string(&file) {
        let s = s.trim().to_string();
        if s.len() >= 6 && s.chars().all(|c| c.is_ascii_digit()) {
            return Ok(s);
        }
    }
    std::fs::create_dir_all(&dir)?;
    let pin = format!("{:06}", OsRng.next_u32() % 1_000_000);
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&file)?;
    std::io::Write::write_all(&mut f, pin.as_bytes())?;
    tracing::info!("created new pairing PIN, stored in {}", file.display());
    Ok(pin)
}

fn local_ip() -> Option<IpAddr> {
    // Route-based detection; no packets are sent for a UDP connect().
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}

fn advertise(name: &str, port: u16) -> Option<mdns_sd::ServiceDaemon> {
    let d = mdns_sd::ServiceDaemon::new().ok()?;
    let host = format!("{}.local.", hostname());
    let props = [("ver", PROTO_VERSION.to_string())];
    let info = mdns_sd::ServiceInfo::new(MDNS_SERVICE, name, &host, "", port, &props[..]).ok()?.enable_addr_auto();
    d.register(info).ok()?;
    tracing::info!("mDNS: advertising {MDNS_SERVICE}");
    Some(d)
}

fn probe(rx: std::sync::mpsc::Receiver<rtrb::Consumer<f32>>, secs: u64, p: &StreamParams, wav: Option<std::path::PathBuf>) -> Result<()> {
    let mut cons = rx.recv_timeout(Duration::from_secs(5)).context("capture thread did not start")?;
    let ch = p.channels as usize;
    let mut all: Vec<f32> = Vec::new();
    let end = std::time::Instant::now() + Duration::from_secs(secs);
    let mut peak = 0f32;
    while std::time::Instant::now() < end {
        while let Ok(c) = rx.try_recv() {
            cons = c;
        }
        let n = cons.slots();
        if n > 0 {
            let chunk = cons.read_chunk(n).unwrap();
            let (a, b) = chunk.as_slices();
            for v in a.iter().chain(b) {
                peak = peak.max(v.abs());
            }
            if wav.is_some() {
                all.extend_from_slice(a);
                all.extend_from_slice(b);
            }
            chunk.commit_all();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    println!("probe: peak amplitude over {secs}s = {peak:.4}  ({})", if peak > 0.0005 { "audio detected" } else { "silence - is anything playing?" });
    if let Some(path) = wav {
        write_wav(&path, &all, p.sample_rate, ch as u16)?;
        println!("wrote {} ({} frames)", path.display(), all.len() / ch);
    }
    Ok(())
}

fn write_wav(path: &std::path::Path, samples: &[f32], rate: u32, ch: u16) -> Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 4) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVEfmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&3u16.to_le_bytes())?; // IEEE float
    f.write_all(&ch.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * ch as u32 * 4).to_le_bytes())?;
    f.write_all(&(ch * 4).to_le_bytes())?;
    f.write_all(&32u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
