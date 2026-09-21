use anyhow::{bail, Result};
use std::collections::HashMap;

use super::{
  CredentialValue, DogmaConfig, IpEntry, PipelineType, SecretLeaf,
  VersionScheme,
};

pub fn validate(config: &DogmaConfig) -> Result<()> {
  let mut errors: Vec<String> = Vec::new();

  let secret_groups: Vec<&str> =
    config.secrets.keys().map(String::as_str).collect();
  let vault_keys: Vec<&str> = config.vault.keys().map(String::as_str).collect();
  let machine_names: Vec<&str> =
    config.machines.keys().map(String::as_str).collect();
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

  // depends-on: every dependency must name another declared machine
  let mut depends_on_ok = true;
  for (host, machine) in &config.machines {
    for dep in machine.depends_on.names() {
      if dep == host {
        depends_on_ok = false;
        errors.push(format!(
          "machines.{host}.depends-on: '{host}' cannot depend on itself"
        ));
      } else if !machine_names.contains(&dep.as_str()) {
        depends_on_ok = false;
        errors.push(format!(
          "machines.{host}.depends-on: '{dep}' is not defined in machines (available: {})",
          machine_names.join(", ")
        ));
      }
    }
  }

  // depends-on: the graph as a whole must be acyclic. Only meaningful once
  // every edge is known to point at a real machine — a dangling edge would
  // otherwise report a confusing cycle on top of the real error.
  if depends_on_ok {
    if let Some(cycle) = find_cycle(config) {
      errors.push(format!("machines: dependency cycle: {}", cycle.join(" → ")));
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

/// DFS over the `depends-on` graph, returning the first cycle found as a
/// readable path (`a → b → a`). Assumes every edge names a declared machine,
/// which the reference checks above guarantee.
fn find_cycle(config: &DogmaConfig) -> Option<Vec<String>> {
  #[derive(Clone, Copy, PartialEq)]
  enum Mark {
    Unvisited,
    OnPath,
    Done,
  }

  fn visit<'a>(
    config: &'a DogmaConfig,
    host: &'a str,
    marks: &mut HashMap<&'a str, Mark>,
    path: &mut Vec<&'a str>,
  ) -> Option<Vec<String>> {
    marks.insert(host, Mark::OnPath);
    path.push(host);

    if let Some(machine) = config.machines.get(host) {
      for dep in machine.depends_on.names() {
        // Look the key up in the config so the borrow outlives this frame.
        let Some((dep, _)) = config.machines.get_key_value(dep.as_str()) else {
          continue;
        };
        let dep = dep.as_str();
        match marks.get(dep).copied().unwrap_or(Mark::Unvisited) {
          // Back edge: the cycle is the path from `dep` onwards, closed up.
          Mark::OnPath => {
            let start = path.iter().position(|p| *p == dep).unwrap_or(0);
            let mut cycle: Vec<String> =
              path[start..].iter().map(|s| s.to_string()).collect();
            cycle.push(dep.to_string());
            return Some(cycle);
          }
          Mark::Unvisited => {
            if let Some(cycle) = visit(config, dep, marks, path) {
              return Some(cycle);
            }
          }
          Mark::Done => {}
        }
      }
    }

    path.pop();
    marks.insert(host, Mark::Done);
    None
  }

  let mut marks: HashMap<&str, Mark> = HashMap::new();
  let mut path: Vec<&str> = Vec::new();

  for start in config.machines.keys() {
    if marks
      .get(start.as_str())
      .copied()
      .unwrap_or(Mark::Unvisited)
      == Mark::Unvisited
    {
      if let Some(cycle) = visit(config, start, &mut marks, &mut path) {
        return Some(cycle);
      }
    }
  }
  None
}

#[cfg(test)]
mod tests {
  use super::validate;
  use crate::config::DogmaConfig;

  /// Machines with the given `depends-on` bodies, sharing one secret group.
  fn config(machines: &str) -> DogmaConfig {
    let yml = format!(
      "name: x\nenv: [test]\nadmin: []\nmachines:\n{machines}\nsecrets: {{}}\n"
    );
    serde_yaml::from_str(&yml).expect("test yaml should parse")
  }

  fn machine(name: &str, depends_on: &str) -> String {
    format!(
      "  {name}:\n    hostname: h-{name}\n    ip: \"1.2.3.4\"\n{depends_on}"
    )
  }

  #[test]
  fn no_depends_on_is_valid() {
    let cfg = config(&format!("{}{}", machine("a", ""), machine("b", "")));
    assert!(validate(&cfg).is_ok());
  }

  #[test]
  fn valid_dependency_accepted() {
    let cfg = config(&format!(
      "{}{}",
      machine("a", "    depends-on: [b]\n"),
      machine("b", "")
    ));
    assert!(validate(&cfg).is_ok());
  }

  #[test]
  fn shorthand_dependency_accepted() {
    let cfg = config(&format!(
      "{}{}",
      machine("a", "    depends-on: b\n"),
      machine("b", "")
    ));
    assert!(validate(&cfg).is_ok());
  }

  #[test]
  fn unknown_dependency_rejected() {
    let cfg = config(&machine("a", "    depends-on: [ghost]\n"));
    let err = validate(&cfg).unwrap_err().to_string();
    assert!(err.contains("machines.a.depends-on"));
    assert!(err.contains("'ghost' is not defined in machines"));
    assert!(err.contains("available: a"));
  }

  #[test]
  fn self_dependency_rejected() {
    let cfg = config(&machine("a", "    depends-on: [a]\n"));
    let err = validate(&cfg).unwrap_err().to_string();
    assert!(err.contains("'a' cannot depend on itself"));
  }

  #[test]
  fn cycle_rejected() {
    let cfg = config(&format!(
      "{}{}",
      machine("a", "    depends-on: [b]\n"),
      machine("b", "    depends-on: [a]\n")
    ));
    let err = validate(&cfg).unwrap_err().to_string();
    assert!(err.contains("dependency cycle"), "got: {err}");
    assert!(err.contains("a"));
    assert!(err.contains("b"));
  }

  #[test]
  fn three_machine_cycle_rejected() {
    let cfg = config(&format!(
      "{}{}{}",
      machine("a", "    depends-on: [b]\n"),
      machine("b", "    depends-on: [c]\n"),
      machine("c", "    depends-on: [a]\n")
    ));
    let err = validate(&cfg).unwrap_err().to_string();
    assert!(err.contains("dependency cycle"), "got: {err}");
  }

  #[test]
  fn diamond_is_not_a_cycle() {
    let cfg = config(&format!(
      "{}{}{}{}",
      machine("top", "    depends-on: [left, right]\n"),
      machine("left", "    depends-on: [base]\n"),
      machine("right", "    depends-on: [base]\n"),
      machine("base", "")
    ));
    assert!(validate(&cfg).is_ok());
  }

  #[test]
  fn all_depends_on_errors_reported_together() {
    let cfg = config(&format!(
      "{}{}",
      machine("a", "    depends-on: [ghost, a]\n"),
      machine("b", "    depends-on: [phantom]\n")
    ));
    let err = validate(&cfg).unwrap_err().to_string();
    assert!(err.contains("'ghost'"));
    assert!(err.contains("cannot depend on itself"));
    assert!(err.contains("'phantom'"));
    assert!(err.contains("3 error(s) found"), "got: {err}");
  }
}
