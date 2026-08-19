mod biphase;
mod decoder;
mod frame;
mod wav;

use decoder::Decoder;
use frame::{encode_frame, sequence, Timecode};
use std::collections::HashMap;

const HELP: &str = "\
ltc-lab — generate and decode SMPTE LTC timecode audio

USAGE:
  ltc-lab generate [--start HH:MM:SS:FF] [--length SEC] [--fps N]
                   [--rate HZ] [--preroll FRAMES] [--out FILE]
  ltc-lab decode-file <FILE>
  ltc-lab listen [--fps N]          (requires: cargo build --features live)

Use ';' before the frames field of --start for drop-frame, e.g. 01:00:00;00";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("generate") => cmd_generate(&flags(&args[1..])),
        Some("decode-file") => match positional(&args[1..]) {
            Some(path) => cmd_decode_file(&path),
            None => fail("decode-file needs a file path"),
        },
        Some("listen") => {
            let f = flags(&args[1..]);
            cmd_listen(get_u32(&f, "fps", 30));
        }
        Some("-h") | Some("--help") | None => println!("{HELP}"),
        Some(other) => fail(&format!("unknown command {other:?}\n\n{HELP}")),
    }
}

/// Collect `--key value` pairs into a map. Bare tokens are ignored here.
fn flags(args: &[String]) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut i = 0;
    while i < args.len() {
        if let Some(key) = args[i].strip_prefix("--") {
            if i + 1 < args.len() {
                m.insert(key.to_string(), args[i + 1].clone());
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    m
}

fn positional(args: &[String]) -> Option<String> {
    args.iter().find(|a| !a.starts_with("--")).cloned()
}

fn get_str(m: &HashMap<String, String>, k: &str, d: &str) -> String {
    m.get(k).cloned().unwrap_or_else(|| d.to_string())
}
fn get_u32(m: &HashMap<String, String>, k: &str, d: u32) -> u32 {
    m.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
}
fn get_f64(m: &HashMap<String, String>, k: &str, d: f64) -> f64 {
    m.get(k).and_then(|v| v.parse().ok()).unwrap_or(d)
}

fn fail(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(2);
}

fn cmd_generate(f: &HashMap<String, String>) {
    let start = Timecode::parse(&get_str(f, "start", "01:00:00:00")).unwrap_or_else(|e| fail(&e));
    let length = get_f64(f, "length", 10.0);
    let fps = get_u32(f, "fps", 30);
    let rate = get_u32(f, "rate", 48_000);
    let preroll = get_u32(f, "preroll", 0);

    if !rate.is_multiple_of(80 * fps) {
        fail(&format!(
            "{rate} Hz is not an integer number of samples per bit at {fps} fps. \
             Use 24/25/30 fps; 29.97 needs fractional timing (not yet supported)."
        ));
    }

    let payload_frames = (length * fps as f64).round() as u32;
    let payload = sequence(start, payload_frames, fps as u8);

    let mut frames: Vec<[bool; 80]> = Vec::new();
    for _ in 0..preroll {
        frames.push(encode_frame(start));
    }
    frames.extend(payload.iter().map(|&tc| encode_frame(tc)));

    let samples = biphase::encode_bits_to_samples(&frames, rate, fps, 16_000);
    let path = f
        .get("out")
        .cloned()
        .unwrap_or_else(|| default_name(start, length, fps, rate));
    wav::write_wav_mono16(&path, &samples, rate).expect("write wav");

    println!(
        "wrote {path}\n  start {start}, {length}s, {fps} fps, {rate} Hz, \
         {payload_frames} payload frames (+{preroll} preroll), {} samples",
        samples.len()
    );
}

fn default_name(tc: Timecode, length: f64, fps: u32, rate: u32) -> String {
    let df = if tc.drop_frame { "_DF" } else { "" };
    format!(
        "ltc_{:02}h{:02}m{:02}s{:02}f_{}s_{}fps_{}Hz{}.wav",
        tc.hours,
        tc.minutes,
        tc.seconds,
        tc.frames,
        trim_len(length),
        fps,
        rate,
        df
    )
}

fn trim_len(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{}", x as i64)
    } else {
        format!("{x}").replace('.', "p")
    }
}

fn cmd_decode_file(path: &str) {
    let (pcm, sr) = wav::read_wav_mono16(path).expect("read wav");
    let mut dec = Decoder::new();
    let mut frames = Vec::new();
    for &s in &pcm {
        if let Some(tc) = dec.push_sample(s) {
            frames.push(tc);
        }
    }
    println!("{path}: {} frames at {} Hz", frames.len(), sr);
    for (n, tc) in frames.iter().enumerate() {
        if n < 3 || n + 3 >= frames.len() {
            println!("  {tc}");
        } else if n == 3 {
            println!("  ...");
        }
    }
}

#[cfg(feature = "live")]
fn cmd_listen(_fps: u32) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("no default input device");
    let name = device.name().unwrap_or_else(|_| "?".into());
    let config = device
        .default_input_config()
        .expect("no default input config");
    println!(
        "listening on \"{name}\" @ {} Hz (Ctrl-C to stop)",
        config.sample_rate().0
    );

    let (mut tx, mut rx) = rtrb::RingBuffer::<Timecode>::new(256);
    let mut dec = Decoder::new();
    let err = |e| eprintln!("stream error: {e}");

    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => device.build_input_stream(
            &config.into(),
            move |data: &[f32], _| {
                for &s in data {
                    if let Some(tc) = dec.push_sample((s * i16::MAX as f32) as i16) {
                        let _ = tx.push(tc);
                    }
                }
            },
            err,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            &config.into(),
            move |data: &[i16], _| {
                for &s in data {
                    if let Some(tc) = dec.push_sample(s) {
                        let _ = tx.push(tc);
                    }
                }
            },
            err,
            None,
        ),
        other => fail(&format!("unsupported sample format: {other:?}")),
    }
    .expect("build input stream");

    stream.play().expect("play stream");
    let mut last = String::new();
    loop {
        while let Ok(tc) = rx.pop() {
            let s = tc.to_string();
            if s != last {
                println!("{s}");
                last = s;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[cfg(not(feature = "live"))]
fn cmd_listen(_fps: u32) {
    fail("live capture not built in. Rebuild with:  cargo run --features live -- listen");
}
