//! Streaming LTC decoder: one sample in, timecodes out. No knowledge of the
//! whole signal, so it works on a live audio callback. This replaces the
//! two-pass offline decoder, which needed the entire file to set its threshold.
//!
//! Signal chain per sample:
//!   DC blocker (one-pole high-pass) -> hysteresis zero-cross -> interval timing
//!   -> adaptive half-bit estimate -> biphase classify -> 80-bit ring -> sync.

use crate::frame::{decode_frame, Timecode, SYNC};
use std::collections::VecDeque;

const DC_R: f32 = 0.9995; // high-pass pole; ~4 Hz cutoff at 48 kHz, preserves the square shape
const PEAK_DECAY: f32 = 0.9999;
const HYST_FRAC: f32 = 0.10; // hysteresis band as a fraction of running peak

pub struct Decoder {
    // DC blocker state
    dc_prev_x: f32,
    dc_prev_y: f32,
    // amplitude tracking (for hysteresis, so noise near zero doesn't retrigger)
    peak: f32,
    // edge / interval state
    last_sign: bool,
    samples_since_edge: u32,
    seen_first_edge: bool,
    // biphase classify state
    half_bit: f32, // running estimate of half a bit period, in samples
    pending_short: bool,
    // frame assembly: most recent up-to-80 logical bits, oldest at front
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
            half_bit: f32::MAX / 4.0, // large; snaps down within the first frame
            pending_short: false,
            reg: VecDeque::with_capacity(81),
        }
    }

    /// Feed one PCM sample. Returns a timecode iff a frame completed here.
    pub fn push_sample(&mut self, x: i16) -> Option<Timecode> {
        // DC block: y = x - x_prev + R*y_prev
        let xf = x as f32;
        let y = xf - self.dc_prev_x + DC_R * self.dc_prev_y;
        self.dc_prev_x = xf;
        self.dc_prev_y = y;

        // track peak for hysteresis band
        let mag = y.abs();
        if mag > self.peak {
            self.peak = mag;
        }
        self.peak *= PEAK_DECAY;
        let hyst = self.peak * HYST_FRAC;

        // hysteresis sign: only flip once clearly past the band
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
            self.seen_first_edge = true; // first edge has no measurable interval before it
        }
        None
    }

    fn on_interval(&mut self, iv: f32) -> Option<Timecode> {
        // Adapt: snap down to any shorter interval (half-bits are the shortest,
        // and every sync word's twelve consecutive 1s guarantees they appear),
        // creep up slowly so a spurious short interval self-corrects.
        if iv < self.half_bit {
            self.half_bit = iv;
        } else {
            self.half_bit += self.half_bit / 64.0;
        }
        let threshold = self.half_bit * 1.5;

        if iv >= threshold {
            // one full-bit interval => logical 0
            self.pending_short = false;
            self.push_bit(false)
        } else if self.pending_short {
            // second of two half-bit intervals => logical 1
            self.pending_short = false;
            self.push_bit(true)
        } else {
            self.pending_short = true;
            None
        }
    }

    fn push_bit(&mut self, bit: bool) -> Option<Timecode> {
        self.reg.push_back(bit);
        if self.reg.len() > 80 {
            self.reg.pop_front();
        }
        if self.reg.len() == 80 {
            // sync word occupies the last 16 bits (frame bits 64..=79)
            let synced = (64..80).all(|i| self.reg[i] == SYNC[i - 64]);
            if synced {
                let bits: Vec<bool> = self.reg.iter().copied().collect();
                return Some(decode_frame(&bits));
            }
        }
        None
    }
}
