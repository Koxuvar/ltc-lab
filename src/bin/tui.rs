//! Terminal UI over the ltc_lab core. Two tabs: Generate (edit a timecode +
//! params, write a WAV) and Decode (load a WAV and watch timecode roll as it
//! "plays" through the streaming decoder — the stand-in for a live readout
//! until audio input is wired up).
//!
//! The app model and its update methods are plain, unit-tested Rust; the
//! ratatui/crossterm code is a thin render + event shell around them.

use std::io;
use std::time::{Duration, Instant};

use ltc_lab::analyze::{analyze, FileSummary};
use ltc_lab::decoder::Decoder;
use ltc_lab::frame::Timecode;
use ltc_lab::generate::generate_to_wav;
use ltc_lab::rate::PRESETS;
use ltc_lab::wav;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph, Tabs};
use ratatui::{Frame, Terminal};

// ------------------------------- model ------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum TabId {
    Generate,
    Decode,
}

/// Generator form. Fields 0..=3 are start, length, fps, sample-rate. fps is
/// chosen from presets with Left/Right; the others are free text.
struct GenForm {
    start: String,
    length: String,
    fps_idx: usize,
    rate: String,
    focus: usize,
    status: String,
}

impl GenForm {
    const N: usize = 4;

    fn new() -> Self {
        GenForm {
            start: "01:00:00:00".into(),
            length: "10".into(),
            fps_idx: 2, // "30"
            rate: "48000".into(),
            focus: 0,
            status: "edit fields, Enter to write".into(),
        }
    }

    fn focus_next(&mut self) {
        self.focus = (self.focus + 1) % Self::N;
    }
    fn focus_prev(&mut self) {
        self.focus = (self.focus + Self::N - 1) % Self::N;
    }

    fn input_char(&mut self, c: char) {
        match self.focus {
            0 => self.start.push(c),
            1 => self.length.push(c),
            3 => self.rate.push(c),
            _ => {} // fps uses Left/Right
        }
    }

    fn backspace(&mut self) {
        match self.focus {
            0 => {
                self.start.pop();
            }
            1 => {
                self.length.pop();
            }
            3 => {
                self.rate.pop();
            }
            _ => {}
        }
    }

    fn cycle_fps(&mut self, forward: bool) {
        let n = PRESETS.len();
        self.fps_idx = if forward {
            (self.fps_idx + 1) % n
        } else {
            (self.fps_idx + n - 1) % n
        };
    }

    fn submit(&mut self) {
        let length: f64 = match self.length.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.status = format!("bad length: {:?}", self.length);
                return;
            }
        };
        let rate: u32 = match self.rate.trim().parse() {
            Ok(v) => v,
            Err(_) => {
                self.status = format!("bad rate: {:?}", self.rate);
                return;
            }
        };
        match generate_to_wav(
            self.start.trim(),
            length,
            PRESETS[self.fps_idx],
            rate,
            0,
            None,
        ) {
            Ok(r) => self.status = format!("wrote {} ({} frames)", r.path, r.payload_frames),
            Err(e) => self.status = format!("error: {e}"),
        }
    }
}

/// Plays a loaded WAV through the streaming decoder, paced to wall-clock time.
struct DecodePlayer {
    samples: Vec<i16>,
    sample_rate: u32,
    pos: usize,
    dec: Decoder,
    current: Option<Timecode>,
    frames_seen: usize,
    playing: bool,
}

impl DecodePlayer {
    fn new(samples: Vec<i16>, sample_rate: u32) -> Self {
        DecodePlayer {
            samples,
            sample_rate,
            pos: 0,
            dec: Decoder::new(),
            current: None,
            frames_seen: 0,
            playing: true,
        }
    }

    /// Feed the next `dt_secs` worth of samples through the decoder.
    fn advance(&mut self, dt_secs: f64) {
        if !self.playing || self.pos >= self.samples.len() {
            return;
        }
        let n = (self.sample_rate as f64 * dt_secs) as usize;
        let end = (self.pos + n).min(self.samples.len());
        for i in self.pos..end {
            if let Some(tc) = self.dec.push_sample(self.samples[i]) {
                self.current = Some(tc);
                self.frames_seen += 1;
            }
        }
        self.pos = end;
        if self.pos >= self.samples.len() {
            self.playing = false;
        }
    }

