use util::command::new_command;

pub const TOKEN_ENV_VAR: &str = "GITLAB_TOKEN";
pub const HOST_ENV_VAR: &str = "GITLAB_HOST";

/// Finds a personal access token for `host`: the environment variables `glab`
/// also reads, then the token `glab auth login` stored for that host.
pub async fn lookup_token(host: &str) -> Option<String> {
    let host_env = std::env::var(HOST_ENV_VAR).ok();
    if environment_token_applies(host, host_env.as_deref())
        && let Some(token) = std::env::var(TOKEN_ENV_VAR)
            .ok()
            .and_then(|token| non_empty(&token))
    {
        return Some(token);
    }

    let output = new_command("glab")
        .args(["config", "get", "token", "--host", host])
        .output()
        .await
        .inspect_err(|error| log::debug!("glab is not available: {error}"))
        .ok()?;
    if !output.status.success() {
        log::debug!(
            "glab config get token failed for {host}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    non_empty(&String::from_utf8_lossy(&output.stdout))
}

/// A repository remote decides which host Zed talks to, so the environment
/// token is sent only to the host it was issued for: `GITLAB_HOST`, or
/// gitlab.com when that is unset, as `glab` does.
fn environment_token_applies(host: &str, host_env: Option<&str>) -> bool {
    let token_host = host_env
        .map(|value| {
            let value = value.trim();
            let without_scheme = value.split_once("://").map_or(value, |(_, rest)| rest);
            without_scheme
                .split(['/', ':'])
                .next()
                .unwrap_or_default()
                .to_string()
        })
        .unwrap_or_else(|| "gitlab.com".to_string());
    token_host.eq_ignore_ascii_case(host)
}

fn non_empty(token: &str) -> Option<String> {
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_token_scope() {
        assert!(environment_token_applies("gitlab.com", None));
        assert!(!environment_token_applies("gitlab.example.com", None));
        assert!(environment_token_applies(
            "gitlab.example.com",
            Some("https://gitlab.example.com")
        ));
        assert!(environment_token_applies("gitlab.example.com", Some("gitlab.example.com")));
        assert!(!environment_token_applies("gitlab.com", Some("gitlab.example.com")));
        assert!(!environment_token_applies(
            "gitlab.attacker.example",
            Some("gitlab.example.com")
        ));
    }
}
