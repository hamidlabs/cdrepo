use anyhow::{Context, Result};
use std::process::Command;
use tracing::debug;

/// Resolve a GitHub token using the standard fallback chain.
/// 1. GH_TOKEN env var
/// 2. GITHUB_TOKEN env var
/// 3. `gh auth token` subprocess (handles keyring)
/// 4. ~/.config/gh/hosts.yml plaintext fallback
pub fn get_token() -> Result<String> {
    // 1. Environment variables
    if let Ok(t) = std::env::var("GH_TOKEN") {
        if !t.is_empty() {
            debug!("auth: using GH_TOKEN env var");
            return Ok(t);
        }
    }
    if let Ok(t) = std::env::var("GITHUB_TOKEN") {
        if !t.is_empty() {
            debug!("auth: using GITHUB_TOKEN env var");
            return Ok(t);
        }
    }

    // 2. gh CLI (handles keyring, oauth, etc.)
    if let Ok(output) = Command::new("gh").args(["auth", "token"]).output() {
        if output.status.success() {
            let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !token.is_empty() {
                debug!("auth: using gh auth token");
                return Ok(token);
            }
        }
    }

    // 3. Plaintext hosts.yml fallback
    if let Some(config_dir) = std::env::var_os("GH_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("gh")))
    {
        let hosts_file = config_dir.join("hosts.yml");
        if hosts_file.exists() {
            let content = std::fs::read_to_string(&hosts_file)
                .context("failed to read gh hosts.yml")?;
            // Simple YAML parsing — look for oauth_token under github.com
            if let Some(token) = parse_hosts_yml_token(&content) {
                debug!("auth: using hosts.yml token");
                return Ok(token);
            }
        }
    }

    anyhow::bail!(
        "No GitHub token found.\n\
         Please authenticate with: gh auth login\n\
         Or set GH_TOKEN environment variable."
    )
}

/// Minimal parser for gh hosts.yml to extract github.com oauth_token.
fn parse_hosts_yml_token(content: &str) -> Option<String> {
    let mut in_github = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("github.com:") {
            in_github = true;
            continue;
        }
        if in_github {
            if !line.starts_with(' ') && !line.starts_with('\t') {
                // Left a nested block
                break;
            }
            if let Some(rest) = trimmed.strip_prefix("oauth_token:") {
                let token = rest.trim().trim_matches('"').trim_matches('\'');
                if !token.is_empty() {
                    return Some(token.to_string());
                }
            }
        }
    }
    None
}

/// Check if the user is authenticated and return the username.
pub fn whoami() -> Result<String> {
    let _ = get_token()?; // Ensure we have a token
    let output = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("failed to run gh api user")?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        anyhow::bail!("failed to get GitHub username — are you logged in?")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hosts_yml() {
        let yml = r#"
github.com:
    user: testuser
    oauth_token: ghp_test123
    git_protocol: https
"#;
        assert_eq!(
            parse_hosts_yml_token(yml),
            Some("ghp_test123".to_string())
        );
    }

    #[test]
    fn test_parse_hosts_yml_no_token() {
        let yml = "gitlab.com:\n    oauth_token: xxx\n";
        assert_eq!(parse_hosts_yml_token(yml), None);
    }
}
