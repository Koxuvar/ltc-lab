//! Biphase mark (differential Manchester) encoding — the generator's physical
//! layer. A transition at every bit boundary; a logical 1 adds a mid-bit
//! transition, a 0 does not.
//!
//! Timing uses a fractional sample clock so non-integer sample-rates-per-bit
//! (e.g. 29.97 fps at 48 kHz = ~20.02 samples/bit) render without drift: each
//! bit boundary lands at round(bit_index * samples_per_bit), so the fractional
//! remainder is distributed across bits (some get 20 samples, some 21).

/// `fps` is the REAL frame rate (29.97003 for drop-frame, not the nominal 30).
pub fn encode_bits_to_samples(
    frames: &[[bool; 80]],
    sample_rate: u32,
    fps: f64,
    amplitude: i16,
) -> Vec<i16> {
    let spb = sample_rate as f64 / (80.0 * fps); // samples per bit, fractional
    let total_bits = frames.len() * 80;
    let mut out = Vec::with_capacity((total_bits as f64 * spb) as usize + 1);
    let mut level: i16 = amplitude;
    let mut bit_index: u64 = 0;

    for frame in frames {
        for &bit in frame.iter() {
            let mid = (((bit_index as f64) + 0.5) * spb).round() as usize;
            let end = (((bit_index as f64) + 1.0) * spb).round() as usize;
            level = -level; // boundary transition, every bit
            if bit {
                while out.len() < mid {
                    out.push(level);
                }
                level = -level; // mid-bit transition => 1
                while out.len() < end {
                    out.push(level);
                }
            } else {
                while out.len() < end {
                    out.push(level);
                }
            }
            bit_index += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{encode_frame, Timecode};

    fn frame() -> [bool; 80] {
        encode_frame(
            Timecode {
                hours: 1,
                minutes: 0,
                seconds: 0,
                frames: 0,
                drop_frame: false,
            },
            30,
        )
    }

    #[test]
    fn integer_rate_exact_20_samples_per_bit() {
        let s = encode_bits_to_samples(&[frame()], 48_000, 30.0, 16_000);
        assert_eq!(s.len(), 80 * 20);
    }

    #[test]
    fn fractional_rate_matches_ideal_length() {
        let frames = vec![frame(); 30];
        let s = encode_bits_to_samples(&frames, 48_000, 30_000.0 / 1001.0, 16_000);
        let bits = (frames.len() * 80) as f64;
        let expected = (bits * 48_000.0 / (80.0 * (30_000.0 / 1001.0))).round() as usize;
        assert_eq!(s.len(), expected);
    }
}
