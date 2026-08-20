use ltc_lab::decoder::Decoder;
use ltc_lab::frame::{encode_frame, sequence, Timecode};
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

/// A frame rate carries a nominal count (for numbering) and a real rate (for
/// sample timing); drop-frame only applies to the 29.97 family.
struct RateSpec {
    nominal: u8,
    real: f64,
    drop: bool,
}

fn parse_rate(fps: &str, start_is_df: bool) -> Result<RateSpec, String> {
    let df_2997 = 30_000.0 / 1001.0; // 29.97003
    let df_2398 = 24_000.0 / 1001.0; // 23.976
    let spec = match fps {
        "24" => RateSpec {
            nominal: 24,
            real: 24.0,
            drop: false,
        },
        "25" => RateSpec {
            nominal: 25,
            real: 25.0,
            drop: false,
        },
        "30" if start_is_df => RateSpec {
            nominal: 30,
            real: df_2997,
            drop: true,
        },
        "30" => RateSpec {
            nominal: 30,
            real: 30.0,
            drop: false,
        },
        "29.97" | "29.97df" => RateSpec {
            nominal: 30,
            real: df_2997,
            drop: start_is_df,
        },
        "23.976" | "23.98" => RateSpec {
            nominal: 24,
            real: df_2398,
            drop: false,
        },
        other => {
            return Err(format!(
                "unsupported --fps {other:?} (try 24, 25, 30, 29.97, 23.976)"
            ))
        }
    };
    if start_is_df && !spec.drop {
        return Err(format!(
            "drop-frame (';') is only defined for 29.97, not {fps} fps"
        ));
    }
    Ok(spec)
}

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
    let mut start =
        Timecode::parse(&get_str(f, "start", "01:00:00:00")).unwrap_or_else(|e| fail(&e));
    let rate = parse_rate(&get_str(f, "fps", "30"), start.drop_frame).unwrap_or_else(|e| fail(&e));
    // Normalise: the frame's drop-frame bit follows the resolved rate.
    start.drop_frame = rate.drop;

    let length = get_f64(f, "length", 10.0);
    let preroll = get_u32(f, "preroll", 0);
    let sample_rate = get_u32(f, "rate", 48_000);

    // Frame COUNT follows the real rate (so a DF file's timecode tracks wall
    // clock); frame NUMBERING follows the nominal rate via next_frame.
    let payload_frames = (length * rate.real).round() as u32;
    let payload = sequence(start, payload_frames, rate.nominal);

    let mut frames: Vec<[bool; 80]> = Vec::new();
    for _ in 0..preroll {
        frames.push(encode_frame(start));
    }
    frames.extend(payload.iter().map(|&tc| encode_frame(tc)));

    let samples = ltc_lab::biphase::encode_bits_to_samples(&frames, sample_rate, rate.real, 16_000);
    let path = f
        .get("out")
        .cloned()
        .unwrap_or_else(|| default_name(start, length, &rate, sample_rate));
    ltc_lab::wav::write_wav_mono16(&path, &samples, sample_rate).expect("write wav");

    let end = payload.last().copied().unwrap_or(start);
    println!(
        "wrote {path}\n  {start} -> {end}  ({length}s, {} fps{}, {sample_rate} Hz)\n  \
         {payload_frames} payload frames (+{preroll} preroll), {} samples",
        fmt_rate(&rate),
        if rate.drop { " drop" } else { "" },
        samples.len()
    );
}

fn fmt_rate(r: &RateSpec) -> String {
    if (r.real - r.real.round()).abs() < 1e-6 {
        format!("{}", r.nominal)
    } else {
        format!("{:.2}", r.real)
    }
}

fn default_name(tc: Timecode, length: f64, rate: &RateSpec, sr: u32) -> String {
    let df = if rate.drop { "DF" } else { "" };
    format!(
        "ltc_{:02}h{:02}m{:02}s{:02}f_{}s_{}fps{}_{}Hz.wav",
        tc.hours,
        tc.minutes,
        tc.seconds,
        tc.frames,
        trim_len(length),
        fmt_rate(rate).replace('.', "p"),
        df,
        sr
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
    let (pcm, sr) = ltc_lab::wav::read_wav_mono16(path).expect("read wav");
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
