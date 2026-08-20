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
    ///
    /// Validates against generic BCD-representable ranges (hours<24,
    /// minutes/seconds<60, frames<30); the exact frame upper bound actually
    /// depends on the target fps, which isn't known at parse time, so a
    /// too-large frame number for a given rate (e.g. "..:29" at 24 fps) isn't
    /// caught here.
    pub fn parse(s: &str) -> Result<Timecode, String> {
        let drop_frame = s.contains(';');
        let parts: Vec<&str> = s.split([':', ';']).collect();
        if parts.len() != 4 {
            return Err(format!("expected HH:MM:SS:FF, got {s:?}"));
        }
        let n = |i: usize| {
            parts[i]
                .parse::<u8>()
                .map_err(|_| format!("bad field {:?}", parts[i]))
        };
        let tc = Timecode {
            hours: n(0)?,
            minutes: n(1)?,
            seconds: n(2)?,
            frames: n(3)?,
            drop_frame,
        };
        if tc.hours >= 24 || tc.minutes >= 60 || tc.seconds >= 60 || tc.frames >= 30 {
            return Err(format!(
                "field out of range in {s:?}: hours<24, minutes/seconds<60, frames<30"
            ));
        }
        Ok(tc)
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

/// `fps` is the NOMINAL frame rate (24, 25, or 30) used only to place the
/// polarity-correction bit correctly; see below.
pub fn encode_frame(tc: Timecode, fps: u8) -> [bool; 80] {
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
    b[64..80].copy_from_slice(&SYNC);
    // Polarity-correction bit: SMPTE 12M puts it at bit 27 for 24/30 fps, but
    // at bit 59 for 25 fps — bits 27/43/59 rotate between the polarity bit
    // and two binary-group-flag bits (BGF0/BGF2) depending on rate. We never
    // emit a user-bits format, so the BGF slots are correctly left at 0; only
    // the polarity slot needs setting, for an even number of logical 0s
    // overall.
    let polarity_bit = if fps == 25 { 59 } else { 27 };
    if b.iter().filter(|&&x| !x).count() % 2 != 0 {
        b[polarity_bit] = true;
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

/// Increment by one frame. `fps` is the NOMINAL frame count per second used for
/// numbering (30 for 29.97, 24 for 23.976). When `drop_frame` is set, frame
/// numbers 00 and 01 are skipped at the top of every minute EXCEPT every tenth
/// minute (00, 10, 20, 30, 40, 50) — which keeps the running count aligned to
/// wall-clock time despite the 29.97 vs 30 discrepancy.
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
            if t.drop_frame && !t.minutes.is_multiple_of(10) {
                t.frames = 2; // skip 00 and 01
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tc(h: u8, m: u8, s: u8, f: u8, d: bool) -> Timecode {
        Timecode {
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
            drop_frame: d,
        }
    }

    #[test]
    fn bcd_field_placement() {
        // Independent oracle: frames=23 => units 3 (bits 0-3, LSB first),
        // tens 2 (bits 8-9). Catches any bit-offset drift in encode_frame.
        let b = encode_frame(tc(0, 0, 0, 23, false), 30);
        assert_eq!(&b[0..4], &[true, true, false, false]); // 3 = 0b0011 LSB-first
        assert_eq!(&b[8..10], &[false, true]); // 2 = 0b10 LSB-first
    }

    #[test]
    fn sync_word_present() {
        assert_eq!(&encode_frame(tc(1, 2, 3, 4, false), 30)[64..80], &SYNC[..]);
    }

    #[test]
    fn frame_layer_roundtrip_sweep() {
        for h in [0u8, 1, 12, 23] {
            for m in [0u8, 7, 59] {
                for s in [0u8, 30, 59] {
                    for f in [0u8, 15, 29] {
                        for fps in [24u8, 25, 30] {
                            let t = tc(h, m, s, f, false);
                            assert_eq!(
                                decode_frame(&encode_frame(t, fps)),
                                t,
                                "roundtrip {t} @ {fps}fps"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn polarity_bit_at_27_for_24_and_30_fps() {
        for fps in [24u8, 30] {
            let b = encode_frame(tc(1, 2, 3, 4, false), fps);
            // BGF0/BGF2 slots (43, 59) stay 0 since we emit no user-bits format.
            assert!(!b[59], "bit 59 (BGF2) should be 0 at {fps}fps");
            assert_eq!(b.iter().filter(|&&x| !x).count() % 2, 0, "odd zero count at {fps}fps");
        }
    }

    #[test]
    fn polarity_bit_at_59_for_25_fps() {
        let b = encode_frame(tc(1, 2, 3, 4, false), 25);
        // BGF0 slot (27) stays 0 since we emit no user-bits format.
        assert!(!b[27], "bit 27 (BGF0) should be 0 at 25fps");
        assert_eq!(b.iter().filter(|&&x| !x).count() % 2, 0, "odd zero count at 25fps");
    }

    #[test]
    fn parse_and_display() {
        assert_eq!(
            Timecode::parse("01:02:03:04").unwrap(),
            tc(1, 2, 3, 4, false)
        );
        assert!(Timecode::parse("01:02:03;04").unwrap().drop_frame);
        assert_eq!(format!("{}", tc(1, 2, 3, 4, false)), "01:02:03:04");
        assert_eq!(format!("{}", tc(1, 2, 3, 4, true)), "01:02:03;04");
    }

    #[test]
    fn parse_rejects_out_of_range_fields() {
        assert!(Timecode::parse("24:00:00:00").is_err(), "hours must be <24");
        assert!(Timecode::parse("00:60:00:00").is_err(), "minutes must be <60");
        assert!(Timecode::parse("00:00:60:00").is_err(), "seconds must be <60");
        assert!(Timecode::parse("00:00:00:30").is_err(), "frames must be <30");
        assert!(Timecode::parse("99:99:99:99").is_err());
        assert!(Timecode::parse("23:59:59:29").is_ok(), "max valid values");
    }

    #[test]
    fn ndf_no_skip_at_minute() {
        assert_eq!(
            next_frame(tc(1, 0, 59, 29, false), 30),
            tc(1, 1, 0, 0, false)
        );
    }

    #[test]
    fn df_skips_at_normal_minute() {
        assert_eq!(next_frame(tc(1, 0, 59, 29, true), 30), tc(1, 1, 0, 2, true));
        assert_eq!(next_frame(tc(1, 1, 59, 29, true), 30), tc(1, 2, 0, 2, true));
    }

    #[test]
    fn df_no_skip_at_tenth_minute() {
        assert_eq!(
            next_frame(tc(1, 9, 59, 29, true), 30),
            tc(1, 10, 0, 0, true)
        );
        assert_eq!(
            next_frame(tc(1, 19, 59, 29, true), 30),
            tc(1, 20, 0, 0, true)
        );
    }

    #[test]
    fn df_no_skip_within_second_or_nonminute_rollover() {
        assert_eq!(
            next_frame(tc(1, 5, 10, 15, true), 30),
            tc(1, 5, 10, 16, true)
        );
        assert_eq!(
            next_frame(tc(1, 5, 10, 29, true), 30),
            tc(1, 5, 11, 0, true)
        );
    }

    #[test]
    fn df_ten_minute_realignment() {
        // 10 min of 29.97 DF is exactly 17982 frame numbers and realigns to
        // 00:10:00;00 — the reason the tenth minute doesn't drop.
        let mut t = tc(0, 0, 0, 0, true);
        for _ in 0..17982 {
            t = next_frame(t, 30);
        }
        assert_eq!(t, tc(0, 10, 0, 0, true));
    }
}
