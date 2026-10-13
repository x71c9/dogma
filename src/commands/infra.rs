use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::config::normalize::normalize;
use crate::config::validate::validate;
use crate::config::{CredentialValue, DogmaConfig};
use crate::error::check_dep;
use crate::infra::encryption::tf_encryption;
use crate::vault;
use crate::{git, log_dim, log_info, log_step, log_warn};

/// Flags controlling how `<cli> init` is run for a unit.
#[derive(Default)]
pub struct InitOptions {
  /// Pass -migrate-state instead of -reconfigure.
  pub migrate_state: bool,
  /// Pass -upgrade (updates the provider lock file).
  pub upgrade: bool,
  /// Bypass the already-initialized heuristic and always re-run init.
  pub force: bool,
}

struct InfraFlags {
  migrate_state: bool,
  upgrade: bool,
  subcommand: &'static str,
  commit_msg: Option<String>,
  /// Appended after `-var-file` (e.g. `-target=…`, `-replace=…`)
  extra_args: Vec<String>,
}

/// Shared preamble for unit-scoped infra commands: env is declared, the infra
/// block exists, its cli is on PATH, and the unit directory exists. Returns
/// the unit directory.
fn check_unit(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit: &str,
) -> Result<PathBuf> {
  config.ensure_env(env)?;

  let infra = config
    .infra
    .as_ref()
    .ok_or_else(|| anyhow::anyhow!("no 'infra' block in dogma.yml"))?;

  let cli = &infra.cli;
  check_dep(cli, &format!("install {cli} and make sure it is on PATH"))?;

  let unit_dir = repo_root
    .join(infra.path.trim_start_matches("./"))
    .join(unit);
  if !unit_dir.is_dir() {
    bail!("unit '{unit}' not found: {}", unit_dir.display());
  }
  Ok(unit_dir)
}

pub fn apply(
  repo_root: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
  upgrade: bool,
  commit_msg: Option<String>,
  extra_args: Vec<String>,
) -> Result<()> {
  let config = normalize(repo_root)?;
  validate(&config)?;
  run_infra(
    &config,
    repo_root,
    env,
    unit,
    InfraFlags {
      migrate_state,
      upgrade,
      subcommand: "apply",
      commit_msg,
      extra_args,
    },
  )
}

/// Standalone `dogma infra init <env> <unit>`: always re-runs `<cli> init`,
/// bypassing the already-initialized heuristic. Escape hatch for cases the
/// skip detection can't see (deleted .terraform/, stale plugin cache, etc.).
pub fn init(
  repo_root: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
  upgrade: bool,
) -> Result<()> {
  let config = normalize(repo_root)?;
  validate(&config)?;

  let unit_dir = check_unit(&config, repo_root, env, unit)?;
  let credentials = resolve_credentials(&config, env)?;

  log_step!("infra init {unit}");
  init_unit(
    &config,
    repo_root,
    &unit_dir,
    env,
    unit,
    &InitOptions {
      migrate_state,
      upgrade,
      force: true,
    },
    &credentials,
  )
}

pub fn destroy(
  repo_root: &Path,
  env: &str,
  unit: &str,
  migrate_state: bool,
  upgrade: bool,
  extra_args: Vec<String>,
) -> Result<()> {
  let config = normalize(repo_root)?;
  validate(&config)?;
  run_infra(
    &config,
    repo_root,
    env,
    unit,
    InfraFlags {
      migrate_state,
      upgrade,
      subcommand: "destroy",
      commit_msg: None,
      extra_args,
    },
  )
}

