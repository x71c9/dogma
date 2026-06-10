use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::normalize::normalize;
use crate::config::validate::validate;
use crate::config::{CredentialValue, DogmaConfig};
use crate::error::check_dep;
use crate::vault;
use crate::{log_info, log_step};

pub fn apply(repo_root: &Path, env: &str, unit: &str, migrate_state: bool) -> Result<()> {
    let config = normalize(repo_root)?;
    validate(&config)?;
    run_infra(&config, repo_root, env, unit, migrate_state, "apply")
}

pub fn destroy(repo_root: &Path, env: &str, unit: &str, migrate_state: bool) -> Result<()> {
    let config = normalize(repo_root)?;
    validate(&config)?;
    run_infra(&config, repo_root, env, unit, migrate_state, "destroy")
}

fn run_infra(
    config: &DogmaConfig,
    repo_root: &Path,
    env: &str,
    unit: &str,
    migrate_state: bool,
    subcommand: &str,
) -> Result<()> {
    if !config.env.contains(&env.to_string()) {
        bail!("env '{env}' is not declared in dogma.yml");
    }

    let infra = config
        .infra
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("no 'infra' block in dogma.yml"))?;

    let cli = &infra.cli;
    check_dep(cli, &format!("install {cli} and make sure it is on PATH"))?;

    let infra_dir = repo_root.join(infra.path.trim_start_matches("./"));
    let unit_dir = infra_dir.join(unit);
    if !unit_dir.is_dir() {
        bail!("unit '{unit}' not found: {}", unit_dir.display());
    }

    let credentials = resolve_credentials(config, env)?;

    log_step!("infra: === init: {unit} ===");
    run_init(config, repo_root, InitArgs { cli, unit_dir: &unit_dir, env, unit, migrate_state, credentials: &credentials })?;

    log_step!("infra: === {subcommand}: {unit} ===");
    run_subcommand(config, repo_root, cli, &unit_dir, env, subcommand, &credentials)?;

    Ok(())
}

pub fn resolve_credentials(config: &DogmaConfig, env: &str) -> Result<Vec<(String, String)>> {
    let infra = match &config.infra {
        Some(i) => i,
        None => return Ok(vec![]),
    };

    let mut creds = Vec::new();
    for (var_name, cred) in &infra.credentials {
        let value = match cred {
            CredentialValue::Static(s) => s.clone(),
            CredentialValue::FromVault { vault_ref, .. } => {
                log_info!("infra: resolving credential {var_name} ...");
                vault::read(config, env, vault_ref)?
            }
        };
        creds.push((var_name.clone(), value));
    }
    Ok(creds)
}

struct InitArgs<'a> {
    cli: &'a str,
    unit_dir: &'a Path,
    env: &'a str,
    unit: &'a str,
    migrate_state: bool,
    credentials: &'a [(String, String)],
}

fn run_init(config: &DogmaConfig, repo_root: &Path, args: InitArgs<'_>) -> Result<()> {
    let InitArgs { cli, unit_dir, env, unit, migrate_state, credentials } = args;
    let infra = config.infra.as_ref().unwrap();
    let infra_path = infra.path.trim_start_matches("./");
    let backend_conf = repo_root.join(format!("{infra_path}/env/{env}/backend.conf"));

    if !backend_conf.exists() {
        bail!("backend config not found: {}", backend_conf.display());
    }

    let reconfigure_flag = if migrate_state {
        "-migrate-state"
    } else {
        "-reconfigure"
    };

    let state_key = format!("key={unit}/terraform.tfstate");
    let backend_conf_flag = format!("-backend-config={}", backend_conf.display());

    let status = Command::new(cli)
        .current_dir(unit_dir)
        .args([
            "init",
            reconfigure_flag,
            &backend_conf_flag,
            &format!("-backend-config={state_key}"),
        ])
        .envs(credentials.iter().cloned())
        .status()
        .with_context(|| format!("failed to run '{cli} init'"))?;

    if !status.success() {
        bail!("'{cli} init' failed for unit '{}'", unit_dir.display());
    }
    Ok(())
}

fn run_subcommand(
    config: &DogmaConfig,
    repo_root: &Path,
    cli: &str,
    unit_dir: &Path,
    env: &str,
    subcommand: &str,
    credentials: &[(String, String)],
) -> Result<()> {
    let infra = config.infra.as_ref().unwrap();
    let infra_path = infra.path.trim_start_matches("./");
    let tfvars = repo_root.join(format!("{infra_path}/env/{env}/{env}.tfvars"));

    if !tfvars.exists() {
        bail!("tfvars not found: {}", tfvars.display());
    }

    let status = Command::new(cli)
        .current_dir(unit_dir)
        .args([subcommand, &format!("-var-file={}", tfvars.display())])
        .envs(credentials.iter().cloned())
        .status()
        .with_context(|| format!("failed to run '{cli} {subcommand}'"))?;

    if !status.success() {
        bail!("'{cli} {subcommand}' failed");
    }
    Ok(())
}
