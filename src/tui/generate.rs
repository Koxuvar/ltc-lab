//! Generate screen: edit a timecode + params, write a WAV.

use crate::generate::generate_to_wav;
use crate::rate::PRESETS;

use crossterm::event::KeyCode;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// Generator form. Fields 0..=3 are start, length, fps, sample-rate. fps is
/// chosen from presets with Left/Right; the others are free text.
pub(crate) struct GenForm {
    start: String,
    length: String,
    fps_idx: usize,
    rate: String,
    focus: usize,
    status: String,
}

impl GenForm {
    const N: usize = 4;

    pub(crate) fn new() -> Self {
        GenForm {
            start: "01:00:00:00".into(),
            length: "10".into(),
            fps_idx: 2, // "30"
            rate: "48000".into(),
            focus: 0,
            status: "edit fields, Enter to write".into(),
        }
    }

    pub(crate) fn handle_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up => self.focus_prev(),
            KeyCode::Down => self.focus_next(),
            KeyCode::Left if self.focus == 2 => self.cycle_fps(false),
            KeyCode::Right if self.focus == 2 => self.cycle_fps(true),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Enter => self.submit(),
            KeyCode::Char(c) => self.input_char(c),
            _ => {}
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

    pub(crate) fn render(&self, f: &mut Frame, area: Rect) {
        let labels = ["start", "length (s)", "fps", "sample rate"];
        let values = [
            self.start.clone(),
            self.length.clone(),
            PRESETS[self.fps_idx].to_string(),
            self.rate.clone(),
        ];
        let mut lines: Vec<Line> = vec![Line::from("")];
        for i in 0..Self::N {
            let focused = i == self.focus;
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
            format!("  {}", self.status),
            Style::default().fg(Color::Yellow),
        )));

        let p = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).title(" generate "));
        f.render_widget(p, area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
