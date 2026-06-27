use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use crate::config::{DogmaConfig, PipelineConfig, PipelineType, VersionScheme};
use crate::git;
use crate::infra::output as infra_output;
use crate::{log_dim, log_info, log_step, log_warn};

#[derive(Debug, Clone)]
pub enum Mode {
  New,
  Latest,
  Version(String),
  Interactive,
}

pub struct PipelineOptions {
  pub pipeline_name: String,
  pub env: Option<String>,
  pub host: Option<String>,
  pub mode: Mode,
  pub skip_infra: bool,
  pub refetch: bool,
  pub commit_msg: Option<String>,
}

pub fn run(repo_root: &Path, opts: PipelineOptions) -> Result<()> {
  // -----------------------------------------------------------------------
  // Step 0: load + validate config, find pipeline
  // -----------------------------------------------------------------------
  log_step!("pipeline [0/6] load config");
  let config = crate::config::normalize::normalize(repo_root)?;
  crate::config::validate::validate(&config)?;

  let pipeline = config
    .pipeline
    .iter()
    .find(|p| p.name == opts.pipeline_name)
    .ok_or_else(|| {
      anyhow::anyhow!(
        "pipeline '{}' not found in dogma.yml",
        opts.pipeline_name
      )
    })?
    .clone();

  let env: String = match opts.env {
    Some(e) => e,
    None => pipeline.env.clone().ok_or_else(|| {
      anyhow::anyhow!(
        "pipeline '{}': no env specified and no default env configured",
        pipeline.name
      )
    })?,
  };

  if !config.env.contains(&env) {
    bail!("env '{}' is not declared in dogma.yml", env);
  }

  // -----------------------------------------------------------------------
  // Step 1: dirty tree check
  // -----------------------------------------------------------------------
  log_step!("pipeline [1/6] dirty tree check");
  let repo = git::open(repo_root)?;
  let dirty = git::dirty_files(&repo, true)?;

  let original_ref = git::current_ref(&repo)?;
  let mut detached = false;
  let mut created_deploy_commit = false; // true once dogma owns a commit to amend into

  if !dirty.is_empty() {
    log_warn!("pipeline working tree has uncommitted changes:");
    eprintln!();
    for f in &dirty.files {
      crate::log::status_line(f.status, &f.path);
    }
    eprintln!();

    if !matches!(opts.mode, Mode::New) {
      bail!("working tree is dirty — commit or stash your changes before promoting a version");
    }

    let msg = match &opts.commit_msg {
      Some(m) => {
        log_info!("pipeline -m flag provided — committing with: {m}");
        m.clone()
      }
      None => {
        eprint!(
          "{}commit these changes before deploying? [Y/n] ",
          crate::log::prompt_prefix()
        );
        io::stderr().flush()?;
        let mut answer = String::new();
        io::stdin().read_line(&mut answer)?;
        if matches!(answer.trim().to_lowercase().as_str(), "n" | "no") {
          bail!("aborted — commit or stash your changes and re-run");
        }
        let suggested = git::suggest_commit_msg(&dirty);
        if let Some(ref s) = suggested {
          log_info!("pipeline suggested message: {}", crate::log::cyan(s));
          eprint!(
            "{}commit message (leave blank to accept): ",
            crate::log::prompt_prefix()
          );
          io::stderr().flush()?;
          let mut m = String::new();
          io::stdin().read_line(&mut m)?;
          let m = m.trim().to_string();
          if m.is_empty() {
            s.clone()
          } else {
            m
          }
        } else {
          eprint!("{}commit message: ", crate::log::prompt_prefix());
          io::stderr().flush()?;
          let mut m = String::new();
          io::stdin().read_line(&mut m)?;
          m.trim().to_string()
        }
      }
    };

    let msg = if msg.is_empty() {
      "chore: pre-deploy snapshot".to_string()
    } else {
      msg
    };

    git::commit_all(&repo, &msg)?;
    created_deploy_commit = true;
    log_info!("pipeline committed: {msg}");
  }

  // -----------------------------------------------------------------------
  // Step 2: resolve version
  // -----------------------------------------------------------------------
  log_step!("pipeline [2/6] resolve version");

  let version = match &opts.mode {
    Mode::New => {
      let v = resolve_new_version(&repo, &pipeline, repo_root)?;
      log_info!("pipeline new version: {v}");
      v
    }
    Mode::Version(tag) => {
      if !git::tag_exists(&repo, tag)? {
        bail!("version tag not found: {tag}");
      }
      log_info!("pipeline promoting: {tag}");
      git::checkout_tag(&repo, tag)?;
      detached = true;
      tag.clone()
    }
    Mode::Latest => {
      let tags = git::list_pipeline_tags(&repo, &pipeline.version_prefix)?;
      let tag = tags.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!(
          "no {}/v* tags found — run 'dogma deploy {} {} --new' first",
          pipeline.version_prefix,
          opts.pipeline_name,
          env
        )
      })?;
      log_info!("pipeline latest: {tag}");
      git::checkout_tag(&repo, &tag)?;
      detached = true;
      tag
    }
    Mode::Interactive => {
      let tags_with_date =
        git::list_pipeline_tags_with_date(&repo, &pipeline.version_prefix)?;
      if tags_with_date.is_empty() {
        bail!(
          "no {}/v* tags found — run 'dogma deploy {} {} --new' first",
          pipeline.version_prefix,
          opts.pipeline_name,
          env
        );
      }
      let items: Vec<String> = tags_with_date
        .iter()
        .enumerate()
        .map(|(i, (t, date))| {
          let date_part = if date.is_empty() {
            String::new()
          } else {
            format!("  {date}")
          };
          if i == 0 {
            format!("{t}{date_part}  (latest)")
          } else {
            format!("{t}{date_part}")
          }
        })
        .collect();

      let theme = dialoguer::theme::ColorfulTheme {
        active_item_style: dialoguer::console::Style::new().for_stderr().red(),
        active_item_prefix: dialoguer::console::style(">".to_string())
          .for_stderr()
          .red(),
        ..dialoguer::theme::ColorfulTheme::default()
      };
      let idx = dialoguer::Select::with_theme(&theme)
        .with_prompt(format!("select version to deploy to '{}'", env))
        .items(&items)
        .default(0)
        .max_length(10)
        .interact_on(&dialoguer::console::Term::stderr())?;
      let tag = tags_with_date[idx].0.clone();
      log_info!("pipeline selected: {tag}");
      git::checkout_tag(&repo, &tag)?;
      detached = true;
      tag
    }
  };

  let _guard = DetachGuard {
    repo_root,
    original_ref: original_ref.clone(),
    detached,
  };

  // -----------------------------------------------------------------------
  // Step 3: pre-deploy hooks (--new only)
  // -----------------------------------------------------------------------
  log_step!("pipeline [3/6] pre-deploy hooks");

  if matches!(opts.mode, Mode::New) {
    run_hooks(
      "pre-deploy",
      &pipeline.hooks.pre_deploy.clone(),
      repo_root,
      &version,
      &env,
      &opts.pipeline_name,
    )?;

    let hook_dirty = git::dirty_files(&repo, false)?;
    if !hook_dirty.is_empty() {
      if created_deploy_commit {
        git::amend_all(&repo)?;
        log_info!("pipeline folded hook changes into deploy commit");
      } else {
        git::commit_all(&repo, &format!("chore: release {version}"))?;
        log_info!("pipeline committed: chore: release {version}");
      }
    } else {
      log_dim!("pipeline hooks made no tracked changes");
    }
  } else {
    log_dim!("pipeline pre-deploy hooks skipped (promotion)");
  }

  // -----------------------------------------------------------------------
  // Step 4: run deploy command
  // -----------------------------------------------------------------------
  log_step!("pipeline [4/6] deploy");
  let is_new = matches!(opts.mode, Mode::New);
  let deployed_targets = run_deploy_command(
    &pipeline,
    &DeployCtx {
      config: &config,
      repo_root,
      env: &env,
      version: &version,
      pipeline_name: &opts.pipeline_name,
      is_new,
      skip_infra: opts.skip_infra,
      refetch: opts.refetch,
      host_filter: opts.host.as_deref(),
    },
  )?;

  // Commit any files written by the deploy step (e.g. nixos encrypted secrets).
  if is_new {
    let deploy_dirty = git::dirty_files(&repo, false)?;
    if !deploy_dirty.is_empty() {
      if created_deploy_commit {
        git::amend_all(&repo)?;
        log_info!("pipeline folded deploy changes into deploy commit");
      } else {
        git::commit_all(&repo, &format!("chore: release {version}"))?;
        log_info!("pipeline committed: chore: release {version}");
      }
    }
  }

  // -----------------------------------------------------------------------
  // Step 5: git tags + push
  // -----------------------------------------------------------------------
  log_step!("pipeline [5/6] release tag");
  release_tag(&repo, &version, &env, &pipeline, &deployed_targets, is_new)?;

  // -----------------------------------------------------------------------
  // Step 6: post-deploy hooks
  // -----------------------------------------------------------------------
  log_step!("pipeline [6/6] post-deploy hooks");
  run_hooks(
    "post-deploy",
    &pipeline.hooks.post_deploy.clone(),
    repo_root,
    &version,
    &env,
    &opts.pipeline_name,
  )?;

  log_step!("pipeline complete {} ({version})", env);
  Ok(())
}

