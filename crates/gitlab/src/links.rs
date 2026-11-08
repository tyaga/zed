use std::sync::Arc;

use anyhow::{Context as _, Result};
use git::{GitHostingProvider, GitHostingProviderRegistry};
use git_hosting_providers::Gitlab;
use sha1::{Digest as _, Sha1};
pub use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitlabProject {
    /// Web root of the instance, including any sub-path it is served under.
    pub instance: Url,
    /// Full project path with subgroups, e.g. `group/subgroup/project`.
    pub path: Arc<str>,
}

impl GitlabProject {
    pub fn host(&self) -> &str {
        self.instance.host_str().unwrap_or_default()
    }

    pub fn encoded_path(&self) -> String {
        urlencoding::encode(&self.path).into_owned()
    }

    /// Instance-level endpoint such as `user`.
    pub fn instance_api_url(&self, endpoint: &str) -> Result<Url> {
        let mut base = self.instance.clone();
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        base.join(&format!("api/v4/{endpoint}"))
            .with_context(|| format!("invalid GitLab API endpoint {endpoint}"))
    }

    /// Project-level endpoint such as `merge_requests`.
    pub fn api_url(&self, endpoint: &str) -> Result<Url> {
        self.instance_api_url(&format!("projects/{}/{endpoint}", self.encoded_path()))
    }
}

/// `configured_instances` are the base URLs of `git_hosting_providers`
/// entries with `provider: "gitlab"`; they may use any host name and may be
/// served under a sub-path.
pub fn gitlab_project_from_remote(
    registry: &GitHostingProviderRegistry,
    configured_instances: &[Url],
    remote_url: &str,
) -> Option<GitlabProject> {
    let configured = configured_instances.iter().find_map(|instance| {
        let provider = Gitlab::new("GitLab", instance.clone());
        Some((instance.clone(), provider.parse_remote_url(remote_url)?))
    });
    let registered = || {
        registry
            .list_hosting_providers()
            .into_iter()
            .filter(|provider| provider.name().starts_with("GitLab"))
            .find_map(|provider| {
                let parsed = provider.parse_remote_url(remote_url)?;
                Some((provider.base_url(), parsed))
            })
    };
    // Self-hosted instances are registered only for the `origin` remote of the
    // first scanned repository, so other remotes need the provider built here.
    let detected = || {
        let provider = Gitlab::from_remote_url(remote_url).ok()?;
        let parsed = provider.parse_remote_url(remote_url)?;
        Some((provider.base_url(), parsed))
    };
    let (instance, parsed) = configured.or_else(registered).or_else(detected)?;

    let full_path = format!("{}/{}", parsed.owner, parsed.repo);
    let instance_prefix = instance.path().trim_matches('/');
    let path = if instance_prefix.is_empty() {
        full_path.as_str()
    } else {
        full_path
            .strip_prefix(instance_prefix)
            .and_then(|rest| rest.strip_prefix('/'))
            .unwrap_or(full_path.as_str())
    };
    Some(GitlabProject {
        instance,
        path: path.into(),
    })
}

