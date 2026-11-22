//! Conversion of encoder output (Annex-B H.264/HEVC, AV1 low-overhead OBUs) into the
//! ISO-BMFF sample format, plus construction of avcC / hvcC / av1C configuration records.

use crate::config::Codec;

/// Accumulates parameter sets seen in the stream and converts access units.
#[derive(Default)]
pub struct BitstreamConverter {
    codec: Option<Codec>,
    pub vps: Option<Vec<u8>>,
    pub sps: Option<Vec<u8>>,
    pub pps: Option<Vec<u8>>,
    pub av1_seq: Option<Vec<u8>>, // full OBU (header + size + payload)
}

impl BitstreamConverter {
    pub fn new(codec: Codec) -> Self {
        BitstreamConverter { codec: Some(codec), ..Default::default() }
    }

    /// Converts one encoded access unit into MP4 sample bytes.
    pub fn convert(&mut self, data: &[u8]) -> Vec<u8> {
        match self.codec.unwrap_or(Codec::H264) {
            Codec::H264 | Codec::Hevc => self.convert_nal(data),
            Codec::Av1 => self.convert_av1(data),
        }
    }

    fn convert_nal(&mut self, data: &[u8]) -> Vec<u8> {
        let hevc = self.codec == Some(Codec::Hevc);
        let mut out = Vec::with_capacity(data.len() + 16);
        for nal in split_annexb(data) {
            if nal.is_empty() {
                continue;
            }
            if hevc {
                let t = (nal[0] >> 1) & 0x3f;
                match t {
                    32 => {
                        self.vps = Some(nal.to_vec());
                        continue;
                    }
                    33 => {
                        self.sps = Some(nal.to_vec());
                        continue;
                    }
                    34 => {
                        self.pps = Some(nal.to_vec());
                        continue;
                    }
                    35 => continue, // AUD
                    _ => {}
                }
            } else {
                let t = nal[0] & 0x1f;
                match t {
                    7 => {
                        self.sps = Some(nal.to_vec());
                        continue;
                    }
                    8 => {
                        self.pps = Some(nal.to_vec());
                        continue;
                    }
                    9 => continue, // AUD
                    _ => {}
                }
            }
            out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            out.extend_from_slice(nal);
        }
        out
    }

    fn convert_av1(&mut self, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(data.len() + 8);
        let mut p = 0usize;
        while p < data.len() {
            let header = data[p];
            let obu_type = (header >> 3) & 0x0f;
            let ext = header & 0x04 != 0;
            let has_size = header & 0x02 != 0;
            let hdr_len = 1 + ext as usize;
            if p + hdr_len > data.len() {
                break;
            }
            let (payload_len, size_len) = if has_size {
                match read_leb128(&data[p + hdr_len..]) {
                    Some(v) => v,
                    None => break,
                }
            } else {
                (data.len() - p - hdr_len, 0)
            };
            let start_payload = p + hdr_len + size_len;
            let end = (start_payload + payload_len).min(data.len());
            let payload = &data[start_payload..end];
            // Rebuild the OBU with an explicit size field (required in ISO-BMFF).
            let mut obu = Vec::with_capacity(hdr_len + 8 + payload.len());
            obu.push(header | 0x02);
            if ext {
                obu.push(data[p + 1]);
            }
            write_leb128(&mut obu, payload.len());
            obu.extend_from_slice(payload);
            match obu_type {
                2 => {} // temporal delimiter: must not be stored in samples
                1 => {
                    self.av1_seq = Some(obu.clone());
                    out.extend_from_slice(&obu);
                }
                15 => {} // padding
                _ => out.extend_from_slice(&obu),
            }
            p = end;
        }
        out
    }

    #[allow(dead_code)]
    pub fn is_ready(&self) -> bool {
        match self.codec {
            Some(Codec::H264) => self.sps.is_some() && self.pps.is_some(),
            Some(Codec::Hevc) => self.vps.is_some() && self.sps.is_some() && self.pps.is_some(),
            Some(Codec::Av1) => self.av1_seq.is_some(),
            None => false,
        }
    }

    /// Builds the codec configuration record (box payload without the box header).
    pub fn config_record(&self) -> Option<Vec<u8>> {
        match self.codec? {
            Codec::H264 => avcc(self.sps.as_ref()?, self.pps.as_ref()?),
            Codec::Hevc => hvcc(self.vps.as_ref()?, self.sps.as_ref()?, self.pps.as_ref()?),
            Codec::Av1 => av1c(self.av1_seq.as_ref()?),
        }
    }
}

