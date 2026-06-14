use anyhow::{bail, Result};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use crate::config::normalize::normalize;
use crate::config::{CredentialValue, DogmaConfig};
use crate::log_info;
use crate::vault;

pub fn run(repo_root: &Path, env: &str) -> Result<()> {
  let config = normalize(repo_root)?;
  exec_shell(&config, env)
}

fn exec_shell(config: &DogmaConfig, env: &str) -> Result<()> {
  if !config.env.contains(&env.to_string()) {
    bail!("env '{env}' is not declared in dogma.yml");
  }

  let mut vars: Vec<(String, String)> = Vec::new();

  if let Some(infra) = &config.infra {
    for (var_name, cred) in &infra.credentials {
      let value = match cred {
        CredentialValue::Static(s) => s.clone(),
        CredentialValue::FromVault { vault_ref, .. } => {
          log_info!("resolving {var_name} ...");
          vault::read(config, env, vault_ref)?
        }
      };
      vars.push((var_name.clone(), value));
    }
  }

  let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".to_string());
  log_info!("entering {env} shell (exit to return)");

  let mut cmd = Command::new(&shell);
  for (k, v) in &vars {
    cmd.env(k, v);
  }

  // For bash: write a minimal rcfile that sets a prompt indicating the dogma env
  if shell.ends_with("bash") {
    let rcfile = write_bash_rcfile(env)?;
    cmd.arg("--rcfile").arg(&rcfile);
    let err = cmd.exec();
    // exec only returns on error
    // best-effort cleanup of the temp file (may not run if exec succeeds)
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
