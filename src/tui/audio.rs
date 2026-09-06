//! cpal audio engines for the TUI (feature = "live").
//!
//! Two engines share one shape: a cpal stream whose real-time callback owns a
//! [`Decoder`], feeds it samples, and hands results back to the UI thread over
//! an rtrb ring buffer plus a couple of atomics. The UI thread only ever drains
//! the ring / reads the atomics — it never touches cpal state from render or
//! key handling, so there's no lock on the audio path.
//!
//! - [`InputEngine`] captures from an input device (Live tab): every sample is
//!   pushed through the decoder and the last buffer's peak magnitude is exposed
//!   for a level meter.
//! - [`OutputEngine`] plays a loaded WAV through an output device (Decode tab):
//!   the callback emits the file's samples *and* feeds the same samples to a
//!   decoder, so the on-screen timecode matches what you hear; a position atomic
//!   backs the progress gauge.
//!
//! This mirrors the CLI `listen` command (`src/main.rs`), but with fallible
//! setup (a device that won't open surfaces as a status string) instead of the
//! CLI's `.expect(...)`/`fail(...)`, since panicking inside the alternate screen
//! would wreck the terminal.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{BufferSize, Device, SampleFormat, SampleRate, Stream, StreamConfig};
use rtrb::{Consumer, RingBuffer};

use crate::decoder::Decoder;
use crate::frame::Timecode;

/// Ring-buffer capacity (decoded timecodes buffered between UI ticks). One
/// frame per ~1/30 s and the UI drains at ~30 Hz, so this is generous slack.
const RING_CAP: usize = 512;

/// Enumerate input devices as `(display name, handle)`, default device first.
pub(crate) fn input_devices() -> Vec<(String, Device)> {
    devices(true)
}

/// Enumerate output devices as `(display name, handle)`, default device first.
pub(crate) fn output_devices() -> Vec<(String, Device)> {
    devices(false)
}

fn devices(input: bool) -> Vec<(String, Device)> {
    let host = cpal::default_host();
    let default_name = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
    .and_then(|d| d.name().ok());

    let listed = if input {
        host.input_devices().map(|it| it.collect::<Vec<_>>())
    } else {
        host.output_devices().map(|it| it.collect::<Vec<_>>())
    };
    let mut out: Vec<(String, Device)> = listed
        .unwrap_or_default()
        .into_iter()
        .filter_map(|d| d.name().ok().map(|n| (n, d)))
        .collect();

    // Float the default device to the front so the picker preselects it.
    if let Some(dn) = default_name {
        if let Some(pos) = out.iter().position(|(n, _)| *n == dn) {
            out.swap(0, pos);
        }
    }
    out
}

// ------------------------------- input ------------------------------------

/// Live capture: decode LTC arriving on an input device in real time.
pub(crate) struct InputEngine {
    _stream: Stream,
    rx: Consumer<Timecode>,
    peak: Arc<AtomicU32>,
    frames: usize,
}

impl InputEngine {
    pub(crate) fn start(device: &Device) -> Result<Self, String> {
        let config = device
            .default_input_config()
            .map_err(|e| format!("no input config: {e}"))?;
        let sample_format = config.sample_format();
        let stream_config: StreamConfig = config.into();

        let (mut tx, rx) = RingBuffer::<Timecode>::new(RING_CAP);
        let peak = Arc::new(AtomicU32::new(0));
        let peak_cb = peak.clone();
        let mut dec = Decoder::new();

        let stream = match sample_format {
            SampleFormat::F32 => device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    let mut p = 0u32;
                    for &s in data {
                        let v = (s * i16::MAX as f32) as i16;
                        p = p.max(v.unsigned_abs() as u32);
                        if let Some(tc) = dec.push_sample(v) {
                            let _ = tx.push(tc);
                        }
                    }
                    peak_cb.store(p, Ordering::Relaxed);
                },
                |_| {},
                None,
            ),
            SampleFormat::I16 => device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let mut p = 0u32;
                    for &s in data {
                        p = p.max(s.unsigned_abs() as u32);
                        if let Some(tc) = dec.push_sample(s) {
                            let _ = tx.push(tc);
                        }
                    }
                    peak_cb.store(p, Ordering::Relaxed);
                },
                |_| {},
                None,
            ),
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("build input stream: {e}"))?;

        stream.play().map_err(|e| format!("play: {e}"))?;
        Ok(InputEngine {
            _stream: stream,
            rx,
            peak,
            frames: 0,
        })
    }

    /// Drain any newly decoded frames; return the most recent one seen.
    pub(crate) fn poll(&mut self) -> Option<Timecode> {
        let mut latest = None;
        while let Ok(tc) = self.rx.pop() {
            latest = Some(tc);
            self.frames += 1;
        }
        latest
    }

    pub(crate) fn frames(&self) -> usize {
        self.frames
    }

    /// Peak magnitude of the most recent callback buffer, `0.0..=1.0`.
    pub(crate) fn peak(&self) -> f64 {
        self.peak.load(Ordering::Relaxed) as f64 / i16::MAX as f64
    }
}

