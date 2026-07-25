use anyhow::Result;

use crate::config::normalize::normalize;
use crate::config::DogmaConfig;

pub fn run(repo_root: &std::path::Path, env: &str) -> Result<()> {
  let config = normalize(repo_root)?;
  print_credentials(&config, env)
}

/// Resolve `infra.credentials` for `env`, validating the env first.
pub fn collect_credentials(
  config: &DogmaConfig,
  env: &str,
) -> Result<Vec<(String, String)>> {
  config.ensure_env(env)?;
  super::infra::resolve_credentials(config, env)
}

pub fn print_credentials(config: &DogmaConfig, env: &str) -> Result<()> {
  for (var_name, value) in collect_credentials(config, env)? {
    println!("export {}={}", var_name, shell_escape(&value));
  }

  Ok(())
}

/// Single-quote escape a value so it survives eval safely.
pub fn shell_escape(value: &str) -> String {
  format!("'{}'", value.replace('\'', r"'\''"))
}
