use anyhow::Result;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::commands::env_cmd::collect_env;
use crate::config::normalize::normalize;
use crate::log_info;

pub fn run(repo_root: &Path, env: &str) -> Result<()> {
  let config = normalize(repo_root)?;
  let vars = collect_env(&config, env, repo_root)?;
  log_info!("entering {env} shell (exit to return)");
  exec_shell(env, vars)
}

/// Spawn a shell with the given env vars set and a dogma prompt.
/// Uses exec(2) — only returns on error.
pub fn exec_shell(env: &str, vars: Vec<(String, String)>) -> Result<()> {
  let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());

  let mut cmd = Command::new(&shell);
  for (k, v) in &vars {
    cmd.env(k, v);
  }

  if shell.ends_with("bash") {
    let rcfile = write_bash_rcfile(env)?;
    cmd.arg("--rcfile").arg(&rcfile);
    let err = cmd.exec();
    let _ = std::fs::remove_file(&rcfile);
    return Err(err.into());
  }

  let err = cmd.exec();
  Err(err.into())
}

fn write_bash_rcfile(env: &str) -> Result<std::path::PathBuf> {
  use std::io::Write;

  let path = std::env::temp_dir().join(format!("dogma-shell-{env}.bashrc"));
  let mut f = std::fs::File::create(&path)?;
  writeln!(f, r#"[ -f ~/.bashrc ] && source ~/.bashrc"#)?;
  writeln!(
    f,
    r#"PROMPT_COMMAND='PS1="\[\e[31m\][dogma-{env} \t] \w \$ \[\e[0m\]"'"#,
    env = env
  )?;
  Ok(path)
}