/// Splits an Annex-B byte stream into NAL units (without start codes).
pub fn split_annexb(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let n = data.len();
    let mut i = 0usize;
    let mut start: Option<usize> = None;
    while i + 2 < n {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(s) = start {
                let mut e = i;
                while e > s && data[e - 1] == 0 {
                    e -= 1;
                }
                out.push(&data[s..e]);
            }
            i += 3;
            start = Some(i);
        } else {
            i += 1;
        }
    }
    match start {
        Some(s) if s <= n => out.push(&data[s..]),
        None if !data.is_empty() => {
            // Already length-prefixed or a bare NAL: try length-prefixed parse.
            let mut p = 0usize;
            let mut ok = Vec::new();
            while p + 4 <= n {
                let l = u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
                if l == 0 || p + 4 + l > n {
                    ok.clear();
                    break;
                }
                ok.push(&data[p + 4..p + 4 + l]);
                p += 4 + l;
            }
            if p == n && !ok.is_empty() {
                out = ok;
            } else {
                out.push(data);
            }
        }
        _ => {}
    }
    out
}

fn unescape_rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        if b == 0 {
            zeros += 1;
        } else {
            zeros = 0;
        }
        out.push(b);
    }
    out
}

fn avcc(sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    if sps.len() < 4 {
        return None;
    }
    let profile = sps[1];
    let mut v = vec![1, sps[1], sps[2], sps[3], 0xFF, 0xE1];
    v.extend_from_slice(&(sps.len() as u16).to_be_bytes());
    v.extend_from_slice(sps);
    v.push(1);
    v.extend_from_slice(&(pps.len() as u16).to_be_bytes());
    v.extend_from_slice(pps);
    if matches!(profile, 100 | 110 | 122 | 244) {
        // chroma_format_idc = 1 (4:2:0), 8-bit luma/chroma, no SPS extensions.
        v.extend_from_slice(&[0xFC | 1, 0xF8, 0xF8, 0]);
    }
    Some(v)
}

fn hvcc(vps: &[u8], sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    // SPS RBSP after the 2-byte NAL header:
    //   [0]      vps_id(4) max_sub_layers_minus1(3) temporal_id_nesting(1)
    //   [1]      general_profile_space(2) tier(1) profile_idc(5)
    //   [2..6]   general_profile_compatibility_flags
    //   [6..12]  constraint flags
    //   [12]     general_level_idc
    let rbsp = unescape_rbsp(sps.get(2..)?);
    if rbsp.len() < 13 {
        return None;
    }
    let mut v = Vec::with_capacity(64 + vps.len() + sps.len() + pps.len());
    v.push(1);
    v.push(rbsp[1]);
    v.extend_from_slice(&rbsp[2..6]);
    v.extend_from_slice(&rbsp[6..12]);
    v.push(rbsp[12]);
    v.extend_from_slice(&[0xF0, 0x00]); // min_spatial_segmentation_idc = 0
    v.push(0xFC); // parallelismType = 0
    v.push(0xFC | 1); // chroma_format_idc = 1
    v.push(0xF8); // bit_depth_luma_minus8 = 0
    v.push(0xF8); // bit_depth_chroma_minus8 = 0
    v.extend_from_slice(&[0, 0]); // avgFrameRate
    let temporal_nesting = rbsp[0] & 1;
    let num_temporal_layers = ((rbsp[0] >> 1) & 0x07) + 1;
    v.push((num_temporal_layers << 3) | (temporal_nesting << 2) | 3);
    v.push(3);
    for (t, nal) in [(32u8, vps), (33, sps), (34, pps)] {
        v.push(0x80 | t);
        v.extend_from_slice(&1u16.to_be_bytes());
        v.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        v.extend_from_slice(nal);
    }
    Some(v)
}

struct BitReader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl BitReader<'_> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.d.get(self.pos / 8)?;
        let b = (byte >> (7 - (self.pos % 8))) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()?;
        }
        Some(v)
    }
    fn uvlc(&mut self) -> Option<u32> {
        let mut lz = 0;
        while self.bit()? == 0 {
            lz += 1;
            if lz >= 32 {
                return None;
            }
        }
        Some(self.bits(lz)? + (1u32 << lz) - 1)
    }
}

