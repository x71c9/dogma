use anyhow::{Context, Result};
use indexmap::IndexMap;
use std::path::Path;

use super::{DogmaConfig, EnvOrMap, HostnameField, IpField, NixBlock};
use crate::log_warn;

pub fn normalize(repo_root: &Path) -> Result<DogmaConfig> {
  let dogma_yml = repo_root.join("dogma.yml");
  let raw = std::fs::read_to_string(&dogma_yml)
    .with_context(|| format!("dogma.yml not found: {}", dogma_yml.display()))?;

  for warning in unknown_key_warnings(&raw) {
    log_warn!("{warning}");
  }

  let mut config: DogmaConfig =
    serde_yaml::from_str(&raw).context("failed to parse dogma.yml")?;

  expand_defaults(&mut config);
  write_expanded(repo_root, &config)?;

  Ok(config)
}

#[allow(dead_code)]
pub fn load_expanded(repo_root: &Path) -> Result<DogmaConfig> {
  let expanded_path = repo_root.join(".dogma/dogma-expanded.yml");
  let raw = std::fs::read_to_string(&expanded_path).with_context(|| {
    format!("expanded config not found: {}", expanded_path.display())
  })?;
  serde_yaml::from_str(&raw).context("failed to parse dogma-expanded.yml")
}