// ---------------------------------------------------------------------------
// Version resolution
// ---------------------------------------------------------------------------

fn resolve_new_version(
  repo: &git2::Repository,
  pipeline: &PipelineConfig,
  repo_root: &Path,
) -> Result<String> {
  match &pipeline.version_scheme {
    VersionScheme::Calver => git::next_calver(repo, &pipeline.version_prefix),
    VersionScheme::Semver => {
      git::next_semver(repo, &pipeline.version_prefix, repo_root)
    }
    VersionScheme::Custom => {
      let script = pipeline.version_script.as_deref().ok_or_else(|| {
        anyhow::anyhow!(
          "pipeline '{}': version_scheme = custom requires version_script",
          pipeline.name
        )
      })?;
      git::next_custom(script, &pipeline.version_prefix, repo_root)
    }
  }
}

// ---------------------------------------------------------------------------
// Deploy command runner — dispatches on pipeline type
// ---------------------------------------------------------------------------

struct DeployCtx<'a> {
  config: &'a DogmaConfig,
  repo_root: &'a Path,
  env: &'a str,
  version: &'a str,
  pipeline_name: &'a str,
  is_new: bool,
  skip_infra: bool,
  refetch: bool,
  host_filter: Option<&'a str>,
}

fn run_deploy_command(
  pipeline: &PipelineConfig,
  ctx: &DeployCtx<'_>,
) -> Result<Vec<String>> {
  match &pipeline.pipeline_type {
    PipelineType::Custom => {
      run_custom_command(
        pipeline,
        ctx.repo_root,
        ctx.env,
        ctx.version,
        ctx.pipeline_name,
      )?;
      Ok(vec![])
    }
    PipelineType::Nixos => run_nixos_deploy(ctx),
  }
}

