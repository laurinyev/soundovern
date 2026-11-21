use opus::{Application, Bitrate, Channels, Decoder, Encoder};
use soundovern_proto::StreamParams;

fn ch(c: u16) -> Channels {
    if c == 1 { Channels::Mono } else { Channels::Stereo }
}

pub struct OpusEnc {
    enc: Encoder,
    params: StreamParams,
    out: Vec<u8>,
}

impl OpusEnc {
    pub fn new(p: &StreamParams) -> Result<Self, opus::Error> {
        let mut enc = Encoder::new(p.sample_rate, ch(p.channels), Application::LowDelay)?;
        enc.set_bitrate(Bitrate::Bits(p.bitrate as i32))?;
        if p.fec {
            enc.set_inband_fec(true)?;
            enc.set_packet_loss_perc(5)?;
        }
        Ok(Self { enc, params: p.clone(), out: vec![0; 1500] })
    }

    /// Encode exactly one frame of interleaved f32 samples.
    pub fn encode(&mut self, pcm: &[f32]) -> Result<&[u8], opus::Error> {
        debug_assert_eq!(pcm.len(), self.params.frame_samples() * self.params.channels as usize);
        let n = self.enc.encode_float(pcm, &mut self.out)?;
        Ok(&self.out[..n])
    }

    pub fn set_loss_hint(&mut self, pct: i32) {
        let _ = self.enc.set_packet_loss_perc(pct.clamp(0, 100));
    }
}

pub struct OpusDec {
    dec: Decoder,
    pub frame_samples: usize,
    pub channels: usize,
}

impl OpusDec {
    pub fn new(p: &StreamParams) -> Result<Self, opus::Error> {
        Ok(Self { dec: Decoder::new(p.sample_rate, ch(p.channels))?, frame_samples: p.frame_samples(), channels: p.channels as usize })
    }

    pub fn frame_len(&self) -> usize {
        self.frame_samples * self.channels
    }

    /// Decode one packet into `out` (len = frame_len). Returns samples per channel.
    pub fn decode(&mut self, packet: &[u8], out: &mut [f32]) -> Result<usize, opus::Error> {
        self.dec.decode_float(packet, out, false)
    }

    /// Packet-loss concealment for a missing frame.
    pub fn conceal(&mut self, out: &mut [f32]) -> Result<usize, opus::Error> {
        self.dec.decode_float(&[], out, false)
    }

    /// Recover a lost frame from the in-band FEC data carried by the *next* packet.
    pub fn decode_fec(&mut self, next_packet: &[u8], out: &mut [f32]) -> Result<usize, opus::Error> {
        self.dec.decode_float(next_packet, out, true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sine() {
        let p = StreamParams::default();
        let mut e = OpusEnc::new(&p).unwrap();
        let mut d = OpusDec::new(&p).unwrap();
        let n = p.frame_samples();
        let mut pcm = vec![0f32; n * 2];
        let mut out = vec![0f32; n * 2];
        let mut t = 0usize;
        let mut energy = 0.0;
        for _ in 0..50 {
            for i in 0..n {
                let s = (2.0 * std::f32::consts::PI * 440.0 * (t + i) as f32 / 48000.0).sin() * 0.5;
                pcm[i * 2] = s;
                pcm[i * 2 + 1] = s;
            }
            t += n;
            let pkt = e.encode(&pcm).unwrap().to_vec();
            assert!(pkt.len() < 600);
            assert_eq!(d.decode(&pkt, &mut out).unwrap(), n);
            energy = out.iter().map(|x| x * x).sum::<f32>();
        }
        assert!(energy > 1.0, "decoded signal is silent");
        d.conceal(&mut out).unwrap();
    }
}
