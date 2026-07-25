use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::commands::infra::{init_unit, resolve_credentials, InitOptions};
use crate::config::{DogmaConfig, IpEntry};
use crate::error::check_dep;
use crate::{log_dim, log_info};

const SENSITIVE_SENTINEL: &str = "__dogma_sensitive__";

/// Resolved infra credentials for one env: (env name, VAR=value pairs).
pub type EnvCreds = (String, Vec<(String, String)>);

/// Resolve infra credentials for every declared env. Vault reads can be slow
/// (e.g. `pass`), so callers should do this once and share the result.
pub fn resolve_all_env_creds(config: &DogmaConfig) -> Result<Vec<EnvCreds>> {
  config
    .env
    .iter()
    .map(|e| Ok((e.clone(), resolve_credentials(config, e)?)))
    .collect()
}

/// The credentials for `env` out of a pre-resolved set; empty if absent.
pub fn lookup_creds<'a>(
  env_creds: &'a [EnvCreds],
  env: &str,
) -> &'a [(String, String)] {
  env_creds
    .iter()
    .find(|(e, _)| e == env)
    .map(|(_, c)| c.as_slice())
    .unwrap_or(&[])
}

/// Resolve a machine's IP for `env`: either the static value from dogma.yml
/// or the cached infra output it points at.
pub fn resolve_machine_ip(
  config: &DogmaConfig,
  repo_root: &Path,
  host: &str,
  env: &str,
  credentials: &[(String, String)],
) -> Result<String> {
  let machine = config
    .machines
    .get(host)
    .ok_or_else(|| anyhow::anyhow!("machine '{host}' not found"))?;

  let ip_entry = machine
    .ip
    .get(env)
    .ok_or_else(|| anyhow::anyhow!("no IP defined for {host}/{env}"))?;

  match ip_entry {
    IpEntry::Static(ip) => Ok(ip.clone()),
    IpEntry::FromInfra { unit, output, .. } => {
      read_cached(config, repo_root, env, unit, output, credentials)
    }
  }
}

/// Refresh the infra output cache for one env (all units, or one unit).
/// Writes .dogma/cache/<env>.json.
pub fn refresh(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit_filter: Option<&str>,
) -> Result<()> {
  let credentials = resolve_credentials(config, env)?;
  refresh_with_creds(config, repo_root, env, unit_filter, &credentials)
}

/// Like `refresh` but accepts pre-resolved credentials to avoid redundant vault
/// reads when the caller has already resolved them for this env.
pub fn refresh_with_creds(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit_filter: Option<&str>,
  credentials: &[(String, String)],
) -> Result<()> {
  let infra = config
    .infra
    .as_ref()
    .ok_or_else(|| anyhow::anyhow!("no 'infra' block in dogma.yml"))?;

  let cli = &infra.cli;
  check_dep(cli, &format!("install {cli} and make sure it is on PATH"))?;

  let infra_dir = repo_root.join(infra.path.trim_start_matches("./"));

  let units = match unit_filter {
    Some(u) => {
      let unit_dir = infra_dir.join(u);
      if !unit_dir.is_dir() {
        bail!("unit '{u}' not found: {}", unit_dir.display());
      }
      vec![u.to_string()]
    }
    None => discover_units(&infra_dir)?,
  };

  let cache_dir = repo_root.join(".dogma/cache");
  std::fs::create_dir_all(&cache_dir)?;
  let cache_file = cache_dir.join(format!("{env}.json"));

  let mut merged: serde_json::Value = if cache_file.exists() {
    let raw = std::fs::read_to_string(&cache_file)?;
    serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
  } else {
    serde_json::json!({})
  };

  for unit in &units {
    let unit_dir = infra_dir.join(unit);
    // Re-init the unit for THIS env's backend before reading outputs: the
    // unit's .terraform/ may have been left pointing at another env's state
    // bucket by a previous run, which would make `tofu output` read the wrong
    // state (or 403 with this env's credentials). See init_unit's -reconfigure.
    log_info!("infra init {unit} ...");
    init_unit(
      config,
      repo_root,
      &unit_dir,
      env,
      unit,
      &InitOptions::default(),
      credentials,
    )?;

    log_info!("infra fetching outputs: {unit} ...");
    let flat = fetch_unit_outputs(cli, &unit_dir, credentials)?;
    merged[unit] = flat;
  }

  std::fs::write(&cache_file, serde_json::to_string_pretty(&merged)?)
    .with_context(|| format!("failed to write {}", cache_file.display()))?;

  log_dim!("infra cache written: {}", cache_file.display());
  Ok(())
}

