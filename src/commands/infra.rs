use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::normalize::normalize;
use crate::config::validate::validate;
use crate::config::{CredentialValue, DogmaConfig};
use crate::error::check_dep;
use crate::vault;
use crate::{git, log_dim, log_info, log_step, log_warn};

pub fn apply(
  repo_root: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
) -> Result<()> {
  let config = normalize(repo_root)?;
  validate(&config)?;
  run_infra(&config, repo_root, env, unit, migrate_state, "apply")
}

pub fn destroy(
  repo_root: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
) -> Result<()> {
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

  // 'apply' records HEAD as the applied state (tag infra-applied/<env>/<unit>),
  // so the working tree must exactly match HEAD — otherwise the tag would
  // point at code that differs from what was actually applied.
  let repo = if subcommand == "apply" {
    let repo = git::open(repo_root)?;
    require_clean_tree(&repo)?;
    Some(repo)
  } else {
    None
  };

  let credentials = resolve_credentials(config, env)?;

  log_step!("infra init {unit}");
  run_init(
    config,
    repo_root,
    InitArgs {
      cli,
      unit_dir: &unit_dir,
      env,
      unit,
      migrate_state,
      credentials: &credentials,
    },
  )?;

  log_step!("infra {subcommand} {unit}");
  run_subcommand(
    config,
    repo_root,
    cli,
    &unit_dir,
    env,
    subcommand,
    &credentials,
  )?;

  // Record the commit that was just applied so the exact code can be checked
  // out later (e.g. to destroy resources whose defining code has since
  // changed). Only apply writes this breadcrumb; failures here are
  // non-fatal — the infrastructure change already succeeded.
  if let Some(repo) = &repo {
    log_step!("infra record applied version {unit}");
    if let Err(e) = record_applied(repo, env, unit) {
      log_warn!("infra failed to record applied commit: {e:#}");
    }
  }

  Ok(())
}

/// Bail unless the working tree exactly matches HEAD (no modified, staged, or
/// untracked files). 'apply' tags HEAD as the applied state, so an uncommitted
/// change would make that tag lie about what was actually applied.
fn require_clean_tree(repo: &git2::Repository) -> Result<()> {
  let dirty = git::dirty_files(repo, true)?;
  if dirty.is_empty() {
    return Ok(());
  }
  log_warn!("infra working tree has uncommitted changes:");
  for f in &dirty.files {
    crate::log::status_line(f.status, &f.path);
  }
  bail!(
    "commit your changes before 'apply' so the recorded version matches what \
     is deployed"
  );
}

/// Force-update a moving tag `infra-applied/<env>/<unit>` to point at the
/// current HEAD, then push it. This is the breadcrumb that maps an
/// (env, unit) to the commit last applied to it.
fn record_applied(
  repo: &git2::Repository,
  env: &str,
  unit: &str,
) -> Result<()> {
  let tag = format!("infra-applied/{env}/{unit}");
  let short = git::head_short_sha(repo)?;
  log_step!("infra tagging {tag} -> {short}");
  git::set_moving_tag(repo, &tag)?;
  log_step!("infra pushing tag {tag} to remotes");
  git::push_tag_force(repo, &tag)?;
  Ok(())
}

pub fn resolve_credentials(
  config: &DogmaConfig,
  env: &str,
) -> Result<Vec<(String, String)>> {
  let infra = match &config.infra {
    Some(i) => i,
    None => return Ok(vec![]),
  };

  let mut creds = Vec::new();
  for (var_name, cred) in &infra.credentials {
    let value = match cred {
      CredentialValue::Static(s) => s.clone(),
      CredentialValue::FromVault { vault_ref, .. } => {
        log_info!("infra resolving credential {var_name} ...");
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

/// Expand a path template relative to the infra directory.
/// The only supported placeholder is `{env}`.
fn resolve_template(
  repo_root: &Path,
  infra_path: &str,
  template: &str,
  env: &str,
) -> PathBuf {
  let rel = template.replace("{env}", env);
  repo_root.join(infra_path).join(rel)
}

fn run_init(
  config: &DogmaConfig,
  repo_root: &Path,
  args: InitArgs<'_>,
) -> Result<()> {
  init_unit(
    config,
    repo_root,
    args.cli,
    args.unit_dir,
    args.env,
    args.unit,
    args.migrate_state,
    args.credentials,
  )
}

/// Run `<cli> init` for a single unit, pointing it at the backend bucket for
/// `env` (via the env's backend.conf) and the per-unit state key. `-reconfigure`
/// is used so the unit's `.terraform/` is re-pointed to the correct env backend
/// even if it was previously initialized for a different env.
#[allow(clippy::too_many_arguments)]
pub fn init_unit(
  config: &DogmaConfig,
  repo_root: &Path,
  cli: &str,
  unit_dir: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
  credentials: &[(String, String)],
) -> Result<()> {
  let infra = config.infra.as_ref().unwrap();
  let infra_path = infra.path.trim_start_matches("./");
  let backend_conf =
    resolve_template(repo_root, infra_path, &infra.backend_config, env);

  if !backend_conf.exists() {
    bail!("backend config not found: {}", backend_conf.display());
  }

  let state_key_value = format!("{unit}/terraform.tfstate");

  // Skip init when the unit is already initialized for this exact backend
  // (same bucket + state key). `-migrate-state` always re-runs so state moves
  // are never silently skipped.
  if !migrate_state
    && backend_already_correct(unit_dir, &backend_conf, &state_key_value)
  {
    log_dim!("infra {unit} already initialized for {env} — skipping init");
    return Ok(());
  }

  let reconfigure_flag = if migrate_state {
    "-migrate-state"
  } else {
    "-reconfigure"
  };

  let state_key = format!("key={state_key_value}");
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

/// True when the unit's existing `.terraform/terraform.tfstate` is already
/// configured for the same S3 backend (bucket + state key) we are about to
/// init. Lets callers skip a redundant `init -reconfigure`.
///
/// The bucket is read from the env's backend.conf (only the `bucket = "..."`
/// line is parsed — no other backend.conf value is touched) and compared with
/// the bucket + key recorded in the unit's local terraform state. Any parse
/// failure or missing file returns false, so we fall back to running init.
fn backend_already_correct(
  unit_dir: &Path,
  backend_conf: &Path,
  state_key: &str,
) -> bool {
  let Some(want_bucket) = backend_conf_bucket(backend_conf) else {
    return false;
  };

  let state_path = unit_dir.join(".terraform/terraform.tfstate");
  let Ok(raw) = std::fs::read_to_string(&state_path) else {
    return false;
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
    return false;
  };

  let cfg = &json["backend"]["config"];
  cfg["bucket"].as_str() == Some(want_bucket.as_str())
    && cfg["key"].as_str() == Some(state_key)
}

/// Extract just the `bucket` value from a backend.conf (HCL-ish `key = "value"`
/// lines). Returns None if absent. Other lines are ignored — backend.conf may
/// hold credentials, so nothing else is read out of it.
fn backend_conf_bucket(backend_conf: &Path) -> Option<String> {
  let raw = std::fs::read_to_string(backend_conf).ok()?;
  for line in raw.lines() {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("bucket") else {
      continue;
    };
    let rest = rest.trim_start();
    let Some(rest) = rest.strip_prefix('=') else {
      continue;
    };
    return Some(rest.trim().trim_matches('"').trim_matches('\'').to_string());
  }
  None
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
  let tfvars = resolve_template(repo_root, infra_path, &infra.var_file, env);

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
