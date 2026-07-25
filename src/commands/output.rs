use anyhow::{bail, Result};
use std::path::Path;

pub fn run(
  repo_root: &Path,
  env: &str,
  unit: Option<&str>,
  key: Option<&str>,
) -> Result<()> {
  let cache_file = repo_root.join(format!(".dogma/cache/{env}.json"));

  if !cache_file.exists() {
    bail!(
      "no cache for env '{env}': {} — run 'dogma deploy {env} --new' or 'dogma infra apply {env} <unit>' first",
      cache_file.display()
    );
  }

  let raw = std::fs::read_to_string(&cache_file)?;
  let cache: serde_json::Value = serde_json::from_str(&raw)?;

  match (unit, key) {
    (None, _) => {
      println!("{}", serde_json::to_string_pretty(&cache)?);
    }
    (Some(u), None) => {
      let unit_val = &cache[u];
      if unit_val.is_null() {
        bail!("unit '{u}' not found in cache for env '{env}'");
      }
      println!("{}", serde_json::to_string_pretty(unit_val)?);
    }
    (Some(u), Some(k)) => {
      let val = &cache[u][k];
      if val.is_null() {
        bail!("key '{k}' not found in unit '{u}' for env '{env}'");
      }
      // Print raw string value if possible, otherwise JSON
      if let Some(s) = val.as_str() {
        println!("{s}");
      } else {
        println!("{}", serde_json::to_string_pretty(val)?);
      }
    }
  }

  Ok(())
}