/// Serde ignores unknown fields, so a mistyped or misplaced key in dogma.yml
/// is silently dropped — e.g. a `pre_deploy:` hook (kebab-case is required).
/// Collect warnings for the schema levels where every key is fixed: top
/// level, pipeline entries, and hook blocks. Sections with user-chosen keys
/// (vault, machines, secrets) are not checked. Also warns when a top-level
/// `hooks` block (valid only for the implicit default pipeline) coexists
/// with declared pipelines, which ignore it.
fn unknown_key_warnings(raw: &str) -> Vec<String> {
  const TOP: &[&str] = &[
    "name", "env", "admin", "vault", "machines", "secrets", "infra", "nix",
    "deploy", "pipeline", "hooks",
  ];
  const PIPELINE: &[&str] = &[
    "name",
    "type",
    "version_prefix",
    "version_scheme",
    "version_script",
    "deployed_prefix",
    "command",
    "env",
    "hooks",
  ];
  const HOOKS: &[&str] = &["pre-deploy", "post-deploy"];

  let mut warnings = Vec::new();

  // Parse errors are not reported here — serde surfaces them right after,
  // with context.
  let Ok(value) = serde_yaml::from_str::<serde_yaml::Value>(raw) else {
    return warnings;
  };
  let Some(top) = value.as_mapping() else {
    return warnings;
  };

  for key in top.keys().filter_map(|k| k.as_str()) {
    if !TOP.contains(&key) {
      warnings.push(format!(
        "dogma.yml: unknown top-level key '{key}' — ignored"
      ));
    }
  }

  let has_pipelines = value
    .get("pipeline")
    .and_then(|p| p.as_sequence())
    .is_some_and(|s| !s.is_empty());

  // Top-level hooks feed the implicit default pipeline only; declared
  // pipelines each carry their own hooks and ignore the top-level block.
  if let Some(hooks) = value.get("hooks").and_then(|h| h.as_mapping()) {
    if has_pipelines {
      warnings.push(
        "dogma.yml: top-level 'hooks' is ignored when pipelines are \
         declared — move it into the pipeline entry (pipeline[].hooks)"
          .to_string(),
      );
    }
    for key in hooks.keys().filter_map(|k| k.as_str()) {
      if !HOOKS.contains(&key) {
        warnings.push(format!(
          "dogma.yml: hooks: unknown hook '{key}' — valid hooks are \
           'pre-deploy' and 'post-deploy'"
        ));
      }
    }
  }

  let Some(pipelines) = value.get("pipeline").and_then(|p| p.as_sequence())
  else {
    return warnings;
  };
  for (i, entry) in pipelines.iter().enumerate() {
    let Some(map) = entry.as_mapping() else {
      continue;
    };
    let label = map
      .get("name")
      .and_then(|n| n.as_str())
      .map(|n| format!("'{n}'"))
      .unwrap_or_else(|| format!("[{i}]"));

    for key in map.keys().filter_map(|k| k.as_str()) {
      if !PIPELINE.contains(&key) {
        warnings.push(format!(
          "dogma.yml: pipeline {label}: unknown key '{key}' — ignored"
        ));
      }
    }

    if let Some(hooks) = map.get("hooks").and_then(|h| h.as_mapping()) {
      for key in hooks.keys().filter_map(|k| k.as_str()) {
        if !HOOKS.contains(&key) {
          warnings.push(format!(
            "dogma.yml: pipeline {label}: unknown hook '{key}' — valid hooks \
             are 'pre-deploy' and 'post-deploy'"
          ));
        }
      }
    }
  }

  warnings
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
    if infra.var_file.is_empty() {
      infra.var_file = crate::config::default_var_file();
    }
    if infra.backend_config.is_empty() {
      infra.backend_config = crate::config::default_backend_config();
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
      let per_env: IndexMap<String, _> =
        envs.iter().map(|e| (e.clone(), entry.clone())).collect();
      machine.ip = IpField::PerEnv(per_env);
    }
  }

  // Vault: expand envvar and pass to per-env maps
  for (key, entry) in &mut config.vault {
    let auto_var = key.to_uppercase().replace('-', "_");

    // envvar: absent -> auto-derive; flat string -> same for all envs; per-env -> as-is
    match &entry.envvar {
      None => {
        let m: IndexMap<String, String> =
          envs.iter().map(|e| (e.clone(), auto_var.clone())).collect();
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
  let yaml = serde_yaml::to_string(config)
    .context("failed to serialize expanded config")?;
  std::fs::write(&expanded_path, yaml)
    .with_context(|| format!("failed to write {}", expanded_path.display()))?;

  Ok(())
}

#[cfg(test)]
mod tests {
  use super::unknown_key_warnings;

  #[test]
  fn clean_config_yields_no_warnings() {
    let yml = r#"
name: myproject
env: [dev, prod]
admin:
  - age: age1xyz
pipeline:
  - name: backend
    type: nixos
    hooks:
      pre-deploy:
        - ./custom/bump-version.sh
      post-deploy:
        - ./custom/notify-slack.sh
"#;
    assert!(unknown_key_warnings(yml).is_empty());
  }

  #[test]
  fn top_level_hooks_accepted_without_pipelines() {
    let yml = "name: x\nenv: [dev]\nadmin: []\nhooks:\n  post-deploy:\n    - ./notify.sh\n";
    assert!(unknown_key_warnings(yml).is_empty());
  }

  #[test]
  fn top_level_hooks_with_pipelines_warns() {
    let yml = r#"
name: x
env: [dev]
admin: []
hooks:
  post-deploy:
    - ./notify.sh
pipeline:
  - name: backend
    type: nixos
"#;
    let warnings = unknown_key_warnings(yml);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("top-level 'hooks'"));
    assert!(warnings[0].contains("pipeline[].hooks"));
  }

  #[test]
  fn top_level_misspelled_hook_name_warns() {
    let yml = "name: x\nenv: [dev]\nadmin: []\nhooks:\n  pre_deploy: []\n";
    let warnings = unknown_key_warnings(yml);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("'pre_deploy'"));
    assert!(warnings[0].contains("pre-deploy"));
  }

  #[test]
  fn unknown_top_level_key_warns() {
    let yml = "name: x\nenv: [dev]\nadmin: []\nmachnies: {}\n";
    let warnings = unknown_key_warnings(yml);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("'machnies'"));
  }

  #[test]
  fn misspelled_hook_name_warns() {
    let yml = r#"
name: x
env: [dev]
admin: []
pipeline:
  - name: backend
    hooks:
      pre_deploy:
        - ./custom/bump-version.sh
"#;
    let warnings = unknown_key_warnings(yml);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("pipeline 'backend'"));
    assert!(warnings[0].contains("'pre_deploy'"));
    assert!(warnings[0].contains("pre-deploy"));
  }

  #[test]
  fn unknown_pipeline_key_warns() {
    let yml = r#"
name: x
env: [dev]
admin: []
pipeline:
  - name: backend
    comand: ./deploy.sh
"#;
    let warnings = unknown_key_warnings(yml);
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("'comand'"));
  }

  #[test]
  fn unparseable_yaml_yields_no_warnings() {
    assert!(unknown_key_warnings(": not yaml : [").is_empty());
  }
}
