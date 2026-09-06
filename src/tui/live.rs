//! Live tab: decode LTC from an external audio input in real time.
//!
//! With the `live` feature you pick an input device and watch timecode roll as
//! it arrives, with a peak level meter to confirm signal is present. Without
//! `live` (the default build has no cpal), the tab renders a short "rebuild
//! with --features live" note so the UI still makes sense.

use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::Frame;

#[cfg(feature = "live")]
pub(crate) use enabled::LiveScreen;

#[cfg(not(feature = "live"))]
pub(crate) use disabled::LiveScreen;

/// Shared screen contract so `mod.rs` dispatches to it like the other tabs.
impl LiveScreen {
    pub(crate) fn new() -> Self {
        Self::make()
    }
}

#[cfg(feature = "live")]
mod enabled {
    use super::*;
    use crate::frame::Timecode;
    use crate::tui::audio::{input_devices, InputEngine};

    use cpal::Device;
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph};

    pub(crate) struct LiveScreen {
        devices: Vec<(String, Device)>,
        sel: usize,
        engine: Option<InputEngine>,
        current: Option<Timecode>,
        status: String,
    }

    impl LiveScreen {
        pub(super) fn make() -> Self {
            let devices = input_devices();
            let status = if devices.is_empty() {
                "no input devices found".into()
            } else {
                "Up/Down: device   Enter/Space: start".into()
            };
            LiveScreen {
                devices,
                sel: 0,
                engine: None,
                current: None,
                status,
            }
        }

        pub(crate) fn handle_key(&mut self, code: KeyCode) {
            match &self.engine {
                // Picker mode: choose a device and start capture.
                None => match code {
                    KeyCode::Up => self.sel = self.sel.saturating_sub(1),
                    KeyCode::Down => {
                        if self.sel + 1 < self.devices.len() {
                            self.sel += 1;
                        }
                    }
                    KeyCode::Char('r') => self.refresh(),
                    KeyCode::Enter | KeyCode::Char(' ') => self.start(),
                    _ => {}
                },
                // Capturing: stop and return to the picker.
                Some(_) => {
                    if matches!(code, KeyCode::Char(' ') | KeyCode::Char('r') | KeyCode::Esc) {
                        self.stop();
                    }
                }
            }
        }

        fn refresh(&mut self) {
            self.devices = input_devices();
            self.sel = 0;
            self.status = if self.devices.is_empty() {
                "no input devices found".into()
            } else {
                "Up/Down: device   Enter/Space: start".into()
            };
        }

        fn start(&mut self) {
            let Some((name, device)) = self.devices.get(self.sel) else {
                self.status = "no input devices found".into();
                return;
            };
            match InputEngine::start(device) {
                Ok(engine) => {
                    self.status = format!("listening on \"{name}\"  (Space: stop)");
                    self.current = None;
                    self.engine = Some(engine);
                }
                Err(e) => self.status = format!("start failed: {e}"),
            }
        }

        fn stop(&mut self) {
            self.engine = None;
            self.status = "stopped   Up/Down: device   Enter/Space: start".into();
        }

        pub(crate) fn tick(&mut self, _dt_secs: f64) {
            if let Some(engine) = &mut self.engine {
                if let Some(tc) = engine.poll() {
                    self.current = Some(tc);
                }
            }
        }

        pub(crate) fn render(&self, f: &mut Frame, area: Rect) {
            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(8),
                    Constraint::Min(0),
                    Constraint::Length(3),
                ])
                .split(area);

            self.render_devices(f, rows[0]);
            self.render_timecode(f, rows[1]);
            self.render_meter(f, rows[2]);
        }

        fn render_devices(&self, f: &mut Frame, area: Rect) {
            let listening = self.engine.is_some();
            let items: Vec<ListItem> = self
                .devices
                .iter()
                .enumerate()
                .map(|(i, (name, _))| {
                    let selected = i == self.sel;
                    let marker = if selected { "> " } else { "  " };
                    let style = if selected && !listening {
                        Style::default()
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD)
                    } else if selected {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::Gray)
                    };
                    ListItem::new(Line::from(Span::styled(format!("{marker}{name}"), style)))
                })
                .collect();

            let title = if listening {
                " input (capturing) "
            } else {
                " input device "
            };
            let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
            f.render_widget(list, area);
        }

        fn render_timecode(&self, f: &mut Frame, area: Rect) {
            let tc = self
                .current
                .map(|t| t.to_string())
                .unwrap_or_else(|| "--:--:--:--".into());
            let (state, state_color) = match &self.engine {
                Some(_) if self.current.is_some() => ("locked", Color::Green),
                Some(_) => ("waiting for signal…", Color::Yellow),
                None => ("stopped", Color::DarkGray),
            };
            let frames = self.engine.as_ref().map(|e| e.frames()).unwrap_or(0);

            let big = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    tc,
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(state, Style::default().fg(state_color))),
                Line::from(Span::styled(
                    format!("frames decoded: {frames}"),
                    Style::default().fg(Color::DarkGray),
                )),
                Line::from(Span::styled(
                    format!("  {}", self.status),
                    Style::default().fg(Color::Yellow),
                )),
            ])
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" live timecode "));
            f.render_widget(big, area);
        }

        fn render_meter(&self, f: &mut Frame, area: Rect) {
            let level = self.engine.as_ref().map(|e| e.peak()).unwrap_or(0.0);
            let gauge = Gauge::default()
                .block(Block::default().borders(Borders::ALL).title(" input level "))
                .gauge_style(Style::default().fg(Color::Green))
                .ratio(level.clamp(0.0, 1.0));
            f.render_widget(gauge, area);
        }
    }
}

#[cfg(not(feature = "live"))]
mod disabled {
    use super::*;
    use ratatui::layout::Alignment;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph};

    /// Placeholder shown when the crate is built without the `live` feature.
    pub(crate) struct LiveScreen;

    impl LiveScreen {
        pub(super) fn make() -> Self {
            LiveScreen
        }

        pub(crate) fn handle_key(&mut self, _code: KeyCode) {}

        pub(crate) fn tick(&mut self, _dt_secs: f64) {}

        pub(crate) fn render(&self, f: &mut Frame, area: Rect) {
            let dim = Style::default().fg(Color::DarkGray);
            let p = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled("live audio capture is not built in", dim)),
                Line::from(""),
                Line::from(Span::styled(
                    "rebuild with:  cargo run --features live",
                    Style::default().fg(Color::Cyan),
                )),
            ])
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" live "));
            f.render_widget(p, area);
        }
    }
}
