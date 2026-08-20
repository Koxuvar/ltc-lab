//! Streaming LTC decoder: one sample in, timecodes out. Works on a live audio
//! callback (no whole-signal knowledge).
//!
//! Signal chain per sample:
//!   DC blocker -> hysteresis zero-cross -> interval timing -> bit-period
//!   tracker -> biphase classify -> 80-bit ring -> sync -> BCD validity gate.

use crate::frame::{decode_frame, Timecode, SYNC};
use std::collections::VecDeque;

const DC_R: f32 = 0.9995; // one-pole high-pass; ~4 Hz cutoff at 48 kHz
const PEAK_DECAY: f32 = 0.9999;
const HYST_FRAC: f32 = 0.10;
const ALPHA: f32 = 1.0 / 16.0; // bit-period EMA rate
const WARMUP_INTERVALS: usize = 64; // <1 frame; seeds the bit-period estimate

pub struct Decoder {
    dc_prev_x: f32,
    dc_prev_y: f32,
    peak: f32,
    last_sign: bool,
    samples_since_edge: u32,
    seen_first_edge: bool,

    // bit-period tracking (full bit, in samples). Updated ONLY toward full-bit
    // equivalents (a lone full interval, or a pair of half intervals summed), so
    // long runs of identical bits can't inflate it — the drift bug that was here.
    bit_period: f32,
    seeded: bool,
    warmup: Vec<f32>,
    pending_half: Option<f32>,

