use anyhow::{bail, Result};

use super::{
  CredentialValue, DogmaConfig, IpEntry, PipelineType, SecretLeaf,
  VersionScheme,
};

pub fn validate(config: &DogmaConfig) -> Result<()> {
  let mut errors: Vec<String> = Vec::new();

  let secret_groups: Vec<&str> =
    config.secrets.keys().map(String::as_str).collect();
  let vault_keys: Vec<&str> = config.vault.keys().map(String::as_str).collect();
  let has_infra = config.infra.is_some();

  // Machines checks
  for (host, machine) in &config.machines {
    // Each listed secret group must exist in secrets
    for group in &machine.secrets {
      if !secret_groups.contains(&group.as_str()) {
        errors.push(format!(
          "machines.{host}.secrets: '{group}' is not defined in secrets"
        ));
      }
    }

    // hostname: after normalization should always be PerEnv — validate all envs present
    if let super::HostnameField::PerEnv(map) = &machine.hostname {
      for env in &config.env {
        if !map.contains_key(env) {
          errors.push(format!("machines.{host}.hostname: missing env '{env}'"));
        }
      }
    }

    // ip: from:infra entries must have output + unit
    if let super::IpField::PerEnv(map) = &machine.ip {
      for (env_name, ip_entry) in map {
        if let IpEntry::FromInfra { unit, output, .. } = ip_entry {
          if output.is_empty() {
            errors.push(format!(
                            "machines.{host}.ip.{env_name}: from:infra requires an 'output' field"
                        ));
          }
          if unit.is_empty() {
            errors.push(format!(
                            "machines.{host}.ip.{env_name}: from:infra requires a 'unit' field"
                        ));
          }
          if !has_infra {
            errors.push(format!(
                            "machines.{host}.ip.{env_name}: from:infra used but 'infra' block is missing"
                        ));
          }
        }
      }
    }
  }

  // Secrets checks
  for (group, fields) in &config.secrets {
    for (key, leaf) in fields {
      let path = format!("secrets.{group}.{key}");
      match leaf {
        SecretLeaf::FromVault { vault_ref, .. } => {
          if vault_ref.is_empty() {
            errors.push(format!("{path}: from:vault requires a 'ref' field"));
          } else if !vault_keys.contains(&vault_ref.as_str()) {
            errors.push(format!(
              "{path}: ref '{vault_ref}' is not defined in vault"
            ));
          }
        }
        SecretLeaf::FromInfra { unit, output, .. } => {
          if output.is_empty() {
            errors
              .push(format!("{path}: from:infra requires an 'output' field"));
          }
          if unit.is_empty() {
            errors.push(format!("{path}: from:infra requires a 'unit' field"));
          }
          if !has_infra {
            errors.push(format!(
              "{path}: from:infra used but 'infra' block is missing"
            ));
          }
        }
      }
    }
  }

  // infra.credentials checks
  if let Some(infra) = &config.infra {
    for (var_name, cred) in &infra.credentials {
      if let CredentialValue::FromVault { vault_ref, .. } = cred {
        if vault_ref.is_empty() {
          errors.push(format!(
            "infra.credentials.{var_name}: from:vault requires a 'ref' field"
          ));
        } else if !vault_keys.contains(&vault_ref.as_str()) {
          errors.push(format!(
                        "infra.credentials.{var_name}: ref '{vault_ref}' is not defined in vault"
                    ));
        }
      }
    }
  }

  // Pipeline checks
  let mut pipeline_names: std::collections::HashSet<&str> =
    std::collections::HashSet::new();
  for p in &config.pipeline {
    if p.name.is_empty() {
      errors.push("pipeline[?].name: must not be empty".to_string());
      continue;
    }
    if !pipeline_names.insert(p.name.as_str()) {
      errors.push(format!("pipeline '{}': duplicate name", p.name));
    }
    match (&p.pipeline_type, &p.command) {
      (PipelineType::Custom, None) => {
        errors.push(format!(
          "pipeline '{}': type=custom requires a command",
          p.name
        ));
      }
      (PipelineType::Custom, Some(s)) if s.is_empty() => {
        errors.push(format!(
          "pipeline '{}': type=custom requires a non-empty command",
          p.name
        ));
      }
      (PipelineType::Nixos, Some(_)) => {
        errors.push(format!(
          "pipeline '{}': type=nixos does not accept a command",
          p.name
        ));
      }
      _ => {}
    }
    if p.version_prefix.is_empty() {
      errors.push(format!(
        "pipeline '{}': version_prefix must not be empty",
        p.name
      ));
    }
    if p.deployed_prefix.is_empty() {
      errors.push(format!(
        "pipeline '{}': deployed_prefix must not be empty",
        p.name
      ));
    }
    if matches!(p.version_scheme, VersionScheme::Custom)
      && p.version_script.is_none()
    {
      errors.push(format!(
        "pipeline '{}': version_scheme = custom requires version_script",
        p.name
      ));
    }
    if let Some(env) = &p.env {
      if !config.env.contains(env) {
        errors.push(format!(
          "pipeline '{}': env '{}' is not declared in dogma.yml",
          p.name, env
        ));
      }
    }
  }

  if !errors.is_empty() {
    let msg = errors.join("\n");
    bail!(
      "{}\n\n{} error(s) found — fix dogma.yml and re-run",
      msg,
      errors.len()
    );
  }

  Ok(())
}