fn run_custom_command(
  pipeline: &PipelineConfig,
  repo_root: &Path,
  env: &str,
  version: &str,
  pipeline_name: &str,
) -> Result<()> {
  let cmd = pipeline.command.as_deref().ok_or_else(|| {
    anyhow::anyhow!("pipeline '{}': missing command", pipeline.name)
  })?;

  let cmd = cmd
    .replace("{env}", env)
    .replace("{version}", version)
    .replace("{pipeline}", pipeline_name);

  log_info!("pipeline running: {cmd}");

  let mut parts = cmd.split_whitespace();
  let exe = parts
    .next()
    .ok_or_else(|| anyhow::anyhow!("pipeline command is empty"))?;
  let args: Vec<&str> = parts.collect();

  let status = Command::new(exe)
    .args(&args)
    .current_dir(repo_root)
    .env("DOGMA_ENV", env)
    .env("DOGMA_VERSION", version)
    .env("DOGMA_PIPELINE", pipeline_name)
    .status()
    .with_context(|| format!("failed to run pipeline command: {cmd}"))?;

  if !status.success() {
    bail!(
      "pipeline command failed (exit {}): {cmd}",
      status.code().unwrap_or(-1)
    );
  }

  Ok(())
}

fn run_nixos_deploy(ctx: &DeployCtx<'_>) -> Result<Vec<String>> {
  use super::deploy;

  let config = ctx.config;
  let repo_root = ctx.repo_root;
  let env = ctx.env;
  let is_new = ctx.is_new;
  let skip_infra = ctx.skip_infra;
  let refetch = ctx.refetch;
  let host_filter = ctx.host_filter;

  deploy::check_all_deps()?;

  let host_list: Vec<String> = match host_filter {
    Some(h) => {
      if !config.machines.contains_key(h) {
        bail!("host '{h}' is not declared in dogma.yml");
      }
      vec![h.to_string()]
    }
    None => config.machines.keys().cloned().collect(),
  };

  let all_envs: Vec<String> = config.env.clone();

  let all_env_creds: Vec<(String, Vec<(String, String)>)> = {
    let mut v = Vec::new();
    for e in &all_envs {
      let creds = infra_output::resolve_infra_credentials(config, e)?;
      v.push((e.clone(), creds));
    }
    v
  };

  // Infra cache
  let cache_path = |e: &str| repo_root.join(format!(".dogma/cache/{e}.json"));

  if skip_infra {
    log_dim!("pipeline --skip-infra: using existing cache");
    for e in &all_envs {
      let cache = cache_path(e);
      if !cache.exists() {
        bail!("--skip-infra set but no cache found for env '{e}'");
      }
    }
  }

  let envs_to_refresh: Vec<&str> = if skip_infra {
    vec![]
  } else if is_new {
    if refetch {
      for e in &all_envs {
        let _ = std::fs::remove_file(cache_path(e));
      }
      all_envs.iter().map(String::as_str).collect()
    } else {
      all_envs
        .iter()
        .map(String::as_str)
        .filter(|e| *e == env || !deploy::cache_is_usable(&cache_path(e)))
        .collect()
    }
  } else {
    if refetch {
      let _ = std::fs::remove_file(cache_path(env));
    }
    vec![env]
  };

  for e in envs_to_refresh {
    if deploy::config_needs_infra(config, e) {
      let creds = deploy::lookup_creds(&all_env_creds, e);
      infra_output::refresh_with_creds(config, repo_root, e, None, creds)?;
    } else {
      log_dim!("pipeline no from:infra refs for {e} — skipping");
    }
  }

  // Sops + secrets (--new only)
  if is_new {
    crate::sops::generate::run(
      config,
      repo_root,
      env,
      refetch,
      Some(all_env_creds.as_slice()),
    )?;
    for e in &all_envs {
      crate::sops::secrets::generate(config, repo_root, e)?;
    }
    deploy::encrypt_secrets(config, repo_root, &all_envs, env, &all_env_creds)?;
  } else {
    deploy::verify_secrets_committed(config, repo_root, env)?;
  }

  // Per-host deploy
  let infra_creds = deploy::lookup_creds(&all_env_creds, env);
  let mut deployed_targets: Vec<String> = Vec::new();
  for host in &host_list {
    log_step!("pipeline host {host}");
    let target =
      deploy::deploy_host(config, repo_root, host, env, infra_creds)?;
    deployed_targets.push(target);
  }

  Ok(deployed_targets)
}

