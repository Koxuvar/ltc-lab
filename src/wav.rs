//! Minimal 16-bit mono PCM WAV read/write. Only meant to interoperate with the
//! files this program writes, so the reader assumes the canonical 44-byte header
//! it produces. Swap in the `hound` crate if you need to read arbitrary WAVs.

use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};

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

pub fn read_wav_mono16(path: &str) -> io::Result<(Vec<i16>, u32)> {
    let mut r = BufReader::new(File::open(path)?);
    let mut buf = Vec::new();
    r.read_to_end(&mut buf)?;

    let sample_rate = u32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
    let data_size = u32::from_le_bytes([buf[40], buf[41], buf[42], buf[43]]) as usize;

    let mut samples = Vec::with_capacity(data_size / 2);
    let mut i = 44;
    while i + 1 < buf.len() && i + 1 < 44 + data_size {
        samples.push(i16::from_le_bytes([buf[i], buf[i + 1]]));
        i += 2;
    }
    Ok((samples, sample_rate))
}
