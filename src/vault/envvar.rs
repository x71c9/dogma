use anyhow::{bail, Result};

use crate::config::VaultEntry;

pub fn read(entry: &VaultEntry, env: &str, vault_key: &str) -> Result<String> {
  let envvar_map = entry.envvar.as_ref().ok_or_else(|| {
    anyhow::anyhow!("vault key '{vault_key}': no envvar defined")
  })?;

  let var_name = envvar_map.get(env).ok_or_else(|| {
    anyhow::anyhow!(
      "vault key '{vault_key}': no envvar defined for env '{env}'"
    )
  })?;

  match std::env::var(var_name) {
        Ok(val) if !val.is_empty() => Ok(val),
        Ok(_) => bail!(
            "env var '{var_name}' is set but empty (vault key '{vault_key}', env '{env}')"
        ),
        Err(_) => bail!(
            "env var '{var_name}' is not set (vault key '{vault_key}', env '{env}')"
        ),
    }
}
