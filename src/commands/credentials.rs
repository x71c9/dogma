use anyhow::{bail, Result};

use crate::config::normalize::normalize;
use crate::config::{CredentialValue, DogmaConfig};
use crate::vault;

pub fn run(repo_root: &std::path::Path, env: &str) -> Result<()> {
    let config = normalize(repo_root)?;
    print_credentials(&config, env)
}

pub fn print_credentials(config: &DogmaConfig, env: &str) -> Result<()> {
    if !config.env.contains(&env.to_string()) {
        bail!("env '{env}' is not declared in dogma.yml");
    }

    let infra = match &config.infra {
        Some(i) => i,
        None => return Ok(()), // no credentials to print
    };

    for (var_name, cred) in &infra.credentials {
        let value = match cred {
            CredentialValue::Static(s) => s.clone(),
            CredentialValue::FromVault { vault_ref, .. } => {
                vault::read(config, env, vault_ref)?
            }
        };
        println!("export {}={}", var_name, shell_escape(&value));
    }

    Ok(())
}

/// Single-quote escape a value so it survives eval safely.
pub fn shell_escape(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}
