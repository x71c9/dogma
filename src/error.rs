pub fn check_dep(tool: &str, install_hint: &str) -> anyhow::Result<()> {
    if which::which(tool).is_err() {
        anyhow::bail!(
            "'{tool}' is required but not found on PATH — {install_hint}"
        );
    }
    Ok(())
}
