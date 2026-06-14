use anyhow::{bail, Result};
use std::path::Path;

use crate::commands::credentials::shell_escape;
use crate::config::normalize::normalize;
use crate::config::{DogmaConfig, SecretLeaf};
use crate::vault;

pub fn run(repo_root: &Path, env: &str) -> Result<()> {
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

  for (group, fields) in &config.secrets {
    for (field, leaf) in fields {
      let var_name = format!("{}_{}", group, field).to_uppercase();

      let value = match leaf {
        SecretLeaf::FromVault { vault_ref, .. } => {
          vault::read(config, env, vault_ref)?
        }
        SecretLeaf::FromInfra { unit, output, .. } => {
          read_infra_output(repo_root, env, unit, output)?
        }
      };

      println!("export {}={}", var_name, shell_escape(&value));
    }
  }

  Ok(())
}

fn read_infra_output(
  repo_root: &Path,
  env: &str,
  unit: &str,
  output: &str,
) -> Result<String> {
  let cache_file = repo_root.join(format!(".dogma/cache/{env}.json"));
  if !cache_file.exists() {
    anyhow::bail!(
            "no infra cache for env '{env}': {} (run: dogma deploy --skip-sops {env})",
            cache_file.display()
        );
  }

  let raw = std::fs::read_to_string(&cache_file)?;
  let cache: serde_json::Value = serde_json::from_str(&raw)?;

  cache[unit][output]
    .as_str()
    .map(str::to_string)
    .ok_or_else(|| {
      anyhow::anyhow!(
        "output '{output}' not found in unit '{unit}' for env '{env}'"
      )
    })
}
