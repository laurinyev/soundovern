//! Wire protocol shared by the soundovern server and clients.
//!
//! * Media: UDP datagrams = [`MediaHeader`] (19 bytes) + one Opus frame.
//! * Control: TCP, each message is `u32 BE length` + postcard-encoded [`Control`].

pub mod crypto;

use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const PROTO_VERSION: u8 = 2;
pub const MAGIC: u8 = 0x53; // 'S'
pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;
pub const DEFAULT_CONTROL_PORT: u16 = 7420;
pub const MDNS_SERVICE: &str = "_soundovern._tcp.local.";
pub const MAX_CONTROL_LEN: u32 = 64 * 1024;
pub const MAX_DATAGRAM: usize = 1400;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ProtoError {
    #[error("packet too short")]
    TooShort,
    #[error("bad magic")]
    BadMagic,
    #[error("unsupported version {0}")]
    BadVersion(u8),
    #[error("payload length mismatch")]
    BadLength,
    #[error("control message error: {0}")]
    Codec(String),
}

pub mod flags {
    /// Frame is encoded silence / synthesized by the server (e.g. capture gap).
    pub const SILENCE: u8 = 1;
    /// Server-side discontinuity (capture restarted); client should resync.
    pub const DISCONT: u8 = 2;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaHeader {
    pub flags: u8,
    pub session_id: u16,
    pub seq: u32,
    /// Capture position in samples per channel @ 48 kHz.
    pub timestamp: u64,
    pub payload_len: u16,
}

impl MediaHeader {
    pub const LEN: usize = 19;

    pub fn write(&self, out: &mut Vec<u8>) {
        out.push(MAGIC);
        out.push(PROTO_VERSION);
        out.push(self.flags);
        out.extend_from_slice(&self.session_id.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&self.timestamp.to_be_bytes());
        out.extend_from_slice(&self.payload_len.to_be_bytes());
    }

    /// Parse a datagram, returning the header and the payload slice.
    pub fn parse(buf: &[u8]) -> Result<(Self, &[u8]), ProtoError> {
        if buf.len() < Self::LEN {
            return Err(ProtoError::TooShort);
        }
        if buf[0] != MAGIC {
            return Err(ProtoError::BadMagic);
        }
        if buf[1] != PROTO_VERSION {
            return Err(ProtoError::BadVersion(buf[1]));
        }
        let h = MediaHeader {
            flags: buf[2],
            session_id: u16::from_be_bytes([buf[3], buf[4]]),
            seq: u32::from_be_bytes(buf[5..9].try_into().unwrap()),
            timestamp: u64::from_be_bytes(buf[9..17].try_into().unwrap()),
            payload_len: u16::from_be_bytes([buf[17], buf[18]]),
        };
        let payload = &buf[Self::LEN..];
        if payload.len() != h.payload_len as usize {
            return Err(ProtoError::BadLength);
        }
        Ok((h, payload))
    }
}

/// Stream parameters negotiated on the control channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamParams {
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_ms: u8,
    pub bitrate: u32,
    pub fec: bool,
}

impl Default for StreamParams {
    fn default() -> Self {
        Self { sample_rate: SAMPLE_RATE, channels: CHANNELS, frame_ms: 10, bitrate: 128_000, fec: true }
    }
}

impl StreamParams {
    pub fn frame_samples(&self) -> usize {
        (self.sample_rate as usize * self.frame_ms as usize) / 1000
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Control {
    /// Client → server (plaintext). `udp_port` is where the client listens for media; `pake` is the
    /// client's SPAKE2 message.
    Hello { proto_version: u8, client_name: String, udp_port: u16, pake: Vec<u8> },
    /// Server → client (plaintext): the server's SPAKE2 message. Everything after this is encrypted.
    HelloReply { pake: Vec<u8> },
    /// Client → server (encrypted): proves the client derived the same key, i.e. knows the PIN.
    Ready,
    /// Server → client (encrypted).
    Accept { session_id: u16, params: StreamParams, server_name: String },
    Reject { reason: String },
    Ping { nonce: u64 },
    Pong { nonce: u64 },
    /// Client → server receiver report, ~1 Hz.
    Stats { received: u32, lost: u32, late: u32, jitter_ms: f32, buffer_ms: f32 },
    Stop,
}

impl Control {
    /// Encode as `u32 BE length` + postcard body.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let body = postcard::to_stdvec(self).map_err(|e| ProtoError::Codec(e.to_string()))?;
        let mut out = Vec::with_capacity(4 + body.len());
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&body);
        Ok(out)
    }

    pub fn decode_body(body: &[u8]) -> Result<Self, ProtoError> {
        postcard::from_bytes(body).map_err(|e| ProtoError::Codec(e.to_string()))
    }

    /// Blocking read of one framed message.
    pub fn read_from<R: std::io::Read>(r: &mut R) -> std::io::Result<Self> {
        let mut len = [0u8; 4];
        r.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len);
        if n > MAX_CONTROL_LEN {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "control frame too large"));
        }
        let mut body = vec![0u8; n as usize];
        r.read_exact(&mut body)?;
        Self::decode_body(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
    }

    pub fn write_to<W: std::io::Write>(&self, w: &mut W) -> std::io::Result<()> {
        let b = self.encode().map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        w.write_all(&b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_roundtrip() {
        let h = MediaHeader { flags: flags::DISCONT, session_id: 7, seq: 0xdead_beef, timestamp: 1 << 40, payload_len: 3 };
        let mut v = Vec::new();
        h.write(&mut v);
        assert_eq!(v.len(), MediaHeader::LEN);
        v.extend_from_slice(&[1, 2, 3]);
        let (h2, p) = MediaHeader::parse(&v).unwrap();
        assert_eq!(h, h2);
        assert_eq!(p, &[1, 2, 3]);
    }

    #[test]
    fn media_rejects_garbage() {
        assert_eq!(MediaHeader::parse(&[0; 5]), Err(ProtoError::TooShort));
        assert_eq!(MediaHeader::parse(&[0; 30]), Err(ProtoError::BadMagic));
        let mut v = Vec::new();
        MediaHeader { flags: 0, session_id: 0, seq: 0, timestamp: 0, payload_len: 9 }.write(&mut v);
        assert_eq!(MediaHeader::parse(&v), Err(ProtoError::BadLength));
    }

    #[test]
    fn control_roundtrip() {
        let m = Control::Hello { proto_version: PROTO_VERSION, client_name: "x".into(), udp_port: 5, pake: vec![1, 2, 3] };
        let b = m.encode().unwrap();
        let m2 = Control::read_from(&mut &b[..]).unwrap();
        assert_eq!(m, m2);
    }
}
