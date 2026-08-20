fn main() {
    if let Err(e) = ltc_lab::tui::run() {
        eprintln!("tui error: {e}");
        std::process::exit(1);
    }
}
