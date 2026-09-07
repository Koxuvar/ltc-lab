//! Terminal UI over the ltc_lab core. Three tabs: Generate (edit a timecode +
//! params, write a WAV), Decode (load a WAV, inspect it, and play it back while
//! timecode rolls), and Live (decode LTC from an audio input in real time).
//!
//! The Decode and Live tabs use the streaming decoder differently depending on
//! the build: with `feature = "live"` they drive real cpal streams (output for
//! Decode, input for Live); without it, Decode falls back to a wall-clock
//! simulation and Live shows a "rebuild with --features live" note.
//!
//! Each screen (`generate`, `decode`, `live`) owns its state, key handling, and
//! rendering; this module is the shell that wires them together: top-level
//! navigation (tab switching, quit), the tab bar / footer chrome, and the
//! terminal setup + event loop. Adding a new screen means adding one module,
//! one `TabId` variant, and one dispatch arm here — the existing screens are
//! untouched.

#[cfg(feature = "live")]
mod audio;
mod decode;
mod generate;
mod live;

use std::io;
use std::time::{Duration, Instant};

use decode::DecodeScreen;
use generate::GenForm;
use live::LiveScreen;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::{Frame, Terminal};

#[derive(Clone, Copy, PartialEq)]
enum TabId {
    Generate,
    Decode,
    Live,
}

struct App {
    tab: TabId,
    gen: GenForm,
    dec: DecodeScreen,
    live: LiveScreen,
}

impl App {
    fn new() -> Self {
        App {
            tab: TabId::Generate,
            gen: GenForm::new(),
            dec: DecodeScreen::new(),
            live: LiveScreen::new(),
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
                TabId::Decode => TabId::Live,
                TabId::Live => TabId::Generate,
            };
            return false;
        }
        match self.tab {
            TabId::Generate => self.gen.handle_key(key.code),
            TabId::Decode => self.dec.handle_key(key.code),
            TabId::Live => self.live.handle_key(key.code),
        }
        false
    }

    fn tick(&mut self, dt_secs: f64) {
        self.dec.tick(dt_secs);
        self.live.tick(dt_secs);
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
        TabId::Live => 2,
    };
    let tabs = Tabs::new(vec!["Generate", "Decode", "Live"])
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
        TabId::Generate => app.gen.render(f, chunks[1]),
        TabId::Decode => app.dec.render(f, chunks[1]),
        TabId::Live => app.live.render(f, chunks[1]),
    }

    let footer = Paragraph::new(Line::from(Span::styled(
        " Tab: switch   Up/Down: field/device   Left/Right: fps   Enter: action   Space: play/pause/start   r: reset   Ctrl-Q: quit ",
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(footer, chunks[2]);
}

// ------------------------------- shell ------------------------------------

fn run_loop() -> io::Result<()> {
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

/// Entry point for the `ltc-tui` binary.
pub fn run() -> io::Result<()> {
    run_loop()
}

// ------------------------------- tests ------------------------------------

// The smoke test drives the Decode tab's simulation backend via
// `load_samples_for_test`, which only exists in the default (non-`live`) build.
#[cfg(all(test, not(feature = "live")))]
mod tests {
    use super::*;
    use crate::biphase::encode_bits_to_samples;
    use crate::frame::{encode_frame, sequence, Timecode};

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
            .map(|&t| encode_frame(t, 30))
            .collect();
        let samples = encode_bits_to_samples(&frames, 48_000, 30.0, 16_000);

        let mut app = App::new();
        app.tab = TabId::Decode;
        app.dec.set_path_for_test("example.wav");
        app.dec.load_samples_for_test(samples, 48_000);
        app.tick(1.5);

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
}
