use anyhow::{bail, Context, Result};
use git2::Repository;
use std::path::Path;

pub fn open(repo_root: &Path) -> Result<Repository> {
  Repository::open(repo_root).context("failed to open git repository")
}

// ---------------------------------------------------------------------------
// Dirty tree check
// ---------------------------------------------------------------------------

pub struct DirtyFile {
  /// Single-letter status, porcelain-style: M, D, A (new), R (renamed), ? else.
  pub status: char,
  pub path: String,
}

pub struct DirtyFiles {
  pub files: Vec<DirtyFile>,
}

impl DirtyFiles {
  pub fn is_empty(&self) -> bool {
    self.files.is_empty()
  }
}

/// Maps a git2 status to a single porcelain-style letter. Index (staged) state
/// takes precedence over worktree state, matching `git status` column ordering.
fn status_char(s: git2::Status) -> char {
  use git2::Status;
  if s.intersects(Status::INDEX_NEW) {
    'A'
  } else if s.intersects(Status::INDEX_RENAMED) {
    'R'
  } else if s.intersects(Status::INDEX_DELETED | Status::WT_DELETED) {
    'D'
  } else if s.intersects(Status::INDEX_MODIFIED | Status::WT_MODIFIED) {
    'M'
  } else {
    '?'
  }
}

/// Returns paths with a pending change relative to HEAD. When
/// `include_untracked` is false only tracked modifications are reported
/// (the usual "dirty tree" notion); when true, untracked files are included
/// too — used to guarantee the working tree exactly matches the committed
/// code before recording HEAD as the applied state. Git-ignored files are
/// always excluded.
pub fn dirty_files(
  repo: &Repository,
  include_untracked: bool,
) -> Result<DirtyFiles> {
  let mut opts = git2::StatusOptions::new();
  opts
    .include_untracked(include_untracked)
    .include_ignored(false)
    .exclude_submodules(true);

  let statuses = repo.statuses(Some(&mut opts))?;
  let files = statuses
    .iter()
    .filter(|e| !e.status().is_empty())
    .filter_map(|e| {
      e.path().map(|p| DirtyFile {
        status: status_char(e.status()),
        path: p.to_string(),
      })
    })
    .collect();

  Ok(DirtyFiles { files })
}

// ---------------------------------------------------------------------------
// Commit
// ---------------------------------------------------------------------------

pub fn commit_all(repo: &Repository, message: &str) -> Result<()> {
  let mut index = repo.index()?;
  index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
  index.write()?;

  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  let sig = repo.signature()?;
  let parent = repo.head()?.peel_to_commit()?;

  repo.commit(Some("HEAD"), &sig, &sig, message, &tree, &[&parent])?;
  Ok(())
}

