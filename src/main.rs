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
use commands::deploy::{DeployOptions, Mode};

fn main() {
    if let Err(e) = run() {
        log::error(&format!("{e:#}"));
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let start = cli.time.then(Instant::now);

    // --list-* flags: read dogma.yml and print completions, then exit.
    // These don't need the full repo-root walk to be meaningful — they just
    // need to find dogma.yml somewhere up the tree.
    if cli.list_envs || cli.list_units || cli.list_hosts {
        let repo_root = find_repo_root().unwrap_or_else(|_| std::env::current_dir().unwrap());
        let config = config::normalize::normalize(&repo_root).unwrap_or_else(|_| {
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
                hooks: Default::default(),
                deploy: Default::default(),
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
        Commands::Deploy {
            env, host, new, latest, version, skip_infra, skip_sops, refetch, message,
        } => {
            let mode = if new {
                Mode::New
            } else if latest {
                Mode::Latest
            } else if let Some(tag) = version {
                Mode::Version(tag)
            } else {
                Mode::Interactive
            };
            commands::deploy::run(&repo_root, DeployOptions {
                env,
                host,
                mode,
                skip_infra,
                skip_sops,
                refetch,
                commit_msg: message,
            })?;
        }
        Commands::Infra { command } => match command {
            InfraCommands::Apply { env, unit, migrate_state } => {
                commands::infra::apply(&repo_root, &env, &unit, migrate_state)?;
            }
            InfraCommands::Destroy { env, unit, migrate_state } => {
                commands::infra::destroy(&repo_root, &env, &unit, migrate_state)?;
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
