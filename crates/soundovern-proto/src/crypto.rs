//! PIN-authenticated key exchange (SPAKE2) and AEAD (ChaCha20-Poly1305) for control + media.
//!
//! * A passive eavesdropper learns nothing about the PIN; an active attacker gets one online
//!   guess per handshake (the server rate-limits failures).
//! * Keys: HKDF-SHA256 over the SPAKE2 output gives independent keys for client→server control,
//!   server→client control, and server→client media.
//! * Control frames: counter nonce (TCP is ordered). Media: nonce = seq || capture timestamp,
//!   header authenticated as AAD, so reordering/loss is fine and replays are caught by the
//!   receiver's jitter buffer.

use crate::{Control, MediaHeader, ProtoError, MAX_CONTROL_LEN};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use hkdf::Hkdf;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity, Password, Spake2};

const ID_CLIENT: &[u8] = b"soundovern-client-v2";
const ID_SERVER: &[u8] = b"soundovern-server-v2";
pub const TAG_LEN: usize = 16;

pub struct PendingClient(Spake2<Ed25519Group>);
pub struct PendingServer(Spake2<Ed25519Group>);

/// Begin the client side. Send the returned bytes in `Hello`.
pub fn client_start(pin: &str) -> (PendingClient, Vec<u8>) {
    let (s, m) = Spake2::<Ed25519Group>::start_a(&Password::new(pin.as_bytes()), &Identity::new(ID_CLIENT), &Identity::new(ID_SERVER));
    (PendingClient(s), m)
}

/// Begin the server side. Send the returned bytes in `HelloReply`.
pub fn server_start(pin: &str) -> (PendingServer, Vec<u8>) {
    let (s, m) = Spake2::<Ed25519Group>::start_b(&Password::new(pin.as_bytes()), &Identity::new(ID_CLIENT), &Identity::new(ID_SERVER));
    (PendingServer(s), m)
}

impl PendingClient {
    pub fn finish(self, server_msg: &[u8]) -> Result<Session, ProtoError> {
        let k = self.0.finish(server_msg).map_err(|e| ProtoError::Codec(format!("key exchange: {e:?}")))?;
        Ok(Session::derive(&k, true))
    }
}

impl PendingServer {
    pub fn finish(self, client_msg: &[u8]) -> Result<Session, ProtoError> {
        let k = self.0.finish(client_msg).map_err(|e| ProtoError::Codec(format!("key exchange: {e:?}")))?;
        Ok(Session::derive(&k, false))
    }
}

struct Dir {
    aead: ChaCha20Poly1305,
    ctr: u64,
}

impl Dir {
    fn nonce(&self) -> Nonce {
        let mut n = [0u8; 12];
        n[4..].copy_from_slice(&self.ctr.to_be_bytes());
        *Nonce::from_slice(&n)
    }
}

/// Encrypted, ordered control channel plus the media cipher for one session.
pub struct Session {
    send: Dir,
    recv: Dir,
    media: MediaCipher,
}

fn expand(hk: &Hkdf<Sha256>, label: &[u8]) -> ChaCha20Poly1305 {
    let mut k = [0u8; 32];
    hk.expand(label, &mut k).expect("32 bytes is a valid HKDF output length");
    ChaCha20Poly1305::new(Key::from_slice(&k))
}

impl Session {
    fn derive(secret: &[u8], is_client: bool) -> Session {
        let hk = Hkdf::<Sha256>::new(Some(b"soundovern v2 keys"), secret);
        let c2s = expand(&hk, b"control client->server");
        let s2c = expand(&hk, b"control server->client");
        let media = MediaCipher(expand(&hk, b"media server->client"));
        let (s, r) = if is_client { (c2s, s2c) } else { (s2c, c2s) };
        Session { send: Dir { aead: s, ctr: 0 }, recv: Dir { aead: r, ctr: 0 }, media }
    }

    pub fn media(&self) -> MediaCipher {
        self.media.clone()
    }

    /// Encrypt and write one control message (`u32 BE length` + ciphertext).
    pub fn write_to<W: std::io::Write>(&mut self, w: &mut W, c: &Control) -> std::io::Result<()> {
        let body = postcard::to_stdvec(c).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        let ct = self
            .send
            .aead
            .encrypt(&self.send.nonce(), Payload { msg: &body, aad: b"ctl" })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "encrypt failed"))?;
        self.send.ctr += 1;
        let mut out = Vec::with_capacity(4 + ct.len());
        out.extend_from_slice(&(ct.len() as u32).to_be_bytes());
        out.extend_from_slice(&ct);
        w.write_all(&out)
    }

    /// Read and decrypt one control message. Authentication failure -> `InvalidData`.
    pub fn read_from<R: std::io::Read>(&mut self, r: &mut R) -> std::io::Result<Control> {
        let mut len = [0u8; 4];
        r.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len);
        if n > MAX_CONTROL_LEN + TAG_LEN as u32 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "control frame too large"));
        }
        let mut ct = vec![0u8; n as usize];
        r.read_exact(&mut ct)?;
        let pt = self
            .recv
            .aead
            .decrypt(&self.recv.nonce(), Payload { msg: &ct, aad: b"ctl" })
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "authentication failed (wrong PIN?)"))?;
        self.recv.ctr += 1;
        Control::decode_body(&pt).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }
}

