use ltc_lab::decoder::Decoder;
use ltc_lab::generate::generate_to_wav;
use std::collections::HashMap;

const HELP: &str = "\
ltc-lab — generate and decode SMPTE LTC timecode audio

USAGE:
  ltc-lab generate [--start HH:MM:SS:FF] [--length SEC] [--fps RATE]
                   [--rate HZ] [--preroll FRAMES] [--out FILE]
  ltc-lab decode-file <FILE>
  ltc-lab listen [--fps N]          (requires: cargo build --features live)

--fps accepts: 24, 25, 30, 29.97, 23.976
--start: use ';' before the frames field for drop-frame, e.g. 01:00:00;00
         (';' with --fps 30 is treated as 29.97 drop-frame, the usual shorthand)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("generate") => cmd_generate(&flags(&args[1..])),
        Some("decode-file") => match positional(&args[1..]) {
            Some(path) => cmd_decode_file(&path),
            None => fail("decode-file needs a file path"),
        },
        Some("listen") => cmd_listen(get_u32(&flags(&args[1..]), "fps", 30)),
        Some("-h") | Some("--help") | None => println!("{HELP}"),
        Some(other) => fail(&format!("unknown command {other:?}\n\n{HELP}")),
    }
}

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
    let start = get_str(f, "start", "01:00:00:00");
    let fps = get_str(f, "fps", "30");
    let length = get_f64(f, "length", 10.0);
    let preroll = get_u32(f, "preroll", 0);
    let sample_rate = get_u32(f, "rate", 48_000);
    let out = f.get("out").cloned();

    let report = generate_to_wav(&start, length, &fps, sample_rate, preroll, out)
        .unwrap_or_else(|e| fail(&e));

    println!(
        "wrote {path}\n  {} -> {}  ({length}s, {} fps{}, {sample_rate} Hz)\n  \
         {} payload frames (+{preroll} preroll), {} samples",
        report.start,
        report.end,
        report.rate.label(),
        if report.rate.drop { " drop" } else { "" },
        report.payload_frames,
        report.samples,
        path = report.path,
    );
}

fn cmd_decode_file(path: &str) {
    let (pcm, sr) = ltc_lab::wav::read_wav_mono16(path)
        .unwrap_or_else(|e| fail(&format!("reading {path:?}: {e}")));
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
    use ltc_lab::frame::Timecode;

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
