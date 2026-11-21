//! Encoder/sender thread: ring buffer -> Opus -> UDP.
use crate::session::Shared;
use soundovern_core::codec::OpusEnc;
use soundovern_proto::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub fn run(rx: Receiver<rtrb::Consumer<f32>>, sock: std::net::UdpSocket, sh: Arc<Shared>, stop: Arc<AtomicBool>) {
    let params = sh.params.clone();
    let mut enc = OpusEnc::new(&params).expect("opus encoder");
    let frame_len = params.frame_samples() * params.channels as usize;
    let frame_dur = Duration::from_millis(params.frame_ms as u64);
    let mut cons: Option<rtrb::Consumer<f32>> = None;
    let mut pcm = vec![0f32; frame_len];
    let mut pkt = Vec::with_capacity(MAX_DATAGRAM);
    let mut seq: u32 = 0;
    let mut ts: u64 = 0;
    let mut cur_session: Option<u16> = None;
    let mut last_data = Instant::now();
    let mut next_silence = Instant::now();
    let mut discont = false;
    let mut last_hint = -1;

    while !stop.load(Ordering::Relaxed) {
        // New capture generation (PipeWire (re)connected)?
        while let Ok(c) = rx.try_recv() {
            cons = Some(c);
            discont = true;
        }
        let session = sh.active.lock().unwrap().clone();
        let Some(c) = cons.as_mut() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        let Some(sess) = session else {
            // No listener: discard audio so the ring stays fresh, idle cheaply.
            let n = c.slots();
            if let Ok(chunk) = c.read_chunk(n) {
                chunk.commit_all();
            }
            cur_session = None;
            std::thread::sleep(Duration::from_millis(20));
            continue;
        };
        if cur_session != Some(sess.id) {
            // new client: start from fresh audio to minimise latency
            let n = c.slots();
            if let Ok(chunk) = c.read_chunk(n) {
                chunk.commit_all();
            }
            cur_session = Some(sess.id);
            seq = 0;
            discont = true;
            last_data = Instant::now();
        }
        let hint = sh.loss_hint.load(Ordering::Relaxed);
        if hint != last_hint {
            enc.set_loss_hint(hint.max(3));
            last_hint = hint;
        }

        let mut flags = if discont { flags::DISCONT } else { 0 };
        if c.slots() >= frame_len {
            let chunk = c.read_chunk(frame_len).unwrap();
            let (a, b) = chunk.as_slices();
            pcm[..a.len()].copy_from_slice(a);
            pcm[a.len()..].copy_from_slice(b);
            chunk.commit_all();
            last_data = Instant::now();
            next_silence = last_data + frame_dur;
            let payload = match enc.encode(&pcm) {
                Ok(p) => p.to_vec(),
                Err(e) => {
                    tracing::warn!("encode error: {e}");
                    continue;
                }
            };
            sess.media.seal(flags, sess.id, seq, ts, &payload, &mut pkt);
        } else if last_data.elapsed() > frame_dur * 3 && Instant::now() >= next_silence {
            // Capture stalled (suspended sink / PipeWire restart): keep the client fed with silence.
            next_silence += frame_dur;
            flags |= flags::SILENCE;
            sess.media.seal(flags, sess.id, seq, ts, &[], &mut pkt);
        } else {
            std::thread::sleep(Duration::from_millis(1));
            continue;
        }
        discont = false;
        seq = seq.wrapping_add(1);
        ts += params.frame_samples() as u64;
        match sock.send_to(&pkt, sess.media_addr) {
            Ok(n) => {
                sh.metrics.sent_packets.fetch_add(1, Ordering::Relaxed);
                sh.metrics.sent_bytes.fetch_add(n as u64, Ordering::Relaxed);
            }
            Err(e) => {
                if sh.metrics.send_errors.fetch_add(1, Ordering::Relaxed) % 100 == 0 {
                    tracing::warn!("udp send to {} failed: {e}", sess.media_addr);
                }
            }
        }
    }
}
