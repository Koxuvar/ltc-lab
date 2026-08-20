//! Frontend-agnostic generation pipeline: timecode + length -> LTC WAV file.

use crate::biphase::encode_bits_to_samples;
use crate::frame::{encode_frame, sequence, Timecode};
use crate::rate::{parse_rate, RateSpec};
use crate::wav;

pub struct GenReport {
    pub path: String,
    pub start: Timecode,
    pub end: Timecode,
    pub rate: RateSpec,
    pub payload_frames: u32,
    pub samples: usize,
}

/// Generate an LTC WAV. `out` = None auto-names the file descriptively.
pub fn generate_to_wav(
    start_str: &str,
    length: f64,
    fps: &str,
    sample_rate: u32,
    preroll: u32,
    out: Option<String>,
) -> Result<GenReport, String> {
    let mut start = Timecode::parse(start_str)?;
    let rate = parse_rate(fps, start.drop_frame)?;
    start.drop_frame = rate.drop;

    let payload_frames = (length * rate.real).round() as u32;
    let payload = sequence(start, payload_frames, rate.nominal);

    let mut frames: Vec<[bool; 80]> = Vec::new();
    for _ in 0..preroll {
        frames.push(encode_frame(start));
    }
    frames.extend(payload.iter().map(|&tc| encode_frame(tc)));

    let samples = encode_bits_to_samples(&frames, sample_rate, rate.real, 16_000);
    let path = out.unwrap_or_else(|| default_name(start, length, &rate, sample_rate));
    wav::write_wav_mono16(&path, &samples, sample_rate).map_err(|e| e.to_string())?;

    Ok(GenReport {
        path,
        start,
        end: payload.last().copied().unwrap_or(start),
        rate,
        payload_frames,
        samples: samples.len(),
    })
}

pub fn default_name(tc: Timecode, length: f64, rate: &RateSpec, sr: u32) -> String {
    let df = if rate.drop { "DF" } else { "" };
    let len = if length.fract() == 0.0 {
        format!("{}", length as i64)
    } else {
        format!("{length}").replace('.', "p")
    };
    format!(
        "ltc_{:02}h{:02}m{:02}s{:02}f_{}s_{}fps{}_{}Hz.wav",
        tc.hours,
        tc.minutes,
        tc.seconds,
        tc.frames,
        len,
        rate.label().replace('.', "p"),
        df,
        sr
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_and_reports() {
        let mut p = std::env::temp_dir();
        p.push("ltc_lab_gentest.wav");
        let path = p.to_str().unwrap().to_string();
        let r = generate_to_wav("01:00:00:00", 1.0, "30", 48_000, 0, Some(path.clone())).unwrap();
        assert_eq!(r.payload_frames, 30);
        assert_eq!(
            r.end,
            Timecode {
                hours: 1,
                minutes: 0,
                seconds: 0,
                frames: 29,
                drop_frame: false
            }
        );
        assert!(std::path::Path::new(&path).exists());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn bad_start_errors() {
        assert!(generate_to_wav("nope", 1.0, "30", 48_000, 0, Some("x.wav".into())).is_err());
    }
}
