//! Receiving engine shared by the desktop CLI client and the Android app (via JNI).
//!
//! `Engine::start` performs the handshake, then runs a UDP receiver thread and a control
//! thread (keepalive, receiver reports, automatic reconnect). The audio device pulls PCM
//! with [`Engine::render`].

use crate::codec::OpusDec;
use crate::drift::{DriftController, LinearResampler};
use crate::jitter::{JitterBuffer, JitterStats, Pop};
use soundovern_proto::crypto::{self, MediaCipher, Session};
use soundovern_proto::*;
use std::collections::VecDeque;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ClientConfig {
    pub server: String,
    /// Pairing PIN shown by the server.
    pub pin: String,
    pub client_name: String,
    pub min_ms: u32,
    pub max_ms: u32,
    /// Test hook: drop this percentage of incoming media packets.
    pub sim_loss_pct: f32,
}

impl ClientConfig {
    pub fn new(server: impl Into<String>) -> Self {
        Self { server: server.into(), pin: String::new(), client_name: "client".into(), min_ms: 20, max_ms: 120, sim_loss_pct: 0.0 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connected,
    Reconnecting,
    Stopped,
}

#[derive(Debug, Clone, Copy)]
pub struct EngineStats {
    pub jitter: JitterStats,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
    pub state: ConnState,
}

struct Player {
    jb: Arc<Mutex<JitterBuffer>>,
    dec: OpusDec,
    rs: LinearResampler,
    drift: DriftController,
    pending: VecDeque<f32>,
    frame: Vec<f32>,
    scratch: Vec<f32>,
}

impl Player {
    fn produce(&mut self) {
        let (pop, depth, target) = {
            let mut jb = self.jb.lock().unwrap();
            let pop = jb.pop();
            (pop, jb.depth() as f64, jb.target() as f64)
        };
        let ok = match pop {
            Pop::Data(p) if p.is_empty() => false, // server-synthesised silence
            Pop::Data(p) => self.dec.decode(&p, &mut self.frame).is_ok(),
            Pop::Lost { next: Some(n) } => self.dec.decode_fec(&n, &mut self.frame).is_ok() || self.dec.conceal(&mut self.frame).is_ok(),
            Pop::Lost { next: None } => self.dec.conceal(&mut self.frame).is_ok(),
            Pop::Empty => false,
        };
        if !ok {
            self.frame.fill(0.0);
        }
        let ratio = if ok { self.drift.update(depth, target) } else { 1.0 };
        self.scratch.clear();
        self.rs.process(&self.frame, ratio, &mut self.scratch);
        self.pending.extend(self.scratch.iter().copied());
    }
}

pub struct Engine {
    params: StreamParams,
    jb: Arc<Mutex<JitterBuffer>>,
    player: Mutex<Player>,
    running: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    muted: AtomicBool,
    volume_milli: AtomicU32,
}

fn resolve(server: &str) -> std::io::Result<SocketAddr> {
    let s = if server.contains(':') && !server.ends_with(']') && server.rsplit(':').next().is_some_and(|p| p.parse::<u16>().is_ok()) {
        server.to_string()
    } else {
        format!("{server}:{DEFAULT_CONTROL_PORT}")
    };
    s.to_socket_addrs()?.next().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "cannot resolve server"))
}

struct Connected {
    tcp: TcpStream,
    sess: Session,
    params: StreamParams,
    session_id: u16,
}

