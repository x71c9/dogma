//! Minimal ANSI logging. All output goes to stderr.
//! Colors suppressed when stderr is not a tty or NO_COLOR is set.

use std::sync::OnceLock;

fn colors_enabled() -> bool {
  static ENABLED: OnceLock<bool> = OnceLock::new();
  *ENABLED.get_or_init(|| {
    std::env::var_os("NO_COLOR").is_none()
      && std::io::IsTerminal::is_terminal(&std::io::stderr())
  })
}

const RED: &str = "\x1b[31m";
const BLUE: &str = "\x1b[34m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Normal progress line: red "dogma: " + plain message.
pub fn info(msg: &str) {
  if colors_enabled() {
    eprintln!("{RED}dogma:{RESET} {msg}");
  } else {
    eprintln!("dogma: {msg}");
  }
}

/// Section header: red "dogma: " + cyan message.
pub fn step(msg: &str) {
  if colors_enabled() {
    eprintln!("{RED}dogma:{RESET} {BLUE}{msg}{RESET}");
  } else {
    eprintln!("dogma: {msg}");
  }
}

/// De-emphasized line (cached, skipped, unchanged): entirely dim.
pub fn dim(msg: &str) {
  if colors_enabled() {
    eprintln!("{DIM}dogma: {msg}{RESET}");
  } else {
    eprintln!("dogma: {msg}");
  }
}

/// Warning: red "dogma: " + yellow message.
pub fn warn(msg: &str) {
  if colors_enabled() {
    eprintln!("{RED}dogma:{RESET} {YELLOW}{msg}{RESET}");
  } else {
    eprintln!("dogma: {msg}");
  }
}

/// Fatal error: red "dogma: error: " + message.
pub fn error(msg: &str) {
  if colors_enabled() {
    eprintln!("{RED}dogma: error:{RESET} {msg}");
  } else {
    eprintln!("dogma: error: {msg}");
  }
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => { $crate::log::info(&format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_step {
    ($($arg:tt)*) => { $crate::log::step(&format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_dim {
    ($($arg:tt)*) => { $crate::log::dim(&format!($($arg)*)) };
}
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => { $crate::log::warn(&format!($($arg)*)) };
}