/// `dogma infra run <env> <unit> <args…>`: the same preamble as apply
/// (validate, credentials, init against the env's backend), then `<cli>
/// <args…>` in the unit directory. The env's tfvars reach the commands that
/// take variables through `TF_CLI_ARGS` (placed by tofu right after the
/// subcommand, so positional args like `state show <addr>` keep working).
pub fn run(
  repo_root: &Path,
  env: &str,
  unit: &str,
  args: Vec<String>,
) -> Result<()> {
  check_passthrough(&args)?;

  let config = normalize(repo_root)?;
  validate(&config)?;

  let unit_dir = check_unit(&config, repo_root, env, unit)?;
  let credentials = resolve_credentials(&config, env)?;

  log_step!("infra init {unit}");
  init_unit(
    &config,
    repo_root,
    &unit_dir,
    env,
    unit,
    &InitOptions::default(),
    &credentials,
  )?;

  let infra = config.infra.as_ref().unwrap();
  let cli = &infra.cli;
  let tfvars = resolve_template(
    repo_root,
    infra.path.trim_start_matches("./"),
    &infra.var_file,
    env,
  );
  let var_file_env = var_file_env(&args, &tfvars);
  if var_file_env.is_none() && !tfvars.exists() {
    log_dim!("infra no tfvars at {} — none passed", tfvars.display());
  }

  let shown = args.join(" ");
  log_step!("infra {cli} {shown} ({unit})");
  let status = Command::new(cli)
    .current_dir(&unit_dir)
    .args(&args)
    .envs(credentials.iter().cloned())
    .envs(var_file_env)
    .status()
    .with_context(|| format!("failed to run '{cli} {shown}'"))?;

  if !status.success() {
    match status.code() {
      Some(code) => bail!("'{cli} {shown}' exited with status {code}"),
      None => bail!("'{cli} {shown}' was terminated by a signal"),
    }
  }
  Ok(())
}

/// The tofu subcommand of a passthrough: the first arg that is not a flag
/// (global options such as `-chdir=…` come before it).
fn passthrough_subcommand(args: &[String]) -> Option<&str> {
  args
    .iter()
    .map(String::as_str)
    .find(|a| !a.starts_with('-'))
}

/// `apply`, `destroy` and `init` have dogma wrappers that do more than tofu
/// (dirty-tree commit, `infra-applied` tag, output cache, backend config):
/// running them raw would skip that.
fn check_passthrough(args: &[String]) -> Result<()> {
  match passthrough_subcommand(args) {
    Some(sub @ ("apply" | "destroy" | "init")) => bail!(
      "'{sub}' is not allowed in 'dogma infra run' — use 'dogma infra {sub} \
       <env> <unit>' (extra tofu args after '--')"
    ),
    _ => Ok(()),
  }
}

/// `TF_CLI_ARGS=-var-file="<tfvars>"` when the tfvars file exists and the
/// subcommand accepts variables. OpenTofu ≥ 1.8 accepts `-var-file` on nearly
/// every command (state, output, show, …); only these reject it.
fn var_file_env(args: &[String], tfvars: &Path) -> Option<(String, String)> {
  const NO_VARS: [&str; 5] = ["fmt", "version", "login", "logout", "metadata"];
  if !tfvars.exists() {
    return None;
  }
  match passthrough_subcommand(args) {
    Some(sub) if NO_VARS.contains(&sub) => None,
    _ => Some((
      "TF_CLI_ARGS".to_string(),
      format!("-var-file=\"{}\"", tfvars.display()),
    )),
  }
}

