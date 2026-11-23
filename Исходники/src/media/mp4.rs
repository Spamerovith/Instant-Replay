//! Minimal, standards-conformant ISO-BMFF (MP4) writer.
//!
//! * Replay clips are written "fast-start" (moov before mdat) from an in-memory packet list.
//! * Recordings are streamed: samples go straight to disk, moov is appended on finish.
//!
//! 64-bit boxes (mvhd/tkhd/mdhd v1, co64, large mdat) are used throughout, so files of any
//! length stay valid.

use std::fs::File;
use std::io::{BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

use crate::config::Codec;
use crate::media::{AudioInfo, Packet, VideoInfo, AAC_FRAME};
use crate::util::HNS;

#[derive(Clone)]
pub enum TrackKind {
    Video(VideoInfo),
    Audio(AudioInfo),
}

struct Sample {
    size: u32,
    ts: i64, // in track timescale, relative to clip base
    key: bool,
}

struct TrackState {
    kind: TrackKind,
    timescale: u32,
    samples: Vec<Sample>,
    /// (offset relative to mdat payload start, sample count)
    chunks: Vec<(u64, u32)>,
    first_pts: Option<i64>,
}

impl TrackState {
    fn new(kind: TrackKind) -> Self {
        let timescale = match &kind {
            TrackKind::Video(v) => {
                if 90_000 % v.fps.max(1) == 0 {
                    90_000
                } else {
                    v.fps * 1000
                }
            }
            TrackKind::Audio(a) => a.sample_rate,
        };
        TrackState { kind, timescale, samples: Vec::new(), chunks: Vec::new(), first_pts: None }
    }

    fn nominal_duration(&self) -> i64 {
        match &self.kind {
            TrackKind::Video(v) => (self.timescale as i64 / v.fps.max(1) as i64).max(1),
            TrackKind::Audio(_) => AAC_FRAME,
        }
    }

    fn durations(&self) -> Vec<u32> {
        let n = self.samples.len();
        let nominal = self.nominal_duration();
        let mut d = Vec::with_capacity(n);
        for i in 0..n {
            let dur = if i + 1 < n { self.samples[i + 1].ts - self.samples[i].ts } else { nominal };
            d.push(dur.clamp(1, u32::MAX as i64) as u32);
        }
        d
    }

    fn media_duration(&self) -> u64 {
        self.durations().iter().map(|&d| d as u64).sum()
    }
}

/// Sample tables for all tracks; independent from where the bytes physically go.
pub struct Layout {
    tracks: Vec<TrackState>,
    rel_pos: u64,
    last_track: Option<usize>,
    base_pts: i64,
}

impl Layout {
    pub fn new(tracks: Vec<TrackKind>, base_pts: i64) -> Self {
        Layout {
            tracks: tracks.into_iter().map(TrackState::new).collect(),
            rel_pos: 0,
            last_track: None,
            base_pts,
        }
    }

    /// Registers a sample written at the current relative position.
    fn add(&mut self, track: usize, pts: i64, key: bool, size: usize) {
        let t = &mut self.tracks[track];
        let rel = (pts - self.base_pts).max(0);
        if t.first_pts.is_none() {
            t.first_pts = Some(rel);
        }
        let ts = ((rel as i128 * t.timescale as i128 + HNS as i128 / 2) / HNS as i128) as i64;
        // Keep timestamps strictly increasing (guards against duplicate pts after rounding).
        let ts = match t.samples.last() {
            Some(prev) if ts <= prev.ts => prev.ts + 1,
            _ => ts,
        };
        t.samples.push(Sample { size: size as u32, ts, key });
        if self.last_track == Some(track) {
            if let Some(c) = t.chunks.last_mut() {
                c.1 += 1;
            }
        } else {
            t.chunks.push((self.rel_pos, 1));
        }
        self.last_track = Some(track);
        self.rel_pos += size as u64;
    }

    pub fn duration_secs(&self) -> f64 {
        self.tracks
            .iter()
            .map(|t| {
                let start = t.first_pts.unwrap_or(0) as f64 / HNS as f64;
                start + t.media_duration() as f64 / t.timescale as f64
            })
            .fold(0.0, f64::max)
    }

    fn movie_duration(&self) -> u64 {
        (self.duration_secs() * 1000.0).round() as u64
    }

    fn ftyp(&self) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend_from_slice(b"isom");
        c.extend_from_slice(&0x200u32.to_be_bytes());
        c.extend_from_slice(b"isomiso2mp41");
        for t in &self.tracks {
            if let TrackKind::Video(v) = &t.kind {
                c.extend_from_slice(match v.codec {
                    Codec::H264 => b"avc1",
                    Codec::Hevc => b"hvc1",
                    Codec::Av1 => b"av01",
                });
            }
        }
        bx(b"ftyp", &c)
    }

    /// Builds the moov box. `base` is the absolute file offset of the first mdat payload byte.
    fn moov(&self, base: u64) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend(mvhd(self.movie_duration(), self.tracks.len() as u32 + 1));
        // The default (enabled) audio track is the mix when there is one, otherwise the first.
        let has_mix = self.tracks.iter().any(|t| matches!(&t.kind, TrackKind::Audio(a) if a.name == "Mix"));
        let mut audio_seen = false;
        for (i, t) in self.tracks.iter().enumerate() {
            let is_default = match &t.kind {
                TrackKind::Audio(a) => if has_mix { a.name == "Mix" } else { !audio_seen },
                _ => false,
            };
            if matches!(t.kind, TrackKind::Audio(_)) {
                audio_seen = true;
            }
            c.extend(trak(t, i as u32 + 1, base, is_default));
        }
        bx(b"moov", &c)
    }
}

