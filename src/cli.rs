use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
  name = "dogma",
  version,
  about = "CLI tool for managing secrets from vault to deployed servers"
)]
pub struct Cli {
  /// Print elapsed time after the command completes
  #[arg(long, global = true)]
  pub time: bool,

  /// Print available environments (for shell completion)
  #[arg(long, global = true, hide = true)]
  pub list_envs: bool,

  /// Print available infra units (for shell completion)
  #[arg(long, global = true, hide = true)]
  pub list_units: bool,

  /// Print declared pipeline names (for shell completion)
  #[arg(long, global = true, hide = true)]
  pub list_pipelines: bool,

  #[command(subcommand)]
  pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
  /// Print export statements for infra credentials (eval $(dogma credentials <env>))
  Credentials {
    /// Environment name (e.g. dev, prod)
    env: String,
  },

  /// Print export statements for all secrets (eval $(dogma env <env>))
  Env {
    /// Environment name
    env: String,
  },

  /// Print cached infra outputs
  Output {
    /// Environment name
    env: String,
    /// Infra unit name (e.g. hetzner)
    unit: Option<String>,
    /// Output key
    key: Option<String>,
  },

  /// Spawn a shell with infra credentials loaded
  Shell {
    /// Environment name
    env: String,
  },

  /// Deploy (run a named pipeline: build, publish, etc.)
  Deploy {
    /// Environment name (e.g. dev, prod), or a pipeline name whose env
    /// attribute is set
    #[arg(value_name = "ENV|PIPELINE")]
    env: String,
    /// Pipeline name as declared in dogma.yml [[pipeline]] — required when
    /// more than one pipeline is declared and the first argument is an env
    pipeline: Option<String>,
    /// Create a new version, run hooks, commit, deploy, tag
    #[arg(long, conflicts_with_all = ["latest", "version"])]
    new: bool,
    /// Deploy the latest existing version tag
    #[arg(long, conflicts_with_all = ["new", "version"])]
    latest: bool,
    /// Deploy a specific existing version tag
    #[arg(long, conflicts_with_all = ["new", "latest"], value_name = "TAG")]
    version: Option<String>,
    /// Skip infra cache refresh (nixos type only)
    #[arg(long)]
    skip_infra: bool,
    /// Clear infra cache and re-fetch everything (nixos type only)
    #[arg(long)]
    refetch: bool,
    /// Pre-commit message for dirty tree (only valid with --new)
    #[arg(short = 'm', long)]
    message: Option<String>,
  },

  /// Infrastructure management (apply/destroy)
  Infra {
    #[command(subcommand)]
    command: InfraCommands,
  },

  /// Print shell completion script to stdout
  Completions {
    /// Shell to generate completions for
    shell: CompletionShell,
  },
}

#[derive(clap::ValueEnum, Clone)]
pub enum CompletionShell {
  Bash,
  Zsh,
  Fish,
}

#[derive(Subcommand)]
pub enum InfraCommands {
  /// Normalize, validate, init, then run tofu apply
  Apply {
    /// Environment name
    env: String,
    /// Infra unit name
    unit: String,
    /// Pass -migrate-state to tofu init instead of -reconfigure
    #[arg(long, conflicts_with = "upgrade")]
    migrate_state: bool,
    /// Pass -upgrade to tofu init (updates provider lock file)
    #[arg(long, conflicts_with = "migrate_state")]
    upgrade: bool,
    /// Commit message for dirty tree (skips interactive prompt)
    #[arg(short = 'm', long)]
    message: Option<String>,
  },
  /// Normalize, validate, init, then run tofu destroy
  Destroy {
    /// Environment name
    env: String,
    /// Infra unit name
    unit: String,
    /// Pass -migrate-state to tofu init instead of -reconfigure
    #[arg(long, conflicts_with = "upgrade")]
    migrate_state: bool,
    /// Pass -upgrade to tofu init (updates provider lock file)
    #[arg(long, conflicts_with = "migrate_state")]
    upgrade: bool,
  },
  /// Normalize, validate, then run tofu init (always re-runs, even if the
  /// unit looks already initialized)
  Init {
    /// Environment name
    env: String,
    /// Infra unit name
    unit: String,
    /// Pass -migrate-state to tofu init instead of -reconfigure
    #[arg(long, conflicts_with = "upgrade")]
    migrate_state: bool,
    /// Pass -upgrade to tofu init (updates provider lock file)
    #[arg(long, conflicts_with = "migrate_state")]
    upgrade: bool,
  },
  /// Spawn a shell with infra credentials loaded
  Auth {
    /// Environment name
    env: String,
  },
}
