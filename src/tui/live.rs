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
    use tui_big_text::{BigText, PixelSize};

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

            // Bordered frame; everything else is drawn into its inner area so the
            // big readout sits above the small status lines.
            let block = Block::default().borders(Borders::ALL).title(" live timecode ");
            let inner = block.inner(area);
            f.render_widget(block, area);

            // The three small lines stay normal size, anchored to the bottom.
            let info = Paragraph::new(vec![
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
            .alignment(Alignment::Center);

            let rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(3)])
                .split(inner);
            f.render_widget(info, rows[1]);

            self.render_big_timecode(f, rows[0], &tc);
        }

        /// Draw the timecode as large block glyphs, centered in `area`, picking the
        /// largest fixed pixel size that fits the available width without clipping.
        fn render_big_timecode(&self, f: &mut Frame, area: Rect, tc: &str) {
            // font8x8 glyphs are 8x8 "pixels"; a PixelSize maps pixels to terminal
            // cells. (cols, rows) below are the resulting cell footprint per glyph.
            let glyphs = tc.chars().count() as u16;
            let (pixel_size, glyph_cols, glyph_rows) = if glyphs * 8 <= area.width {
                (PixelSize::Full, 8u16, 8u16) // full-cell pixels: tallest and widest
            } else if glyphs * 4 <= area.width {
                (PixelSize::HalfWidth, 4u16, 8u16) // half as wide, still 8 rows tall
            } else {
                (PixelSize::Quadrant, 4u16, 4u16) // half in both dimensions
            };

            let color = if self.current.is_some() {
                Color::Green
            } else {
                Color::DarkGray
            };

            let big = BigText::builder()
                .pixel_size(pixel_size)
                .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
                .lines(vec![Line::from(tc.to_string())])
                .build();

            match big {
                Ok(widget) => {
                    // BigText renders from the top-left; center it manually.
                    let w = (glyphs * glyph_cols).min(area.width);
                    let h = glyph_rows.min(area.height);
                    let x = area.x + (area.width.saturating_sub(w)) / 2;
                    let y = area.y + (area.height.saturating_sub(h)) / 2;
                    let target = Rect::new(x, y, w, h);
                    f.render_widget(widget, target);
                }
                // Defensive fallback: keep the tab usable if a glyph is unsupported.
                Err(_) => {
                    let para = Paragraph::new(Line::from(Span::styled(
                        tc.to_string(),
                        Style::default().fg(color).add_modifier(Modifier::BOLD),
                    )))
                    .alignment(Alignment::Center);
                    f.render_widget(para, area);
                }
            }
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
