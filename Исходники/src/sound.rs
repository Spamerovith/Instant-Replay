//! Short synthesized feedback chimes (no audio files shipped).

use std::sync::OnceLock;

use windows::core::PCWSTR;
use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};

#[derive(Clone, Copy)]
pub enum Chime {
    Saved,
    RecStart,
    RecStop,
    Toggle,
    Error,
}

fn synth(notes: &[(f32, f32)], gap: f32) -> Vec<u8> {
    const RATE: u32 = 44_100;
    let mut pcm: Vec<i16> = Vec::new();
    for &(freq, dur) in notes {
        let n = (dur * RATE as f32) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // Soft attack, exponential decay: a quiet "glass" tick that cuts through game audio.
            let attack = (t / 0.004).min(1.0);
            let decay = (-t * 9.0 / dur).exp();
            let s = (2.0 * std::f32::consts::PI * freq * t).sin() * 0.6
                + (2.0 * std::f32::consts::PI * freq * 2.0 * t).sin() * 0.15;
            pcm.push((s * attack * decay * 0.28 * i16::MAX as f32) as i16);
        }
        for _ in 0..(gap * RATE as f32) as usize {
            pcm.push(0);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for s in pcm {
        w.extend_from_slice(&s.to_le_bytes());
    }
    w
}

fn wav(c: Chime) -> &'static [u8] {
    static SAVED: OnceLock<Vec<u8>> = OnceLock::new();
    static START: OnceLock<Vec<u8>> = OnceLock::new();
    static STOP: OnceLock<Vec<u8>> = OnceLock::new();
    static TOGGLE: OnceLock<Vec<u8>> = OnceLock::new();
    static ERROR: OnceLock<Vec<u8>> = OnceLock::new();
    match c {
        Chime::Saved => SAVED.get_or_init(|| synth(&[(1046.5, 0.09), (1568.0, 0.16)], 0.01)),
        Chime::RecStart => START.get_or_init(|| synth(&[(880.0, 0.14)], 0.0)),
        Chime::RecStop => STOP.get_or_init(|| synth(&[(880.0, 0.08), (659.3, 0.14)], 0.01)),
        Chime::Toggle => TOGGLE.get_or_init(|| synth(&[(1318.5, 0.08)], 0.0)),
        Chime::Error => ERROR.get_or_init(|| synth(&[(392.0, 0.12), (329.6, 0.18)], 0.02)),
    }
}

pub fn play(c: Chime) {
    let data = wav(c);
    unsafe {
        let _ = PlaySoundW(PCWSTR(data.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}