// ------------------------------------------------------------------ box helpers

fn bx(typ: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(content.len() + 8);
    v.extend_from_slice(&((content.len() + 8) as u32).to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(content);
    v
}

fn full(typ: &[u8; 4], version: u8, flags: u32, content: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(content.len() + 4);
    c.push(version);
    c.extend_from_slice(&flags.to_be_bytes()[1..]);
    c.extend_from_slice(content);
    bx(typ, &c)
}

const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn put_matrix(c: &mut Vec<u8>) {
    for m in MATRIX {
        c.extend_from_slice(&m.to_be_bytes());
    }
}

fn mvhd(duration_ms: u64, next_track: u32) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&0u64.to_be_bytes());
    c.extend_from_slice(&0u64.to_be_bytes());
    c.extend_from_slice(&1000u32.to_be_bytes());
    c.extend_from_slice(&duration_ms.to_be_bytes());
    c.extend_from_slice(&0x0001_0000u32.to_be_bytes());
    c.extend_from_slice(&0x0100u16.to_be_bytes());
    c.extend_from_slice(&[0u8; 10]);
    put_matrix(&mut c);
    c.extend_from_slice(&[0u8; 24]);
    c.extend_from_slice(&next_track.to_be_bytes());
    full(b"mvhd", 1, 0, &c)
}

