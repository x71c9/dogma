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
const GREEN: &str = "\x1b[32m";
const BLUE: &str = "\x1b[34m";
const YELLOW: &str = "\x1b[33m";
const DIM: &str = "\x1b[2m";
const RESET: &str = "\x1b[0m";

/// Splits a leading "subsystem: " off a message, returning (tag, rest).
/// A tag is the leading run of word/`-` chars immediately followed by ": ".
/// Messages without that shape yield `(None, whole message)`.
fn split_tag(msg: &str) -> (Option<&str>, &str) {
  if let Some((head, rest)) = msg.split_once(": ") {
    if !head.is_empty()
      && head.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
      return (Some(head), rest);
    }
  }
  (None, msg)
}

/// Renders "[dogma] [tag] rest" (or "[dogma] rest" when `tag` is None) with the
/// whole line wrapped in `color`. The leading "subsystem: " in the message is
/// always stripped — it is no longer shown as a tag. `color` empty (or colors
/// disabled) prints uncolored.
fn emit(color: &str, tag: Option<&str>, msg: &str) {
  let (_subsystem, rest) = split_tag(msg);
  let line = match tag {
    Some(t) => format!("[dogma] [{t}] {rest}"),
    None => format!("[dogma] {rest}"),
  };
  if colors_enabled() && !color.is_empty() {
    eprintln!("{color}{line}{RESET}");
  } else {
    eprintln!("{line}");
  }
}

/// Normal progress line: default color.
pub fn info(msg: &str) {
  emit("", None, msg);
}

/// Section header: blue.
pub fn step(msg: &str) {
  emit(BLUE, None, msg);
}

/// De-emphasized line (cached, skipped, unchanged): dim.
pub fn dim(msg: &str) {
  emit(DIM, None, msg);
}

/// Warning: yellow, tagged "[warning]".
pub fn warn(msg: &str) {
  emit(YELLOW, Some("warning"), msg);
}

/// Formats a `git status`-style entry: a colored status letter then the path,
/// indented two spaces (e.g. "  M shell.nix"). Green for added, red for deleted,
/// yellow for modified/renamed.
pub fn status_line(status: char, path: &str) {
  if colors_enabled() {
    let color = match status {
      'A' => GREEN,
      'D' => RED,
      _ => YELLOW,
    };
    eprintln!("  {color}{status}{RESET} {path}");
  } else {
    eprintln!("  {status} {path}");
  }
}

/// "[dogma] " prefix for inline prompts (no trailing newline). Use with
/// `eprint!` so prompts match the bracketed style of info lines.
pub fn prompt_prefix() -> String {
  "[dogma] ".to_string()
}

/// Fatal error: whole line red, rendered as "[dogma] [error] <msg>".
pub fn error(msg: &str) {
  emit(RED, Some("error"), msg);
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
