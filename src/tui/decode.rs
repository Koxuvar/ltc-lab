//! Decode screen: load a WAV and watch timecode roll as it "plays" through
//! the streaming decoder — the stand-in for a live readout until audio input
//! is wired up.

use crate::analyze::{analyze, FileSummary};
use crate::decoder::Decoder;
use crate::frame::Timecode;
use crate::wav;

use crossterm::event::KeyCode;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

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

/// Decode screen state: the file path being entered/loaded, its status line,
/// the whole-file analysis (once loaded), and the playback decoder.
pub(crate) struct DecodeScreen {
    path: String,
    status: String,
    player: Option<DecodePlayer>,
    summary: Option<FileSummary>,
}

impl DecodeScreen {
    pub(crate) fn new() -> Self {
        DecodeScreen {
            path: String::new(),
            status: "enter a .wav path, Enter to load".into(),
            player: None,
            summary: None,
        }
    }

    pub(crate) fn handle_key(&mut self, code: KeyCode) {
        match &mut self.player {
            None => match code {
                KeyCode::Char(c) => self.path.push(c),
                KeyCode::Backspace => {
                    self.path.pop();
                }
                KeyCode::Enter => self.load(),
                _ => {}
            },
            Some(p) => match code {
                KeyCode::Char(' ') => p.playing = !p.playing,
                KeyCode::Char('r') => p.reset(),
                KeyCode::Backspace => {
                    self.player = None;
                    self.summary = None;
                    self.status = "enter a .wav path, Enter to load".into();
                }
                _ => {}
            },
        }
    }

    fn load(&mut self) {
        match wav::read_wav_mono16(self.path.trim()) {
            Ok((samples, sr)) => {
                // Analyze the whole file up front (one decode pass) for the
                // details panel, then hand the samples to the player.
                let summary = analyze(&samples, sr);
                self.status = format!("loaded {} samples @ {} Hz", samples.len(), sr);
                self.summary = Some(summary);
                self.player = Some(DecodePlayer::new(samples, sr));
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    pub(crate) fn tick(&mut self, dt_secs: f64) {
        if let Some(p) = &mut self.player {
            p.advance(dt_secs);
        }
    }

    pub(crate) fn render(&self, f: &mut Frame, area: Rect) {
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
            Span::raw(self.path.clone()),
        ]))
        .block(Block::default().borders(Borders::ALL).title(" file "));
        f.render_widget(path, rows[0]);

        // Middle row: details on the left, live timecode on the right.
        let mid = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[1]);

        self.render_details(f, mid[0]);
        self.render_timecode(f, mid[1]);

        let ratio = self.player.as_ref().map(|p| p.progress()).unwrap_or(0.0);
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(" progress "))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(ratio.clamp(0.0, 1.0));
        f.render_widget(gauge, rows[2]);
    }

    fn render_details(&self, f: &mut Frame, area: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let val = Style::default().fg(Color::White);
        let row = |label: &str, value: String| {
            Line::from(vec![
                Span::styled(format!("{label:>12}: "), dim),
                Span::styled(value, val),
            ])
        };

        let lines: Vec<Line> = match &self.summary {
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

        let p = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" details "));
        f.render_widget(p, area);
    }

    fn render_timecode(&self, f: &mut Frame, area: Rect) {
        let tc = self
            .player
            .as_ref()
            .and_then(|p| p.current)
            .map(|t| t.to_string())
            .unwrap_or_else(|| "--:--:--:--".into());
        let state = match &self.player {
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
                format!("  {}", self.status),
                Style::default().fg(Color::Yellow),
            )),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" timecode "));
        f.render_widget(big, area);
    }

    #[cfg(test)]
    pub(crate) fn set_path_for_test(&mut self, path: &str) {
        self.path = path.into();
    }

    #[cfg(test)]
    pub(crate) fn load_samples_for_test(&mut self, samples: Vec<i16>, sample_rate: u32) {
        self.summary = Some(analyze(&samples, sample_rate));
        self.player = Some(DecodePlayer::new(samples, sample_rate));
    }
}

fn fmt_dur(secs: f64) -> String {
    let m = (secs / 60.0).floor() as u64;
    let s = secs - (m as f64) * 60.0;
    format!("{m:02}:{s:06.3}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biphase::encode_bits_to_samples;
    use crate::frame::{encode_frame, sequence};

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
            .map(|&t| encode_frame(t, 30))
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
