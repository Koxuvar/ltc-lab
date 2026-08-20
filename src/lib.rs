//! LTC core: frame model, biphase codec, streaming decoder, WAV I/O

pub mod analyze;
pub mod biphase;
pub mod decoder;
pub mod frame;
pub mod generate;
pub mod rate;
#[cfg(feature = "tui")]
pub mod tui;
pub mod wav;
