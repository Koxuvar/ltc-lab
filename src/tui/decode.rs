//! Decode screen: load a WAV, inspect its whole-file analysis, and watch
//! timecode roll.
//!
//! Two playback backends, chosen at build time:
//!
//! - Default (`not(feature = "live")`): a [`DecodePlayer`] feeds the loaded
//!   samples through the streaming decoder on the UI tick, paced to wall-clock
//!   time. No sound — a stand-in for real playback.
//! - `feature = "live"`: after loading you pick an output device and the WAV is
//!   *played* through it via [`OutputEngine`], decoding the same samples in the
//!   audio callback so the readout tracks what you hear.

use crate::analyze::{analyze, FileSummary};
use crate::frame::Timecode;
use crate::wav;

use crossterm::event::KeyCode;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::Frame;

#[cfg(feature = "live")]
use crate::tui::audio::{output_devices, OutputEngine};
#[cfg(feature = "live")]
use cpal::Device;
#[cfg(feature = "live")]
use ratatui::widgets::{List, ListItem};

// --------------------------- simulation backend ---------------------------

/// Plays a loaded WAV through the streaming decoder, paced to wall-clock time.
/// The default (no-audio) backend for the Decode tab.
#[cfg(not(feature = "live"))]
struct DecodePlayer {
    samples: Vec<i16>,
    sample_rate: u32,
    pos: usize,
    dec: crate::decoder::Decoder,
    current: Option<Timecode>,
    frames_seen: usize,
    playing: bool,
}

