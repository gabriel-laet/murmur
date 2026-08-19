//! Secrets are first-class on the bus as *references*, never as values.
//!
//! A ref like `secret://infisical/<projectId>/<env>/[folders/]<NAME>` is
//! ordinary text: it travels through inboxes and the log safely because it
//! grants nothing. Resolution happens at the receiving edge through the
//! receiver's own backend credentials (e.g. `INFISICAL_TOKEN`), so access
//! control, encryption, rotation, and audit stay in the secret manager.
//! Murmur never stores a secret byte and hooks never auto-resolve.

use anyhow::{bail, Context, Result};

pub fn contains_ref(text: &str) -> bool {
    text.contains("secret://")
}

pub fn resolve(reference: &str) -> Result<String> {
    let rest = reference.strip_prefix("secret://").with_context(|| {
        format!(
            "not a secret reference (expected secret://...): {}",
            reference
        )
    })?;
    let (backend, locator) = rest
        .split_once('/')
        .context("malformed secret reference: expected secret://<backend>/<locator>")?;
    match backend {
        "env" => std::env::var(locator)
            .with_context(|| format!("environment variable '{}' is not set", locator)),
        "infisical" => resolve_infisical(locator),
        other => bail!(
            "unknown secret backend '{}' (supported: infisical, env)",
            other
        ),
    }
}

struct InfisicalRef {
    project: String,
    env: String,
    path: String,
    name: String,
}

/// `<projectId>/<env>/[folders...]/<NAME>`
fn parse_infisical(locator: &str) -> Result<InfisicalRef> {
    let parts: Vec<&str> = locator.split('/').filter(|s| !s.is_empty()).collect();
    if parts.len() < 3 {
        bail!(
            "infisical ref must be secret://infisical/<projectId>/<env>/[folders/]<NAME>, got '{}'",
            locator
        );
    }
    let path = if parts.len() > 3 {
        format!("/{}", parts[2..parts.len() - 1].join("/"))
    } else {
        "/".to_string()
    };
    Ok(InfisicalRef {
        project: parts[0].to_string(),
        env: parts[1].to_string(),
        path,
        name: parts[parts.len() - 1].to_string(),
    })
}

fn resolve_infisical(locator: &str) -> Result<String> {
    let r = parse_infisical(locator)?;
    let out = std::process::Command::new("infisical")
        .args([
            "secrets",
            "get",
            &r.name,
            "--projectId",
            &r.project,
            "--env",
            &r.env,
            "--path",
            &r.path,
            "--plain",
            "--silent",
        ])
        .output()
        .context("failed to run the infisical CLI — is it installed and on PATH?")?;
    if !out.status.success() {
        bail!(
            "infisical refused '{}' (your identity may lack access — that's the point of refs): {}",
            r.name,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let value = String::from_utf8_lossy(&out.stdout);
    Ok(value.strip_suffix('\n').unwrap_or(&value).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_infisical_ref() {
        let r = parse_infisical("proj-123/dev/DATABASE_URL").unwrap();
        assert_eq!(r.project, "proj-123");
        assert_eq!(r.env, "dev");
        assert_eq!(r.path, "/");
        assert_eq!(r.name, "DATABASE_URL");
    }

    #[test]
    fn parses_nested_folders() {
        let r = parse_infisical("proj-123/prod/backend/payments/STRIPE_KEY").unwrap();
        assert_eq!(r.path, "/backend/payments");
        assert_eq!(r.name, "STRIPE_KEY");
    }

    #[test]
    fn rejects_short_refs() {
        assert!(parse_infisical("proj-123/dev").is_err());
    }

    #[test]
    fn rejects_unknown_backends() {
        let err = resolve("secret://vault/whatever/KEY").unwrap_err();
        assert!(err.to_string().contains("unknown secret backend"));
    }

    #[test]
    fn env_backend_resolves() {
        std::env::set_var("MURMUR_UNIT_TEST_SECRET", "s3cr3t");
        assert_eq!(
            resolve("secret://env/MURMUR_UNIT_TEST_SECRET").unwrap(),
            "s3cr3t"
        );
    }
}
