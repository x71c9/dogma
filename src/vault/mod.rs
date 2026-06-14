mod envvar;
mod pass;

use anyhow::Result;

use crate::config::DogmaConfig;

pub enum Backend {
  Envvar,
  Pass,
}

impl Backend {
  pub fn from_env() -> Self {
    match std::env::var("DOGMA_VAULT").as_deref() {
      Ok("pass") => Backend::Pass,
      _ => Backend::Envvar,
    }
  }
}

/// Read one vault key for the given env, using whichever backend is configured.
pub fn read(
  config: &DogmaConfig,
  env: &str,
  vault_key: &str,
) -> Result<String> {
  let entry = config.vault.get(vault_key).ok_or_else(|| {
    anyhow::anyhow!("vault key '{vault_key}' is not defined in dogma.yml")
  })?;

  match Backend::from_env() {
    Backend::Envvar => envvar::read(entry, env, vault_key),
    Backend::Pass => pass::read(entry, env, vault_key),
  }
}
