use anyhow::{Context, Result};
use indexmap::IndexMap;
use std::path::Path;

use super::{DogmaConfig, EnvOrMap, HostnameField, IpField, NixBlock};

pub fn normalize(repo_root: &Path) -> Result<DogmaConfig> {
    let dogma_yml = repo_root.join("dogma.yml");
    let raw = std::fs::read_to_string(&dogma_yml)
        .with_context(|| format!("dogma.yml not found: {}", dogma_yml.display()))?;

    let mut config: DogmaConfig =
        serde_yaml::from_str(&raw).context("failed to parse dogma.yml")?;

    expand_defaults(&mut config);
    write_expanded(repo_root, &config)?;

    Ok(config)
}

#[allow(dead_code)]
pub fn load_expanded(repo_root: &Path) -> Result<DogmaConfig> {
    let expanded_path = repo_root.join(".dogma/dogma-expanded.yml");
    let raw = std::fs::read_to_string(&expanded_path)
        .with_context(|| format!("expanded config not found: {}", expanded_path.display()))?;
    serde_yaml::from_str(&raw).context("failed to parse dogma-expanded.yml")
}

fn expand_defaults(config: &mut DogmaConfig) {
    let envs = config.env.clone();
    let name = config.name.clone();

    // Infra defaults
    if let Some(infra) = &mut config.infra {
        if infra.cli.is_empty() {
            infra.cli = "tofu".to_string();
        }
        if infra.path.is_empty() {
            infra.path = "./infra".to_string();
        }
    }

    // Nix defaults: secrets and sops derived from path
    let nix_path = config.nix.path.clone();
    if config.nix.secrets == NixBlock::default().secrets {
        config.nix.secrets = format!("{}/secrets", nix_path);
    }
    if config.nix.sops == NixBlock::default().sops {
        config.nix.sops = format!("{}/.sops.yaml", nix_path);
    }

    // Machines: default user, expand hostname and ip shorthands
    for (_, machine) in &mut config.machines {
        // user default
        if machine.user.is_empty() {
            machine.user = "root".to_string();
        }

        // hostname: flat string with {env} placeholder -> per-env map
        if let HostnameField::Flat(hn) = &machine.hostname.clone() {
            let per_env: IndexMap<String, String> = envs
                .iter()
                .map(|e| (e.clone(), hn.replace("{env}", e)))
                .collect();
            machine.hostname = HostnameField::PerEnv(per_env);
        }

        // ip shorthand: single IpEntry -> per-env map
        if let IpField::Shorthand(entry) = &machine.ip.clone() {
            let per_env: IndexMap<String, _> = envs
                .iter()
                .map(|e| (e.clone(), entry.clone()))
                .collect();
            machine.ip = IpField::PerEnv(per_env);
        }
    }

    // Vault: expand envvar and pass to per-env maps
    for (key, entry) in &mut config.vault {
        let auto_var = key.to_uppercase().replace('-', "_");

        // envvar: absent -> auto-derive; flat string -> same for all envs; per-env -> as-is
        match &entry.envvar {
            None => {
                let m: IndexMap<String, String> = envs
                    .iter()
                    .map(|e| (e.clone(), auto_var.clone()))
                    .collect();
                entry.envvar = Some(EnvOrMap::PerEnv(m));
            }
            Some(EnvOrMap::Flat(var)) => {
                let var = var.clone();
                let m: IndexMap<String, String> =
                    envs.iter().map(|e| (e.clone(), var.clone())).collect();
                entry.envvar = Some(EnvOrMap::PerEnv(m));
            }
            Some(EnvOrMap::PerEnv(_)) => {}
        }

        // pass: absent -> auto-derive as <name>/<env>/<key>; flat with {env} -> expand; per-env -> as-is
        match &entry.pass {
            None => {
                let m: IndexMap<String, String> = envs
                    .iter()
                    .map(|e| (e.clone(), format!("{}/{}/{}", name, e, key)))
                    .collect();
                entry.pass = Some(EnvOrMap::PerEnv(m));
            }
            Some(EnvOrMap::Flat(path)) => {
                let path = path.clone();
                let m: IndexMap<String, String> = envs
                    .iter()
                    .map(|e| (e.clone(), path.replace("{env}", e)))
                    .collect();
                entry.pass = Some(EnvOrMap::PerEnv(m));
            }
            Some(EnvOrMap::PerEnv(_)) => {}
        }
    }
}

fn write_expanded(repo_root: &Path, config: &DogmaConfig) -> Result<()> {
    let dogma_dir = repo_root.join(".dogma");
    std::fs::create_dir_all(&dogma_dir)
        .with_context(|| format!("failed to create {}", dogma_dir.display()))?;

    let expanded_path = dogma_dir.join("dogma-expanded.yml");
    let yaml = serde_yaml::to_string(config).context("failed to serialize expanded config")?;
    std::fs::write(&expanded_path, yaml)
        .with_context(|| format!("failed to write {}", expanded_path.display()))?;

    Ok(())
}