#[cfg(not(feature = "live"))]
impl DecodePlayer {
    fn new(samples: Vec<i16>, sample_rate: u32) -> Self {
        DecodePlayer {
            samples,
            sample_rate,
            pos: 0,
            dec: crate::decoder::Decoder::new(),
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
        self.dec = crate::decoder::Decoder::new();
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

// ----------------------------- live backend -------------------------------

/// Live playback backend: pick an output device, then play + decode through it.
#[cfg(feature = "live")]
enum Playback {
    /// A file is loaded; choose which output device to play it on.
    Picking {
        devices: Vec<(String, Device)>,
        sel: usize,
        samples: Vec<i16>,
        sample_rate: u32,
    },
    /// Playing through `engine`; `current` is the last decoded timecode.
    Playing {
        name: String,
        device: Device,
        samples: Vec<i16>,
        sample_rate: u32,
        engine: OutputEngine,
        current: Option<Timecode>,
    },
}

// ------------------------------- screen -----------------------------------

/// Decode screen state: the file path being entered/loaded, its status line,
/// the whole-file analysis (once loaded), and the playback backend.
pub(crate) struct DecodeScreen {
    path: String,
    status: String,
    summary: Option<FileSummary>,
    #[cfg(not(feature = "live"))]
    player: Option<DecodePlayer>,
    #[cfg(feature = "live")]
    playback: Option<Playback>,
}

const PROMPT: &str = "enter a .wav path, Enter to load";

impl DecodeScreen {
    pub(crate) fn new() -> Self {
        DecodeScreen {
            path: String::new(),
            status: PROMPT.into(),
            summary: None,
            #[cfg(not(feature = "live"))]
            player: None,
            #[cfg(feature = "live")]
            playback: None,
        }
    }

    // --- key handling -----------------------------------------------------

    #[cfg(not(feature = "live"))]
    pub(crate) fn handle_key(&mut self, code: KeyCode) {
        match &mut self.player {
            None => self.handle_path_key(code),
            Some(p) => match code {
                KeyCode::Char(' ') => p.playing = !p.playing,
                KeyCode::Char('r') => p.reset(),
                KeyCode::Backspace => self.unload(),
                _ => {}
            },
        }
    }

    #[cfg(feature = "live")]
    pub(crate) fn handle_key(&mut self, code: KeyCode) {
        if self.playback.is_none() {
            self.handle_path_key(code);
            return;
        }
        // Backspace unloads from either playback state.
        if code == KeyCode::Backspace {
            self.unload();
            return;
        }
        let picking = matches!(self.playback, Some(Playback::Picking { .. }));
        if picking {
            self.handle_picking_key(code);
        } else {
            self.handle_playing_key(code);
        }
    }

    /// Path-entry keys, shared by both backends.
    fn handle_path_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(c) => self.path.push(c),
            KeyCode::Backspace => {
                self.path.pop();
            }
            KeyCode::Enter => self.load(),
            _ => {}
        }
    }

    #[cfg(feature = "live")]
    fn handle_picking_key(&mut self, code: KeyCode) {
        // Adjust selection in place, or compute a start request without holding
        // the borrow across the reassignment `start_playing` performs.
        let start = {
            let Some(Playback::Picking {
                devices,
                sel,
                samples,
                sample_rate,
            }) = &mut self.playback
            else {
                return;
            };
            match code {
                KeyCode::Up => {
                    *sel = sel.saturating_sub(1);
                    None
                }
                KeyCode::Down => {
                    if *sel + 1 < devices.len() {
                        *sel += 1;
                    }
                    None
                }
                KeyCode::Enter => devices
                    .get(*sel)
                    .map(|(name, device)| (name.clone(), device.clone(), samples.clone(), *sample_rate)),
                _ => None,
            }
        };
        if let Some((name, device, samples, sr)) = start {
            self.start_playing(&name, device, samples, sr);
        }
    }

    #[cfg(feature = "live")]
    fn handle_playing_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Char(' ') => {
                if let Some(Playback::Playing { engine, .. }) = &self.playback {
                    engine.set_playing(!engine.is_playing());
                }
            }
            KeyCode::Char('r') => {
                // Rebuild the engine from the top on the same device.
                let restart = match &self.playback {
                    Some(Playback::Playing {
                        name,
                        device,
                        samples,
                        sample_rate,
                        ..
                    }) => Some((name.clone(), device.clone(), samples.clone(), *sample_rate)),
                    _ => None,
                };
                if let Some((name, device, samples, sr)) = restart {
                    self.start_playing(&name, device, samples, sr);
                }
            }
            _ => {}
        }
    }

    // --- loading / lifecycle ---------------------------------------------

    fn load(&mut self) {
        match wav::read_wav_mono16(self.path.trim()) {
            Ok((samples, sr)) => {
                // Analyze the whole file up front (one decode pass) for the
                // details panel, then hand the samples to the backend.
                let summary = analyze(&samples, sr);
                self.status = format!("loaded {} samples @ {} Hz", samples.len(), sr);
                self.summary = Some(summary);
                self.on_loaded(samples, sr);
            }
            Err(e) => self.status = format!("load failed: {e}"),
        }
    }

    #[cfg(not(feature = "live"))]
    fn on_loaded(&mut self, samples: Vec<i16>, sr: u32) {
        self.player = Some(DecodePlayer::new(samples, sr));
    }

    #[cfg(feature = "live")]
    fn on_loaded(&mut self, samples: Vec<i16>, sr: u32) {
        let devices = output_devices();
        self.status = if devices.is_empty() {
            "no output devices found".into()
        } else {
            "Up/Down: output device   Enter: play".into()
        };
        self.playback = Some(Playback::Picking {
            devices,
            sel: 0,
            samples,
            sample_rate: sr,
        });
    }

    #[cfg(feature = "live")]
    fn start_playing(&mut self, name: &str, device: Device, samples: Vec<i16>, sr: u32) {
        match OutputEngine::start(&device, samples.clone(), sr) {
            Ok(engine) => {
                self.status = match engine.rate_note() {
                    Some(note) => format!("playing on \"{name}\" — {note}"),
                    None => format!("playing on \"{name}\""),
                };
                self.playback = Some(Playback::Playing {
                    name: name.to_string(),
                    device,
                    samples,
                    sample_rate: sr,
                    engine,
                    current: None,
                });
            }
            Err(e) => self.status = format!("play failed: {e}"),
        }
    }

    fn unload(&mut self) {
        #[cfg(not(feature = "live"))]
        {
            self.player = None;
        }
        #[cfg(feature = "live")]
        {
            self.playback = None;
        }
        self.summary = None;
        self.status = PROMPT.into();
    }

    // --- tick -------------------------------------------------------------

    #[cfg(not(feature = "live"))]
    pub(crate) fn tick(&mut self, dt_secs: f64) {
        if let Some(p) = &mut self.player {
            p.advance(dt_secs);
        }
    }

    #[cfg(feature = "live")]
    pub(crate) fn tick(&mut self, _dt_secs: f64) {
        if let Some(Playback::Playing { engine, current, .. }) = &mut self.playback {
            if let Some(tc) = engine.poll() {
                *current = Some(tc);
            }
        }
    }

    // --- render -----------------------------------------------------------

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

        // Middle row: details on the left, live timecode / picker on the right.
        let mid = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows[1]);

        self.render_details(f, mid[0]);
        self.render_playback(f, mid[1]);

        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::ALL).title(" progress "))
            .gauge_style(Style::default().fg(Color::Cyan))
            .ratio(self.progress_ratio().clamp(0.0, 1.0));
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

    /// Progress ratio for the bottom gauge (backend-specific).
    #[cfg(not(feature = "live"))]
    fn progress_ratio(&self) -> f64 {
        self.player.as_ref().map(|p| p.progress()).unwrap_or(0.0)
    }

    #[cfg(feature = "live")]
    fn progress_ratio(&self) -> f64 {
        match &self.playback {
            Some(Playback::Playing { engine, .. }) => engine.progress(),
            _ => 0.0,
        }
    }

    /// Right-hand panel: the rolling timecode (and, under `live`, the device
    /// picker before playback starts).
    #[cfg(not(feature = "live"))]
    fn render_playback(&self, f: &mut Frame, area: Rect) {
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
        self.render_timecode(f, area, &tc, state);
    }

    #[cfg(feature = "live")]
    fn render_playback(&self, f: &mut Frame, area: Rect) {
        match &self.playback {
            Some(Playback::Picking { devices, sel, .. }) => {
                let items: Vec<ListItem> = devices
                    .iter()
                    .enumerate()
                    .map(|(i, (name, _))| {
                        let selected = i == *sel;
                        let marker = if selected { "> " } else { "  " };
                        let style = if selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(Color::Cyan)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::Gray)
                        };
                        ListItem::new(Line::from(Span::styled(format!("{marker}{name}"), style)))
                    })
                    .collect();
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(" output device "));
                f.render_widget(list, area);
            }
            Some(Playback::Playing { engine, current, .. }) => {
                let tc = current
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "--:--:--:--".into());
                let state = if engine.finished() {
                    "finished"
                } else if engine.is_playing() {
                    "playing"
                } else {
                    "paused"
                };
                self.render_timecode(f, area, &tc, state);
            }
            None => self.render_timecode(f, area, "--:--:--:--", "no file"),
        }
    }

    /// Shared big-timecode panel.
    fn render_timecode(&self, f: &mut Frame, area: Rect, tc: &str, state: &str) {
        let big = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                tc.to_string(),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(state.to_string(), Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(
                format!("  {}", self.status),
                Style::default().fg(Color::Yellow),
            )),
        ])
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).title(" timecode "));
        f.render_widget(big, area);
    }

    #[cfg(all(test, not(feature = "live")))]
    pub(crate) fn set_path_for_test(&mut self, path: &str) {
        self.path = path.into();
    }

    #[cfg(all(test, not(feature = "live")))]
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

#[cfg(all(test, not(feature = "live")))]
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
