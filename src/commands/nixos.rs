use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::{
  DeployStrategy, DogmaConfig, IpEntry, IpField, SecretLeaf,
};
use crate::error::check_dep;
use crate::git;
use crate::infra::output as infra_output;
use crate::infra::output::{lookup_creds, EnvCreds};
use crate::vault;
use crate::{log_dim, log_info};

// ---------------------------------------------------------------------------
// Upfront dependency check
// ---------------------------------------------------------------------------

pub fn check_all_deps() -> Result<()> {
  check_dep("ssh-keyscan", "install openssh")?;
  check_dep(
    "ssh-to-age",
    "install ssh-to-age from https://github.com/Mic92/ssh-to-age",
  )?;
  check_dep("sops", "install sops from https://github.com/getsops/sops")?;
  check_dep(
    "nixos-rebuild",
    "install nixos-rebuild (available on NixOS or via nixpkgs)",
  )?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether an env's infra cache can be reused as-is. A cache is usable only if
/// it parses and has at least one unit with a non-empty output object. A missing
/// file, unparseable JSON, or a skeleton like `{"hetzner":{},"mailgun":{}}`
/// (written before the env's infra was applied) is treated as needing a refresh.
pub fn cache_is_usable(cache_file: &Path) -> bool {
  let Ok(raw) = std::fs::read_to_string(cache_file) else {
    return false;
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
    return false;
  };
  json.as_object().is_some_and(|units| {
    units
      .values()
      .any(|u| u.as_object().is_some_and(|o| !o.is_empty()))
  })
}

pub fn config_needs_infra(config: &DogmaConfig, env: &str) -> bool {
  for machine in config.machines.values() {
    if let IpField::PerEnv(map) = &machine.ip {
      if let Some(IpEntry::FromInfra { .. }) = map.get(env) {
        return true;
      }
    }
  }
  for fields in config.secrets.values() {
    for leaf in fields.values() {
      if matches!(leaf, SecretLeaf::FromInfra { .. }) {
        return true;
      }
    }
  }
  false
}

pub fn encrypt_secrets(
  config: &DogmaConfig,
  repo_root: &Path,
  all_envs: &[String],
  env_creds: &[EnvCreds],
) -> Result<()> {
  let nix_secrets = config.nix.secrets.trim_start_matches("./");
  let nix_path = config.nix.path.trim_start_matches("./");
  let sops_config = repo_root.join(format!("{nix_path}/.sops.yaml"));

  for env in all_envs {
    let infra_creds = lookup_creds(env_creds, env);
    for (host_name, machine) in &config.machines {
      for group in &machine.secrets {
        if let Some(fields) = config.secrets.get(group) {
          let mut tmp = tempfile::NamedTempFile::new()?;

          for (field, leaf) in fields {
            let value = match leaf {
              SecretLeaf::FromVault { vault_ref, .. } => {
                vault::read(config, env, vault_ref)?
              }
              SecretLeaf::FromInfra { unit, output, .. } => {
                infra_output::read_cached(
                  config,
                  repo_root,
                  env,
                  unit,
                  output,
                  infra_creds,
                )?
              }
            };
            use std::io::Write;
            writeln!(tmp, "{field}: {}", serde_json::to_string(&value)?)?;
          }

          let out_dir =
            repo_root.join(format!("{nix_secrets}/{env}/{host_name}"));
          std::fs::create_dir_all(&out_dir)?;
          let out_file = out_dir.join(format!("{group}.yaml"));

          log_info!(
            "deploy encrypting {host_name}/{env}/{group} → {}",
            out_file.display()
          );

          let status = Command::new("sops")
            .args([
              "--config",
              &sops_config.to_string_lossy(),
              "--encrypt",
              "--input-type",
              "yaml",
              "--output-type",
              "yaml",
              "--filename-override",
              &out_file.to_string_lossy(),
              tmp.path().to_str().unwrap(),
            ])
            .stdout(std::fs::File::create(&out_file)?)
            .status()
            .context("failed to run sops")?;

          if !status.success() {
            bail!("sops encryption failed for {host_name}/{env}/{group}");
          }
        }
      }
    }
  }
  Ok(())
}

pub fn verify_secrets_committed(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
) -> Result<()> {
  let nix_secrets = config.nix.secrets.trim_start_matches("./");
  let repo = git::open(repo_root)?;
  let index = repo.index()?;
  // include_untracked=false: an untracked secret file is already caught by
  // the index check below, so there's no need to scan untracked files too.
  let dirty = git::dirty_files(&repo, false)?;

  for (host_name, machine) in &config.machines {
    for group in &machine.secrets {
      let secret_file =
        repo_root.join(format!("{nix_secrets}/{env}/{host_name}/{group}.yaml"));
      if !secret_file.exists() {
        bail!(
          "secret not committed for {env}: {}\nRun: dogma deploy {env} --new",
          secret_file.display()
        );
      }
      let rel = secret_file.strip_prefix(repo_root)?;
      if index.get_path(rel, 0).is_none() {
        bail!(
          "secret not committed for {env}: {}\nRun: dogma deploy {env} --new",
          secret_file.display()
        );
      }
      let rel_str = rel.to_string_lossy();
      if dirty.iter().any(|f| f.path == rel_str) {
        bail!(
          "secret has uncommitted changes for {env}: {}\nThe working tree no longer matches what's committed — commit or discard the change, or re-run: dogma deploy {env} --new",
          secret_file.display()
        );
      }
    }
  }
  log_info!("deploy all secrets committed — ok");
  Ok(())
}

pub fn deploy_host(
  config: &DogmaConfig,
  repo_root: &Path,
  host: &str,
  env: &str,
  infra_creds: &[(String, String)],
) -> Result<String> {
  let machine = config
    .machines
    .get(host)
    .ok_or_else(|| anyhow::anyhow!("machine '{host}' not found"))?;

  let host_ip = infra_output::resolve_machine_ip(
    config,
    repo_root,
    host,
    env,
    infra_creds,
  )?;

  log_info!("deploy {host}/{env} → {host_ip}");

  let host_user = &machine.user;
  let hostname = machine.hostname.get(env, host);
  let nix_path = config.nix.path.trim_start_matches("./");
  let flake_path = repo_root.join(nix_path);

  let sudo_flag = if Command::new("ssh")
    .args([&format!("{host_user}@{host_ip}"), "sudo -n true"])
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false)
  {
    log_dim!("deploy passwordless sudo available");
    "--sudo"
  } else {
    log_info!("deploy will prompt for sudo password");
    "--ask-sudo-password"
  };

  match &config.deploy.strategy {
    DeployStrategy::NixosRebuild => {
      log_info!(
        "deploy nixos-rebuild switch --flake {}#{hostname} --target-host {host_user}@{host_ip} {sudo_flag}",
        flake_path.display()
      );

      let status = Command::new("nixos-rebuild")
        .args([
          "switch",
          "--flake",
          &format!("{}#{hostname}", flake_path.display()),
          "--target-host",
          &format!("{host_user}@{host_ip}"),
          sudo_flag,
        ])
        .status()
        .context("failed to run nixos-rebuild")?;

      if !status.success() {
        bail!(
          "nixos-rebuild failed for {host} (exit {})",
          status.code().unwrap_or(-1)
        );
      }
    }
  }

  log_info!("deploy done: {env}/{host}");
  Ok(format!("{host_user}@{host_ip}"))
}
