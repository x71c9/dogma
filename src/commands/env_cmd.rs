use anyhow::Result;
use std::path::Path;

use crate::commands::credentials::shell_escape;
use crate::commands::infra::resolve_credentials;
use crate::config::normalize::normalize;
use crate::config::{DogmaConfig, SecretLeaf};
use crate::infra::output::read_cached;
use crate::vault;

pub fn run(repo_root: &Path, env: &str) -> Result<()> {
  crate::log::set_quiet(true);
  let config = normalize(repo_root)?;
  print_env(&config, env, repo_root)
}

pub fn collect_env(
  config: &DogmaConfig,
  env: &str,
  repo_root: &Path,
) -> Result<Vec<(String, String)>> {
  config.ensure_env(env)?;

  let infra_creds = resolve_credentials(config, env)?;
  let mut vars = Vec::new();

  for (group, fields) in &config.secrets {
    for (field, leaf) in fields {
      let var_name = format!("{}_{}", group, field).to_uppercase();

      let value = match leaf {
        SecretLeaf::FromVault { vault_ref, .. } => {
          vault::read(config, env, vault_ref)?
        }
        SecretLeaf::FromInfra { unit, output, .. } => {
          read_cached(config, repo_root, env, unit, output, &infra_creds)?
        }
      };

      vars.push((var_name, value));
    }
  }

  Ok(vars)
}

pub fn print_env(
  config: &DogmaConfig,
  env: &str,
  repo_root: &Path,
) -> Result<()> {
  for (var_name, value) in collect_env(config, env, repo_root)? {
    println!("export {}={}", var_name, shell_escape(&value));
  }

  Ok(())
}