// ---------------------------------------------------------------------------
// Hook runner
// ---------------------------------------------------------------------------

fn run_hooks(
  hook_name: &str,
  hooks: &[String],
  repo_root: &Path,
  version: &str,
  env: &str,
  pipeline_name: &str,
) -> Result<()> {
  if hooks.is_empty() {
    log_dim!("pipeline no hooks.{hook_name} defined — skipping");
    return Ok(());
  }
  for hook in hooks {
    let hook = hook
      .replace("{env}", env)
      .replace("{version}", version)
      .replace("{pipeline}", pipeline_name);
    let mut parts = hook.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    let hook_path = repo_root.join(cmd);
    if !hook_path.exists() {
      bail!("hook not found: {cmd}");
    }
    log_info!("pipeline running {hook_name} hook: {hook}");
    let status = Command::new(&hook_path)
      .args(&args)
      .env("DOGMA_VERSION", version)
      .env("DOGMA_ENV", env)
      .env("DOGMA_PIPELINE", pipeline_name)
      .status()
      .with_context(|| format!("failed to run hook: {hook}"))?;
    if !status.success() {
      bail!("{hook_name} hook failed: {hook}");
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Tag management
// ---------------------------------------------------------------------------

fn release_tag(
  repo: &git2::Repository,
  version: &str,
  env: &str,
  pipeline: &PipelineConfig,
  deployed_targets: &[String],
  is_new: bool,
) -> Result<()> {
  let version_suffix = version
    .strip_prefix(&format!("{}/", pipeline.version_prefix))
    .unwrap_or(version);
  let deployed_tag =
    format!("{}-{}-{}", pipeline.deployed_prefix, env, version_suffix);

  let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
  let gen_summary = fetch_nix_generations(deployed_targets);
  let tag_msg = format!(
    "env={env}{} deployed-at={timestamp}",
    if gen_summary.is_empty() {
      String::new()
    } else {
      format!(" {gen_summary}")
    }
  );

  if is_new {
    git::push_commits(repo)?;

    if git::tag_exists(repo, version)? {
      bail!("tag '{version}' already exists");
    }
    log_info!("pipeline creating annotated tag {version}");
    git::create_annotated_tag(repo, version, &tag_msg)?;
    git::push_tag(repo, version)?;
  }

  if git::tag_exists(repo, &deployed_tag)? {
    log_dim!("pipeline tag '{deployed_tag}' already exists — skipping");
  } else {
    log_info!("pipeline creating tag {deployed_tag}");
    git::create_lightweight_tag(repo, &deployed_tag)?;
    git::push_tag(repo, &deployed_tag)?;
  }

  Ok(())
}

fn fetch_nix_generations(targets: &[String]) -> String {
  targets
    .iter()
    .filter_map(|target| {
      let out = Command::new("ssh")
        .args([
          "-o", "ConnectTimeout=10",
          "-o", "BatchMode=yes",
          target,
          "sudo nix-env --list-generations --profile /nix/var/nix/profiles/system 2>/dev/null | tail -1 | awk '{print $1}'",
        ])
        .output()
        .ok()?;
      let gen = String::from_utf8(out.stdout).ok()?.trim().to_string();
      if gen.is_empty() {
        Some(format!("{target}=gen?"))
      } else {
        Some(format!("{target}=gen{gen}"))
      }
    })
    .collect::<Vec<_>>()
    .join(" ")
}

// ---------------------------------------------------------------------------
// RAII guard: restore git branch on drop if we detached
// ---------------------------------------------------------------------------

struct DetachGuard<'a> {
  repo_root: &'a Path,
  original_ref: String,
  detached: bool,
}

impl Drop for DetachGuard<'_> {
  fn drop(&mut self) {
    if self.detached {
      if let Ok(repo) = git::open(self.repo_root) {
        let _ = git::restore_ref(&repo, &self.original_ref);
        log_info!("pipeline restored branch: {}", self.original_ref);
      }
    }
  }
}
