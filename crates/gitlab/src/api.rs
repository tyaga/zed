use std::sync::Arc;

use anyhow::anyhow;
use futures::AsyncReadExt as _;
use http_client::{AsyncBody, HttpClient, Method, Request, StatusCode};
use serde::{Deserialize, de::DeserializeOwned};
use url::Url;

use crate::GitlabProject;

#[derive(Debug)]
pub enum ApiError {
    Unauthorized,
    NotFound,
    Other(anyhow::Error),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(formatter, "GitLab rejected the token"),
            ApiError::NotFound => write!(formatter, "GitLab project not found"),
            ApiError::Other(error) => write!(formatter, "{error:#}"),
        }
    }
}

impl std::error::Error for ApiError {}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        ApiError::Other(error)
    }
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MergeRequestState {
    Opened,
    Merged,
    Closed,
    Locked,
}

impl MergeRequestState {
    fn as_query(&self) -> &'static str {
        match self {
            MergeRequestState::Opened => "opened",
            MergeRequestState::Merged => "merged",
            MergeRequestState::Closed => "closed",
            MergeRequestState::Locked => "locked",
        }
    }
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PipelineStatus {
    Created,
    WaitingForResource,
    Preparing,
    Pending,
    Running,
    Success,
    Failed,
    Canceled,
    Skipped,
    Manual,
    Scheduled,
    #[serde(other)]
    Unknown,
}