fn trak(t: &TrackState, id: u32, base: u64, default_audio: bool) -> Vec<u8> {
    let media_dur = t.media_duration();
    let start_hns = t.first_pts.unwrap_or(0);
    let media_ms = (media_dur as u128 * 1000 / t.timescale as u128) as u64;
    let start_ms = (start_hns / 10_000) as u64;
    let (is_video, w, h) = match &t.kind {
        TrackKind::Video(v) => (true, v.width, v.height),
        TrackKind::Audio(_) => (false, 0, 0),
    };

    // tkhd
    let mut k = Vec::new();
    k.extend_from_slice(&0u64.to_be_bytes());
    k.extend_from_slice(&0u64.to_be_bytes());
    k.extend_from_slice(&id.to_be_bytes());
    k.extend_from_slice(&0u32.to_be_bytes());
    k.extend_from_slice(&(media_ms + start_ms).to_be_bytes());
    k.extend_from_slice(&[0u8; 8]);
    k.extend_from_slice(&0i16.to_be_bytes()); // layer
    k.extend_from_slice(&(if is_video { 0i16 } else { 1i16 }).to_be_bytes()); // alternate group
    k.extend_from_slice(&(if is_video { 0u16 } else { 0x0100 }).to_be_bytes());
    k.extend_from_slice(&0u16.to_be_bytes());
    put_matrix(&mut k);
    k.extend_from_slice(&(w << 16).to_be_bytes());
    k.extend_from_slice(&(h << 16).to_be_bytes());
    // flags: enabled | in movie. Extra audio tracks stay in the movie but are not enabled by
    // default, so players play the main mix while editors still import every track.
    let flags = if is_video || default_audio { 3 } else { 2 };
    let mut c = full(b"tkhd", 1, flags, &k);

    // edts: delay tracks that start after the clip base.
    if start_ms > 0 {
        let mut e = Vec::new();
        e.extend_from_slice(&2u32.to_be_bytes());
        e.extend_from_slice(&start_ms.to_be_bytes());
        e.extend_from_slice(&(-1i64).to_be_bytes());
        e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        e.extend_from_slice(&media_ms.to_be_bytes());
        e.extend_from_slice(&0i64.to_be_bytes());
        e.extend_from_slice(&0x0001_0000u32.to_be_bytes());
        c.extend(bx(b"edts", &full(b"elst", 1, 0, &e)));
    }

    // mdia
    let mut m = Vec::new();
    {
        let mut d = Vec::new();
        d.extend_from_slice(&0u64.to_be_bytes());
        d.extend_from_slice(&0u64.to_be_bytes());
        d.extend_from_slice(&t.timescale.to_be_bytes());
        d.extend_from_slice(&media_dur.to_be_bytes());
        d.extend_from_slice(&0x55c4u16.to_be_bytes()); // 'und'
        d.extend_from_slice(&0u16.to_be_bytes());
        m.extend(full(b"mdhd", 1, 0, &d));
    }
    {
        let (handler, name): (&[u8; 4], &str) = match &t.kind {
            TrackKind::Video(_) => (b"vide", "Video"),
            TrackKind::Audio(a) => (b"soun", a.name),
        };
        let mut d = Vec::new();
        d.extend_from_slice(&0u32.to_be_bytes());
        d.extend_from_slice(handler);
        d.extend_from_slice(&[0u8; 12]);
        d.extend_from_slice(name.as_bytes());
        d.push(0);
        m.extend(full(b"hdlr", 0, 0, &d));
    }
    {
        let mut mi = Vec::new();
        if is_video {
            mi.extend(full(b"vmhd", 0, 1, &[0u8; 8]));
        } else {
            mi.extend(full(b"smhd", 0, 0, &[0u8; 4]));
        }
        let url = full(b"url ", 0, 1, &[]);
        let mut dref = 1u32.to_be_bytes().to_vec();
        dref.extend(url);
        mi.extend(bx(b"dinf", &full(b"dref", 0, 0, &dref)));
        mi.extend(stbl(t, base));
        m.extend(bx(b"minf", &mi));
    }
    c.extend(bx(b"mdia", &m));
    bx(b"trak", &c)
}

