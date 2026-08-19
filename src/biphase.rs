//! Biphase mark (differential Manchester) encoding — the generator's physical
//! layer. A transition at every bit boundary; a logical 1 adds a mid-bit
//! transition, a 0 does not. (Decoding now lives in decoder.rs, streaming.)

pub fn encode_bits_to_samples(
    frames: &[[bool; 80]],
    sample_rate: u32,
    fps: u32,
    amplitude: i16,
) -> Vec<i16> {
    let bit_period = (sample_rate / (80 * fps)) as usize;
    let half = bit_period / 2;
    let mut out = Vec::with_capacity(frames.len() * 80 * bit_period);
    let mut level: i16 = amplitude;
    for frame in frames {
        for &bit in frame.iter() {
            level = -level; // boundary transition, every bit
            if bit {
                for _ in 0..half {
                    out.push(level);
                }
                level = -level; // mid-bit transition => 1
                for _ in 0..(bit_period - half) {
                    out.push(level);
                }
            } else {
                for _ in 0..bit_period {
                    out.push(level);
                }
            }
        }
    }
    out
}