/// Link to a file, and optionally a line, on the Changes tab of a merge request.
/// `line` is `(old position, new position)`, 1-based, as in GitLab's line
/// codes: for an added line the old position is that of the next base line.
pub fn merge_request_file_url(
    merge_request_web_url: &Url,
    repo_relative_path: &str,
    line: Option<(u32, u32)>,
) -> Url {
    let path_hash: String = Sha1::digest(repo_relative_path.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let fragment = match line {
        None => path_hash,
        Some((old_line, new_line)) => format!("{path_hash}_{old_line}_{new_line}"),
    };
    let mut url = merge_request_web_url.clone();
    url.set_path(&format!("{}/diffs", url.path().trim_end_matches('/')));
    url.set_query(None);
    url.set_fragment(Some(&fragment));
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn registry() -> Arc<GitHostingProviderRegistry> {
        let registry = Arc::new(GitHostingProviderRegistry::new());
        registry.register_hosting_provider(Arc::new(Gitlab::public_instance()));
        registry
    }

    fn sha1_hex(text: &str) -> String {
        Sha1::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn merge_request_url() -> Url {
        Url::parse("https://gitlab.example.com/group/project/-/merge_requests/5")
            .expect("valid url")
    }

    #[test]
    fn gitlab_com_https() {
        let project = gitlab_project_from_remote(&registry(), &[], "https://gitlab.com/group/project.git")
            .expect("gitlab.com remote");
        assert_eq!(project.instance.as_str(), "https://gitlab.com/");
        assert_eq!(project.path.as_ref(), "group/project");
    }

    #[test]
    fn self_hosted_ssh_with_subgroups() {
        let project = gitlab_project_from_remote(
            &registry(),
            &[],
            "git@gitlab.example.com:group/subgroup/project.git",
        )
        .expect("self-hosted remote");
        assert_eq!(project.instance.as_str(), "https://gitlab.example.com/");
        assert_eq!(project.path.as_ref(), "group/subgroup/project");
        assert_eq!(project.encoded_path(), "group%2Fsubgroup%2Fproject");
        assert_eq!(project.host(), "gitlab.example.com");
    }

    #[test]
    fn ssh_with_port() {
        let project = gitlab_project_from_remote(
            &registry(),
            &[],
            "ssh://git@gitlab.example.com:2222/group/project.git",
        )
        .expect("ssh remote with port");
        assert_eq!(project.host(), "gitlab.example.com");
        assert_eq!(project.instance.as_str(), "https://gitlab.example.com/");
        assert_eq!(project.path.as_ref(), "group/project");
    }

    #[test]
    fn instance_sub_path() {
        let project = GitlabProject {
            instance: Url::parse("https://gitlab.example.com/gitlab/").expect("valid url"),
            path: "group/project".into(),
        };
        assert_eq!(
            project
                .api_url("merge_requests")
                .expect("api url")
                .as_str(),
            "https://gitlab.example.com/gitlab/api/v4/projects/group%2Fproject/merge_requests"
        );
    }

    #[test]
    fn github_remote_is_not_gitlab() {
        assert_eq!(
            gitlab_project_from_remote(&registry(), &[], "https://github.com/owner/repo.git"),
            None
        );
    }

    #[test]
    fn configured_instance_without_gitlab_in_host() {
        let instance = Url::parse("https://code.example.com/").expect("valid url");
        let project = gitlab_project_from_remote(
            &registry(),
            &[instance],
            "git@code.example.com:group/project.git",
        )
        .expect("configured instance");
        assert_eq!(project.instance.as_str(), "https://code.example.com/");
        assert_eq!(project.path.as_ref(), "group/project");
    }

    #[test]
    fn configured_instance_with_sub_path() {
        let instance = Url::parse("https://example.com/gitlab").expect("valid url");
        let project = gitlab_project_from_remote(
            &registry(),
            &[instance],
            "https://example.com/gitlab/group/subgroup/project.git",
        )
        .expect("configured instance");
        assert_eq!(project.path.as_ref(), "group/subgroup/project");
        assert_eq!(
            project.api_url("merge_requests").expect("api url").as_str(),
            "https://example.com/gitlab/api/v4/projects/group%2Fsubgroup%2Fproject/merge_requests"
        );
    }

    #[test]
    fn file_url_without_line() {
        let url = merge_request_file_url(&merge_request_url(), "src/main.rs", None);
        assert_eq!(
            url.as_str(),
            format!(
                "https://gitlab.example.com/group/project/-/merge_requests/5/diffs#{}",
                sha1_hex("src/main.rs")
            )
        );
    }

    #[test]
    fn file_url_added_line() {
        let url = merge_request_file_url(&merge_request_url(), "src/main.rs", Some((4, 12)));
        assert_eq!(url.fragment(), Some(format!("{}_4_12", sha1_hex("src/main.rs")).as_str()));
    }

    #[test]
    fn file_url_unchanged_line() {
        let url =
            merge_request_file_url(&merge_request_url(), "src/main.rs", Some((10, 12)));
        assert_eq!(
            url.fragment(),
            Some(format!("{}_10_12", sha1_hex("src/main.rs")).as_str())
        );
    }
}