fn run_infra(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit: &str,
  flags: InfraFlags,
) -> Result<()> {
  let InfraFlags {
    migrate_state,
    upgrade,
    subcommand,
    commit_msg,
    extra_args,
  } = flags;

  let unit_dir = check_unit(config, repo_root, env, unit)?;

  // 'apply' records HEAD as the applied state (tag infra-applied/<env>/<unit>).
  // The working tree must match HEAD before we run — offer to commit dirty
  // changes rather than refusing outright (mirrors `dogma deploy --new`).
  let repo = if subcommand == "apply" {
    let repo = git::open(repo_root)?;
    maybe_commit_dirty(&repo, commit_msg)?;
    Some(repo)
  } else {
    None
  };

  let credentials = resolve_credentials(config, env)?;

  log_step!("infra init {unit}");
  init_unit(
    config,
    repo_root,
    &unit_dir,
    env,
    unit,
    &InitOptions {
      migrate_state,
      upgrade,
      force: false,
    },
    &credentials,
  )?;

  log_step!("infra {subcommand} {unit}");
  run_subcommand(
    config,
    repo_root,
    &unit_dir,
    env,
    subcommand,
    &extra_args,
    &credentials,
  )?;

  // Cache this unit's outputs now that apply succeeded: they exist in remote
  // state, so `dogma output`/`env`/`deploy` can read them without a separate
  // run. refresh merges into .dogma/cache/<env>.json, preserving other units'
  // cached values. Only apply caches; failures here are non-fatal — the infra
  // change already succeeded.
  if subcommand == "apply" {
    log_step!("infra cache outputs {unit}");
    if let Err(e) =
      crate::infra::output::refresh(config, repo_root, env, Some(unit))
    {
      log_warn!("infra failed to cache outputs: {e:#}");
    }
  }

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

/// Offer to commit dirty changes before 'apply'. The infra-applied tag must
/// point at the exact code that was applied; an uncommitted change would make
/// it lie. If the user declines, bail — they must clean up themselves.
fn maybe_commit_dirty(
  repo: &git2::Repository,
  commit_msg: Option<String>,
) -> Result<()> {
  let dirty = git::dirty_files(repo, true)?;
  if dirty.is_empty() {
    return Ok(());
  }

  super::warn_dirty("infra", &dirty);
  super::commit_dirty(
    repo,
    &dirty,
    commit_msg,
    "infra",
    "applying",
    "chore: pre-infra snapshot",
  )
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

  let resolve = |cred: &CredentialValue| -> Result<String> {
    match cred {
      CredentialValue::Static(s) => Ok(s.clone()),
      CredentialValue::FromVault { vault_ref, .. } => {
        vault::read(config, env, vault_ref)
      }
    }
  };

  let mut creds = Vec::new();
  for (var_name, cred) in &infra.credentials {
    creds.push((var_name.clone(), resolve(cred)?));
  }
  // Every tofu invocation (init, plan/apply/destroy, output) takes its env
  // from here, so the state is never read or written without encryption.
  if let Some(enc) = &infra.state_encryption {
    let passphrase = resolve(&enc.passphrase)?;
    creds.push((
      "TF_ENCRYPTION".to_string(),
      tf_encryption(&passphrase, enc.migrate)?,
    ));
  }
  Ok(creds)
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

/// Run `<cli> init` for a single unit, pointing it at the backend bucket for
/// `env` (via the env's backend.conf) and the per-unit state key. `-reconfigure`
/// is used so the unit's `.terraform/` is re-pointed to the correct env backend
/// even if it was previously initialized for a different env.
pub fn init_unit(
  config: &DogmaConfig,
  repo_root: &Path,
  unit_dir: &Path,
  env: &str,
  unit: &str,
  opts: &InitOptions,
  credentials: &[(String, String)],
) -> Result<()> {
  let infra = config.infra.as_ref().unwrap();
  let cli = &infra.cli;
  let infra_path = infra.path.trim_start_matches("./");
  let backend_conf =
    resolve_template(repo_root, infra_path, &infra.backend_config, env);

  if !backend_conf.exists() {
    bail!("backend config not found: {}", backend_conf.display());
  }

  let state_key_value = infra
    .state_key
    .as_deref()
    .map(|t| t.replace("{env}", env).replace("{unit}", unit))
    .unwrap_or_else(|| format!("{env}/{unit}/terraform.tfstate"));

  // Skip init when the unit is already initialized for this exact backend
  // (same bucket + state key) AND every provider pinned in the lock file is
  // present in the local plugin cache. `-migrate-state` and `-upgrade` always
  // re-run so lock file or state moves are never silently skipped; `force`
  // (dogma infra init) bypasses the heuristic entirely.
  if !opts.force
    && !opts.migrate_state
    && !opts.upgrade
    && backend_already_correct(unit_dir, &backend_conf, &state_key_value)
    && providers_cached(unit_dir)
  {
    log_dim!("infra {unit} already initialized for {env} — skipping init");
    return Ok(());
  }

  let reconfigure_flag = if opts.migrate_state {
    "-migrate-state"
  } else if opts.upgrade {
    "-upgrade"
  } else {
    "-reconfigure"
  };

  let mut args = vec![
    "init".to_string(),
    reconfigure_flag.to_string(),
    format!("-backend-config={}", backend_conf.display()),
    format!("-backend-config=key={state_key_value}"),
  ];
  // -input=false keeps a captured run from silently blocking on a prompt
  // nobody can see; -migrate-state must stay interactive (see below).
  if !opts.migrate_state {
    args.push("-input=false".to_string());
  }

  log_info!("infra running: {cli} {}", args.join(" "));

  let mut cmd = Command::new(cli);
  cmd
    .current_dir(unit_dir)
    .args(&args)
    .envs(credentials.iter().cloned());

  // -migrate-state may prompt on stdin (copy existing state?), so its output
  // must stream through untouched. Every other init runs captured: tofu's
  // chatter is replaced by the dogma lines around it, and the full output is
  // printed only when init fails.
  if opts.migrate_state {
    let status = cmd
      .status()
      .with_context(|| format!("failed to run '{cli} init'"))?;
    if !status.success() {
      bail!("'{cli} init' failed for unit '{}'", unit_dir.display());
    }
    log_info!("infra {unit} initialized for {env}");
    return Ok(());
  }

  let output = cmd
    .output()
    .with_context(|| format!("failed to run '{cli} init'"))?;

  if !output.status.success() {
    io::stderr().write_all(&output.stdout)?;
    io::stderr().write_all(&output.stderr)?;
    bail!("'{cli} init' failed for unit '{}'", unit_dir.display());
  }

  log_info!("infra {unit} initialized for {env}");
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

/// True when every provider pinned in the unit's `.terraform.lock.hcl` has a
/// package cached under `.terraform/providers/<addr>/<version>/`. A lock file
/// that changed since the last init (new module, pull from another machine)
/// leaves the backend pointing at the right place but the plugin cache stale,
/// and apply would fail with "Required plugins are not installed". A missing
/// or unparseable lock file — or one pinning no providers — returns false, so
/// we fall back to running init.
fn providers_cached(unit_dir: &Path) -> bool {
  let Ok(raw) = std::fs::read_to_string(unit_dir.join(".terraform.lock.hcl"))
  else {
    return false;
  };

  // Lock file blocks look like:
  //   provider "registry.opentofu.org/hashicorp/random" {
  //     version = "3.9.0"
  //     ...
  // Pair each block header with the first following `version` line.
  let mut provider: Option<String> = None;
  let mut seen_any = false;
  for line in raw.lines() {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("provider") {
      provider = first_quoted(rest);
    } else if line.starts_with("version") {
      let Some(addr) = provider.take() else {
        continue;
      };
      let Some(version) = first_quoted(line) else {
        return false;
      };
      seen_any = true;
      if !unit_dir
        .join(".terraform/providers")
        .join(&addr)
        .join(&version)
        .is_dir()
      {
        return false;
      }
    }
  }
  seen_any
}

/// First double-quoted substring of `s`, if any.
fn first_quoted(s: &str) -> Option<String> {
  let start = s.find('"')? + 1;
  let end = start + s[start..].find('"')?;
  Some(s[start..end].to_string())
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
  unit_dir: &Path,
  env: &str,
  subcommand: &str,
  extra_args: &[String],
  credentials: &[(String, String)],
) -> Result<()> {
  let infra = config.infra.as_ref().unwrap();
  let cli = &infra.cli;
  let infra_path = infra.path.trim_start_matches("./");
  let tfvars = resolve_template(repo_root, infra_path, &infra.var_file, env);

  if !tfvars.exists() {
    bail!("tfvars not found: {}", tfvars.display());
  }

  let status = Command::new(cli)
    .current_dir(unit_dir)
    .args([subcommand, &format!("-var-file={}", tfvars.display())])
    .args(extra_args)
    .envs(credentials.iter().cloned())
    .status()
    .with_context(|| format!("failed to run '{cli} {subcommand}'"))?;

  if !status.success() {
    bail!("'{cli} {subcommand}' failed");
  }
  Ok(())
}

#[cfg(test)]
mod passthrough_tests {
  use super::{check_passthrough, var_file_env};
  use std::path::Path;

  fn args(s: &str) -> Vec<String> {
    s.split_whitespace().map(String::from).collect()
  }

  #[test]
  fn read_only_commands_allowed() {
    for a in [
      "plan",
      "state list",
      "state show x.y",
      "output",
      "import a b",
    ] {
      assert!(check_passthrough(&args(a)).is_ok(), "{a}");
    }
  }

  #[test]
  fn wrapped_commands_rejected() {
    for a in ["apply", "destroy -auto-approve", "-chdir=x init"] {
      let err = check_passthrough(&args(a)).unwrap_err().to_string();
      assert!(
        err.contains("not allowed in 'dogma infra run'"),
        "{a}: {err}"
      );
    }
  }

  #[test]
  fn var_file_set_when_tfvars_exists() {
    let tfvars = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let (k, v) = var_file_env(&args("state show x"), &tfvars).unwrap();
    assert_eq!(k, "TF_CLI_ARGS");
    assert_eq!(v, format!("-var-file=\"{}\"", tfvars.display()));
  }

  #[test]
  fn var_file_skipped_for_commands_without_vars() {
    let tfvars = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    for a in ["fmt -check", "version", "-chdir=x metadata functions"] {
      assert!(var_file_env(&args(a), &tfvars).is_none(), "{a}");
    }
  }

  #[test]
  fn var_file_skipped_when_tfvars_missing() {
    let missing = Path::new("/nonexistent/main.tfvars");
    assert!(var_file_env(&args("plan"), missing).is_none());
  }
}

#[cfg(test)]
mod tests {
  use super::providers_cached;
  use std::fs;
  use std::path::PathBuf;

  const LOCK: &str = r#"
# This file is maintained automatically by "tofu init".

provider "registry.opentofu.org/hashicorp/random" {
  version     = "3.9.0"
  constraints = "~> 3.0"
  hashes = [
    "h1:aaaa",
    "zh:bbbb",
  ]
}

provider "registry.opentofu.org/hetznercloud/hcloud" {
  version = "1.66.0"
  hashes = [
    "h1:cccc",
  ]
}
"#;

  fn fixture(name: &str) -> PathBuf {
    let dir = std::env::temp_dir()
      .join(format!("dogma-test-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
  }

  fn cache_provider(unit_dir: &std::path::Path, addr: &str, version: &str) {
    fs::create_dir_all(
      unit_dir
        .join(".terraform/providers")
        .join(addr)
        .join(version),
    )
    .unwrap();
  }

  #[test]
  fn all_providers_cached() {
    let dir = fixture("all-cached");
    fs::write(dir.join(".terraform.lock.hcl"), LOCK).unwrap();
    cache_provider(&dir, "registry.opentofu.org/hashicorp/random", "3.9.0");
    cache_provider(&dir, "registry.opentofu.org/hetznercloud/hcloud", "1.66.0");
    assert!(providers_cached(&dir));
    fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn missing_provider_package() {
    let dir = fixture("missing-pkg");
    fs::write(dir.join(".terraform.lock.hcl"), LOCK).unwrap();
    cache_provider(&dir, "registry.opentofu.org/hashicorp/random", "3.9.0");
    assert!(!providers_cached(&dir));
    fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn wrong_version_cached() {
    let dir = fixture("wrong-version");
    fs::write(dir.join(".terraform.lock.hcl"), LOCK).unwrap();
    cache_provider(&dir, "registry.opentofu.org/hashicorp/random", "3.8.0");
    cache_provider(&dir, "registry.opentofu.org/hetznercloud/hcloud", "1.66.0");
    assert!(!providers_cached(&dir));
    fs::remove_dir_all(&dir).unwrap();
  }

  #[test]
  fn missing_or_empty_lock_file() {
    let dir = fixture("no-lock");
    assert!(!providers_cached(&dir));
    fs::write(dir.join(".terraform.lock.hcl"), "").unwrap();
    assert!(!providers_cached(&dir));
    fs::remove_dir_all(&dir).unwrap();
  }
}
