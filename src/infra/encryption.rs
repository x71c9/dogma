use anyhow::{bail, Result};

/// Minimum passphrase length accepted by OpenTofu's pbkdf2 key provider.
const MIN_PASSPHRASE_LEN: usize = 16;

/// Build the `TF_ENCRYPTION` value (HCL) for OpenTofu state encryption:
/// a pbkdf2 key provider over `passphrase` and an aes_gcm method applied to
/// both state and plan files.
///
/// `migrate = false` sets `enforced = true`, so no unencrypted data is ever
/// read or written. `migrate = true` adds an `unencrypted` fallback instead,
/// so existing plaintext states can be read once and written back encrypted
/// (OpenTofu forbids the fallback while `enforced` is set).
pub fn tf_encryption(passphrase: &str, migrate: bool) -> Result<String> {
  if passphrase.chars().count() < MIN_PASSPHRASE_LEN {
    bail!(
      "infra.state_encryption.passphrase: must be at least \
       {MIN_PASSPHRASE_LEN} characters"
    );
  }

  let mut hcl = format!(
    "key_provider \"pbkdf2\" \"dogma\" {{\n  passphrase = \"{}\"\n}}\n\
     method \"aes_gcm\" \"dogma\" {{\n  keys = key_provider.pbkdf2.dogma\n}}\n",
    hcl_escape(passphrase)
  );
  if migrate {
    hcl.push_str("method \"unencrypted\" \"migrate\" {}\n");
  }
  for target in ["state", "plan"] {
    hcl.push_str(&format!("{target} {{\n  method = method.aes_gcm.dogma\n"));
    if migrate {
      hcl.push_str(
        "  fallback {\n    method = method.unencrypted.migrate\n  }\n",
      );
    } else {
      hcl.push_str("  enforced = true\n");
    }
    hcl.push_str("}\n");
  }
  Ok(hcl)
}

/// Escape a value for an HCL quoted string: backslashes, double quotes,
/// newlines and the `${` / `%{` template sequences.
fn hcl_escape(value: &str) -> String {
  value
    .replace('\\', r"\\")
    .replace('"', r#"\""#)
    .replace('\n', r"\n")
    .replace('\r', r"\r")
    .replace("${", "$${")
    .replace("%{", "%%{")
}

#[cfg(test)]
mod tests {
  use super::{hcl_escape, tf_encryption};

  const PASS: &str = "0123456789abcdefXYZ";

  #[test]
  fn enforced_without_migrate() {
    let hcl = tf_encryption(PASS, false).unwrap();
    assert!(hcl.contains(&format!("passphrase = \"{PASS}\"")));
    assert_eq!(hcl.matches("enforced = true").count(), 2);
    assert!(!hcl.contains("unencrypted"));
    assert!(!hcl.contains("fallback"));
    assert!(hcl.contains("state {"));
    assert!(hcl.contains("plan {"));
  }

  #[test]
  fn fallback_with_migrate() {
    let hcl = tf_encryption(PASS, true).unwrap();
    assert!(hcl.contains("method \"unencrypted\" \"migrate\" {}"));
    assert_eq!(
      hcl.matches("method = method.unencrypted.migrate").count(),
      2
    );
    assert!(!hcl.contains("enforced"));
  }

  #[test]
  fn short_passphrase_rejected() {
    let err = tf_encryption("too-short", false).unwrap_err().to_string();
    assert!(err.contains("at least 16"));
  }

  #[test]
  fn escapes_hcl_specials() {
    assert_eq!(hcl_escape(r#"a"b\c${x}%{y}"#), r#"a\"b\\c$${x}%%{y}"#);
    assert_eq!(hcl_escape("a\nb"), r"a\nb");
  }
}
