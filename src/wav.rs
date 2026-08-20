//! Minimal, dependency-free WAV read/write.
//!
//! Writing always produces canonical 16-bit mono PCM (44-byte header, no
//! extra chunks) — exactly what this program needs to emit.
//!
//! Reading walks RIFF chunks properly instead of assuming a fixed 44-byte
//! header, so files with extra chunks before `data` (LIST/bext/JUNK/fact,
//! common from DAW exports) still parse. All buffer access is bounds-checked
//! and returns `WavError` instead of panicking on short/malformed input.
//! Supports mono or stereo 16-bit PCM; for stereo, only the first (left)
//! channel is kept, since LTC is conventionally recorded on a single channel.
//! Other bit depths / formats are rejected with a clear error rather than
//! silently misread — swap in the `hound` crate if you need broader format
//! support.

use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Read, Write};

#[derive(Debug)]
pub enum WavError {
    Io(io::Error),
    NotRiff,
    NotWave,
    MissingFmtChunk,
    MissingDataChunk,
    Truncated(&'static str),
    UnsupportedFormat { format_tag: u16, channels: u16, bits_per_sample: u16 },
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            WavError::Io(e) => write!(f, "{e}"),
            WavError::NotRiff => write!(f, "not a RIFF file (missing \"RIFF\" magic)"),
            WavError::NotWave => write!(f, "not a WAVE file (missing \"WAVE\" magic)"),
            WavError::MissingFmtChunk => write!(f, "no \"fmt \" chunk found"),
            WavError::MissingDataChunk => write!(f, "no \"data\" chunk found"),
            WavError::Truncated(what) => write!(f, "truncated or corrupt WAV: {what}"),
            WavError::UnsupportedFormat {
                format_tag,
                channels,
                bits_per_sample,
            } => write!(
                f,
                "unsupported format (tag {format_tag}, {channels} ch, {bits_per_sample}-bit); \
                 only mono/stereo 16-bit PCM is supported"
            ),
        }
    }
}

impl std::error::Error for WavError {}

impl From<io::Error> for WavError {
    fn from(e: io::Error) -> Self {
        WavError::Io(e)
    }
}

pub fn write_wav_mono16(path: &str, samples: &[i16], sample_rate: u32) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let data_len = (samples.len() * 2) as u32;
    let byte_rate = sample_rate * 2;

    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_len).to_le_bytes())?;
    w.write_all(b"WAVE")?;
    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    w.write_all(&1u16.to_le_bytes())?; // format = PCM
    w.write_all(&1u16.to_le_bytes())?; // channels = mono
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?; // block align
    w.write_all(&16u16.to_le_bytes())?; // bits per sample
    w.write_all(b"data")?;
    w.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        w.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

pub fn read_wav_mono16(path: &str) -> Result<(Vec<i16>, u32), WavError> {
    let mut r = File::open(path)?;
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;
    parse_wav(&buf)
}

struct FmtChunk {
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
}

impl FmtChunk {
    /// Parse the body of a "fmt " chunk (everything after the 8-byte
    /// id+size header). `body` may be longer than 16 bytes (cbSize and
    /// extension fields for WAVE_FORMAT_EXTENSIBLE); only the base fields
    /// plus, when present, the extensible subformat tag are used.
    fn parse(body: &[u8]) -> Result<Self, WavError> {
        if body.len() < 16 {
            return Err(WavError::Truncated("fmt chunk shorter than 16 bytes"));
        }
        let mut format_tag = u16::from_le_bytes([body[0], body[1]]);
        let channels = u16::from_le_bytes([body[2], body[3]]);
        let sample_rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
        let bits_per_sample = u16::from_le_bytes([body[14], body[15]]);

        // WAVE_FORMAT_EXTENSIBLE: real type lives in the subformat GUID's
        // first two bytes, at offset 24 into the fmt chunk body.
        const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
        if format_tag == WAVE_FORMAT_EXTENSIBLE && body.len() >= 26 {
            format_tag = u16::from_le_bytes([body[24], body[25]]);
        }

        Ok(FmtChunk {
            format_tag,
            channels,
            sample_rate,
            bits_per_sample,
        })
    }
}

