use anyhow::{Context, Result};
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

/// Returns tracked modified/staged files (excludes untracked).
pub fn dirty_files(repo: &Repository) -> Result<DirtyFiles> {
  let mut opts = git2::StatusOptions::new();
  opts
    .include_untracked(false)
    .include_ignored(false)
    .exclude_submodules(true);

  let statuses = repo.statuses(Some(&mut opts))?;
  let files = statuses
    .iter()
    .filter(|e| {
      let s = e.status();
      s.intersects(
        git2::Status::INDEX_NEW
          | git2::Status::INDEX_MODIFIED
          | git2::Status::INDEX_DELETED
          | git2::Status::INDEX_RENAMED
          | git2::Status::WT_MODIFIED
          | git2::Status::WT_DELETED,
      )
    })
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

// ---------------------------------------------------------------------------
// Checkout / restore
// ---------------------------------------------------------------------------

pub fn current_ref(repo: &Repository) -> Result<String> {
  match repo.head() {
    Ok(head) if head.is_branch() => {
      Ok(head.shorthand().unwrap_or("HEAD").to_string())
    }
    _ => {
      // detached HEAD — use short sha
      let oid = repo.head()?.peel_to_commit()?.id();
      Ok(oid.to_string()[..7].to_string())
    }
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
  push_refspec(repo, "HEAD")
}

pub fn push_tag(repo: &Repository, tag_name: &str) -> Result<()> {
  push_refspec(repo, &format!("refs/tags/{tag_name}"))
}

fn push_refspec(repo: &Repository, refspec: &str) -> Result<()> {
  for remote_name in repo.remotes()?.iter().flatten() {
    let mut remote = repo.find_remote(remote_name)?;
    remote.push(&[refspec], None).with_context(|| {
      format!("failed to push '{refspec}' to remote '{remote_name}'")
    })?;
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