/// Read one output value from the cache, fetching sensitive outputs live.
/// Pass pre-resolved `credentials` to avoid re-reading vault on every call.
pub fn read_cached(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit: &str,
  output: &str,
  credentials: &[(String, String)],
) -> Result<String> {
  let cache_file = repo_root.join(format!(".dogma/cache/{env}.json"));
  if !cache_file.exists() {
    bail!(
      "no infra cache for env '{env}': {} — run deploy first",
      cache_file.display()
    );
  }
  let raw = std::fs::read_to_string(&cache_file)?;
  let cache: serde_json::Value = serde_json::from_str(&raw)?;

  let entry = &cache[unit][output];

  // Sensitive outputs are stored as a sentinel; fetch their value live.
  if entry.get("__dogma_sensitive__").and_then(|v| v.as_bool()) == Some(true) {
    return fetch_sensitive_output(
      config,
      repo_root,
      env,
      unit,
      output,
      credentials,
    );
  }

  entry.as_str().map(str::to_string).ok_or_else(|| {
    anyhow::anyhow!(
      "output '{output}' not found in unit '{unit}' for env '{env}'"
    )
  })
}

fn fetch_sensitive_output(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit: &str,
  output: &str,
  credentials: &[(String, String)],
) -> Result<String> {
  let infra = config
    .infra
    .as_ref()
    .ok_or_else(|| anyhow::anyhow!("no 'infra' block in dogma.yml"))?;

  let cli = &infra.cli;
  let unit_dir = repo_root
    .join(infra.path.trim_start_matches("./"))
    .join(unit);

  // Re-init before reading so that .terraform/ is pointing at the correct
  // env's backend. Without this, a previous run that processed a different
  // env's units last would leave .terraform/ on the wrong state bucket, and
  // `tofu output -raw` would silently return that env's value instead.
  init_unit(
    config,
    repo_root,
    &unit_dir,
    env,
    unit,
    &InitOptions::default(),
    credentials,
  )?;

  log_dim!("infra output '{output}' is sensitive — fetching live via {cli}");

  let out = Command::new(cli)
    .current_dir(&unit_dir)
    .args(["output", "-raw", output])
    .envs(credentials.iter().cloned())
    .output()
    .with_context(|| format!("failed to run '{cli} output -raw {output}'"))?;

  if !out.status.success() {
    let stderr = String::from_utf8_lossy(&out.stderr);
    bail!("'{cli} output -raw {output}' failed: {stderr}");
  }

  String::from_utf8(out.stdout)
    .with_context(|| format!("output '{output}' returned non-UTF-8 bytes"))
}

fn fetch_unit_outputs(
  cli: &str,
  unit_dir: &Path,
  credentials: &[(String, String)],
) -> Result<serde_json::Value> {
  let out = Command::new(cli)
    .current_dir(unit_dir)
    .args(["output", "-json"])
    .envs(credentials.iter().cloned())
    .output()
    .with_context(|| format!("failed to run '{cli} output'"))?;

  if !out.status.success() {
    let stderr = String::from_utf8_lossy(&out.stderr);
    bail!("'{cli} output -json' failed: {stderr}");
  }

  let raw: serde_json::Value = serde_json::from_slice(&out.stdout)
    .context("tofu output was not valid JSON")?;

  // Flatten outputs: extract .value for non-sensitive ones, store a sentinel
  // for sensitive ones so read_cached can fetch them live via `tofu output -raw`.
  let flat = raw
    .as_object()
    .map(|m| {
      m.iter()
        .map(|(k, v)| {
          let val = if v["sensitive"] == serde_json::json!(true) {
            serde_json::json!({ SENSITIVE_SENTINEL: true })
          } else {
            v["value"].clone()
          };
          (k.clone(), val)
        })
        .collect::<serde_json::Map<_, _>>()
    })
    .unwrap_or_default();

  Ok(serde_json::Value::Object(flat))
}

fn discover_units(infra_dir: &Path) -> Result<Vec<String>> {
  let mut units = Vec::new();
  for entry in std::fs::read_dir(infra_dir).with_context(|| {
    format!("cannot read infra dir: {}", infra_dir.display())
  })? {
    let entry = entry?;
    let path = entry.path();
    if !path.is_dir() {
      continue;
    }
    let name = entry.file_name().to_string_lossy().to_string();
    if name.starts_with('.') {
      continue;
    }
    // Only include dirs that have a backend block in at least one .tf file.
    // Shared-variable directories (.tf files but no backend) are skipped —
    // they cannot be init-ed and have no outputs to cache.
    if dir_has_backend(&path) {
      units.push(name);
    }
  }
  units.sort();
  if units.is_empty() {
    bail!("no unit directories found under {}", infra_dir.display());
  }
  Ok(units)
}

fn dir_has_backend(dir: &Path) -> bool {
  let Ok(rd) = std::fs::read_dir(dir) else {
    return false;
  };
  for entry in rd.flatten() {
    let p = entry.path();
    if p.extension().and_then(|e| e.to_str()) != Some("tf") {
      continue;
    }
    let Ok(contents) = std::fs::read_to_string(&p) else {
      continue;
    };
    if contents.contains("backend \"") {
      return true;
    }
  }
  false
}
