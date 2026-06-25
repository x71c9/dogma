use anyhow::{bail, Context, Result};
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;

use crate::config::normalize::normalize;
use crate::config::validate::validate;
use crate::config::{
  DeployStrategy, DogmaConfig, IpEntry, IpField, SecretLeaf,
};
use crate::error::check_dep;
use crate::git;
use crate::infra::output as infra_output;
use crate::sops;
use crate::vault;
use crate::{log_dim, log_info, log_step, log_warn};

#[derive(Debug, Clone)]
pub enum Mode {
  New,
  Latest,
  Version(String),
  Interactive,
}

pub struct DeployOptions {
  pub env: String,
  pub host: Option<String>,
  pub mode: Mode,
  pub skip_infra: bool,
  pub skip_sops: bool,
  pub refetch: bool,
  pub commit_msg: Option<String>,
}

pub fn run(repo_root: &Path, opts: DeployOptions) -> Result<()> {
  // -----------------------------------------------------------------------
  // Step 0: upfront dependency check — fail before touching anything
  // -----------------------------------------------------------------------
  log_step!("deploy [0/10] checking dependencies");
  check_all_deps()?;

  // -----------------------------------------------------------------------
  // Step 1: normalize + validate
  // -----------------------------------------------------------------------
  log_step!("deploy [1/10] normalize + validate");
  let config = normalize(repo_root)?;
  validate(&config)?;

  if !config.env.contains(&opts.env) {
    bail!("env '{}' is not declared in dogma.yml", opts.env);
  }

  // -----------------------------------------------------------------------
  // Step 2: dirty tree check
  // -----------------------------------------------------------------------
  log_step!("deploy [2/10] dirty tree check");
  let repo = git::open(repo_root)?;
  // Include untracked files: a deploy/* tag must be a COMPLETE snapshot so it
  // can be promoted to other envs. An untracked file (e.g. terraform-generated
  // nixos-config) left out of the tagged commit would make the promoted version
  // differ from what was built locally.
  let dirty = git::dirty_files(&repo, true)?;

  let original_ref = git::current_ref(&repo)?;
  let mut detached = false;
  // Tracks whether dogma has authored a commit this run. Once true, later
  // stages (hooks, secrets) fold their changes into it via amend instead of
  // creating separate commits — yielding a single clean deploy commit.
  let mut created_deploy_commit = false;

  if !dirty.is_empty() {
    log_warn!("deploy working tree has uncommitted changes:");
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
        log_info!("deploy -m flag provided — committing with: {m}");
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
        let prompt_msg = if let Some(ref s) = suggested {
          log_info!("deploy suggested message: {}", crate::log::cyan(s));
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
        };
        if prompt_msg.is_empty() {
          "chore: pre-deploy snapshot".to_string()
        } else {
          prompt_msg
        }
      }
    };

    git::commit_all(&repo, &msg)?;
    created_deploy_commit = true;
    log_info!("deploy committed: {msg}");
  }

  // -----------------------------------------------------------------------
  // Step 3: resolve version
  // -----------------------------------------------------------------------
  log_step!("deploy [3/10] resolve version");

  let dogma_version = match &opts.mode {
    Mode::New => {
      let v = git::next_version(&repo)?;
      log_info!("deploy new version: {v}");
      v
    }
    Mode::Version(tag) => {
      if !git::tag_exists(&repo, tag)? {
        bail!("version tag not found: {tag}");
      }
      log_info!("deploy promoting: {tag}");
      log_info!("deploy checking out {tag} (detached HEAD) ...");
      git::checkout_tag(&repo, tag)?;
      detached = true;
      tag.clone()
    }
    Mode::Latest => {
      let tags = git::list_deploy_tags(&repo)?;
      let tag = tags.into_iter().next().ok_or_else(|| {
        anyhow::anyhow!(
          "no deploy/* tags found — run 'dogma deploy {} --new' first",
          opts.env
        )
      })?;
      log_info!("deploy latest: {tag}");
      log_info!("deploy checking out {tag} (detached HEAD) ...");
      git::checkout_tag(&repo, &tag)?;
      detached = true;
      tag
    }
    Mode::Interactive => {
      let tags_with_date = git::list_deploy_tags_with_date(&repo)?;
      if tags_with_date.is_empty() {
        bail!(
          "no deploy/* tags found — run 'dogma deploy {} --new' first",
          opts.env
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
        .with_prompt(format!("select version to deploy to '{}'", opts.env))
        .items(&items)
        .default(0)
        .max_length(10)
        .interact_on(&dialoguer::console::Term::stderr())?;
      let tag = tags_with_date[idx].0.clone();
      log_info!("deploy selected: {tag}");
      log_info!("deploy checking out {tag} (detached HEAD) ...");
      git::checkout_tag(&repo, &tag)?;
      detached = true;
      tag
    }
  };

  // Restore original branch on any exit path after detach
  let _guard = DetachGuard {
    repo_root,
    original_ref: original_ref.clone(),
    detached,
  };

  // -----------------------------------------------------------------------
  // Step 4: pre-deploy hooks (--new only)
  // -----------------------------------------------------------------------
  log_step!("deploy [4/10] pre-deploy hooks");

  let host_list: Vec<String> = match &opts.host {
    Some(h) => {
      if !config.machines.contains_key(h.as_str()) {
        bail!("host '{h}' is not declared in dogma.yml");
      }
      vec![h.clone()]
    }
    None => config.machines.keys().cloned().collect(),
  };
  let dogma_hosts = host_list.join("\n");

  if matches!(opts.mode, Mode::New) {
    run_hooks(
      "pre-deploy",
      &config.hooks.pre_deploy.clone(),
      repo_root,
      &dogma_version,
      &opts.env,
      &dogma_hosts,
      "",
    )?;

    let hook_dirty = git::dirty_files(&repo, false)?;
    if !hook_dirty.is_empty() {
      if created_deploy_commit {
        git::amend_all(&repo)?;
        log_info!("deploy folded hook changes into deploy commit");
      } else {
        let msg = format!("chore: release {dogma_version}");
        git::commit_all(&repo, &msg)?;
        created_deploy_commit = true;
        log_info!("deploy committed: {msg}");
      }
    } else {
      log_dim!("deploy hooks made no tracked changes");
    }
  } else {
    log_dim!("deploy pre-deploy hooks skipped (promotion)");
  }

  // -----------------------------------------------------------------------
  // Step 5: infra cache
  // -----------------------------------------------------------------------
  log_step!("deploy [5/10] infra cache");

  let all_envs: Vec<String> = config.env.clone();

  // Resolve credentials once per env for all subsequent steps (steps 5–8).
  // Vault reads are expensive; threading resolved creds avoids re-reading on
  // every step.
  let all_env_creds: Vec<(String, Vec<(String, String)>)> = {
    let mut v = Vec::new();
    for e in &all_envs {
      let creds = infra_output::resolve_infra_credentials(&config, e)?;
      v.push((e.clone(), creds));
    }
    v
  };

  if opts.skip_infra {
    log_dim!("deploy --skip-infra: using existing cache");
    for e in &all_envs {
      let cache = repo_root.join(format!(".dogma/cache/{e}.json"));
      if !cache.exists() {
        bail!("--skip-infra set but no cache found for env '{e}'");
      }
    }
  } else {
    let cache_path = |e: &str| repo_root.join(format!(".dogma/cache/{e}.json"));

    let envs_to_refresh: Vec<&str> = if matches!(opts.mode, Mode::New) {
      // --new builds a promotable commit: it encrypts EVERY env's secrets so a
      // later `dogma deploy <other-env>` can deploy this exact commit. Those
      // secrets are read from each env's infra cache. But hitting the cloud for
      // every env would require every env's credentials on this machine, which
      // is rarely true. So: always refresh the target env, and for the other
      // envs only hit the cloud when their cache is missing OR empty/stale
      // (otherwise reuse it). --refetch forces a full refresh of all envs.
      if opts.refetch {
        for e in &all_envs {
          let _ = std::fs::remove_file(cache_path(e));
        }
        all_envs.iter().map(String::as_str).collect()
      } else {
        all_envs
          .iter()
          .map(String::as_str)
          .filter(|e| *e == opts.env || !cache_is_usable(&cache_path(e)))
          .collect()
      }
    } else {
      if opts.refetch {
        let _ = std::fs::remove_file(cache_path(&opts.env));
      }
      vec![opts.env.as_str()]
    };

    for e in envs_to_refresh {
      if config_needs_infra(&config, e) {
        let creds = lookup_creds(&all_env_creds, e);
        infra_output::refresh_with_creds(&config, repo_root, e, None, creds)?;
      } else {
        log_dim!("deploy no from:infra refs for {e} — skipping");
      }
    }
  }

  // -----------------------------------------------------------------------
  // Step 6: generate .sops.yaml
  // -----------------------------------------------------------------------
  log_step!("deploy [6/10] generate .sops.yaml");

  if matches!(opts.mode, Mode::New) {
    if opts.skip_sops {
      log_dim!("deploy --skip-sops: using existing .sops.yaml");
    } else {
      sops::generate::run(
        &config,
        repo_root,
        &opts.env,
        opts.refetch,
        Some(all_env_creds.as_slice()),
      )?;
    }
  } else {
    log_dim!("deploy .sops.yaml skipped (promotion)");
  }

  // -----------------------------------------------------------------------
  // Step 7: generate secrets.nix + encrypt secrets
  // -----------------------------------------------------------------------
  log_step!("deploy [7/10] generate + encrypt secrets");

  if matches!(opts.mode, Mode::New) {
    for e in &all_envs {
      sops::secrets::generate(&config, repo_root, e)?;
    }
    encrypt_secrets(&config, repo_root, &all_envs, &opts.env, &all_env_creds)?;

    let secrets_dir =
      repo_root.join(config.nix.secrets.trim_start_matches("./"));
    let secret_dirty = git::dirty_files(&repo, false)?;
    if !secret_dirty.is_empty() {
      if created_deploy_commit {
        git::amend_all(&repo)?;
        log_info!("deploy folded encrypted secrets into deploy commit");
      } else {
        // Last stage that can commit — no need to update the flag.
        git::commit_all(
          &repo,
          "chore(secrets): update encrypted secrets (all envs)",
        )?;
        log_info!("deploy committed secrets");
      }
    } else {
      log_dim!("deploy secrets unchanged — nothing to commit");
    }
    drop(secrets_dir);
  } else {
    log_info!("deploy verifying committed secrets ...");
    verify_secrets_committed(&config, repo_root, &opts.env)?;
  }

  // -----------------------------------------------------------------------
  // Step 8: per-host deploy
  // -----------------------------------------------------------------------
  log_step!("deploy [8/10] deploy");

  let mut deployed_targets: Vec<String> = Vec::new();
  let infra_creds = lookup_creds(&all_env_creds, &opts.env);

  for host in &host_list {
    log_step!("deploy host {host}");
    let target = deploy_host(&config, repo_root, host, &opts.env, infra_creds)?;
    deployed_targets.push(target);
  }

  // -----------------------------------------------------------------------
  // Step 9: git tags + push
  // -----------------------------------------------------------------------
  log_step!("deploy [9/10] release tag");
  release_tag(
    &repo,
    &dogma_version,
    &opts.env,
    &deployed_targets,
    matches!(opts.mode, Mode::New),
  )?;

  // -----------------------------------------------------------------------
  // Step 10: post-deploy hooks
  // -----------------------------------------------------------------------
  log_step!("deploy [10/10] post-deploy hooks");
  let dogma_deployed_ips = deployed_targets.join("\n");
  run_hooks(
    "post-deploy",
    &config.hooks.post_deploy.clone(),
    repo_root,
    &dogma_version,
    &opts.env,
    &dogma_hosts,
    &dogma_deployed_ips,
  )?;

  log_step!("deploy complete {} ({dogma_version})", opts.env);
  Ok(())
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
  hosts: &str,
  deployed_ips: &str,
) -> Result<()> {
  if hooks.is_empty() {
    log_dim!("deploy no hooks.{hook_name} defined — skipping");
    return Ok(());
  }
  for hook in hooks {
    let hook = hook
      .replace("{env}", env)
      .replace("{version}", version)
      .replace("{hosts}", hosts)
      .replace("{deployed_ips}", deployed_ips);
    let mut parts = hook.split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<&str> = parts.collect();
    let hook_path = repo_root.join(cmd);
    if !hook_path.exists() {
      bail!("hook not found: {cmd}");
    }
    log_info!("deploy running {hook_name} hook: {hook}");
    let status = Command::new(&hook_path)
      .args(&args)
      .env("DOGMA_VERSION", version)
      .env("DOGMA_ENV", env)
      .env("DOGMA_HOSTS", hosts)
      .env("DOGMA_DEPLOYED_IPS", deployed_ips)
      .status()
      .with_context(|| format!("failed to run hook: {hook}"))?;
    if !status.success() {
      bail!("{hook_name} hook failed: {hook}");
    }
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Upfront dependency check
// ---------------------------------------------------------------------------

fn check_all_deps() -> Result<()> {
  check_dep("ssh-keyscan", "install openssh")?;
  check_dep(
    "ssh-to-age",
    "install ssh-to-age from https://github.com/Mic92/ssh-to-age",
  )?;
  check_dep("sops", "install sops from https://github.com/getsops/sops")?;
  check_dep(
    "nixos-rebuild",
    "install nixos-rebuild (available on NixOS or via nixpkgs)",
  )?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Whether an env's infra cache can be reused as-is. A cache is usable only if
/// it parses and has at least one unit with a non-empty output object. A missing
/// file, unparseable JSON, or a skeleton like `{"hetzner":{},"mailgun":{}}`
/// (written before the env's infra was applied) is treated as needing a refresh.
fn cache_is_usable(cache_file: &Path) -> bool {
  let Ok(raw) = std::fs::read_to_string(cache_file) else {
    return false;
  };
  let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) else {
    return false;
  };
  json.as_object().is_some_and(|units| {
    units
      .values()
      .any(|u| u.as_object().is_some_and(|o| !o.is_empty()))
  })
}

fn config_needs_infra(config: &DogmaConfig, env: &str) -> bool {
  for machine in config.machines.values() {
    if let IpField::PerEnv(map) = &machine.ip {
      if let Some(IpEntry::FromInfra { .. }) = map.get(env) {
        return true;
      }
    }
  }
  for fields in config.secrets.values() {
    for leaf in fields.values() {
      if matches!(leaf, SecretLeaf::FromInfra { .. }) {
        return true;
      }
    }
  }
  false
}

fn encrypt_secrets(
  config: &DogmaConfig,
  repo_root: &Path,
  all_envs: &[String],
  _active_env: &str,
  env_creds: &[(String, Vec<(String, String)>)],
) -> Result<()> {
  check_dep("sops", "install sops from https://github.com/getsops/sops")?;

  let nix_secrets = config.nix.secrets.trim_start_matches("./");
  let nix_path = config.nix.path.trim_start_matches("./");
  let sops_config = repo_root.join(format!("{nix_path}/.sops.yaml"));

  for env in all_envs {
    let infra_creds = lookup_creds(env_creds, env);
    for (host_name, machine) in &config.machines {
      for group in &machine.secrets {
        if let Some(fields) = config.secrets.get(group) {
          let mut tmp = tempfile::NamedTempFile::new()?;

          for (field, leaf) in fields {
            let value = match leaf {
              SecretLeaf::FromVault { vault_ref, .. } => {
                vault::read(config, env, vault_ref)?
              }
              SecretLeaf::FromInfra { unit, output, .. } => {
                infra_output::read_cached(
                  config,
                  repo_root,
                  env,
                  unit,
                  output,
                  infra_creds,
                )?
              }
            };
            use std::io::Write;
            writeln!(tmp, "{field}: {}", serde_json::to_string(&value)?)?;
          }

          let out_dir =
            repo_root.join(format!("{nix_secrets}/{env}/{host_name}"));
          std::fs::create_dir_all(&out_dir)?;
          let out_file = out_dir.join(format!("{group}.yaml"));

          log_info!(
            "deploy encrypting {host_name}/{env}/{group} → {}",
            out_file.display()
          );

          let status = Command::new("sops")
            .args([
              "--config",
              &sops_config.to_string_lossy(),
              "--encrypt",
              "--input-type",
              "yaml",
              "--output-type",
              "yaml",
              "--filename-override",
              &out_file.to_string_lossy(),
              tmp.path().to_str().unwrap(),
            ])
            .stdout(std::fs::File::create(&out_file)?)
            .status()
            .context("failed to run sops")?;

          if !status.success() {
            bail!("sops encryption failed for {host_name}/{env}/{group}");
          }
        }
      }
    }
  }
  Ok(())
}

fn verify_secrets_committed(
  config: &DogmaConfig,
  repo_root: &Path,
  env: &str,
) -> Result<()> {
  let nix_secrets = config.nix.secrets.trim_start_matches("./");
  let repo = git::open(repo_root)?;

  for (host_name, machine) in &config.machines {
    for group in &machine.secrets {
      let secret_file =
        repo_root.join(format!("{nix_secrets}/{env}/{host_name}/{group}.yaml"));
      if !secret_file.exists() {
        bail!(
          "secret not committed for {env}: {}\nRun: dogma deploy {env} --new",
          secret_file.display()
        );
      }
      let rel = secret_file.strip_prefix(repo_root)?;
      let index = repo.index()?;
      if index.get_path(rel, 0).is_none() {
        bail!(
          "secret not committed for {env}: {}\nRun: dogma deploy {env} --new",
          secret_file.display()
        );
      }
    }
  }
  log_info!("deploy all secrets committed — ok");
  Ok(())
}

fn deploy_host(
  config: &DogmaConfig,
  repo_root: &Path,
  host: &str,
  env: &str,
  infra_creds: &[(String, String)],
) -> Result<String> {
  let machine = config.machines.get(host).unwrap();

  let ip_entry = match &machine.ip {
    IpField::PerEnv(m) => m
      .get(env)
      .ok_or_else(|| anyhow::anyhow!("no IP defined for {host}/{env}"))?,
    IpField::Shorthand(e) => e,
  };

  let host_ip = match ip_entry {
    IpEntry::Static(ip) => ip.clone(),
    IpEntry::FromInfra { unit, output, .. } => infra_output::read_cached(
      config,
      repo_root,
      env,
      unit,
      output,
      infra_creds,
    )?,
  };

  log_info!("deploy {host}/{env} → {host_ip}");

  let host_user = &machine.user;
  let hostname = machine.hostname.get(env, host);
  let nix_path = config.nix.path.trim_start_matches("./");
  let flake_path = repo_root.join(nix_path);

  let sudo_flag = if Command::new("ssh")
    .args([&format!("{host_user}@{host_ip}"), "sudo -n true"])
    .output()
    .map(|o| o.status.success())
    .unwrap_or(false)
  {
    log_dim!("deploy passwordless sudo available");
    "--sudo"
  } else {
    log_info!("deploy will prompt for sudo password");
    "--ask-sudo-password"
  };

  match &config.deploy.strategy {
    DeployStrategy::NixosRebuild => {
      check_dep(
        "nixos-rebuild",
        "install nixos-rebuild (available on NixOS or via nixpkgs)",
      )?;

      log_info!(
                "deploy nixos-rebuild switch --flake {}#{hostname} --target-host {host_user}@{host_ip} {sudo_flag}",
                flake_path.display()
            );

      let status = Command::new("nixos-rebuild")
        .args([
          "switch",
          "--flake",
          &format!("{}#{hostname}", flake_path.display()),
          "--target-host",
          &format!("{host_user}@{host_ip}"),
          sudo_flag,
        ])
        .status()
        .context("failed to run nixos-rebuild")?;

      if !status.success() {
        bail!(
          "nixos-rebuild failed for {host} (exit {})",
          status.code().unwrap_or(-1)
        );
      }
    }
  }

  log_info!("deploy done: {env}/{host}");
  Ok(format!("{host_user}@{host_ip}"))
}

fn release_tag(
  repo: &git2::Repository,
  version: &str,
  env: &str,
  deployed_targets: &[String],
  is_new: bool,
) -> Result<()> {
  let version_suffix = version.strip_prefix("deploy/").unwrap_or(version);
  let deployed_tag = format!("deployed-{env}-{version_suffix}");

  let gen_summary = fetch_nix_generations(deployed_targets);
  let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
  let tag_msg = format!(
    "env={env}{} deployed-at={timestamp}",
    if gen_summary.is_empty() {
      String::new()
    } else {
      format!(" {gen_summary}")
    }
  );

  if is_new {
    // Only --new creates a commit (on a branch) + the version tag. Promotion
    // modes run on a detached HEAD at an existing deploy/* tag — the commit and
    // version tag already exist and were pushed by the original --new, so there
    // is nothing to push here beyond the deployed-* marker below.
    git::push_commits(repo)?;

    if git::tag_exists(repo, version)? {
      bail!("tag '{version}' already exists");
    }
    log_info!("deploy creating annotated tag {version}");
    git::create_annotated_tag(repo, version, &tag_msg)?;
    git::push_tag(repo, version)?;
  }

  if git::tag_exists(repo, &deployed_tag)? {
    log_dim!("deploy tag '{deployed_tag}' already exists — skipping");
  } else {
    log_info!("deploy creating tag {deployed_tag}");
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

fn lookup_creds<'a>(
  env_creds: &'a [(String, Vec<(String, String)>)],
  env: &str,
) -> &'a [(String, String)] {
  env_creds
    .iter()
    .find(|(e, _)| e == env)
    .map(|(_, c)| c.as_slice())
    .unwrap_or(&[])
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
        log_info!("deploy restored branch: {}", self.original_ref);
      }
    }
  }
}
