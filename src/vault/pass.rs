use anyhow::{bail, Context, Result};
use std::process::Command;

use crate::config::VaultEntry;
use crate::error::check_dep;

pub fn read(entry: &VaultEntry, env: &str, vault_key: &str) -> Result<String> {
    check_dep("pass", "install pass from https://www.passwordstore.org")?;

    let pass_map = entry.pass.as_ref().ok_or_else(|| {
        anyhow::anyhow!("vault key '{vault_key}': no pass path defined")
    })?;

    let path = pass_map.get(env).ok_or_else(|| {
        anyhow::anyhow!("vault key '{vault_key}': no pass path defined for env '{env}'")
    })?;

    let out = Command::new("pass")
        .arg(path)
        .output()
        .context("failed to run 'pass'")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("pass failed for path '{path}': {stderr}");
    }

    let value = String::from_utf8(out.stdout)
        .context("pass output is not valid UTF-8")?
        .trim_end_matches('\n')
        .to_string();

    if value.is_empty() {
        bail!("pass returned empty value for path '{path}'");
    }

    Ok(value)
}