fn stbl(t: &TrackState, base: u64) -> Vec<u8> {
    let mut s = Vec::new();

    // stsd
    let entry = match &t.kind {
        TrackKind::Video(v) => visual_entry(v),
        TrackKind::Audio(a) => audio_entry(a),
    };
    let mut d = 1u32.to_be_bytes().to_vec();
    d.extend(entry);
    s.extend(full(b"stsd", 0, 0, &d));

    // stts (run-length durations)
    let durs = t.durations();
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for d in durs {
        match runs.last_mut() {
            Some(r) if r.1 == d => r.0 += 1,
            _ => runs.push((1, d)),
        }
    }
    let mut d = (runs.len() as u32).to_be_bytes().to_vec();
    for (n, du) in runs {
        d.extend_from_slice(&n.to_be_bytes());
        d.extend_from_slice(&du.to_be_bytes());
    }
    s.extend(full(b"stts", 0, 0, &d));

    // stss (video only; audio samples are all sync)
    if matches!(t.kind, TrackKind::Video(_)) {
        let keys: Vec<u32> =
            t.samples.iter().enumerate().filter(|(_, x)| x.key).map(|(i, _)| i as u32 + 1).collect();
        let mut d = (keys.len() as u32).to_be_bytes().to_vec();
        for k in keys {
            d.extend_from_slice(&k.to_be_bytes());
        }
        s.extend(full(b"stss", 0, 0, &d));
    }

    // stsc
    let mut entries: Vec<(u32, u32)> = Vec::new();
    for (i, &(_, n)) in t.chunks.iter().enumerate() {
        if entries.last().map(|e| e.1) != Some(n) {
            entries.push((i as u32 + 1, n));
        }
    }
    let mut d = (entries.len() as u32).to_be_bytes().to_vec();
    for (first, n) in entries {
        d.extend_from_slice(&first.to_be_bytes());
        d.extend_from_slice(&n.to_be_bytes());
        d.extend_from_slice(&1u32.to_be_bytes());
    }
    s.extend(full(b"stsc", 0, 0, &d));

    // stsz
    let mut d = 0u32.to_be_bytes().to_vec();
    d.extend_from_slice(&(t.samples.len() as u32).to_be_bytes());
    for x in &t.samples {
        d.extend_from_slice(&x.size.to_be_bytes());
    }
    s.extend(full(b"stsz", 0, 0, &d));

    // co64
    let mut d = (t.chunks.len() as u32).to_be_bytes().to_vec();
    for &(off, _) in &t.chunks {
        d.extend_from_slice(&(base + off).to_be_bytes());
    }
    s.extend(full(b"co64", 0, 0, &d));

    bx(b"stbl", &s)
}

fn visual_entry(v: &VideoInfo) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&[0u8; 6]);
    c.extend_from_slice(&1u16.to_be_bytes());
    c.extend_from_slice(&[0u8; 16]);
    c.extend_from_slice(&(v.width as u16).to_be_bytes());
    c.extend_from_slice(&(v.height as u16).to_be_bytes());
    c.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    c.extend_from_slice(&0x0048_0000u32.to_be_bytes());
    c.extend_from_slice(&0u32.to_be_bytes());
    c.extend_from_slice(&1u16.to_be_bytes());
    let mut name = [0u8; 32];
    let label = b"Instant Replay";
    name[0] = label.len() as u8;
    name[1..1 + label.len()].copy_from_slice(label);
    c.extend_from_slice(&name);
    c.extend_from_slice(&0x0018u16.to_be_bytes());
    c.extend_from_slice(&(-1i16).to_be_bytes());
    let (typ, cfg): (&[u8; 4], &[u8; 4]) = match v.codec {
        Codec::H264 => (b"avc1", b"avcC"),
        Codec::Hevc => (b"hvc1", b"hvcC"),
        Codec::Av1 => (b"av01", b"av1C"),
    };
    c.extend(bx(cfg, &v.config));
    // BT.709 limited range — matches the GPU colour conversion.
    let mut colr = b"nclx".to_vec();
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.extend_from_slice(&1u16.to_be_bytes());
    colr.push(0);
    c.extend(bx(b"colr", &colr));
    let mut pasp = 1u32.to_be_bytes().to_vec();
    pasp.extend_from_slice(&1u32.to_be_bytes());
    c.extend(bx(b"pasp", &pasp));
    bx(typ, &c)
}

fn descriptor(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let len = body.len() as u32;
    v.extend_from_slice(&[
        0x80 | ((len >> 21) & 0x7f) as u8,
        0x80 | ((len >> 14) & 0x7f) as u8,
        0x80 | ((len >> 7) & 0x7f) as u8,
        (len & 0x7f) as u8,
    ]);
    v.extend_from_slice(body);
    v
}

