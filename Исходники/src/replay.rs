//! Ring buffer of encoded packets for Instant Replay.
//!
//! Only compressed data is stored, so memory stays proportional to bitrate × duration.
//! Trimming always happens on GOP boundaries: the oldest retained video packet is a keyframe.

use std::collections::VecDeque;

use crate::media::Packet;
use crate::util::HNS;

pub struct ReplayBuffer {
    max_hns: i64,
    video: VecDeque<Packet>,
    key_pts: VecDeque<i64>,
    audio: Vec<VecDeque<Packet>>,
    bytes: usize,
}

pub struct Snapshot {
    pub base_pts: i64,
    pub video: Vec<Packet>,
    pub audio: Vec<Vec<Packet>>,
}

impl ReplayBuffer {
    pub fn new(seconds: u32, audio_tracks: usize) -> Self {
        ReplayBuffer {
            max_hns: seconds as i64 * HNS,
            video: VecDeque::new(),
            key_pts: VecDeque::new(),
            audio: (0..audio_tracks).map(|_| VecDeque::new()).collect(),
            bytes: 0,
        }
    }

    pub fn set_duration(&mut self, seconds: u32) {
        self.max_hns = seconds as i64 * HNS;
        self.trim();
    }

    pub fn push_video(&mut self, p: Packet) {
        if self.video.is_empty() && !p.key {
            return; // never start a buffer on a non-keyframe
        }
        if p.key {
            self.key_pts.push_back(p.pts);
        }
        self.bytes += p.data.len();
        self.video.push_back(p);
        self.trim();
    }

    pub fn push_audio(&mut self, track: usize, p: Packet) {
        if let Some(q) = self.audio.get_mut(track) {
            self.bytes += p.data.len();
            q.push_back(p);
        }
    }

    fn trim(&mut self) {
        let Some(latest) = self.video.back().map(|p| p.pts) else { return };
        let cutoff = latest - self.max_hns;
        // Drop whole GOPs while the *next* keyframe is still old enough to cover the window.
        while self.key_pts.len() >= 2 && self.key_pts[1] <= cutoff {
            let next_key = self.key_pts[1];
            while let Some(front) = self.video.front() {
                if front.pts >= next_key {
                    break;
                }
                self.bytes -= front.data.len();
                self.video.pop_front();
            }
            self.key_pts.pop_front();
        }
        let start = self.video.front().map(|p| p.pts).unwrap_or(latest);
        for q in &mut self.audio {
            while let Some(front) = q.front() {
                if front.pts >= start {
                    break;
                }
                self.bytes -= front.data.len();
                q.pop_front();
            }
        }
        if self.video.capacity() > self.video.len() * 4 && self.video.capacity() > 4096 {
            self.video.shrink_to(self.video.len() * 2);
        }
    }

    pub fn buffered_secs(&self) -> f64 {
        match (self.video.front(), self.video.back()) {
            (Some(a), Some(b)) => ((b.pts - a.pts) as f64 / HNS as f64).min(self.max_hns as f64 / HNS as f64),
            _ => 0.0,
        }
    }

    /// Empties the buffer (after a save the counter starts again from zero).
    pub fn clear(&mut self) {
        self.video.clear();
        self.key_pts.clear();
        for q in &mut self.audio {
            q.clear();
        }
        self.bytes = 0;
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Copies the last `max` seconds (cheap: packet data is reference-counted).
    pub fn snapshot(&self) -> Option<Snapshot> {
        let latest = self.video.back()?.pts;
        let want = latest - self.max_hns;
        // Latest keyframe at or before the window start; otherwise the oldest one we have.
        let start = self
            .key_pts
            .iter()
            .rev()
            .find(|&&k| k <= want)
            .copied()
            .or_else(|| self.key_pts.front().copied())?;
        let video: Vec<Packet> = self.video.iter().filter(|p| p.pts >= start).cloned().collect();
        if video.is_empty() {
            return None;
        }
        let audio = self
            .audio
            .iter()
            .map(|q| q.iter().filter(|p| p.pts >= start && p.pts <= latest + HNS).cloned().collect())
            .collect();
        Some(Snapshot { base_pts: start, video, audio })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn pkt(pts: i64, key: bool) -> Packet {
        Packet { pts, key, data: Arc::from(vec![0u8; 10].into_boxed_slice()) }
    }

    #[test]
    fn trims_on_gop_boundaries() {
        let mut b = ReplayBuffer::new(15, 1);
        // 60 fps, keyframe every 2 s, 60 s of data
        for i in 0..3600i64 {
            let pts = i * HNS / 60;
            b.push_video(pkt(pts, i % 120 == 0));
            b.push_audio(0, pkt(pts, true));
        }
        let secs = b.buffered_secs();
        assert!(secs >= 15.0 && secs <= 15.0, "buffered {secs}");
        let s = b.snapshot().unwrap();
        assert!(s.video[0].key);
        let dur = (s.video.last().unwrap().pts - s.base_pts) as f64 / HNS as f64;
        assert!(dur >= 15.0 && dur < 17.1, "clip {dur}");
        assert!(s.audio[0].first().unwrap().pts >= s.base_pts);
    }
}