/// Fold all working-tree changes into the current HEAD commit, keeping its
/// original message and parents (equivalent to `git commit --amend --no-edit -a`).
pub fn amend_all(repo: &Repository) -> Result<()> {
  let mut index = repo.index()?;
  index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
  index.write()?;

  let oid = index.write_tree()?;
  let tree = repo.find_tree(oid)?;
  let head = repo.head()?.peel_to_commit()?;
  let message = head.message().unwrap_or("").to_string();

  head.amend(Some("HEAD"), None, None, None, Some(&message), Some(&tree))?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Version (CalVer: deploy/vYY.MM.NNNN)
// ---------------------------------------------------------------------------

pub fn next_version(repo: &Repository) -> Result<String> {
  let month = chrono::Utc::now().format("%y.%m").to_string();
  let prefix = format!("deploy/v{}.", month);

  let mut max_n: u32 = 0;
  repo
    .tag_names(Some(&format!("{}*", prefix)))?
    .iter()
    .flatten()
    .for_each(|tag| {
      if let Some(suffix) = tag.strip_prefix(&prefix) {
        if let Ok(n) = suffix.parse::<u32>() {
          if n > max_n {
            max_n = n;
          }
        }
      }
    });

  Ok(format!("deploy/v{}.{:04}", month, max_n + 1))
}

// ---------------------------------------------------------------------------
// Tag operations
// ---------------------------------------------------------------------------

pub fn list_deploy_tags(repo: &Repository) -> Result<Vec<String>> {
  let mut tags: Vec<String> = repo
    .tag_names(Some("deploy/*"))?
    .iter()
    .flatten()
    .map(str::to_string)
    .collect();

  // Sort newest first by semver-like comparison
  tags.sort_by(|a, b| b.cmp(a));
  Ok(tags)
}

pub fn tag_exists(repo: &Repository, name: &str) -> Result<bool> {
  Ok(
    repo
      .tag_names(Some(name))?
      .iter()
      .flatten()
      .any(|t| t == name),
  )
}

pub fn create_annotated_tag(
  repo: &Repository,
  name: &str,
  message: &str,
) -> Result<()> {
  let head = repo.head()?.peel_to_commit()?;
  let sig = repo.signature()?;
  repo
    .tag(name, head.as_object(), &sig, message, false)
    .with_context(|| format!("failed to create tag '{name}'"))?;
  Ok(())
}

pub fn create_lightweight_tag(repo: &Repository, name: &str) -> Result<()> {
  let head = repo.head()?.peel_to_commit()?;
  repo
    .tag_lightweight(name, head.as_object(), false)
    .with_context(|| format!("failed to create tag '{name}'"))?;
  Ok(())
}

/// Create or move a lightweight tag to current HEAD, overwriting any existing
/// tag of the same name. Used for moving "pointer" tags (e.g. last-applied
/// markers) rather than immutable release tags.
pub fn set_moving_tag(repo: &Repository, name: &str) -> Result<()> {
  let head = repo.head()?.peel_to_commit()?;
  repo
    .tag_lightweight(name, head.as_object(), true)
    .with_context(|| format!("failed to set tag '{name}'"))?;
  Ok(())
}

// ---------------------------------------------------------------------------
// Checkout / restore
// ---------------------------------------------------------------------------

/// The abbreviated (7-char) hex sha of the current HEAD commit.
pub fn head_short_sha(repo: &Repository) -> Result<String> {
  let sha = repo.head()?.peel_to_commit()?.id().to_string();
  Ok(sha[..sha.len().min(7)].to_string())
}

pub fn current_ref(repo: &Repository) -> Result<String> {
  match repo.head() {
    Ok(head) if head.is_branch() => {
      Ok(head.shorthand().unwrap_or("HEAD").to_string())
    }
    // detached HEAD — use short sha
    _ => head_short_sha(repo),
  }
}

pub fn checkout_tag(repo: &Repository, tag_name: &str) -> Result<()> {
  let obj = repo
    .revparse_single(tag_name)
    .with_context(|| format!("tag '{tag_name}' not found"))?;

  repo
    .checkout_tree(&obj, None)
    .with_context(|| format!("failed to checkout '{tag_name}'"))?;

  repo
    .set_head_detached(obj.peel_to_commit()?.id())
    .with_context(|| format!("failed to detach HEAD at '{tag_name}'"))?;

  Ok(())
}

pub fn restore_ref(repo: &Repository, ref_name: &str) -> Result<()> {
  // Try as branch first, fall back to detached commit
  let branch_ref = format!("refs/heads/{ref_name}");
  if repo.find_reference(&branch_ref).is_ok() {
    repo.set_head(&branch_ref)?;
  } else {
    let obj = repo.revparse_single(ref_name)?;
    repo.set_head_detached(obj.peel_to_commit()?.id())?;
  }

  repo
    .checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
    .context("failed to restore HEAD")?;

  Ok(())
}

// ---------------------------------------------------------------------------
// Push
// ---------------------------------------------------------------------------

pub fn push_commits(repo: &Repository) -> Result<()> {
  // Push HEAD to its branch by name. A bare "HEAD" refspec is ambiguous on a
  // normal remote (`git push origin HEAD` can't tell which branch to update)
  // and is rejected; `HEAD:refs/heads/<branch>` is explicit and works across
  // gcrypt/ssh/https alike.
  let head = repo.head()?;
  if !head.is_branch() {
    bail!(
      "HEAD is detached — refusing to push commits without a branch to push to"
    );
  }
  let branch = head.shorthand().ok_or_else(|| {
    anyhow::anyhow!("could not determine current branch name")
  })?;
  push_refspec(repo, &format!("HEAD:refs/heads/{branch}"))
}

pub fn push_tag(repo: &Repository, tag_name: &str) -> Result<()> {
  push_refspec(repo, &format!("refs/tags/{tag_name}"))
}

/// Force-push a tag to every remote, allowing it to move (overwrite). Used for
/// moving "pointer" tags whose target changes over time.
pub fn push_tag_force(repo: &Repository, tag_name: &str) -> Result<()> {
  push_refspec(repo, &format!("+refs/tags/{tag_name}"))
}

/// Push a refspec to every remote.
///
/// Shells out to the system `git` rather than using libgit2: libgit2 only
/// supports its built-in transports and cannot push remote-helper URLs (e.g.
/// `gcrypt::rsync://...`) or SSH unless built with libssh2. The `git` binary
/// honours remote helpers and the user's ssh-agent. Failures are collected so
/// one unreachable remote still reports the others.
fn push_refspec(repo: &Repository, refspec: &str) -> Result<()> {
  let workdir = repo
    .workdir()
    .ok_or_else(|| anyhow::anyhow!("repository has no working directory"))?;

  let mut failures = Vec::new();
  for remote_name in repo.remotes()?.iter().flatten() {
    let status = std::process::Command::new("git")
      .current_dir(workdir)
      .args(["push", remote_name, refspec])
      .status();
    match status {
      Ok(s) if s.success() => {}
      Ok(s) => failures
        .push(format!("{remote_name} (exit {})", s.code().unwrap_or(-1))),
      Err(e) => failures.push(format!("{remote_name} ({e})")),
    }
  }

  if !failures.is_empty() {
    bail!("failed to push '{refspec}' to: {}", failures.join(", "));
  }
  Ok(())
}

// ---------------------------------------------------------------------------
// Suggest commit message (heuristic, mirrors suggest-commit-msg.sh)
// ---------------------------------------------------------------------------

pub fn suggest_commit_msg(repo: &Repository) -> Option<String> {
  let mut opts = git2::DiffOptions::new();
  let diff = repo.diff_index_to_workdir(None, Some(&mut opts)).ok()?;

  let mut files: Vec<String> = Vec::new();
  diff
    .foreach(
      &mut |delta, _| {
        if let Some(p) = delta.new_file().path() {
          files.push(p.to_string_lossy().to_string());
        }
        true
      },
      None,
      None,
      None,
    )
    .ok()?;

  if files.is_empty() {
    return None;
  }

  let commit_type = infer_type(&files);
  let scope = infer_scope(&files);
  let desc = files
    .iter()
    .map(|f| {
      std::path::Path::new(f)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
    })
    .take(3)
    .collect::<Vec<_>>()
    .join(", ");

  let extra = if files.len() > 3 {
    format!(" +{} more", files.len() - 3)
  } else {
    String::new()
  };

  let scope_str = scope.map(|s| format!("({s})")).unwrap_or_default();
  Some(format!("{commit_type}{scope_str}: {desc}{extra}"))
}

fn infer_type(files: &[String]) -> &'static str {
  for f in files {
    if f.contains("test") || f.contains("spec") {
      return "test";
    }
    if f.ends_with(".md") || f.starts_with("docs/") {
      return "docs";
    }
    if f.starts_with(".github/") || f.contains("ci") {
      return "ci";
    }
    if f.starts_with("nix/") || f == "flake.nix" || f == "Makefile" {
      return "build";
    }
    if f.ends_with(".toml") || f.ends_with(".lock") || f == "dogma.yml" {
      return "chore";
    }
  }
  "fix"
}

fn infer_scope(files: &[String]) -> Option<String> {
  let dirs: Vec<&str> = files
    .iter()
    .filter_map(|f| f.split('/').next())
    .filter(|d| !d.contains('.'))
    .collect();

  let mut counts = std::collections::HashMap::new();
  for d in &dirs {
    *counts.entry(*d).or_insert(0u32) += 1;
  }

  counts
    .into_iter()
    .max_by_key(|(_, c)| *c)
    .map(|(d, _)| d.to_string())
}