// ------------------------------- output -----------------------------------

/// File playback: play a loaded WAV through an output device while decoding the
/// same samples, so the readout tracks the audio.
pub(crate) struct OutputEngine {
    _stream: Stream,
    rx: Consumer<Timecode>,
    pos: Arc<AtomicUsize>,
    playing: Arc<AtomicBool>,
    total: usize,
    rate_note: Option<String>,
}

impl OutputEngine {
    pub(crate) fn start(device: &Device, samples: Vec<i16>, file_rate: u32) -> Result<Self, String> {
        let default_cfg = device
            .default_output_config()
            .map_err(|e| format!("no output config: {e}"))?;
        let sample_format = default_cfg.sample_format();
        let channels = default_cfg.channels();

        // Prefer playing at the file's own sample rate so timecode rolls at true
        // wall-clock speed. If the device can't do that rate, fall back to its
        // default and warn: the decoder is rate-adaptive so it still locks, but
        // pitch/speed shift. (Resampling is out of scope.)
        let supports_file_rate = device
            .supported_output_configs()
            .map(|mut it| {
                it.any(|r| {
                    r.channels() == channels
                        && r.min_sample_rate().0 <= file_rate
                        && file_rate <= r.max_sample_rate().0
                })
            })
            .unwrap_or(false);

        let (config, rate_note): (StreamConfig, Option<String>) = if supports_file_rate {
            (
                StreamConfig {
                    channels,
                    sample_rate: SampleRate(file_rate),
                    buffer_size: BufferSize::Default,
                },
                None,
            )
        } else {
            let dr = default_cfg.sample_rate().0;
            (
                default_cfg.clone().into(),
                Some(format!(
                    "device rate {dr} Hz != file {file_rate} Hz; pitch/speed differ"
                )),
            )
        };
        let out_channels = config.channels as usize;

        let total = samples.len();
        let src = Arc::new(samples);
        let pos = Arc::new(AtomicUsize::new(0));
        let playing = Arc::new(AtomicBool::new(true));
        let (mut tx, rx) = RingBuffer::<Timecode>::new(RING_CAP);

        let src_cb = src.clone();
        let pos_cb = pos.clone();
        let playing_cb = playing.clone();
        let mut dec = Decoder::new();

        let stream = match sample_format {
            SampleFormat::F32 => device.build_output_stream(
                &config,
                move |out: &mut [f32], _| {
                    let on = playing_cb.load(Ordering::Relaxed);
                    let mut p = pos_cb.load(Ordering::Relaxed);
                    for frame in out.chunks_mut(out_channels) {
                        let s = if on && p < src_cb.len() {
                            let s = src_cb[p];
                            p += 1;
                            if let Some(tc) = dec.push_sample(s) {
                                let _ = tx.push(tc);
                            }
                            s
                        } else {
                            0
                        };
                        let f = s as f32 / i16::MAX as f32;
                        for ch in frame.iter_mut() {
                            *ch = f;
                        }
                    }
                    pos_cb.store(p, Ordering::Relaxed);
                },
                |_| {},
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                &config,
                move |out: &mut [i16], _| {
                    let on = playing_cb.load(Ordering::Relaxed);
                    let mut p = pos_cb.load(Ordering::Relaxed);
                    for frame in out.chunks_mut(out_channels) {
                        let s = if on && p < src_cb.len() {
                            let s = src_cb[p];
                            p += 1;
                            if let Some(tc) = dec.push_sample(s) {
                                let _ = tx.push(tc);
                            }
                            s
                        } else {
                            0
                        };
                        for ch in frame.iter_mut() {
                            *ch = s;
                        }
                    }
                    pos_cb.store(p, Ordering::Relaxed);
                },
                |_| {},
                None,
            ),
            other => return Err(format!("unsupported sample format: {other:?}")),
        }
        .map_err(|e| format!("build output stream: {e}"))?;

        stream.play().map_err(|e| format!("play: {e}"))?;
        Ok(OutputEngine {
            _stream: stream,
            rx,
            pos,
            playing,
            total,
            rate_note,
        })
    }

    /// Drain any newly decoded frames; return the most recent one seen.
    pub(crate) fn poll(&mut self) -> Option<Timecode> {
        let mut latest = None;
        while let Ok(tc) = self.rx.pop() {
            latest = Some(tc);
        }
        latest
    }

    pub(crate) fn set_playing(&self, playing: bool) {
        self.playing.store(playing, Ordering::Relaxed);
    }

    pub(crate) fn is_playing(&self) -> bool {
        self.playing.load(Ordering::Relaxed)
    }

    pub(crate) fn progress(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.pos.load(Ordering::Relaxed) as f64 / self.total as f64).clamp(0.0, 1.0)
        }
    }

    pub(crate) fn finished(&self) -> bool {
        self.pos.load(Ordering::Relaxed) >= self.total
    }

    pub(crate) fn rate_note(&self) -> Option<&str> {
        self.rate_note.as_deref()
    }
}
