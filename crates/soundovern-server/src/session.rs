//! Control-channel server and shared session state (single active client).
use anyhow::Result;
use soundovern_proto::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicI32, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ActiveSession {
    pub id: u16,
    pub media_addr: SocketAddr,
    pub client_name: String,
}

pub struct Shared {
    pub params: StreamParams,
    pub server_name: String,
    pub pin: Option<String>,
    pub active: Mutex<Option<ActiveSession>>,
    pub next_id: AtomicU16,
    /// Loss percentage derived from client reports; the encoder reads this to tune FEC.
    pub loss_hint: AtomicI32,
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
    let hello = Control::read_from(&mut s)?;
    let Control::Hello { proto_version, client_name, udp_port, pin } = hello else {
        anyhow::bail!("expected Hello");
    };
    if proto_version != PROTO_VERSION {
        Control::Reject { reason: format!("protocol version {proto_version} unsupported") }.write_to(&mut s)?;
        return Ok(());
    }
    if let Some(want) = &sh.pin {
        if pin.as_deref() != Some(want.as_str()) {
            Control::Reject { reason: "bad PIN".into() }.write_to(&mut s)?;
            tracing::warn!("{peer}: rejected (bad PIN)");
            return Ok(());
        }
    }
    let id = sh.next_id.fetch_add(1, Ordering::Relaxed);
    let me = ActiveSession { id, media_addr: SocketAddr::new(peer.ip(), udp_port), client_name: client_name.clone() };
    *sh.active.lock().unwrap() = Some(me.clone()); // newest client takes over
    Control::Accept { session_id: id, params: sh.params.clone(), server_name: sh.server_name.clone() }.write_to(&mut s)?;
    tracing::info!("session {id}: '{}' at {} streaming", me.client_name, me.media_addr);

    // Keepalive: client pings ~1/s; 5 s read timeout means it's gone.
    let result = (|| -> Result<()> {
        loop {
            match Control::read_from(&mut s) {
                Ok(Control::Ping { nonce }) => Control::Pong { nonce }.write_to(&mut s)?,
                Ok(Control::Stats { received, lost, late, jitter_ms, buffer_ms }) => {
                    let total = (received + lost).max(1);
                    let pct = (lost as f32 * 100.0 / total as f32).round() as i32;
                    sh.loss_hint.store(pct, Ordering::Relaxed);
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
    }
    tracing::info!("session {id} closed");
    result
}
