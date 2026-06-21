use anyhow::{bail, Result};
use std::path::Path;

use crate::commands::credentials::shell_escape;
use crate::config::normalize::normalize;
use crate::config::{DogmaConfig, SecretLeaf};
use crate::infra::output::{read_cached, resolve_infra_credentials};
use crate::vault;

pub fn run(repo_root: &Path, env: &str) -> Result<()> {
  crate::log::set_quiet(true);
  let config = normalize(repo_root)?;
  print_env(&config, env, repo_root)
}

pub fn print_env(
  config: &DogmaConfig,
  env: &str,
  repo_root: &Path,
) -> Result<()> {
  if !config.env.contains(&env.to_string()) {
    bail!("env '{env}' is not declared in dogma.yml");
  }

  let infra_creds = resolve_infra_credentials(config, env)?;

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

      println!("export {}={}", var_name, shell_escape(&value));
    }
  }

  Ok(())
}
