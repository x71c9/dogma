/// generate.rs — mirrors generate-sops.sh
///
/// For each host×env:
///   1. Resolve IP (static or from infra cache)
///   2. Fetch SSH host ed25519 key via ssh-keyscan
///   3. Convert to age key via ssh-to-age
///   4. Write .sops.yaml with one creation_rule per host×env
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::config::{AdminKey, DogmaConfig};
use crate::infra::output::{lookup_creds, resolve_machine_ip, EnvCreds};
use crate::{log_dim, log_info, log_warn};

/// `env_creds` — pre-resolved infra credentials per env (see
/// `infra::output::resolve_all_env_creds`).
pub fn run(
  config: &DogmaConfig,
  repo_root: &Path,
  env_creds: &[EnvCreds],
) -> Result<()> {
  let nix_secrets = config.nix.secrets.trim_start_matches("./");
  let sops_file = repo_root.join(config.nix.sops.trim_start_matches("./"));
  let sops_dir = sops_file.parent().unwrap_or(repo_root);
  let secrets_abs = repo_root.join(nix_secrets);
  let secrets_rel = pathdiff::diff_paths(&secrets_abs, sops_dir)
    .unwrap_or_else(|| secrets_abs.clone());

  // Collect admin keys
  let (pgp_keys, age_keys) = collect_admin_keys(config)?;

  let all_envs: Vec<&str> = config.env.iter().map(String::as_str).collect();

  let mut rules = String::from("creation_rules:");

  for env_name in &all_envs {
    let infra_creds = lookup_creds(env_creds, env_name);
    for (host_name, machine) in &config.machines {
      let hostname = machine.hostname.get(env_name, host_name);

      let ip = match resolve_machine_ip(
        config,
        repo_root,
        host_name,
        env_name,
        infra_creds,
      ) {
        Ok(ip) => ip,
        Err(e) => {
          log_warn!(
            "sops {host_name}/{env_name}: cannot resolve IP — skipping ({e})"
          );
          continue;
        }
      };

      let host_age = fetch_age_key(&ip, &hostname).with_context(|| {
        format!("sops {host_name}/{env_name}: failed to fetch age key")
      })?;

      let path_regex = format!(
        "{}/{}/{}/.*\\.yaml$",
        secrets_rel.display(),
        env_name,
        host_name
      );

      rules.push_str(&format!("\n  - path_regex: {path_regex}"));

      if !pgp_keys.is_empty() {
        rules.push_str(&format!("\n    pgp: {}", pgp_keys.join(",")));
      }

      let mut all_age = age_keys.clone();
      all_age.push(host_age);
      rules.push_str(&format!("\n    age: {}", all_age.join(",")));

      log_info!("sops {host_name}/{env_name} → {hostname}");
    }
  }

  std::fs::create_dir_all(sops_dir)?;
  std::fs::write(&sops_file, format!("{rules}\n"))
    .with_context(|| format!("failed to write {}", sops_file.display()))?;

  log_dim!("sops written: {}", sops_file.display());
  Ok(())
}

fn collect_admin_keys(
  config: &DogmaConfig,
) -> Result<(Vec<String>, Vec<String>)> {
  let mut pgp = Vec::new();
  let mut age = Vec::new();

  for key in &config.admin {
    match key {
      AdminKey::Gpg { gpg } => pgp.push(gpg.clone()),
      AdminKey::Age { age: a } => age.push(a.clone()),
      AdminKey::Ssh { ssh } => {
        let path = shellexpand::tilde(ssh).to_string();
        let out = Command::new("ssh-to-age")
          .stdin(
            std::fs::File::open(&path)
              .with_context(|| format!("admin ssh key not found: {path}"))?,
          )
          .output()
          .context("failed to run ssh-to-age")?;
        if !out.status.success() {
          bail!("ssh-to-age failed for admin key {path}");
        }
        let converted = String::from_utf8(out.stdout)?.trim().to_string();
        if converted.is_empty() {
          bail!("ssh-to-age produced empty output for {path}");
        }
        age.push(converted);
      }
    }
  }

  Ok((pgp, age))
}

fn fetch_age_key(ip: &str, hostname: &str) -> Result<String> {
  log_info!("sops {hostname}: fetching SSH host key from {ip} ...");

  let out = Command::new("ssh-keyscan")
    .args(["-t", "ed25519", "-T", "10", ip])
    .output()
    .context("failed to run ssh-keyscan")?;

  let stdout = String::from_utf8_lossy(&out.stdout);
  let ssh_pubkey = stdout
    .lines()
    .find(|l| !l.starts_with('#') && l.contains("ssh-ed25519"))
    .and_then(|l| {
      let mut parts = l.split(' ');
      let key_type = parts.nth(1)?;
      let key_data = parts.next().unwrap_or("");
      Some(format!("{key_type} {key_data}"))
    })
    .ok_or_else(|| {
      anyhow::anyhow!("{hostname}: ssh-keyscan got no ed25519 key from {ip}")
    })?;

  let mut child = Command::new("ssh-to-age")
    .stdin(std::process::Stdio::piped())
    .stdout(std::process::Stdio::piped())
    .spawn()
    .context("failed to spawn ssh-to-age")?;

  use std::io::Write;
  child
    .stdin
    .take()
    .unwrap()
    .write_all(ssh_pubkey.as_bytes())?;
  let output = child.wait_with_output()?;

  if !output.status.success() {
    bail!("{hostname}: ssh-to-age conversion failed");
  }

  let age_key = String::from_utf8(output.stdout)?.trim().to_string();
  if age_key.is_empty() {
    bail!("{hostname}: ssh-to-age produced empty output");
  }

  Ok(age_key)
}
