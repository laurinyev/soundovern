//! Control-channel server: PIN-authenticated key exchange, encrypted control, single active client.
use anyhow::Result;
use soundovern_proto::crypto::{self, MediaCipher};
use soundovern_proto::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicI32, AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub struct ActiveSession {
    pub id: u16,
    pub media_addr: SocketAddr,
    pub client_name: String,
    pub media: MediaCipher,
}

/// Last receiver report from the client (server logs it in its periodic status line).
#[derive(Clone, Copy, Default)]
pub struct ClientReport {
    pub received: u32,
    pub lost: u32,
    pub late: u32,
    pub jitter_ms: f32,
    pub buffer_ms: f32,
}

#[derive(Default)]
pub struct Metrics {
    pub sent_packets: AtomicU64,
    pub sent_bytes: AtomicU64,
    pub send_errors: AtomicU64,
    pub report: Mutex<Option<ClientReport>>,
}

#[derive(Default)]
struct FailState {
    count: u32,
    locked_until: Option<Instant>,
}

pub struct Shared {
    pub params: StreamParams,
    pub server_name: String,
    pub pin: String,
    pub active: Mutex<Option<ActiveSession>>,
    pub next_id: AtomicU16,
    /// Loss percentage derived from client reports; the encoder reads this to tune FEC.
    pub loss_hint: AtomicI32,
    pub metrics: Metrics,
    fails: Mutex<FailState>,
}

impl Shared {
    pub fn new(params: StreamParams, server_name: String, pin: String) -> Self {
        Shared { params, server_name, pin, active: Mutex::new(None), next_id: AtomicU16::new(1), loss_hint: AtomicI32::new(0), metrics: Metrics::default(), fails: Mutex::new(FailState::default()) }
    }

    fn lockout_remaining(&self) -> Option<Duration> {
        let g = self.fails.lock().unwrap();
        g.locked_until.and_then(|t| t.checked_duration_since(Instant::now()))
    }

    /// 5 failures in a row lock handshakes out for 30 s, doubling each further failure (max 5 min).
    fn record_failure(&self) {
        let mut g = self.fails.lock().unwrap();
        g.count += 1;
        if g.count >= 5 {
            let secs = (30u64 << (g.count - 5).min(4)).min(300);
            g.locked_until = Some(Instant::now() + Duration::from_secs(secs));
            tracing::warn!("{} failed pairing attempts; locking out for {secs}s", g.count);
        }
    }

    fn record_success(&self) {
        *self.fails.lock().unwrap() = FailState::default();
    }
}

pub fn serve(listener: TcpListener, shared: Arc<Shared>) {
    for conn in listener.incoming() {
        match conn {
            Ok(s) => {
                let sh = shared.clone();
                std::thread::spawn(move || {
                    if let Err(e) = handle(s, sh) {
                        tracing::info!("control connection ended: {e}");
                    }
                });
            }
            Err(e) => tracing::warn!("accept failed: {e}"),
        }
    }
}

fn handle(mut s: TcpStream, sh: Arc<Shared>) -> Result<()> {
    let peer = s.peer_addr()?;
    s.set_nodelay(true)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    if let Some(wait) = sh.lockout_remaining() {
        Control::Reject { reason: format!("too many failed attempts; retry in {}s", wait.as_secs() + 1) }.write_to(&mut s)?;
        anyhow::bail!("{peer}: refused during lockout");
    }
    let Control::Hello { proto_version, client_name, udp_port, pake } = Control::read_from(&mut s)? else {
        anyhow::bail!("expected Hello");
    };
    if proto_version != PROTO_VERSION {
        Control::Reject { reason: format!("protocol version {proto_version} unsupported (server speaks {PROTO_VERSION}); update the app") }.write_to(&mut s)?;
        return Ok(());
    }
    let (pending, my_msg) = crypto::server_start(&sh.pin);
    Control::HelloReply { pake: my_msg }.write_to(&mut s)?;
    let mut sess = match pending.finish(&pake) {
        Ok(k) => k,
        Err(_) => {
            sh.record_failure();
            anyhow::bail!("{peer}: malformed key-exchange message");
        }
    };
    // The client must prove it derived the same key (i.e. knows the PIN) before we stream anything.
    match sess.read_from(&mut s) {
        Ok(Control::Ready) => sh.record_success(),
        _ => {
            sh.record_failure();
            anyhow::bail!("{peer}: pairing failed (wrong PIN?)");
        }
    }
    let id = sh.next_id.fetch_add(1, Ordering::Relaxed);
    let me = ActiveSession { id, media_addr: SocketAddr::new(peer.ip(), udp_port), client_name: client_name.clone(), media: sess.media() };
    sess.write_to(&mut s, &Control::Accept { session_id: id, params: sh.params.clone(), server_name: sh.server_name.clone() })?;
    *sh.active.lock().unwrap() = Some(me.clone()); // newest client takes over
    tracing::info!("session {id}: '{}' at {} streaming (encrypted)", me.client_name, me.media_addr);

    // Keepalive: client pings ~1/s; 5 s read timeout means it's gone.
    let result = (|| -> Result<()> {
        loop {
            match sess.read_from(&mut s) {
                Ok(Control::Ping { nonce }) => sess.write_to(&mut s, &Control::Pong { nonce })?,
                Ok(Control::Stats { received, lost, late, jitter_ms, buffer_ms }) => {
                    let total = (received + lost).max(1);
                    let pct = (lost as f32 * 100.0 / total as f32).round() as i32;
                    sh.loss_hint.store(pct, Ordering::Relaxed);
                    *sh.metrics.report.lock().unwrap() = Some(ClientReport { received, lost, late, jitter_ms, buffer_ms });
                    tracing::debug!("stats: rx={received} lost={lost} late={late} jitter={jitter_ms:.1}ms buf={buffer_ms:.0}ms");
                }
                Ok(Control::Stop) => return Ok(()),
                Ok(_) => {}
                Err(e) => return Err(e.into()),
            }
            // Superseded by a newer session?
            if sh.active.lock().unwrap().as_ref().map(|a| a.id) != Some(id) {
                return Ok(());
            }
        }
    })();
    let mut g = sh.active.lock().unwrap();
    if g.as_ref().map(|a| a.id) == Some(id) {
        *g = None;
        *sh.metrics.report.lock().unwrap() = None;
    }
    tracing::info!("session {id} closed");
    result
}