    fn reset(&mut self) {
        self.pos = 0;
        self.dec = Decoder::new();
        self.current = None;
        self.frames_seen = 0;
        self.playing = true;
    }

    fn progress(&self) -> f64 {
        if self.samples.is_empty() {
            0.0
        } else {
            self.pos as f64 / self.samples.len() as f64
        }
    }
}

struct App {
    tab: TabId,
    gen: GenForm,
    dec_path: String,
    dec_status: String,
    player: Option<DecodePlayer>,
    summary: Option<FileSummary>,
}

impl App {
    fn new() -> Self {
        App {
            tab: TabId::Generate,
            gen: GenForm::new(),
            dec_path: String::new(),
            dec_status: "enter a .wav path, Enter to load".into(),
            player: None,
            summary: None,
        }
    }

    /// Returns true when the app should quit.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('q'))
        {
            return true;
        }
        if key.code == KeyCode::Tab {
            self.tab = match self.tab {
                TabId::Generate => TabId::Decode,
                TabId::Decode => TabId::Generate,
            };
            return false;
        }
        match self.tab {
            TabId::Generate => self.gen_key(key.code),
            TabId::Decode => self.dec_key(key.code),
        }
        false
    }

    fn gen_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.gen.focus_prev(),
            KeyCode::Down => self.gen.focus_next(),
            KeyCode::Left if self.gen.focus == 2 => self.gen.cycle_fps(false),
            KeyCode::Right if self.gen.focus == 2 => self.gen.cycle_fps(true),
            KeyCode::Backspace => self.gen.backspace(),
            KeyCode::Enter => self.gen.submit(),
            KeyCode::Char(c) => self.gen.input_char(c),
            _ => {}
        }
    }

    fn dec_key(&mut self, code: KeyCode) {
        match &mut self.player {
            None => match code {
                KeyCode::Char(c) => self.dec_path.push(c),
                KeyCode::Backspace => {
                    self.dec_path.pop();
                }
                KeyCode::Enter => self.load_decode(),
                _ => {}
            },
            Some(p) => match code {
                KeyCode::Char(' ') => p.playing = !p.playing,
                KeyCode::Char('r') => p.reset(),
                KeyCode::Backspace => {
                    self.player = None;
                    self.summary = None;
                    self.dec_status = "enter a .wav path, Enter to load".into();
                }
                _ => {}
            },
        }
    }

    fn load_decode(&mut self) {
        match wav::read_wav_mono16(self.dec_path.trim()) {
            Ok((samples, sr)) => {
                // Analyze the whole file up front (one decode pass) for the
                // details panel, then hand the samples to the player.
                let summary = analyze(&samples, sr);
                self.dec_status = format!("loaded {} samples @ {} Hz", samples.len(), sr);
                self.summary = Some(summary);
                self.player = Some(DecodePlayer::new(samples, sr));
            }
            Err(e) => self.dec_status = format!("load failed: {e}"),
        }
    }

    fn tick(&mut self, dt_secs: f64) {
        if let Some(p) = &mut self.player {
            p.advance(dt_secs);
        }
    }
}

// ------------------------------- render -----------------------------------

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.size());

    let tab_idx = match app.tab {
        TabId::Generate => 0,
        TabId::Decode => 1,
    };
    let tabs = Tabs::new(vec!["Generate", "Decode"])
        .select(tab_idx)
        .block(Block::default().borders(Borders::ALL).title(" ltc-lab "))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    f.render_widget(tabs, chunks[0]);

    match app.tab {
        TabId::Generate => render_generate(f, app, chunks[1]),
        TabId::Decode => render_decode(f, app, chunks[1]),
    }

    let footer = Paragraph::new(Line::from(Span::styled(
        " Tab: switch   Up/Down: field   Left/Right: fps   Enter: action   Space: play/pause   r: reset   Ctrl-Q: quit ",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(footer, chunks[2]);
}

fn render_generate(f: &mut Frame, app: &App, area: Rect) {
    let g = &app.gen;
    let labels = ["start", "length (s)", "fps", "sample rate"];
    let values = [
        g.start.clone(),
        g.length.clone(),
        PRESETS[g.fps_idx].to_string(),
        g.rate.clone(),
    ];
    let mut lines: Vec<Line> = vec![Line::from("")];
    for i in 0..GenForm::N {
        let focused = i == g.focus;
        let marker = if focused { "> " } else { "  " };
        let style = if focused {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{marker}{:>12}: ", labels[i]), style),
            Span::styled(values[i].clone(), style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("  {}", g.status),
        Style::default().fg(Color::Yellow),
    )));

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" generate "));
    f.render_widget(p, area);
}