fn av1c(seq_obu: &[u8]) -> Option<Vec<u8>> {
    let ext = seq_obu[0] & 0x04 != 0;
    let hdr = 1 + ext as usize;
    let (_, sl) = read_leb128(&seq_obu[hdr..])?;
    let payload = &seq_obu[hdr + sl..];
    let mut r = BitReader { d: payload, pos: 0 };
    let seq_profile = r.bits(3)?;
    let _still = r.bit()?;
    let reduced = r.bit()?;
    let (level, tier);
    if reduced == 1 {
        level = r.bits(5)?;
        tier = 0;
    } else {
        let timing_info_present = r.bit()?;
        let mut decoder_model_info_present = 0;
        let mut buffer_delay_len = 0;
        if timing_info_present == 1 {
            r.bits(32)?;
            r.bits(32)?;
            if r.bit()? == 1 {
                r.uvlc()?;
            }
            decoder_model_info_present = r.bit()?;
            if decoder_model_info_present == 1 {
                buffer_delay_len = r.bits(5)? + 1;
                r.bits(32)?;
                r.bits(5)?;
                r.bits(5)?;
            }
        }
        let _initial_display_delay_present = r.bit()?;
        let _op_cnt = r.bits(5)?;
        let _idc = r.bits(12)?;
        level = r.bits(5)?;
        tier = if level > 7 { r.bit()? } else { 0 };
        let _ = (decoder_model_info_present, buffer_delay_len);
    }
    let mut v = vec![
        0x81,
        ((seq_profile as u8) << 5) | (level as u8 & 0x1f),
        // tier, high_bitdepth=0, twelve_bit=0, mono=0, subsampling_x=1, subsampling_y=1, csp=0
        ((tier as u8) << 7) | (1 << 3) | (1 << 2),
        0,
    ];
    v.extend_from_slice(seq_obu);
    Some(v)
}

fn read_leb128(d: &[u8]) -> Option<(usize, usize)> {
    let mut v: u64 = 0;
    for i in 0..8 {
        let b = *d.get(i)?;
        v |= ((b & 0x7f) as u64) << (i * 7);
        if b & 0x80 == 0 {
            return Some((v as usize, i + 1));
        }
    }
    None
}

fn write_leb128(out: &mut Vec<u8>, mut v: usize) {
    loop {
        let mut b = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            b |= 0x80;
        }
        out.push(b);
        if v == 0 {
            break;
        }
    }
}

/// Detects a random-access sample in MP4-formatted data (fallback when the encoder does
/// not flag keyframes).
pub fn is_keyframe(codec: Codec, sample: &[u8]) -> bool {
    match codec {
        Codec::H264 | Codec::Hevc => {
            let mut p = 0usize;
            while p + 5 <= sample.len() {
                let l = u32::from_be_bytes([sample[p], sample[p + 1], sample[p + 2], sample[p + 3]]) as usize;
                let h = sample[p + 4];
                let key = if codec == Codec::H264 { h & 0x1f == 5 } else { (16..=21).contains(&((h >> 1) & 0x3f)) };
                if key {
                    return true;
                }
                p += 4 + l;
            }
            false
        }
        Codec::Av1 => sample.first().map(|h| (h >> 3) & 0x0f == 1).unwrap_or(false),
    }
}

/// AudioSpecificConfig for AAC-LC.
pub fn aac_asc(sample_rate: u32, channels: u16) -> Vec<u8> {
    let idx: u16 = match sample_rate {
        96000 => 0,
        88200 => 1,
        64000 => 2,
        48000 => 3,
        44100 => 4,
        32000 => 5,
        24000 => 6,
        22050 => 7,
        16000 => 8,
        _ => 3,
    };
    let v: u16 = (2 << 11) | (idx << 7) | ((channels & 0x0f) << 3);
    v.to_be_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_split_and_convert() {
        let data = [0, 0, 0, 1, 0x67, 1, 2, 3, 0, 0, 1, 0x68, 4, 0, 0, 1, 0x65, 9, 9];
        let mut c = BitstreamConverter::new(Codec::H264);
        let out = c.convert(&data);
        assert_eq!(c.sps.as_deref(), Some(&[0x67, 1, 2, 3][..]));
        assert_eq!(c.pps.as_deref(), Some(&[0x68, 4][..]));
        assert_eq!(out, vec![0, 0, 0, 3, 0x65, 9, 9]);
        assert!(c.config_record().is_some());
    }

    #[test]
    fn asc_lc_48k_stereo() {
        assert_eq!(aac_asc(48000, 2), vec![0x11, 0x90]);
    }

    #[test]
    fn leb() {
        let mut v = Vec::new();
        write_leb128(&mut v, 300);
        assert_eq!(read_leb128(&v), Some((300, 2)));
    }
}
