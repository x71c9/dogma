pub mod normalize;
pub mod validate;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Top-level config (dogma.yml raw form)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DogmaConfig {
  pub name: String,
  pub env: Vec<String>,
  pub admin: Vec<AdminKey>,

  #[serde(default)]
  pub vault: IndexMap<String, VaultEntry>,

  #[serde(default)]
  pub machines: IndexMap<String, Machine>,

  #[serde(default)]
  pub secrets: IndexMap<String, IndexMap<String, SecretLeaf>>,

  #[serde(skip_serializing_if = "Option::is_none")]
  pub infra: Option<InfraBlock>,

  #[serde(default)]
  pub nix: NixBlock,

  #[serde(default)]
  pub hooks: HooksBlock,

  #[serde(default)]
  pub deploy: DeployBlock,
}

// ---------------------------------------------------------------------------
// Admin keys
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum AdminKey {
  Gpg { gpg: String },
  Age { age: String },
  Ssh { ssh: String },
}

// ---------------------------------------------------------------------------
// Vault
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VaultEntry {
  /// After normalization: always a per-env map.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub envvar: Option<EnvOrMap>,

  /// After normalization: always a per-env map.
  #[serde(skip_serializing_if = "Option::is_none")]
  pub pass: Option<EnvOrMap>,
}

/// A value that can be either a flat string (same for all envs) or a per-env map.
/// After normalization it is always stored as a per-env map.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum EnvOrMap {
  Flat(String),
  PerEnv(IndexMap<String, String>),
}

impl EnvOrMap {
  pub fn get(&self, env: &str) -> Option<&str> {
    match self {
      EnvOrMap::Flat(s) => Some(s.as_str()),
      EnvOrMap::PerEnv(m) => m.get(env).map(String::as_str),
    }
  }
}

// ---------------------------------------------------------------------------
// Machines
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Machine {
  /// After normalization: always a per-env map.
  pub hostname: HostnameField,

  /// After normalization: always a per-env map of IpEntry.
  pub ip: IpField,

  #[serde(default = "default_root")]
  pub user: String,

  #[serde(default)]
  pub secrets: Vec<String>,

  #[serde(skip_serializing_if = "Option::is_none")]
  pub deployer: Option<DeployStrategy>,
}

fn default_root() -> String {
  "root".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum HostnameField {
  Flat(String),
  PerEnv(IndexMap<String, String>),
}

impl HostnameField {
  #[allow(dead_code)]
  pub fn get(&self, env: &str, machine_name: &str) -> String {
    match self {
      HostnameField::Flat(s) => s.replace("{env}", env),
      HostnameField::PerEnv(m) => m
        .get(env)
        .cloned()
        .unwrap_or_else(|| machine_name.to_string()),
    }
  }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum IpField {
  /// Shorthand: a single IpEntry applied to all envs.
  Shorthand(IpEntry),
  /// Per-env map.
  PerEnv(IndexMap<String, IpEntry>),
}

impl IpField {
  #[allow(dead_code)]
  pub fn get(&self, env: &str) -> Option<&IpEntry> {
    match self {
      IpField::Shorthand(e) => Some(e),
      IpField::PerEnv(m) => m.get(env),
    }
  }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum IpEntry {
  Static(String),
  FromInfra {
    from: String, // "infra"
    unit: String,
    output: String,
  },
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum SecretLeaf {
  FromVault {
    from: String, // "vault"
    #[serde(rename = "ref")]
    vault_ref: String,
  },
  FromInfra {
    from: String, // "infra"
    unit: String,
    output: String,
  },
}

impl SecretLeaf {
  #[allow(dead_code)]
  pub fn source(&self) -> &str {
    match self {
      SecretLeaf::FromVault { from, .. } => from,
      SecretLeaf::FromInfra { from, .. } => from,
    }
  }
}

// ---------------------------------------------------------------------------
// Infra block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct InfraBlock {
  #[serde(default = "default_tofu")]
  pub cli: String,

  #[serde(default = "default_infra_path")]
  pub path: String,

  #[serde(default = "default_var_file")]
  pub var_file: String,

  #[serde(default = "default_backend_config")]
  pub backend_config: String,

  #[serde(default)]
  pub credentials: IndexMap<String, CredentialValue>,
}

fn default_tofu() -> String {
  "tofu".to_string()
}
fn default_infra_path() -> String {
  "./infra".to_string()
}
pub fn default_var_file() -> String {
  "variables/{env}/main.tfvars".to_string()
}
pub fn default_backend_config() -> String {
  "variables/{env}/backend.conf".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CredentialValue {
  Static(String),
  FromVault {
    from: String, // "vault"
    #[serde(rename = "ref")]
    vault_ref: String,
  },
}

// ---------------------------------------------------------------------------
// Nix block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NixBlock {
  #[serde(default = "default_nix_path")]
  pub path: String,

  #[serde(default = "default_nix_secrets")]
  pub secrets: String,

  #[serde(default = "default_nix_sops")]
  pub sops: String,
}

impl Default for NixBlock {
  fn default() -> Self {
    Self {
      path: default_nix_path(),
      secrets: default_nix_secrets(),
      sops: default_nix_sops(),
    }
  }
}

fn default_nix_path() -> String {
  "./nix".to_string()
}
fn default_nix_secrets() -> String {
  "./nix/secrets".to_string()
}
fn default_nix_sops() -> String {
  "./nix/.sops.yaml".to_string()
}

// ---------------------------------------------------------------------------
// Hooks block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HooksBlock {
  #[serde(rename = "pre-deploy", default)]
  pub pre_deploy: Vec<String>,

  #[serde(rename = "post-deploy", default)]
  pub post_deploy: Vec<String>,
}

// ---------------------------------------------------------------------------
// Deploy block
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DeployBlock {
  #[serde(default)]
  pub strategy: DeployStrategy,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeployStrategy {
  #[default]
  NixosRebuild,
}