/// TCP connect + SPAKE2 handshake. Fails with a readable message for a wrong PIN or refusal.
fn connect(addr: SocketAddr, udp_port: u16, cfg: &ClientConfig) -> Result<Connected, String> {
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| format!("connect: {e}"))?;
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let (pending, my_msg) = crypto::client_start(&cfg.pin);
    Control::Hello { proto_version: PROTO_VERSION, client_name: cfg.client_name.clone(), udp_port, pake: my_msg }
        .write_to(&mut s)
        .map_err(|e| e.to_string())?;
    let server_msg = match Control::read_from(&mut s).map_err(|e| format!("handshake: {e}"))? {
        Control::HelloReply { pake } => pake,
        Control::Reject { reason } => return Err(format!("rejected: {reason}")),
        o => return Err(format!("unexpected message {o:?}")),
    };
    let mut sess = pending.finish(&server_msg).map_err(|e| e.to_string())?;
    sess.write_to(&mut s, &Control::Ready).map_err(|e| e.to_string())?;
    match sess.read_from(&mut s) {
        Ok(Control::Accept { session_id, params, server_name }) => {
            tracing::info!("connected to '{server_name}' session {session_id} (encrypted)");
            Ok(Connected { tcp: s, sess, params, session_id })
        }
        Ok(o) => Err(format!("unexpected message {o:?}")),
        Err(_) => Err("pairing failed: wrong PIN, or the server refused the connection".into()),
    }
}

impl Engine {
    /// Blocking handshake, then background threads. Fails fast on connect error / bad PIN.
    pub fn start(cfg: ClientConfig) -> Result<Arc<Engine>, String> {
        let addr = resolve(&cfg.server).map_err(|e| e.to_string())?;
        let udp = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        udp.set_read_timeout(Some(Duration::from_millis(200))).map_err(|e| e.to_string())?;
        let udp_port = udp.local_addr().map_err(|e| e.to_string())?.port();
        let Connected { tcp: ctl, sess, params, session_id: sid } = connect(addr, udp_port, &cfg)?;
        let media = Arc::new(Mutex::new(sess.media()));

        let jb = Arc::new(Mutex::new(JitterBuffer::new(params.frame_ms, cfg.min_ms, cfg.max_ms)));
        let dec = OpusDec::new(&params).map_err(|e| e.to_string())?;
        let n = dec.frame_len();
        let player = Player {
            jb: jb.clone(),
            dec,
            rs: LinearResampler::new(params.channels as usize),
            drift: DriftController::new(0.005),
            pending: VecDeque::new(),
            frame: vec![0.0; n],
            scratch: Vec::new(),
        };
        let eng = Arc::new(Engine {
            params: params.clone(),
            jb: jb.clone(),
            player: Mutex::new(player),
            running: Arc::new(AtomicBool::new(true)),
            connected: Arc::new(AtomicBool::new(true)),
            muted: AtomicBool::new(false),
            volume_milli: AtomicU32::new(1000),
        });
        let session_id = Arc::new(AtomicU16::new(sid));

        // UDP receiver
        {
            let (jb, sid, running, udp, media) = (jb.clone(), session_id.clone(), eng.running.clone(), udp, media.clone());
            let sim = cfg.sim_loss_pct;
            std::thread::Builder::new().name("sn-udp".into()).spawn(move || {
                let mut buf = [0u8; 2048];
                let mut rng = 0x1234_5678u32;
                while running.load(Ordering::Relaxed) {
                    let Ok(n) = udp.recv(&mut buf) else { continue };
                    // Quick plaintext-header check first, then authenticate + decrypt.
                    let Ok((h0, _)) = MediaHeader::parse(&buf[..n]) else { continue };
                    if h0.session_id != sid.load(Ordering::Relaxed) {
                        continue;
                    }
                    let cipher: MediaCipher = media.lock().unwrap().clone();
                    let Ok((h, payload)) = cipher.open(&buf[..n]) else { continue };
                    rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
                    if sim > 0.0 && ((rng >> 8) as f32 / (1u32 << 24) as f32) * 100.0 < sim {
                        continue;
                    }
                    let mut g = jb.lock().unwrap();
                    if h.flags & flags::DISCONT != 0 {
                        g.reset();
                    }
                    g.push(h.seq, payload, Instant::now());
                }
            }).map_err(|e| e.to_string())?;
        }

        // Control: keepalive, reports, reconnect
        {
            let eng2 = eng.clone();
            std::thread::Builder::new().name("sn-ctl".into()).spawn(move || control_loop(eng2, ctl, sess, addr, udp_port, cfg, session_id, media)).map_err(|e| e.to_string())?;
        }
        Ok(eng)
    }