impl PipelineStatus {
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            PipelineStatus::Created
                | PipelineStatus::WaitingForResource
                | PipelineStatus::Preparing
                | PipelineStatus::Pending
                | PipelineStatus::Running
        )
    }

    pub fn label(&self) -> &'static str {
        match self {
            PipelineStatus::Created => "created",
            PipelineStatus::WaitingForResource => "waiting for resource",
            PipelineStatus::Preparing => "preparing",
            PipelineStatus::Pending => "pending",
            PipelineStatus::Running => "running",
            PipelineStatus::Success => "passed",
            PipelineStatus::Failed => "failed",
            PipelineStatus::Canceled => "canceled",
            PipelineStatus::Skipped => "skipped",
            PipelineStatus::Manual => "manual",
            PipelineStatus::Scheduled => "scheduled",
            PipelineStatus::Unknown => "unknown",
        }
    }
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct User {
    pub id: u64,
    pub username: String,
    pub name: String,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Pipeline {
    pub id: u64,
    pub status: PipelineStatus,
    pub web_url: Url,
    #[serde(default)]
    pub duration: Option<u64>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct MergeRequest {
    pub iid: u64,
    pub title: String,
    pub state: MergeRequestState,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub source_project_id: Option<u64>,
    #[serde(default)]
    pub target_project_id: Option<u64>,
    pub source_branch: String,
    pub target_branch: String,
    pub web_url: Url,
    pub author: User,
    pub updated_at: String,
    #[serde(default)]
    pub head_pipeline: Option<Pipeline>,
}

impl MergeRequest {
    /// Merge requests opened from forks share branch names with the project's
    /// own branches but are not about them.
    pub fn is_from_same_project(&self) -> bool {
        match (self.source_project_id, self.target_project_id) {
            (Some(source), Some(target)) => source == target,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeRequestFilter {
    AssignedToMe,
    ReviewRequested,
    CreatedByMe,
    AllOpen,
}

#[derive(Clone)]
pub struct GitlabClient {
    http: Arc<dyn HttpClient>,
    project: GitlabProject,
    token: String,
}

impl GitlabClient {
    pub fn new(http: Arc<dyn HttpClient>, project: GitlabProject, token: String) -> Self {
        Self {
            http,
            project,
            token,
        }
    }

    pub fn project(&self) -> &GitlabProject {
        &self.project
    }

    pub async fn merge_requests_for_branch(
        &self,
        branch: &str,
        state: MergeRequestState,
    ) -> Result<Vec<MergeRequest>, ApiError> {
        let mut url = self.project.api_url("merge_requests")?;
        url.query_pairs_mut()
            .append_pair("source_branch", branch)
            .append_pair("state", state.as_query())
            .append_pair("order_by", "updated_at")
            .append_pair("sort", "desc")
            .append_pair("per_page", "20");
        self.get(url).await
    }

    /// The list endpoint omits `head_pipeline`; the single merge request has it.
    pub async fn merge_request(&self, iid: u64) -> Result<MergeRequest, ApiError> {
        let url = self.project.api_url(&format!("merge_requests/{iid}"))?;
        self.get(url).await
    }

    pub async fn latest_pipeline_for_ref(
        &self,
        branch: &str,
    ) -> Result<Option<Pipeline>, ApiError> {
        let mut url = self.project.api_url("pipelines")?;
        url.query_pairs_mut()
            .append_pair("ref", branch)
            .append_pair("per_page", "1");
        let pipelines: Vec<Pipeline> = self.get(url).await?;
        Ok(pipelines.into_iter().next())
    }

    pub async fn open_merge_requests(
        &self,
        filter: MergeRequestFilter,
        current_user: Option<u64>,
    ) -> Result<Vec<MergeRequest>, ApiError> {
        let mut url = self.project.api_url("merge_requests")?;
        {
            let mut query = url.query_pairs_mut();
            query
                .append_pair("state", "opened")
                .append_pair("scope", "all")
                .append_pair("order_by", "updated_at")
                .append_pair("sort", "desc")
                .append_pair("per_page", "100");
            let user_key = match filter {
                MergeRequestFilter::AssignedToMe => Some("assignee_id"),
                MergeRequestFilter::ReviewRequested => Some("reviewer_id"),
                MergeRequestFilter::CreatedByMe => Some("author_id"),
                MergeRequestFilter::AllOpen => None,
            };
            if let Some(key) = user_key {
                let user = current_user
                    .ok_or_else(|| anyhow!("current GitLab user is unknown"))?;
                query.append_pair(key, &user.to_string());
            }
        }
        self.get(url).await
    }

    pub async fn current_user(&self) -> Result<User, ApiError> {
        let url = self.project.instance_api_url("user")?;
        self.get(url).await
    }

    async fn get<T: DeserializeOwned>(&self, url: Url) -> Result<T, ApiError> {
        let request = Request::builder()
            .method(Method::GET)
            .uri(url.as_str())
            .header("PRIVATE-TOKEN", &self.token)
            .header("Accept", "application/json")
            .body(AsyncBody::default())
            .map_err(|error| anyhow!(error))?;
        let mut response = self.http.send(request).await?;
        let mut body = String::new();
        response
            .body_mut()
            .read_to_string(&mut body)
            .await
            .map_err(|error| anyhow!(error))?;
        match response.status() {
            StatusCode::UNAUTHORIZED => Err(ApiError::Unauthorized),
            StatusCode::NOT_FOUND => Err(ApiError::NotFound),
            status if !status.is_success() => Err(ApiError::Other(anyhow!(
                "GitLab request {} failed with {status}: {}",
                url.path(),
                body.chars().take(200).collect::<String>()
            ))),
            _ => serde_json::from_str(&body)
                .map_err(|error| ApiError::Other(anyhow!("unexpected GitLab response: {error}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use http_client::{FakeHttpClient, Response};
    use pretty_assertions::assert_eq;
    use std::sync::Mutex;

    fn project() -> GitlabProject {
        GitlabProject {
            instance: Url::parse("https://gitlab.example.com/").expect("valid url"),
            path: "group/project".into(),
        }
    }

    const MERGE_REQUEST_JSON: &str = r#"[{
        "iid": 5,
        "title": "Add feature",
        "state": "opened",
        "draft": false,
        "source_branch": "feature/a+b",
        "target_branch": "main",
        "web_url": "https://gitlab.example.com/group/project/-/merge_requests/5",
        "author": {"id": 7, "username": "jdoe", "name": "Jane Doe"},
        "updated_at": "2026-11-08T10:00:00.000Z",
        "head_pipeline": {
            "id": 42,
            "status": "running",
            "web_url": "https://gitlab.example.com/group/project/-/pipelines/42",
            "duration": null,
            "updated_at": "2026-11-08T10:01:00.000Z"
        }
    }]"#;

    fn client_returning(
        status: u16,
        body: &'static str,
        requests: Arc<Mutex<Vec<(String, Option<String>)>>>,
    ) -> GitlabClient {
        let http = FakeHttpClient::create(move |request| {
            let requests = requests.clone();
            async move {
                requests.lock().expect("lock").push((
                    request.uri().to_string(),
                    request
                        .headers()
                        .get("PRIVATE-TOKEN")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string),
                ));
                Ok(Response::builder().status(status).body(body.into())?)
            }
        });
        GitlabClient::new(http, project(), "token".to_string())
    }

    #[test]
    fn merge_requests_query() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = client_returning(200, MERGE_REQUEST_JSON, requests.clone());
        let merge_requests =
            block_on(client.merge_requests_for_branch("feature/a+b", MergeRequestState::Opened))
                .expect("request succeeds");

        let requests = requests.lock().expect("lock");
        assert_eq!(
            requests[0].0,
            "https://gitlab.example.com/api/v4/projects/group%2Fproject/merge_requests?source_branch=feature%2Fa%2Bb&state=opened&order_by=updated_at&sort=desc&per_page=20"
        );
        assert_eq!(requests[0].1.as_deref(), Some("token"));
        assert_eq!(merge_requests.len(), 1);
        assert_eq!(merge_requests[0].iid, 5);
        assert_eq!(merge_requests[0].author.username, "jdoe");
        assert_eq!(
            merge_requests[0]
                .head_pipeline
                .as_ref()
                .map(|pipeline| pipeline.status),
            Some(PipelineStatus::Running)
        );
    }

    #[test]
    fn unknown_pipeline_status() {
        let status: PipelineStatus =
            serde_json::from_str("\"something_new\"").expect("parses");
        assert_eq!(status, PipelineStatus::Unknown);
        assert!(!status.is_active());
        assert!(PipelineStatus::WaitingForResource.is_active());
        assert!(!PipelineStatus::Success.is_active());
    }

    #[test]
    fn error_statuses() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let unauthorized = block_on(
            client_returning(401, "{}", requests.clone()).latest_pipeline_for_ref("main"),
        );
        assert!(matches!(unauthorized, Err(ApiError::Unauthorized)));

        let not_found = block_on(
            client_returning(404, "{}", requests.clone()).latest_pipeline_for_ref("main"),
        );
        assert!(matches!(not_found, Err(ApiError::NotFound)));

        let server_error =
            block_on(client_returning(500, "boom", requests).latest_pipeline_for_ref("main"));
        match server_error {
            Err(ApiError::Other(error)) => assert!(error.to_string().contains("500")),
            other => panic!("expected server error, got {other:?}"),
        }
    }

    #[test]
    fn open_merge_requests_review_filter() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = client_returning(200, "[]", requests.clone());
        block_on(client.open_merge_requests(MergeRequestFilter::ReviewRequested, Some(7)))
            .expect("request succeeds");
        let requests = requests.lock().expect("lock");
        assert!(requests[0].0.contains("state=opened"));
        assert!(requests[0].0.contains("scope=all"));
        assert!(requests[0].0.contains("reviewer_id=7"));
    }

    #[test]
    fn single_merge_request_url() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let body: &'static str = MERGE_REQUEST_JSON
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']');
        let client = client_returning(200, body, requests.clone());
        let merge_request = block_on(client.merge_request(5)).expect("request succeeds");
        assert_eq!(merge_request.iid, 5);
        assert_eq!(
            requests.lock().expect("lock")[0].0,
            "https://gitlab.example.com/api/v4/projects/group%2Fproject/merge_requests/5"
        );
    }

    #[test]
    fn current_user_url() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        let client = client_returning(
            200,
            r#"{"id": 7, "username": "jdoe", "name": "Jane Doe"}"#,
            requests.clone(),
        );
        let user = block_on(client.current_user()).expect("request succeeds");
        assert_eq!(user.id, 7);
        assert_eq!(
            requests.lock().expect("lock")[0].0,
            "https://gitlab.example.com/api/v4/user"
        );
    }
}
