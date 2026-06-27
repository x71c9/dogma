mod cli;
mod commands;
mod config;
mod error;
mod git;
mod infra;
mod log;
mod sops;
mod vault;

use std::time::Instant;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Commands, InfraCommands};
use commands::pipeline::PipelineOptions;

fn main() {
  if let Err(e) = run() {
    log::error(&format!("{e:#}"));
    std::process::exit(1);
  }
}

fn run() -> Result<()> {
  let cli = parse_cli();
  let start = cli.time.then(Instant::now);

  // --list-* flags: read dogma.yml and print completions, then exit.
  // These don't need the full repo-root walk to be meaningful — they just
  // need to find dogma.yml somewhere up the tree.
  if cli.list_envs || cli.list_units || cli.list_hosts {
    let repo_root =
      find_repo_root().unwrap_or_else(|_| std::env::current_dir().unwrap());
    let config =
      config::normalize::normalize(&repo_root).unwrap_or_else(|_| {
        // Return an empty-ish config rather than error, so completion
        // degrades gracefully when dogma.yml is absent or malformed.
        config::DogmaConfig {
          name: String::new(),
          env: vec![],
          admin: vec![],
          vault: Default::default(),
          machines: Default::default(),
          secrets: Default::default(),
          infra: None,
          nix: Default::default(),
          deploy: Default::default(),
          pipeline: Default::default(),
        }
      });

    if cli.list_envs {
      for e in &config.env {
        println!("{e}");
      }
    }

    if cli.list_units {
      let infra_path = config
        .infra
        .as_ref()
        .map(|i| i.path.as_str())
        .unwrap_or("./infra");
      let abs = repo_root.join(infra_path.trim_start_matches("./"));
      if let Ok(rd) = std::fs::read_dir(&abs) {
        for entry in rd.flatten() {
          if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            if let Some(name) = entry.file_name().to_str() {
              println!("{name}");
            }
          }
        }
      }
    }

    if cli.list_hosts {
      for name in config.machines.keys() {
        println!("{name}");
      }
    }

    return Ok(());
  }

  // Completions don't need a repo root — handle before find_repo_root().
  if let Some(Commands::Completions { shell }) = &cli.command {
    commands::completions::run(shell);
    return Ok(());
  }

  let repo_root = find_repo_root()?;

  let Some(command) = cli.command else {
    // No subcommand — print help (clap would normally handle this, but
    // since command is Option we do it manually).
    use clap::CommandFactory;
    Cli::command().print_help()?;
    println!();
    return Ok(());
  };

  match command {
    Commands::Credentials { env } => {
      commands::credentials::run(&repo_root, &env)?;
    }
    Commands::Env { env } => {
      commands::env_cmd::run(&repo_root, &env)?;
    }
    Commands::Output { env, unit, key } => {
      commands::output::run(&repo_root, &env, unit.as_deref(), key.as_deref())?;
    }
    Commands::Shell { env } => {
      commands::shell::run(&repo_root, &env)?;
    }
    Commands::Pipeline {
      pipeline,
      env,
      host,
      new,
      latest,
      version,
      skip_infra,
      refetch,
      message,
    } => {
      let mode = if new {
        commands::pipeline::Mode::New
      } else if latest {
        commands::pipeline::Mode::Latest
      } else if let Some(tag) = version {
        commands::pipeline::Mode::Version(tag)
      } else {
        commands::pipeline::Mode::Interactive
      };
      commands::pipeline::run(
        &repo_root,
        PipelineOptions {
          pipeline_name: pipeline,
          env,
          host,
          mode,
          skip_infra,
          refetch,
          commit_msg: message,
        },
      )?;
    }
    Commands::Infra { command } => match command {
      InfraCommands::Apply {
        env,
        unit,
        migrate_state,
        upgrade,
        message,
      } => {
        commands::infra::apply(
          &repo_root,
          &env,
          &unit,
          migrate_state,
          upgrade,
          message,
        )?;
      }
      InfraCommands::Destroy {
        env,
        unit,
        migrate_state,
        upgrade,
      } => {
        commands::infra::destroy(
          &repo_root,
          &env,
          &unit,
          migrate_state,
          upgrade,
        )?;
      }
      InfraCommands::Auth { env } => {
        let config = config::normalize::normalize(&repo_root)?;
        let vars = commands::credentials::collect_credentials(&config, &env)?;
        log::info(&format!("entering {env} infra shell (exit to return)"));
        commands::shell::exec_shell(&env, vars)?;
      }
    },
    Commands::Completions { shell } => {
      commands::completions::run(&shell);
    }
  }

  if let Some(start) = start {
    let ms = start.elapsed().as_millis();
    log::dim(&format!("done in {ms}ms"));
  }

  Ok(())
}

/// Parse argv. On an unrecognized/missing subcommand, clap's default message is
/// terse (just a usage line). Augment those cases to also print the list of
/// available subcommands, then exit as clap normally would.
fn parse_cli() -> Cli {
  use clap::error::ErrorKind;
  use clap::CommandFactory;

  match Cli::try_parse() {
    Ok(cli) => cli,
    Err(e) => {
      if matches!(
        e.kind(),
        ErrorKind::InvalidSubcommand | ErrorKind::MissingSubcommand
      ) {
        // Print clap's own error (usage + tip), then the subcommand list.
        e.print().ok();
        eprintln!();
        Cli::command().print_help().ok();
        eprintln!();
        std::process::exit(2);
      }
      // All other parse errors (bad flags, --help, --version) keep clap's
      // native handling and exit codes.
      e.exit();
    }
  }
}

fn find_repo_root() -> Result<std::path::PathBuf> {
  let mut dir = std::env::current_dir()?;
  loop {
    if dir.join("dogma.yml").exists() {
      return Ok(dir);
    }
    match dir.parent() {
      Some(parent) => dir = parent.to_path_buf(),
      None => anyhow::bail!(
        "dogma.yml not found in current directory or any parent directory"
      ),
    }
  }
}
