//! Whole-file analysis: decode an LTC WAV once and derive at-a-glance metadata.
//! Reusable by any frontend (the TUI details panel, the CLI).

use crate::decoder::Decoder;
use crate::frame::{next_frame, Timecode};

pub struct FileSummary {
    pub start: Option<Timecode>,
    pub end: Option<Timecode>,
    pub frame_count: usize,
    /// Nominal fps inferred from a second rollover (frame just before the
    /// seconds increment is fps-1). None if the clip is too short to observe one.
    pub nominal_fps: Option<u8>,
    pub drop_frame: bool,
    pub sample_rate: u32,
    pub duration_secs: f64,
    /// Whether every decoded frame is exactly one increment from the previous
    /// (drop-frame aware). None when fps couldn't be inferred.
    pub continuous: Option<bool>,
}

impl FileSummary {
    /// Human label for the rate: "30", "25", "29.97 drop", or "unknown".
    pub fn rate_label(&self) -> String {
        match self.nominal_fps {
            Some(_) if self.drop_frame => format!("{:.2} drop", 30_000.0 / 1001.0),
            Some(n) => format!("{n}"),
            None => "unknown".into(),
        }
    }
}

pub fn analyze(samples: &[i16], sample_rate: u32) -> FileSummary {
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for &s in samples {
        if let Some(tc) = dec.push_sample(s) {
            frames.push(tc);
        }
    }

    let duration_secs = if sample_rate > 0 {
        samples.len() as f64 / sample_rate as f64
    } else {
        0.0
    };
    let start = frames.first().copied();
    let end = frames.last().copied();
    let drop_frame = start.map(|t| t.drop_frame).unwrap_or(false);

    // Nominal fps: at each second rollover, the earlier frame's number is fps-1.
    // Take the max seen (robust to a partial rollover early in the clip).
    let mut nominal: Option<u8> = None;
    for w in frames.windows(2) {
        if w[0].seconds != w[1].seconds {
            let cand = w[0].frames + 1;
            nominal = Some(nominal.map_or(cand, |m: u8| m.max(cand)));
        }
    }

    let continuous = nominal.map(|fps| frames.windows(2).all(|w| w[1] == next_frame(w[0], fps)));

    FileSummary {
        start,
        end,
        frame_count: frames.len(),
        nominal_fps: nominal,
        drop_frame,
        sample_rate,
        duration_secs,
        continuous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biphase::encode_bits_to_samples;
    use crate::frame::{encode_frame, sequence};

    fn tc(h: u8, m: u8, s: u8, f: u8, d: bool) -> Timecode {
        Timecode {
            hours: h,
            minutes: m,
            seconds: s,
            frames: f,
            drop_frame: d,
        }
    }

    fn samples_for(start: Timecode, secs: f64, nominal: u8, real: f64) -> Vec<i16> {
        let n = (secs * real).round() as u32;
        let frames: Vec<[bool; 80]> = sequence(start, n, nominal)
            .iter()
            .map(|&t| encode_frame(t, nominal))
            .collect();
        encode_bits_to_samples(&frames, 48_000, real, 16_000)
    }

    #[test]
    fn infers_30_ndf() {
        let s = analyze(&samples_for(tc(1, 0, 0, 0, false), 3.0, 30, 30.0), 48_000);
        assert_eq!(s.nominal_fps, Some(30));
        assert!(!s.drop_frame);
        assert_eq!(s.continuous, Some(true));
        assert_eq!(s.start.unwrap().hours, 1);
        assert_eq!(s.rate_label(), "30");
    }

    #[test]
    fn infers_25() {
        let s = analyze(&samples_for(tc(1, 0, 0, 0, false), 3.0, 25, 25.0), 48_000);
        assert_eq!(s.nominal_fps, Some(25));
        assert_eq!(s.rate_label(), "25");
    }

    #[test]
    fn infers_2997_drop() {
        let s = analyze(
            &samples_for(tc(0, 0, 0, 0, true), 3.0, 30, 30_000.0 / 1001.0),
            48_000,
        );
        assert_eq!(s.nominal_fps, Some(30));
        assert!(s.drop_frame);
        assert_eq!(s.continuous, Some(true));
        assert_eq!(s.rate_label(), "29.97 drop");
    }

    #[test]
    fn short_clip_cannot_infer_fps() {
        // ~10 frames, no second rollover
        let s = analyze(&samples_for(tc(1, 0, 0, 0, false), 0.3, 30, 30.0), 48_000);
        assert_eq!(s.nominal_fps, None);
        assert_eq!(s.continuous, None);
        assert_eq!(s.rate_label(), "unknown");
    }
}
