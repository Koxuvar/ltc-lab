//! LTC frame model: SMPTE 12M 80-bit frame, BCD time fields, sync word.
//! Bit 0 is transmitted first, bit 79 last; low bit of each field sits at the
//! lower bit number (little-endian within the field).

use std::fmt;

pub const SYNC: [bool; 16] = [
    false, false, true, true, true, true, true, true, true, true, true, true, true, true, false,
    true,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timecode {
    pub hours: u8,
    pub minutes: u8,
    pub seconds: u8,
    pub frames: u8,
    pub drop_frame: bool,
}

impl Timecode {
    /// Parse "HH:MM:SS:FF". A ';' before the frames field marks drop-frame,
    /// following the usual convention (e.g. "01:00:00;00").
    pub fn parse(s: &str) -> Result<Timecode, String> {
        let drop_frame = s.contains(';');
        let parts: Vec<&str> = s.split(|c| c == ':' || c == ';').collect();
        if parts.len() != 4 {
            return Err(format!("expected HH:MM:SS:FF, got {s:?}"));
        }
        let n = |i: usize| {
            parts[i]
                .parse::<u8>()
                .map_err(|_| format!("bad field {:?}", parts[i]))
        };
        Ok(Timecode {
            hours: n(0)?,
            minutes: n(1)?,
            seconds: n(2)?,
            frames: n(3)?,
            drop_frame,
        })
    }
}

impl fmt::Display for Timecode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let sep = if self.drop_frame { ';' } else { ':' };
        write!(
            f,
            "{:02}:{:02}:{:02}{}{:02}",
            self.hours, self.minutes, self.seconds, sep, self.frames
        )
    }
}

fn set_field(bits: &mut [bool; 80], start: usize, n: usize, value: u8) {
    for i in 0..n {
        bits[start + i] = (value >> i) & 1 == 1;
    }
}

fn get_field(bits: &[bool], start: usize, n: usize) -> u8 {
    let mut v = 0u8;
    for i in 0..n {
        if bits[start + i] {
            v |= 1 << i;
        }
    }
    v
}

pub fn encode_frame(tc: Timecode) -> [bool; 80] {
    let mut b = [false; 80];
    set_field(&mut b, 0, 4, tc.frames % 10);
    set_field(&mut b, 8, 2, tc.frames / 10);
    b[10] = tc.drop_frame;
    set_field(&mut b, 16, 4, tc.seconds % 10);
    set_field(&mut b, 24, 3, tc.seconds / 10);
    set_field(&mut b, 32, 4, tc.minutes % 10);
    set_field(&mut b, 40, 3, tc.minutes / 10);
    set_field(&mut b, 48, 4, tc.hours % 10);
    set_field(&mut b, 56, 2, tc.hours / 10);
    for i in 0..16 {
        b[64 + i] = SYNC[i];
    }
    // Polarity-correction bit (24/30 fps): even number of logical 0s overall.
    if b.iter().filter(|&&x| !x).count() % 2 != 0 {
        b[27] = true;
    }
    b
}

pub fn decode_frame(bits: &[bool]) -> Timecode {
    Timecode {
        hours: get_field(bits, 56, 2) * 10 + get_field(bits, 48, 4),
        minutes: get_field(bits, 40, 3) * 10 + get_field(bits, 32, 4),
        seconds: get_field(bits, 24, 3) * 10 + get_field(bits, 16, 4),
        frames: get_field(bits, 8, 2) * 10 + get_field(bits, 0, 4),
        drop_frame: bits[10],
    }
}

/// Increment by one frame. Drop-frame renumbering (skipping frames 00/01 at
/// minute boundaries except every tenth minute) is NOT applied yet.
pub fn next_frame(tc: Timecode, fps: u8) -> Timecode {
    let mut t = tc;
    t.frames += 1;
    if t.frames >= fps {
        t.frames = 0;
        t.seconds += 1;
        if t.seconds >= 60 {
            t.seconds = 0;
            t.minutes += 1;
            if t.minutes >= 60 {
                t.minutes = 0;
                t.hours = (t.hours + 1) % 24;
            }
        }
    }
    t
}

pub fn sequence(start: Timecode, count: u32, fps: u8) -> Vec<Timecode> {
    let mut v = Vec::with_capacity(count as usize);
    let mut t = start;
    for _ in 0..count {
        v.push(t);
        t = next_frame(t, fps);
    }
    v
}