/// Server→client media cipher. Cheap to clone.
#[derive(Clone)]
pub struct MediaCipher(ChaCha20Poly1305);

impl MediaCipher {
    fn nonce(seq: u32, ts: u64) -> Nonce {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&seq.to_be_bytes());
        n[4..].copy_from_slice(&ts.to_be_bytes());
        *Nonce::from_slice(&n)
    }

    /// Append `header || ciphertext` to `out` (clears it first). `plaintext` may be empty.
    pub fn seal(&self, flags: u8, session_id: u16, seq: u32, timestamp: u64, plaintext: &[u8], out: &mut Vec<u8>) {
        out.clear();
        MediaHeader { flags, session_id, seq, timestamp, payload_len: (plaintext.len() + TAG_LEN) as u16 }.write(out);
        let ct = self
            .0
            .encrypt(&Self::nonce(seq, timestamp), Payload { msg: plaintext, aad: &out[..MediaHeader::LEN] })
            .expect("encryption cannot fail for in-memory buffers");
        out.extend_from_slice(&ct);
    }

    /// Verify and decrypt a datagram. Anything not authentic is rejected.
    pub fn open(&self, datagram: &[u8]) -> Result<(MediaHeader, Vec<u8>), ProtoError> {
        let (h, ct) = MediaHeader::parse(datagram)?;
        let pt = self
            .0
            .decrypt(&Self::nonce(h.seq, h.timestamp), Payload { msg: ct, aad: &datagram[..MediaHeader::LEN] })
            .map_err(|_| ProtoError::Codec("media authentication failed".into()))?;
        Ok((h, pt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(pc: &str, ps: &str) -> (Session, Session) {
        let (c, cm) = client_start(pc);
        let (s, sm) = server_start(ps);
        (c.finish(&sm).unwrap(), s.finish(&cm).unwrap())
    }

    #[test]
    fn control_roundtrip_both_directions() {
        let (mut c, mut s) = pair("123456", "123456");
        let mut wire = Vec::new();
        for i in 0..3 {
            c.write_to(&mut wire, &Control::Ping { nonce: i }).unwrap();
        }
        let mut r = &wire[..];
        for i in 0..3 {
            assert_eq!(s.read_from(&mut r).unwrap(), Control::Ping { nonce: i });
        }
        let mut wire = Vec::new();
        s.write_to(&mut wire, &Control::Pong { nonce: 9 }).unwrap();
        assert_eq!(c.read_from(&mut &wire[..]).unwrap(), Control::Pong { nonce: 9 });
    }

    #[test]
    fn wrong_pin_cannot_talk() {
        let (mut c, mut s) = pair("123456", "654321");
        let mut wire = Vec::new();
        c.write_to(&mut wire, &Control::Ready).unwrap();
        let e = s.read_from(&mut &wire[..]).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
        // and media from the wrong key is rejected too
        let mut pkt = Vec::new();
        c.media().seal(0, 1, 1, 480, b"audio", &mut pkt);
        assert!(s.media().open(&pkt).is_err());
    }

    #[test]
    fn tamper_and_replay_of_control_detected() {
        let (mut c, mut s) = pair("1", "1");
        let mut wire = Vec::new();
        c.write_to(&mut wire, &Control::Ping { nonce: 1 }).unwrap();
        let mut bad = wire.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(s.read_from(&mut &bad[..]).is_err());
        // a valid frame still works (counter was not advanced by the failure)
        assert!(s.read_from(&mut &wire[..]).is_ok());
        // replaying the same frame fails: the counter has moved on
        assert!(s.read_from(&mut &wire[..]).is_err());
    }

    #[test]
    fn media_roundtrip_and_tamper() {
        let (c, s) = pair("42", "42");
        let _ = c;
        let m = s.media();
        let mut pkt = Vec::new();
        m.seal(crate::flags::DISCONT, 7, 0xffff_fffe, 1 << 40, b"opus frame", &mut pkt);
        let (h, pt) = m.open(&pkt).unwrap();
        assert_eq!((h.session_id, h.seq, h.flags), (7, 0xffff_fffe, crate::flags::DISCONT));
        assert_eq!(pt, b"opus frame");
        // flipping any header byte or payload byte breaks authentication
        for i in 5..pkt.len() {
            let mut b = pkt.clone();
            b[i] ^= 0x80;
            assert!(m.open(&b).is_err(), "byte {i} not authenticated");
        }
        // empty (silence) frames are authenticated too
        m.seal(crate::flags::SILENCE, 7, 1, 480, &[], &mut pkt);
        assert_eq!(m.open(&pkt).unwrap().1.len(), 0);
    }

    #[test]
    fn client_and_server_media_keys_match() {
        let (c, s) = pair("777777", "777777");
        let mut pkt = Vec::new();
        s.media().seal(0, 2, 5, 2400, b"x", &mut pkt);
        assert_eq!(c.media().open(&pkt).unwrap().1, b"x");
    }
}