fn fmt_dur(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - (m as f64) * 60.0;
    format!("{m:02}:{s:06.3}")
}

fn render_decode(f: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(3),
        ])
        .split(area);

    let path = Paragraph::new(Line::from(vec![
        Span::styled("path: ", Style::default().fg(Color::DarkGray)),
        Span::raw(app.dec_path.clone()),
    ]))
    .block(Block::default().borders(Borders::ALL).title(" file "));
    f.render_widget(path, rows[0]);

    // Middle row: details on the left, live timecode on the right.
    let mid = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(rows[1]);

    render_details(f, app, mid[0]);
    render_timecode(f, app, mid[1]);

    let ratio = app.player.as_ref().map(|p| p.progress()).unwrap_or(0.0);
    let gauge = Gauge::default()
        .block(Block::default().borders(Borders::ALL).title(" progress "))
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(ratio.clamp(0.0, 1.0));
    f.render_widget(gauge, rows[2]);
}

fn render_details(f: &mut Frame, app: &App, area: Rect) {
    let dim = Style::default().fg(Color::DarkGray);
    let val = Style::default().fg(Color::White);
    let row = |label: &str, value: String| {
        Line::from(vec![
            Span::styled(format!("{label:>12}: "), dim),
            Span::styled(value, val),
        ])
    };

    let lines: Vec<Line> = match &app.summary {
        None => vec![
            Line::from(""),
            Line::from(Span::styled("  load a file to inspect", dim)),
        ],
        Some(s) => {
            let opt_tc =
                |t: Option<Timecode>| t.map(|t| t.to_string()).unwrap_or_else(|| "—".into());
            let cont = match s.continuous {
                Some(true) => Span::styled("yes", Style::default().fg(Color::Green)),
                Some(false) => Span::styled("no (jumps)", Style::default().fg(Color::Red)),
                None => Span::styled("—", dim),
            };
            vec![
                Line::from(""),
                row("start", opt_tc(s.start)),
                row("end", opt_tc(s.end)),
                row("frames", s.frame_count.to_string()),
                row("rate", format!("{} fps", s.rate_label())),
                row("duration", fmt_dur(s.duration_secs)),
                row("sample rate", format!("{} Hz", s.sample_rate)),
                Line::from(vec![
                    Span::styled(format!("{:>12}: ", "continuous"), dim),
                    cont,
                ]),
            ]
        }
    };

    let p = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" details "));
    f.render_widget(p, area);
}

fn render_timecode(f: &mut Frame, app: &App, area: Rect) {
    let tc = app
        .player
        .as_ref()
        .and_then(|p| p.current)
        .map(|t| t.to_string())
        .unwrap_or_else(|| "--:--:--:--".into());
    let state = match &app.player {
        Some(p) if p.playing => "playing",
        Some(_) => "paused",
        None => "no file",
    };
    let big = Paragraph::new(vec![
        Line::from(""),
        Line::from(Span::styled(
            tc,
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(state, Style::default().fg(Color::DarkGray))),
        Line::from(Span::styled(
            format!("  {}", app.dec_status),
            Style::default().fg(Color::Yellow),
        )),
    ])
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::ALL).title(" timecode "));
    f.render_widget(big, area);
}

// ------------------------------- shell ------------------------------------