    reg: VecDeque<bool>,
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            dc_prev_x: 0.0,
            dc_prev_y: 0.0,
            peak: 1.0,
            last_sign: true,
            samples_since_edge: 0,
            seen_first_edge: false,
            bit_period: 0.0,
            seeded: false,
            warmup: Vec::with_capacity(WARMUP_INTERVALS),
            pending_half: None,
            reg: VecDeque::with_capacity(81),
        }
    }

    pub fn push_sample(&mut self, x: i16) -> Option<Timecode> {
        // DC block: y = x - x_prev + R*y_prev
        let xf = x as f32;
        let y = xf - self.dc_prev_x + DC_R * self.dc_prev_y;
        self.dc_prev_x = xf;
        self.dc_prev_y = y;

        let mag = y.abs();
        if mag > self.peak {
            self.peak = mag;
        }
        self.peak *= PEAK_DECAY;
        let hyst = self.peak * HYST_FRAC;

        let sign = if y > hyst {
            true
        } else if y < -hyst {
            false
        } else {
            self.last_sign
        };

        self.samples_since_edge += 1;

        if sign != self.last_sign {
            self.last_sign = sign;
            let interval = self.samples_since_edge as f32;
            self.samples_since_edge = 0;
            if self.seen_first_edge {
                return self.on_interval(interval);
            }
            self.seen_first_edge = true;
        }
        None
    }

    fn on_interval(&mut self, iv: f32) -> Option<Timecode> {
        // Seed the bit period from a short warmup: the shortest interval seen is
        // a half-bit (every sync word's twelve 1s guarantee half-bits appear),
        // so 2x that is one full bit. No bits are emitted during warmup.
        if !self.seeded {
            self.warmup.push(iv);
            if self.warmup.len() >= WARMUP_INTERVALS {
                let min = self.warmup.iter().cloned().fold(f32::MAX, f32::min);
                self.bit_period = 2.0 * min;
                self.seeded = true;
            }
            return None;
        }

        let threshold = 0.75 * self.bit_period; // midway between half (0.5) and full (1.0)

        if iv >= threshold {
            // one full-bit interval => logical 0
            self.pending_half = None;
            self.bit_period += (iv - self.bit_period) * ALPHA;
            self.push_bit(false)
        } else if let Some(first) = self.pending_half.take() {
            // second of two half-bit intervals => logical 1
            let full_equiv = first + iv; // ~one full bit
            self.bit_period += (full_equiv - self.bit_period) * ALPHA;
            self.push_bit(true)
        } else {
            self.pending_half = Some(iv);
            None
        }
    }

    fn push_bit(&mut self, bit: bool) -> Option<Timecode> {
        self.reg.push_back(bit);
        if self.reg.len() > 80 {
            self.reg.pop_front();
        }
        if self.reg.len() == 80 && (64..80).all(|i| self.reg[i] == SYNC[i - 64]) {
            let bits: Vec<bool> = self.reg.iter().copied().collect();
            let tc = decode_frame(&bits);
            // Validity gate: a true sync at a wrong lock can still align by luck;
            // reject anything whose BCD is out of range. (frames<30 covers
            // 24/25/30 fps; thread real fps in for an exact bound.)
            if tc.hours < 24 && tc.minutes < 60 && tc.seconds < 60 && tc.frames < 30 {
                return Some(tc);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::Decoder;
    use crate::biphase::encode_bits_to_samples;
    use crate::frame::{encode_frame, next_frame, sequence, Timecode};

    const DF_2997: f64 = 30_000.0 / 1001.0;
    const DF_2398: f64 = 24_000.0 / 1001.0;

    fn tc(h: u8, m: u8, s: u8, f: u8, d: bool) -> Timecode {
        Timecode {
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
            drop_frame: d,
        }
    }

    /// Generate `secs` of LTC and decode it sample-by-sample (the live path).
    fn decode_generated(start: Timecode, secs: f64, nominal: u8, real: f64) -> Vec<Timecode> {
        let n = (secs * real).round() as u32;
        let frames: Vec<[bool; 80]> = sequence(start, n, nominal)
            .iter()
            .map(|&t| encode_frame(t))
            .collect();
        let samples = encode_bits_to_samples(&frames, 48_000, real, 16_000);
        let mut dec = Decoder::new();
        let mut out = Vec::new();
        for s in samples {
            if let Some(t) = dec.push_sample(s) {
                out.push(t);
            }
        }
        out
    }

    /// Every frame valid BCD; every adjacent pair exactly one increment apart
    /// (drop-frame aware); most of the payload recovered.
    fn assert_clean(out: &[Timecode], nominal: u8, min_len: usize) {
        assert!(out.len() >= min_len, "recovered only {} frames", out.len());
        for t in out {
            assert!(
                t.hours < 24 && t.minutes < 60 && t.seconds < 60 && t.frames < 30,
                "invalid BCD: {t}"
            );
        }
        for w in out.windows(2) {
            assert_eq!(
                w[1],
                next_frame(w[0], nominal),
                "continuity break {} -> {}",
                w[0],
                w[1]
            );
        }
    }

    #[test]
    fn roundtrip_30() {
        assert_clean(
            &decode_generated(tc(1, 0, 0, 0, false), 5.0, 30, 30.0),
            30,
            145,
        );
    }

    #[test]
    fn roundtrip_25() {
        assert_clean(
            &decode_generated(tc(1, 0, 0, 0, false), 5.0, 25, 25.0),
            25,
            120,
        );
    }

    #[test]
    fn roundtrip_24() {
        assert_clean(
            &decode_generated(tc(1, 0, 0, 0, false), 5.0, 24, 24.0),
            24,
            110,
        );
    }

    #[test]
    fn roundtrip_2997_drop_noninteger_timing() {
        assert_clean(
            &decode_generated(tc(0, 0, 0, 0, true), 5.0, 30, DF_2997),
            30,
            145,
        );
    }

    #[test]
    fn roundtrip_23976_noninteger_timing() {
        assert_clean(
            &decode_generated(tc(1, 0, 0, 0, false), 5.0, 24, DF_2398),
            24,
            110,
        );
    }

    #[test]
    fn no_drift_on_zero_heavy_start() {
        // Regression for the threshold-drift bug: 01:00:00:00 has ~25 leading
        // zero bits; 30s exercises many zero-heavy values. Must stay clean.
        assert_clean(
            &decode_generated(tc(1, 0, 0, 0, false), 30.0, 30, 30.0),
            30,
            880,
        );
    }

    #[test]
    fn drop_frame_skips_at_normal_minute() {
        let out = decode_generated(tc(1, 0, 58, 0, true), 4.0, 30, DF_2997);
        assert!(
            out.windows(2)
                .any(|w| w[0] == tc(1, 0, 59, 29, true) && w[1] == tc(1, 1, 0, 2, true)),
            "did not observe ;29 -> ;02 across a normal minute"
        );
    }

    #[test]
    fn drop_frame_no_skip_at_tenth_minute() {
        let out = decode_generated(tc(1, 9, 58, 0, true), 4.0, 30, DF_2997);
        assert!(
            out.windows(2)
                .any(|w| w[0] == tc(1, 9, 59, 29, true) && w[1] == tc(1, 10, 0, 0, true)),
            "did not observe ;29 -> ;00 at the tenth minute"
        );
    }
}