fn audio_entry(a: &AudioInfo) -> Vec<u8> {
    let mut c = Vec::new();
    c.extend_from_slice(&[0u8; 6]);
    c.extend_from_slice(&1u16.to_be_bytes());
    c.extend_from_slice(&[0u8; 8]);
    c.extend_from_slice(&a.channels.to_be_bytes());
    c.extend_from_slice(&16u16.to_be_bytes());
    c.extend_from_slice(&0u16.to_be_bytes());
    c.extend_from_slice(&0u16.to_be_bytes());
    c.extend_from_slice(&(a.sample_rate << 16).to_be_bytes());

    let mut dcd = vec![0x40, 0x15, 0, 0, 0];
    dcd.extend_from_slice(&a.bitrate.to_be_bytes());
    dcd.extend_from_slice(&a.bitrate.to_be_bytes());
    dcd.extend(descriptor(0x05, &a.asc));
    let mut es = vec![0, 0, 0]; // ES_ID = 0, flags = 0
    es.extend(descriptor(0x04, &dcd));
    es.extend(descriptor(0x06, &[0x02]));
    c.extend(full(b"esds", 0, 0, &descriptor(0x03, &es)));
    bx(b"mp4a", &c)
}

// ------------------------------------------------------------------ writers

/// Writes a complete fast-start clip from buffered packets.
/// `tracks[i]` corresponds to `packets[i]`. Returns the clip duration in seconds.
pub fn write_clip(
    path: &Path,
    tracks: Vec<TrackKind>,
    packets: Vec<Vec<Packet>>,
    base_pts: i64,
) -> std::io::Result<f64> {
    // Interleave by timestamp (stable: video first on ties).
    let mut order: Vec<(i64, usize, usize)> = Vec::new();
    for (ti, list) in packets.iter().enumerate() {
        for (pi, p) in list.iter().enumerate() {
            order.push((p.pts, ti, pi));
        }
    }
    order.sort_by_key(|&(pts, ti, pi)| (pts, ti, pi));

    let mut layout = Layout::new(tracks, base_pts);
    for &(_, ti, pi) in &order {
        let p = &packets[ti][pi];
        layout.add(ti, p.pts, p.key, p.data.len());
    }
    let ftyp = layout.ftyp();
    let moov_len = layout.moov(0).len() as u64;
    let base = ftyp.len() as u64 + moov_len + 16;
    let moov = layout.moov(base);
    debug_assert_eq!(moov.len() as u64, moov_len);

    let tmp = path.with_extension("mp4.part");
    {
        let f = File::create(&tmp)?;
        let mut w = BufWriter::with_capacity(4 << 20, f);
        w.write_all(&ftyp)?;
        w.write_all(&moov)?;
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&(layout.rel_pos + 16).to_be_bytes())?;
        for &(_, ti, pi) in &order {
            w.write_all(&packets[ti][pi].data)?;
        }
        let f = w.into_inner().map_err(|e| e.into_error())?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(layout.duration_secs())
}

/// Streaming recorder: samples are written as they arrive, moov is appended on `finish`.
pub struct StreamWriter {
    w: BufWriter<File>,
    layout: Layout,
    mdat_start: u64,
    data_start: u64,
    pub bytes: u64,
}

impl StreamWriter {
    pub fn create(path: &Path, tracks: Vec<TrackKind>, base_pts: i64) -> std::io::Result<Self> {
        let layout = Layout::new(tracks, base_pts);
        let f = File::create(path)?;
        let mut w = BufWriter::with_capacity(8 << 20, f);
        let ftyp = layout.ftyp();
        w.write_all(&ftyp)?;
        let mdat_start = ftyp.len() as u64;
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(b"mdat")?;
        w.write_all(&0u64.to_be_bytes())?;
        Ok(StreamWriter { w, layout, mdat_start, data_start: mdat_start + 16, bytes: 0 })
    }