fn run() -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();
    let tick = Duration::from_millis(33);
    let mut last = Instant::now();

    let result = loop {
        if let Err(e) = terminal.draw(|f| ui(f, &app)) {
            break Err(e);
        }
        let timeout = tick.saturating_sub(last.elapsed());
        match event::poll(timeout) {
            Ok(true) => match event::read() {
                Ok(Event::Key(k)) if k.kind == KeyEventKind::Press => {
                    if app.on_key(k) {
                        break Ok(());
                    }
                }
                Ok(_) => {}
                Err(e) => break Err(e),
            },
            Ok(false) => {}
            Err(e) => break Err(e),
        }
        let dt = last.elapsed();
        if dt >= tick {
            app.tick(dt.as_secs_f64());
            last = Instant::now();
        }
    };

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn main() {
    if let Err(e) = run() {
        eprintln!("tui error: {e}");
        std::process::exit(1);
    }
}

// ------------------------------- tests ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ltc_lab::biphase::encode_bits_to_samples;
    use ltc_lab::frame::{encode_frame, sequence};

    #[test]
    fn gen_form_text_editing() {
        let mut g = GenForm::new();
        g.focus = 0;
        g.start.clear();
        for c in "12:00:00:00".chars() {
            g.input_char(c);
        }
        assert_eq!(g.start, "12:00:00:00");
        g.backspace();
        assert_eq!(g.start, "12:00:00:0");
    }

    #[test]
    fn gen_form_focus_wraps() {
        let mut g = GenForm::new();
        g.focus = 0;
        g.focus_prev();
        assert_eq!(g.focus, GenForm::N - 1);
        g.focus_next();
        assert_eq!(g.focus, 0);
    }

    #[test]
    fn gen_form_fps_cycles_and_ignores_typing() {
        let mut g = GenForm::new();
        g.focus = 2;
        let before = g.fps_idx;
        g.input_char('9'); // fps field ignores text
        assert_eq!(g.fps_idx, before);
        g.cycle_fps(true);
        assert_eq!(g.fps_idx, (before + 1) % PRESETS.len());
    }

    fn dump(buf: &ratatui::buffer::Buffer) -> String {
        let area = *buf.area();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf.get(x, y).symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_decode_view_smoke() {
        use ltc_lab::analyze::analyze;
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let start = Timecode {
            hours: 1,
            minutes: 0,
            seconds: 0,
            frames: 0,
            drop_frame: false,
        };
        let frames: Vec<[bool; 80]> = sequence(start, 90, 30)
            .iter()
            .map(|&t| encode_frame(t))
            .collect();
        let samples = encode_bits_to_samples(&frames, 48_000, 30.0, 16_000);

        let mut app = App::new();
        app.tab = TabId::Decode;
        app.dec_path = "example.wav".into();
        app.summary = Some(analyze(&samples, 48_000));
        let mut player = DecodePlayer::new(samples, 48_000);
        player.advance(1.5);
        app.player = Some(player);

        let mut terminal = Terminal::new(TestBackend::new(96, 22)).unwrap();
        terminal.draw(|f| ui(f, &app)).unwrap();
        let text = dump(terminal.backend().buffer());

        // Prove the panel actually rendered the derived metadata.
        assert!(text.contains("details"), "details panel missing");
        assert!(text.contains("start"), "start row missing");
        assert!(text.contains("30 fps"), "rate missing");
        assert!(text.contains("continuous"), "continuity row missing");
        println!("\n{text}");
    }

    #[test]
    fn decode_player_advances_timecode() {
        let start = Timecode {
            hours: 1,
            minutes: 0,
            seconds: 0,
            frames: 0,
            drop_frame: false,
        };
        let frames: Vec<[bool; 80]> = sequence(start, 60, 30)
            .iter()
            .map(|&t| encode_frame(t))
            .collect();
        let samples = encode_bits_to_samples(&frames, 48_000, 30.0, 16_000);

        let mut p = DecodePlayer::new(samples, 48_000);
        for _ in 0..25 {
            p.advance(0.1);
        }
        assert!(!p.playing, "should have consumed all samples");
        assert!(
            p.frames_seen > 50,
            "expected most frames, got {}",
            p.frames_seen
        );
        let tc = p.current.expect("should have a current timecode");
        assert!(
            tc.hours == 1 && tc.minutes == 0 && tc.seconds >= 1,
            "unexpected {tc}"
        );
    }
}
