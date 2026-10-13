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

  /// Infrastructure management (apply/destroy/init/run/auth)
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
    /// Extra tofu apply args after `--` (e.g. -- -target=module.x)
    #[arg(last = true, value_name = "TOFU_ARGS")]
    extra_args: Vec<String>,
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
    /// Extra tofu destroy args after `--` (e.g. -- -target=module.x)
    #[arg(last = true, value_name = "TOFU_ARGS")]
    extra_args: Vec<String>,
  },
  /// Normalize, validate, init, then run any tofu command in the unit
  /// (plan, state list, output, import, …; not apply/destroy/init)
  Run {
    /// Environment name
    env: String,
    /// Infra unit name
    unit: String,
    /// Tofu command and args, passed as-is (e.g. `state list`,
    /// `plan -target=module.x`)
    #[arg(
      required = true,
      trailing_var_arg = true,
      allow_hyphen_values = true,
      value_name = "TOFU_ARGS"
    )]
    args: Vec<String>,
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

#[cfg(test)]
mod tests {
  use super::{Cli, Commands, InfraCommands};
  use clap::Parser;

  fn infra(argv: &[&str]) -> Result<InfraCommands, clap::Error> {
    let mut full = vec!["dogma", "infra"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full)?.command {
      Some(Commands::Infra { command }) => Ok(command),
      _ => panic!("expected an infra command"),
    }
  }

  #[test]
  fn run_keeps_args_verbatim() {
    let InfraCommands::Run { env, unit, args } =
      infra(&["run", "prod", "u", "plan", "-target=module.x", "-no-color"])
        .unwrap()
    else {
      panic!("expected run");
    };
    assert_eq!((env.as_str(), unit.as_str()), ("prod", "u"));
    assert_eq!(args, ["plan", "-target=module.x", "-no-color"]);
  }

  #[test]
  fn run_accepts_double_dash() {
    let InfraCommands::Run { args, .. } =
      infra(&["run", "prod", "u", "--", "state", "list"]).unwrap()
    else {
      panic!("expected run");
    };
    assert_eq!(args, ["state", "list"]);
  }

  #[test]
  fn run_global_flag_position() {
    let cli = Cli::try_parse_from([
      "dogma", "infra", "run", "p", "u", "--time", "plan",
    ])
    .unwrap();
    let Some(Commands::Infra {
      command: InfraCommands::Run { args, .. },
    }) = cli.command
    else {
      panic!("expected run");
    };
    assert!(cli.time);
    assert_eq!(args, ["plan"]);

    // once the tofu command has started, every arg is tofu's
    let cli = Cli::try_parse_from([
      "dogma", "infra", "run", "p", "u", "plan", "--time",
    ])
    .unwrap();
    let Some(Commands::Infra {
      command: InfraCommands::Run { args, .. },
    }) = cli.command
    else {
      panic!("expected run");
    };
    assert!(!cli.time);
    assert_eq!(args, ["plan", "--time"]);
  }

  #[test]
  fn run_requires_args() {
    assert!(infra(&["run", "prod", "u"]).is_err());
  }

  #[test]
  fn apply_extra_args_after_double_dash() {
    let InfraCommands::Apply { extra_args, .. } =
      infra(&["apply", "prod", "u", "--", "-target=module.x"]).unwrap()
    else {
      panic!("expected apply");
    };
    assert_eq!(extra_args, ["-target=module.x"]);
  }

  #[test]
  fn apply_without_extra_args() {
    let InfraCommands::Apply { extra_args, .. } =
      infra(&["apply", "prod", "u", "--upgrade"]).unwrap()
    else {
      panic!("expected apply");
    };
    assert!(extra_args.is_empty());
  }
}