    pub fn write(&mut self, track: usize, p: &Packet) -> std::io::Result<()> {
        self.w.write_all(&p.data)?;
        self.layout.add(track, p.pts, p.key, p.data.len());
        self.bytes += p.data.len() as u64;
        Ok(())
    }

    pub fn finish(mut self) -> std::io::Result<f64> {
        let moov = self.layout.moov(self.data_start);
        let mdat_size = 16 + self.layout.rel_pos;
        self.w.write_all(&moov)?;
        self.w.flush()?;
        let mut f = self.w.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(self.mdat_start + 8))?;
        f.write_all(&mdat_size.to_be_bytes())?;
        f.sync_all()?;
        Ok(self.layout.duration_secs())
    }
}

/// Duration (seconds) of an MP4 file from its `moov/mvhd` box, without reading the media data.
pub fn probe_duration(path: &Path) -> Option<f64> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos)).ok()?;
        let mut h = [0u8; 16];
        f.read_exact(&mut h[..8]).ok()?;
        let mut size = u32::from_be_bytes(h[0..4].try_into().ok()?) as u64;
        let mut hdr = 8u64;
        if size == 1 {
            f.read_exact(&mut h[8..16]).ok()?;
            size = u64::from_be_bytes(h[8..16].try_into().ok()?);
            hdr = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < hdr {
            return None;
        }
        if &h[4..8] == b"moov" {
            // The first child of moov is mvhd.
            let mut m = [0u8; 8 + 4 + 8 + 8 + 4 + 8];
            f.seek(SeekFrom::Start(pos + hdr)).ok()?;
            f.read_exact(&mut m[..8]).ok()?;
            if &m[4..8] != b"mvhd" {
                return None;
            }
            f.read_exact(&mut m[8..12]).ok()?;
            return if m[8] == 1 {
                f.read_exact(&mut m[12..40]).ok()?;
                let ts = u32::from_be_bytes(m[28..32].try_into().ok()?) as f64;
                let d = u64::from_be_bytes(m[32..40].try_into().ok()?) as f64;
                (ts > 0.0).then(|| d / ts)
            } else {
                f.read_exact(&mut m[12..28]).ok()?;
                let ts = u32::from_be_bytes(m[20..24].try_into().ok()?) as f64;
                let d = u32::from_be_bytes(m[24..28].try_into().ok()?) as f64;
                (ts > 0.0).then(|| d / ts)
            };
        }
        pos += size;
    }
    None
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn probe_duration_reads_mvhd_v0_and_v1() {
        let dir = std::env::temp_dir().join(format!("ir-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // v0: version/flags, creation, modification, timescale 1000, duration 12500 -> 12.5 s
        let mut v0 = vec![0u8; 4];
        v0.extend_from_slice(&[0; 8]);
        v0.extend_from_slice(&1000u32.to_be_bytes());
        v0.extend_from_slice(&12500u32.to_be_bytes());
        let mut file = boxed(b"ftyp", b"isom0000");
        file.extend(boxed(b"moov", &boxed(b"mvhd", &v0)));
        let p0 = dir.join("v0.mp4");
        std::fs::write(&p0, &file).unwrap();
        assert_eq!(probe_duration(&p0), Some(12.5));
        // v1: 64-bit times, timescale 90000, duration 450000 -> 5 s
        let mut v1 = vec![1u8, 0, 0, 0];
        v1.extend_from_slice(&[0; 16]);
        v1.extend_from_slice(&90000u32.to_be_bytes());
        v1.extend_from_slice(&450000u64.to_be_bytes());
        let mut file = boxed(b"ftyp", b"isom0000");
        file.extend(boxed(b"moov", &boxed(b"mvhd", &v1)));
        let p1 = dir.join("v1.mp4");
        std::fs::write(&p1, &file).unwrap();
        assert_eq!(probe_duration(&p1), Some(5.0));
        std::fs::write(dir.join("junk.mp4"), b"nope").unwrap();
        assert_eq!(probe_duration(&dir.join("junk.mp4")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