fn parse_wav(buf: &[u8]) -> Result<(Vec<i16>, u32), WavError> {
    if buf.len() < 12 {
        return Err(WavError::Truncated("file shorter than RIFF header"));
    }
    if &buf[0..4] != b"RIFF" {
        return Err(WavError::NotRiff);
    }
    if &buf[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }

    let mut fmt: Option<FmtChunk> = None;
    let mut data: Option<&[u8]> = None;

    let mut pos = 12usize;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let declared_size = u32::from_le_bytes(buf[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body_start = pos + 8;
        let available = buf.len() - body_start;
        // Some writers get the data-chunk size wrong (e.g. streaming
        // encoders that leave it as 0). Clamp to what's actually present
        // rather than erroring, since the samples themselves are still
        // fully readable.
        let size = declared_size.min(available);
        let body_end = body_start + size;

        match id {
            b"fmt " => fmt = Some(FmtChunk::parse(&buf[body_start..body_end])?),
            b"data" => data = Some(&buf[body_start..body_end]),
            _ => {}
        }

        if declared_size > available {
            break; // chunk ran off the end of the file; nothing more to walk
        }
        // Chunks are word-aligned: an odd-sized chunk has one pad byte.
        pos = body_end + (declared_size % 2);
    }

    let fmt = fmt.ok_or(WavError::MissingFmtChunk)?;
    let data = data.ok_or(WavError::MissingDataChunk)?;

    let channels = fmt.channels as usize;
    if fmt.format_tag != 1 || fmt.bits_per_sample != 16 || !(1..=2).contains(&channels) {
        return Err(WavError::UnsupportedFormat {
            format_tag: fmt.format_tag,
            channels: fmt.channels,
            bits_per_sample: fmt.bits_per_sample,
        });
    }

    let frame_bytes = 2 * channels;
    let mut samples = Vec::with_capacity(data.len() / frame_bytes);
    let mut i = 0;
    while i + frame_bytes <= data.len() {
        // First channel only; LTC is conventionally carried on one channel
        // of a stereo file, so a stray second channel is simply ignored
        // rather than averaged into the signal.
        samples.push(i16::from_le_bytes([data[i], data[i + 1]]));
        i += frame_bytes;
    }

    Ok((samples, fmt.sample_rate))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_path(name: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("ltc_lab_wavtest_{name}.wav"));
        p.to_str().unwrap().to_string()
    }

    #[test]
    fn roundtrip_mono16() {
        let path = roundtrip_path("roundtrip_mono16");
        let samples: Vec<i16> = (0..100).map(|i| (i * 37) as i16).collect();
        write_wav_mono16(&path, &samples, 48_000).unwrap();
        let (out, sr) = read_wav_mono16(&path).unwrap();
        assert_eq!(sr, 48_000);
        assert_eq!(out, samples);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn extra_chunks_before_data_are_skipped() {
        // Hand-build: RIFF/WAVE, a junk chunk, then fmt, then a fact chunk,
        // then data. Exercises the chunk walker beyond the canonical layout.
        let samples: [i16; 4] = [1, -2, 3, -4];
        let mut data_bytes = Vec::new();
        for s in samples {
            data_bytes.extend_from_slice(&s.to_le_bytes());
        }

        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt_body.extend_from_slice(&44_100u32.to_le_bytes());
        fmt_body.extend_from_slice(&88_200u32.to_le_bytes()); // byte rate
        fmt_body.extend_from_slice(&2u16.to_le_bytes()); // block align
        fmt_body.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

        let junk_body = vec![0u8; 5]; // odd size, exercises pad-byte handling
        let fact_body = 4u32.to_le_bytes().to_vec();

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        push_chunk(&mut body, b"JUNK", &junk_body);
        push_chunk(&mut body, b"fmt ", &fmt_body);
        push_chunk(&mut body, b"fact", &fact_body);
        push_chunk(&mut body, b"data", &data_bytes);

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        let (out, sr) = parse_wav(&file).unwrap();
        assert_eq!(sr, 44_100);
        assert_eq!(out, samples);
    }

    #[test]
    fn stereo_keeps_first_channel_only() {
        // Interleaved L/R: L = 10,20,30 ; R = -10,-20,-30
        let interleaved: [i16; 6] = [10, -10, 20, -20, 30, -30];
        let mut data_bytes = Vec::new();
        for s in interleaved {
            data_bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt_body.extend_from_slice(&2u16.to_le_bytes()); // stereo
        fmt_body.extend_from_slice(&48_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&192_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&4u16.to_le_bytes()); // block align
        fmt_body.extend_from_slice(&16u16.to_le_bytes());

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        push_chunk(&mut body, b"fmt ", &fmt_body);
        push_chunk(&mut body, b"data", &data_bytes);

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        let (out, sr) = parse_wav(&file).unwrap();
        assert_eq!(sr, 48_000);
        assert_eq!(out, vec![10, 20, 30]);
    }

    #[test]
    fn empty_file_errors_without_panicking() {
        assert!(matches!(parse_wav(&[]), Err(WavError::Truncated(_))));
    }

    #[test]
    fn short_garbage_errors_without_panicking() {
        assert!(matches!(parse_wav(&[1, 2, 3]), Err(WavError::Truncated(_))));
    }

    #[test]
    fn bad_riff_magic_errors() {
        let mut file = vec![0u8; 44];
        file[0..4].copy_from_slice(b"OGGS");
        assert!(matches!(parse_wav(&file), Err(WavError::NotRiff)));
    }

    #[test]
    fn bad_wave_magic_errors() {
        let mut file = vec![0u8; 44];
        file[0..4].copy_from_slice(b"RIFF");
        file[8..12].copy_from_slice(b"AVI ");
        assert!(matches!(parse_wav(&file), Err(WavError::NotWave)));
    }

    #[test]
    fn missing_data_chunk_errors() {
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&48_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&96_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&2u16.to_le_bytes());
        fmt_body.extend_from_slice(&16u16.to_le_bytes());

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        push_chunk(&mut body, b"fmt ", &fmt_body);

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        assert!(matches!(parse_wav(&file), Err(WavError::MissingDataChunk)));
    }

    #[test]
    fn unsupported_bit_depth_errors_cleanly() {
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // PCM
        fmt_body.extend_from_slice(&1u16.to_le_bytes()); // mono
        fmt_body.extend_from_slice(&48_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&144_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&3u16.to_le_bytes()); // block align
        fmt_body.extend_from_slice(&24u16.to_le_bytes()); // 24-bit: unsupported

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        push_chunk(&mut body, b"fmt ", &fmt_body);
        push_chunk(&mut body, b"data", &[0u8; 6]);

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        assert!(matches!(
            parse_wav(&file),
            Err(WavError::UnsupportedFormat { bits_per_sample: 24, .. })
        ));
    }

    #[test]
    fn truncated_data_chunk_is_clamped_not_panicking() {
        // Declares more data than is actually present; should read what's
        // there instead of panicking or erroring.
        let mut fmt_body = Vec::new();
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&1u16.to_le_bytes());
        fmt_body.extend_from_slice(&48_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&96_000u32.to_le_bytes());
        fmt_body.extend_from_slice(&2u16.to_le_bytes());
        fmt_body.extend_from_slice(&16u16.to_le_bytes());

        let mut body = Vec::new();
        body.extend_from_slice(b"WAVE");
        push_chunk(&mut body, b"fmt ", &fmt_body);

        // Manually append a data chunk header claiming 1000 bytes but only
        // supply 4.
        body.extend_from_slice(b"data");
        body.extend_from_slice(&1000u32.to_le_bytes());
        body.extend_from_slice(&[1, 0, 2, 0]); // two i16 samples: 1, 2

        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(body.len() as u32).to_le_bytes());
        file.extend_from_slice(&body);

        let (out, _) = parse_wav(&file).unwrap();
        assert_eq!(out, vec![1, 2]);
    }

    fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
        out.extend_from_slice(id);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(body);
        if body.len() % 2 == 1 {
            out.push(0);
        }
    }
}
