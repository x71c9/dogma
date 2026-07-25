pub mod completions;
pub mod credentials;
pub mod env_cmd;
pub mod infra;
pub mod nixos;
pub mod output;
pub mod pipeline;
pub mod shell;

use anyhow::Result;
use std::io::{self, Write};

use crate::git;
use crate::log_info;

/// Print the "working tree has uncommitted changes" warning with a
/// `git status`-style file list.
fn warn_dirty(prefix: &str, dirty: &[git::DirtyFile]) {
  crate::log_warn!("{prefix} working tree has uncommitted changes:");
  eprintln!();
  for f in dirty {
    crate::log::status_line(f.status, &f.path);
  }
  eprintln!();
}

/// Commit all dirty changes, taking the message from `-m` when given and
/// prompting interactively otherwise (Y/n confirm, then a message with a
/// heuristic suggestion). An empty message falls back to `fallback_msg`.
/// Expects `warn_dirty` to have been called already.
fn commit_dirty(
  repo: &git2::Repository,
  dirty: &[git::DirtyFile],
  commit_msg: Option<String>,
  prefix: &str,
  action: &str,
  fallback_msg: &str,
) -> Result<()> {
  let msg = match commit_msg {
    Some(m) => {
      log_info!("{prefix} -m flag provided — committing with: {m}");
      m
    }
    None => {
      eprint!(
        "{}commit these changes before {action}? [Y/n] ",
        crate::log::prompt_prefix()
      );
      io::stderr().flush()?;
      let mut answer = String::new();
      io::stdin().read_line(&mut answer)?;
      if matches!(answer.trim().to_lowercase().as_str(), "n" | "no") {
        anyhow::bail!("aborted — commit or stash your changes and re-run");
      }
      match git::suggest_commit_msg(dirty) {
        Some(suggested) => {
          log_info!(
            "{prefix} suggested message: {}",
            crate::log::cyan(&suggested)
          );
          let typed =
            read_prompt_line("commit message (leave blank to accept): ")?;
          if typed.is_empty() {
            suggested
          } else {
            typed
          }
        }
        None => read_prompt_line("commit message: ")?,
      }
    }
  };

  let msg = if msg.is_empty() {
    fallback_msg.to_string()
  } else {
    msg
  };

  git::commit_all(repo, &msg)?;
  log_info!("{prefix} committed: {msg}");
  Ok(())
}

fn read_prompt_line(prompt: &str) -> Result<String> {
  eprint!("{}{prompt}", crate::log::prompt_prefix());
  io::stderr().flush()?;
  let mut line = String::new();
  io::stdin().read_line(&mut line)?;
  Ok(line.trim().to_string())
}
