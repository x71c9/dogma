use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

use crate::commands::infra::resolve_credentials;
use crate::config::DogmaConfig;
use crate::error::check_dep;
use crate::{log_dim, log_info};

/// Refresh the infra output cache for one env (all units, or one unit).
/// Writes .dogma/cache/<env>.json.
pub fn refresh(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
  unit_filter: Option<&str>,
) -> Result<()> {
  let infra = config
    .infra
    .as_ref()
    .ok_or_else(|| anyhow::anyhow!("no 'infra' block in dogma.yml"))?;

  let cli = &infra.cli;
  check_dep(cli, &format!("install {cli} and make sure it is on PATH"))?;

  let infra_dir = repo_root.join(infra.path.trim_start_matches("./"));
  let credentials = resolve_credentials(config, env)?;

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
    log_info!("infra: fetching outputs: {unit} ...");
    let flat = fetch_unit_outputs(cli, &infra_dir.join(unit), &credentials)?;
    merged[unit] = flat;
  }

  std::fs::write(&cache_file, serde_json::to_string_pretty(&merged)?)
    .with_context(|| format!("failed to write {}", cache_file.display()))?;

  log_dim!("infra: cache written: {}", cache_file.display());
  Ok(())
}

/// Read one output value from the cache (or bail if missing).
pub fn read_cached(
  repo_root: &Path,
  env: &str,
  unit: &str,
  output: &str,
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

  cache[unit][output]
    .as_str()
    .map(str::to_string)
    .ok_or_else(|| {
      anyhow::anyhow!(
        "output '{output}' not found in unit '{unit}' for env '{env}'"
      )
    })
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

  // Flatten: keep only non-sensitive outputs, extract .value
  let flat = raw
    .as_object()
    .map(|m| {
      m.iter()
        .filter(|(_, v)| v["sensitive"] != serde_json::json!(true))
        .map(|(k, v)| (k.clone(), v["value"].clone()))
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
    // Only dirs containing at least one .tf file
    let has_tf = std::fs::read_dir(&path)?.any(|e| {
      e.ok()
        .and_then(|e| {
          let n = e.file_name();
          let s = n.to_string_lossy();
          s.ends_with(".tf").then_some(())
        })
        .is_some()
    });
    if has_tf {
      units.push(name);
    }
  }
  units.sort();
  if units.is_empty() {
    bail!("no unit directories found under {}", infra_dir.display());
  }
  Ok(units)
}