    pub fn params(&self) -> &StreamParams {
        &self.params
    }

    pub fn set_muted(&self, m: bool) {
        self.muted.store(m, Ordering::Relaxed);
    }
    pub fn set_volume(&self, v: f32) {
        self.volume_milli.store((v.clamp(0.0, 1.0) * 1000.0) as u32, Ordering::Relaxed);
    }

    pub fn stats(&self) -> EngineStats {
        let g = self.jb.lock().unwrap();
        let state = if !self.running.load(Ordering::Relaxed) {
            ConnState::Stopped
        } else if self.connected.load(Ordering::Relaxed) {
            ConnState::Connected
        } else {
            ConnState::Reconnecting
        };
        EngineStats { jitter: g.stats, jitter_ms: g.jitter_ms(), buffer_ms: g.buffered_ms(), state }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::Relaxed);
    }

    /// Fill `out` with interleaved samples (`params.channels`). Safe to call from an audio thread.
    pub fn render(&self, out: &mut [f32]) {
        let mut p = self.player.lock().unwrap();
        while p.pending.len() < out.len() {
            p.produce();
        }
        let gain = if self.muted.load(Ordering::Relaxed) { 0.0 } else { self.volume_milli.load(Ordering::Relaxed) as f32 / 1000.0 };
        for o in out.iter_mut() {
            *o = p.pending.pop_front().unwrap() * gain;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn control_loop(eng: Arc<Engine>, mut ctl: TcpStream, mut sess: Session, addr: SocketAddr, udp_port: u16, cfg: ClientConfig, sid: Arc<AtomicU16>, media: Arc<Mutex<MediaCipher>>) {
    let mut last = JitterStats::default();
    let mut nonce = 0u64;
    let mut backoff = Duration::from_millis(500);
    while eng.running.load(Ordering::Relaxed) {
        // 1 s tick, but wake quickly on stop
        for _ in 0..10 {
            if !eng.running.load(Ordering::Relaxed) {
                let _ = sess.write_to(&mut ctl, &Control::Stop);
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        nonce += 1;
        let (stats, jitter, buf) = {
            let g = eng.jb.lock().unwrap();
            (g.stats, g.jitter_ms(), g.buffered_ms())
        };
        tracing::info!("rx={} lost={} late={} underruns={} jitter={jitter:.1}ms buf={buf:.0}ms", stats.received, stats.lost, stats.late, stats.underruns);
        let res = (|| -> std::io::Result<()> {
            sess.write_to(&mut ctl, &Control::Ping { nonce })?;
            sess.write_to(
                &mut ctl,
                &Control::Stats {
                    received: stats.received.wrapping_sub(last.received),
                    lost: stats.lost.wrapping_sub(last.lost),
                    late: stats.late.wrapping_sub(last.late),
                    jitter_ms: jitter,
                    buffer_ms: buf,
                },
            )?;
            ctl.set_read_timeout(Some(Duration::from_millis(50)))?;
            loop {
                match sess.read_from(&mut ctl) {
                    Ok(Control::Stop) => return Err(std::io::ErrorKind::ConnectionAborted.into()),
                    Ok(_) => {}
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(()),
                    Err(e) => return Err(e),
                }
            }
        })();
        last = stats;
        if let Err(e) = res {
            tracing::warn!("control lost ({e}); reconnecting");
            eng.connected.store(false, Ordering::Relaxed);
            loop {
                if !eng.running.load(Ordering::Relaxed) {
                    return;
                }
                match connect(addr, udp_port, &cfg) {
                    Ok(c) => {
                        ctl = c.tcp;
                        *media.lock().unwrap() = c.sess.media();
                        sess = c.sess;
                        sid.store(c.session_id, Ordering::Relaxed);
                        eng.jb.lock().unwrap().reset();
                        eng.connected.store(true, Ordering::Relaxed);
                        backoff = Duration::from_millis(500);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("reconnect failed: {e}");
                        std::thread::sleep(backoff);
                        backoff = (backoff * 2).min(Duration::from_secs(5));
                    }
                }
            }
        }
    }
}
