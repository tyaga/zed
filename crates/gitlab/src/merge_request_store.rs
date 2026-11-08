use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use collections::{HashMap, HashSet};
use git::GitHostingProviderRegistry;
use gpui::{
    App, AppContext as _, AsyncApp, Context, Entity, EntityId, EventEmitter, Global, SharedString,
    Subscription, Task, WeakEntity,
};
use git_hosting_providers::GitHostingProviderSettings;
use http_client::HttpClient;
use settings::{GitHostingProviderKind, Settings as _};
use project::{
    Project,
    git_store::{GitStore, GitStoreEvent, RepositoryEvent, RepositoryId},
};

use crate::{
    ApiError, GitlabClient, GitlabProject, MergeRequest, MergeRequestState, Pipeline, Url,
    gitlab_project_from_remote, lookup_token,
};

const ACTIVE_PIPELINE_POLL_INTERVAL: Duration = Duration::from_secs(15);
const IDLE_POLL_INTERVAL: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub enum BranchState {
    NotGitlab,
    /// Detached HEAD or no active repository branch.
    NoBranch,
    NoToken {
        host: SharedString,
    },
    Loading,
    Ready(BranchInfo),
}

#[derive(Clone, Debug)]
pub struct BranchInfo {
    pub project: GitlabProject,
    pub repository_id: RepositoryId,
    pub remote: SharedString,
    pub branch: SharedString,
    pub merge_request: Option<MergeRequest>,
    pub pipeline: Option<Pipeline>,
    pub last_refreshed: Instant,
    pub last_error: Option<SharedString>,
    /// The merge request targets a branch whose remote-tracking ref is not
    /// present locally, so the gutter keeps diffing against the default branch.
    pub target_ref_missing: bool,
}

impl BranchInfo {
    /// `<remote>/<target branch>` of the opened merge request.
    pub fn target_ref(&self) -> Option<SharedString> {
        let merge_request = self.merge_request.as_ref()?;
        (merge_request.state == MergeRequestState::Opened)
            .then(|| format!("{}/{}", self.remote, merge_request.target_branch).into())
    }

    /// [`Self::target_ref`] when it exists locally.
    pub fn fetched_target_ref(&self) -> Option<SharedString> {
        self.target_ref().filter(|_| !self.target_ref_missing)
    }
}

pub enum GitlabStoreEvent {
    Changed,
}

impl EventEmitter<GitlabStoreEvent> for GitlabStore {}

#[derive(Default)]
struct GlobalGitlabStores(HashMap<EntityId, WeakEntity<GitlabStore>>);

impl Global for GlobalGitlabStores {}

pub struct GitlabStore {
    git_store: Entity<GitStore>,
    http: Arc<dyn HttpClient>,
    hosting_providers: Arc<GitHostingProviderRegistry>,
    tokens: HashMap<String, String>,
    state: BranchState,
    generation: u64,
    window_active: bool,
    stale: bool,
    override_repository: Option<RepositoryId>,
    refresh_task: Option<Task<()>>,
    poll_task: Option<Task<()>>,
    _subscription: Subscription,
}

struct RefreshRequest {
    repository_id: RepositoryId,
    remote: SharedString,
    branch: SharedString,
    remote_branch: String,
    remote_refs: HashSet<String>,
}

enum FetchOutcome {
    NotGitlab,
    RemotesUnavailable {
        error: SharedString,
    },
    NoToken { host: SharedString },
    Fetched {
        project: GitlabProject,
        merge_request: Option<MergeRequest>,
        pipeline: Option<Pipeline>,
    },
    Failed {
        project: GitlabProject,
        error: SharedString,
    },
}

impl GitlabStore {
    pub fn for_project(project: &Entity<Project>, cx: &mut App) -> Entity<GitlabStore> {
        let project_id = project.entity_id();
        if let Some(store) = cx
            .try_global::<GlobalGitlabStores>()
            .and_then(|stores| stores.0.get(&project_id))
            .and_then(|store| store.upgrade())
        {
            return store;
        }
        let git_store = project.read(cx).git_store().clone();
        let store = cx.new(|cx| GitlabStore::new(git_store, cx));
        let stores = cx.default_global::<GlobalGitlabStores>();
        stores.0.retain(|_, store| store.upgrade().is_some());
        stores.0.insert(project_id, store.downgrade());
        store
    }

    fn new(git_store: Entity<GitStore>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&git_store, |this, _, event, cx| match event {
            GitStoreEvent::ActiveRepositoryChanged(_)
            | GitStoreEvent::RepositoryUpdated(
                _,
                RepositoryEvent::HeadChanged | RepositoryEvent::BranchListChanged,
                true,
            ) => this.refresh(cx),
            _ => {}
        });
        let mut this = Self {
            git_store,
            http: cx.http_client(),
            hosting_providers: GitHostingProviderRegistry::default_global(cx),
            tokens: HashMap::default(),
            state: BranchState::Loading,
            generation: 0,
            window_active: false,
            stale: false,
            override_repository: None,
            refresh_task: None,
            poll_task: None,
            _subscription: subscription,
        };
        this.refresh(cx);
        this
    }

    pub fn state(&self) -> &BranchState {
        &self.state
    }

    pub fn branch_info(&self) -> Option<&BranchInfo> {
        match &self.state {
            BranchState::Ready(info) => Some(info),
            _ => None,
        }
    }

    pub fn client(&self) -> Option<GitlabClient> {
        let info = self.branch_info()?;
        let token = self.tokens.get(info.project.host())?;
        Some(GitlabClient::new(
            self.http.clone(),
            info.project.clone(),
            token.clone(),
        ))
    }

    /// Polling runs only while a window showing this project is active; the
    /// first activation after a skipped poll refreshes right away.
    pub fn set_window_active(&mut self, active: bool, cx: &mut Context<Self>) {
        self.window_active = active;
        if active && self.stale {
            self.refresh(cx);
        }
    }

    #[cfg(test)]
    pub(crate) fn set_token_for_tests(&mut self, host: &str, token: &str) {
        self.tokens.insert(host.to_string(), token.to_string());
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.stale = false;
        self.poll_task = None;

        let Some(request) = self.refresh_request(cx) else {
            self.refresh_task = None;
            self.set_override(None, None, cx);
            self.set_state(BranchState::NoBranch, cx);
            return;
        };
        let Some(repository) = self.git_store.read(cx).active_repository() else {
            return;
        };

        let same_branch = self.branch_info().is_some_and(|info| {
            info.repository_id == request.repository_id && info.branch == request.branch
        });
        if !same_branch {
            self.set_override(None, None, cx);
            self.set_state(BranchState::Loading, cx);
        }

        let remote_urls = repository.update(cx, |repository, _| repository.remote_urls());
        let http = self.http.clone();
        let hosting_providers = self.hosting_providers.clone();
        let configured_instances = configured_gitlab_instances(cx);
        self.refresh_task = Some(cx.spawn(async move |this, cx| {
            let remote_urls = match remote_urls.await {
                Ok(Ok(urls)) => Ok(urls),
                Ok(Err(error)) => Err(SharedString::from(format!(
                    "Reading git remotes failed: {error:#}"
                ))),
                Err(_) => Err(SharedString::from("Reading git remotes was canceled")),
            };
            let outcome = match remote_urls {
                Err(error) => FetchOutcome::RemotesUnavailable { error },
                Ok(remote_urls) => match remote_urls
                    .get(request.remote.as_ref())
                    .and_then(|url| {
                        gitlab_project_from_remote(&hosting_providers, &configured_instances, url)
                    }) {
                    None => FetchOutcome::NotGitlab,
                    Some(project) => {
                        fetch_branch(this.clone(), http, project, &request.remote_branch, cx)
                            .await
                    }
                },
            };
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.apply(request, outcome, cx);
                }
            })
            .ok();
        }));
    }

    fn refresh_request(&self, cx: &App) -> Option<RefreshRequest> {
        let repository = self.git_store.read(cx).active_repository()?;
        let repository = repository.read(cx);
        let branch = repository.branch.as_ref()?;
        let upstream = branch.upstream.as_ref().filter(|upstream| upstream.is_remote());
        let remote = upstream
            .and_then(|upstream| upstream.remote_name())
            .unwrap_or("origin");
        let remote_branch = upstream
            .and_then(|upstream| upstream.branch_name())
            .unwrap_or(branch.name());
        Some(RefreshRequest {
            repository_id: repository.id,
            remote: remote.to_string().into(),
            branch: branch.name().to_string().into(),
            remote_branch: remote_branch.to_string(),
            remote_refs: repository
                .branch_list
                .iter()
                .filter(|branch| branch.is_remote())
                .map(|branch| branch.name().to_string())
                .collect(),
        })
    }

    fn apply(&mut self, request: RefreshRequest, outcome: FetchOutcome, cx: &mut Context<Self>) {
        match outcome {
            FetchOutcome::NotGitlab => {
                self.set_override(None, None, cx);
                self.set_state(BranchState::NotGitlab, cx);
            }
            FetchOutcome::RemotesUnavailable { error } => {
                log::warn!("{error}");
                match self.branch_info() {
                    Some(info) => {
                        let info = BranchInfo {
                            last_error: Some(error),
                            ..info.clone()
                        };
                        self.set_state(BranchState::Ready(info), cx);
                    }
                    None => {
                        self.set_state(BranchState::NotGitlab, cx);
                        self.schedule_poll(IDLE_POLL_INTERVAL, cx);
                    }
                }
            }
            FetchOutcome::NoToken { host } => {
                self.tokens.remove(host.as_ref());
                self.set_override(None, None, cx);
                self.set_state(BranchState::NoToken { host }, cx);
            }
            FetchOutcome::Fetched {
                project,
                merge_request,
                pipeline,
            } => {
                let mut info = BranchInfo {
                    project,
                    repository_id: request.repository_id,
                    remote: request.remote,
                    branch: request.branch,
                    merge_request,
                    pipeline,
                    last_refreshed: Instant::now(),
                    last_error: None,
                    target_ref_missing: false,
                };
                let target_ref = info.target_ref();
                let base_ref = target_ref
                    .clone()
                    .filter(|target| request.remote_refs.contains(target.as_ref()));
                info.target_ref_missing = target_ref.is_some() && base_ref.is_none();
                self.set_override(Some(info.repository_id), base_ref, cx);
                self.set_state(BranchState::Ready(info), cx);
            }
            FetchOutcome::Failed { project, error } => {
                log::warn!("GitLab refresh failed: {error}");
                let info = match self.branch_info() {
                    Some(info)
                        if info.repository_id == request.repository_id
                            && info.branch == request.branch =>
                    {
                        BranchInfo {
                            last_error: Some(error),
                            ..info.clone()
                        }
                    }
                    _ => BranchInfo {
                        project,
                        repository_id: request.repository_id,
                        remote: request.remote,
                        branch: request.branch,
                        merge_request: None,
                        pipeline: None,
                        last_refreshed: Instant::now(),
                        last_error: Some(error),
                        target_ref_missing: false,
                    },
                };
                self.set_state(BranchState::Ready(info), cx);
            }
        }
        if let Some(info) = self.branch_info() {
            let interval = if info
                .pipeline
                .as_ref()
                .is_some_and(|pipeline| pipeline.status.is_active())
            {
                ACTIVE_PIPELINE_POLL_INTERVAL
            } else {
                IDLE_POLL_INTERVAL
            };
            self.schedule_poll(interval, cx);
        }
    }

    fn schedule_poll(&mut self, interval: Duration, cx: &mut Context<Self>) {
        self.poll_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(interval).await;
            this.update(cx, |this, cx| {
                if this.window_active {
                    this.refresh(cx);
                } else {
                    this.stale = true;
                }
            })
            .ok();
        }));
    }

    fn set_state(&mut self, state: BranchState, cx: &mut Context<Self>) {
        self.state = state;
        cx.emit(GitlabStoreEvent::Changed);
        cx.notify();
    }

    fn set_override(
        &mut self,
        repository_id: Option<RepositoryId>,
        base_ref: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        if let Some(previous) = self.override_repository.take()
            && Some(previous) != repository_id
        {
            self.git_store.update(cx, |git_store, cx| {
                git_store.set_diff_base_override(previous, None, cx)
            });
        }
        if let Some(repository_id) = repository_id {
            if base_ref.is_some() {
                self.override_repository = Some(repository_id);
            }
            self.git_store.update(cx, |git_store, cx| {
                git_store.set_diff_base_override(repository_id, base_ref, cx)
            });
        }
    }
}

fn configured_gitlab_instances(cx: &App) -> Vec<Url> {
    GitHostingProviderSettings::get_global(cx)
        .git_hosting_providers
        .iter()
        .filter(|provider| provider.provider == GitHostingProviderKind::Gitlab)
        .filter_map(|provider| Url::parse(&provider.base_url).ok())
        .collect()
}

async fn fetch_branch(
    this: WeakEntity<GitlabStore>,
    http: Arc<dyn HttpClient>,
    project: GitlabProject,
    remote_branch: &str,
    cx: &mut AsyncApp,
) -> FetchOutcome {
    let host: SharedString = project.host().to_string().into();
    let cached_token = this
        .read_with(cx, |this, _| this.tokens.get(host.as_ref()).cloned())
        .ok()
        .flatten();
    let token = match cached_token {
        Some(token) => token,
        None => match lookup_token(&host).await {
            Some(token) => {
                this.update(cx, |this, _| {
                    this.tokens.insert(host.to_string(), token.clone())
                })
                .ok();
                token
            }
            None => return FetchOutcome::NoToken { host },
        },
    };

    let client = GitlabClient::new(http, project.clone(), token);
    match fetch_merge_request_and_pipeline(&client, remote_branch).await {
        Ok((merge_request, pipeline)) => FetchOutcome::Fetched {
            project,
            merge_request,
            pipeline,
        },
        Err(ApiError::Unauthorized) => FetchOutcome::NoToken { host },
        Err(error) => FetchOutcome::Failed {
            project,
            error: error.to_string().into(),
        },
    }
}

async fn fetch_merge_request_and_pipeline(
    client: &GitlabClient,
    branch: &str,
) -> Result<(Option<MergeRequest>, Option<Pipeline>), ApiError> {
    let mut merge_request = None;
    for state in [
        MergeRequestState::Opened,
        MergeRequestState::Merged,
        MergeRequestState::Closed,
    ] {
        if let Some(found) = client
            .merge_requests_for_branch(branch, state)
            .await?
            .into_iter()
            .find(MergeRequest::is_from_same_project)
        {
            merge_request = Some(found);
            break;
        }
    }
    if let Some(found) = &merge_request
        && found.head_pipeline.is_none()
    {
        match client.merge_request(found.iid).await {
            Ok(detailed) => merge_request = Some(detailed),
            Err(ApiError::Unauthorized) => return Err(ApiError::Unauthorized),
            Err(error) => log::debug!("loading merge request !{} failed: {error}", found.iid),
        }
    }
    let pipeline = match merge_request
        .as_ref()
        .and_then(|merge_request| merge_request.head_pipeline.clone())
    {
        Some(pipeline) => Some(pipeline),
        None => client.latest_pipeline_for_ref(branch).await?,
    };
    Ok((merge_request, pipeline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PipelineStatus;
    use fs::FakeFs;
    use futures::{FutureExt as _, future::BoxFuture};
    use git::repository::{Upstream, UpstreamTracking, UpstreamTrackingStatus};
    use gpui::TestAppContext;
    use http_client::{FakeHttpClient, Response};
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use settings::SettingsStore;
    use std::{
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use util::path;

    const DOT_GIT: &str = path!("/project/.git");
    const HOST: &str = "gitlab.example.com";

    type Handler = Arc<dyn Fn(&str) -> BoxFuture<'static, (u16, String)> + Send + Sync>;

    struct Harness {
        fs: Arc<FakeFs>,
        store: Entity<GitlabStore>,
        project: Entity<Project>,
        requests: Arc<Mutex<Vec<String>>>,
    }

    fn merge_request_json(iid: u64, source: &str, target: &str, state: &str, pipeline: &str) -> serde_json::Value {
        json!({
            "iid": iid,
            "title": format!("MR {iid}"),
            "state": state,
            "draft": false,
            "source_project_id": 1,
            "target_project_id": 1,
            "source_branch": source,
            "target_branch": target,
            "web_url": format!("https://{HOST}/group/project/-/merge_requests/{iid}"),
            "author": {"id": 7, "username": "jdoe", "name": "Jane Doe"},
            "updated_at": "2026-11-08T10:00:00.000Z",
            "head_pipeline": {
                "id": 100 + iid,
                "status": pipeline,
                "web_url": format!("https://{HOST}/group/project/-/pipelines/{}", 100 + iid),
                "duration": null
            }
        })
    }

    fn respond(status: u16, body: serde_json::Value) -> BoxFuture<'static, (u16, String)> {
        futures::future::ready((status, body.to_string())).boxed()
    }

    fn query_value<'a>(uri: &'a str, key: &str) -> Option<&'a str> {
        uri.split(['?', '&'])
            .find_map(|pair| pair.strip_prefix(&format!("{key}=")))
    }

    fn set_upstream(fs: &FakeFs, branch: &str) {
        fs.with_git_state(Path::new(DOT_GIT), true, |state| {
            state.branch_upstreams.insert(
                branch.to_string(),
                Upstream {
                    ref_name: format!("refs/remotes/origin/{branch}").into(),
                    tracking: UpstreamTracking::Tracked(UpstreamTrackingStatus {
                        ahead: 0,
                        behind: 0,
                    }),
                },
            );
        })
        .expect("fake repository exists");
    }

    async fn setup(cx: &mut TestAppContext, remote: &str, handler: Handler) -> Harness {
        zlog::init_test();
        cx.update(|cx| {
            let settings_store = SettingsStore::test(cx);
            cx.set_global(settings_store);
        });
        let fs = FakeFs::new(cx.executor());
        fs.insert_tree(path!("/project"), json!({".git": {}, "file.rs": "text\n"}))
            .await;
        fs.set_remote_for_repo(Path::new(DOT_GIT), "origin", remote);
        fs.insert_branches(
            Path::new(DOT_GIT),
            &["feature", "other", "origin/feature", "origin/main", "origin/other"],
        );
        set_upstream(&fs, "feature");
        set_upstream(&fs, "other");
        fs.set_branch_name(Path::new(DOT_GIT), Some("feature"));

        let project = Project::test(fs.clone(), [path!("/project").as_ref()], cx).await;
        project
            .update(cx, |project, cx| project.git_scans_complete(cx))
            .await;

        let requests = Arc::new(Mutex::new(Vec::new()));
        let http = FakeHttpClient::create({
            let requests = requests.clone();
            move |request| {
                let uri = request.uri().to_string();
                requests.lock().expect("lock").push(uri.clone());
                let response = handler(&uri);
                async move {
                    let (status, body) = response.await;
                    Ok(Response::builder().status(status).body(body.into())?)
                }
            }
        });
        cx.update(|cx| cx.set_http_client(http));

        let store = cx.update(|cx| {
            let store = GitlabStore::for_project(&project, cx);
            store.update(cx, |store, _| store.set_token_for_tests(HOST, "token"));
            store
        });
        cx.run_until_parked();
        Harness {
            fs,
            store,
            project,
            requests,
        }
    }

    fn ready(harness: &Harness, cx: &mut TestAppContext) -> BranchInfo {
        harness.store.read_with(cx, |store, _| match store.state() {
            BranchState::Ready(info) => info.clone(),
            other => panic!("expected ready state, got {other:?}"),
        })
    }

    fn diff_base_override(harness: &Harness, cx: &mut TestAppContext) -> Option<String> {
        harness.project.read_with(cx, |project, cx| {
            let git_store = project.git_store().read(cx);
            let repo_id = git_store.active_repository()?.read(cx).id;
            git_store.diff_base_override(repo_id).map(|base| base.to_string())
        })
    }

    fn standard_handler(opened: Vec<serde_json::Value>) -> Handler {
        Arc::new(move |uri: &str| {
            if uri.contains("/merge_requests") {
                let state = query_value(uri, "state");
                let branch = query_value(uri, "source_branch");
                if state == Some("opened") && branch == Some("feature") {
                    return respond(200, json!(opened));
                }
                return respond(200, json!([]));
            }
            respond(200, json!([]))
        })
    }

    #[gpui::test]
    async fn finds_opened_merge_request(cx: &mut TestAppContext) {
        let harness = setup(
            cx,
            "git@gitlab.example.com:group/project.git",
            standard_handler(vec![merge_request_json(5, "feature", "main", "opened", "running")]),
        )
        .await;
        let info = ready(&harness, cx);
        assert_eq!(info.branch.as_ref(), "feature");
        assert_eq!(info.merge_request.as_ref().map(|mr| mr.iid), Some(5));
        assert_eq!(
            info.pipeline.as_ref().map(|pipeline| pipeline.status),
            Some(PipelineStatus::Running)
        );
        assert_eq!(info.project.path.as_ref(), "group/project");
    }

    #[gpui::test]
    async fn ignores_merge_requests_from_forks(cx: &mut TestAppContext) {
        let mut fork_merge_request = merge_request_json(8, "feature", "release", "opened", "success");
        fork_merge_request["source_project_id"] = json!(2);
        let harness = setup(
            cx,
            "git@gitlab.example.com:group/project.git",
            standard_handler(vec![fork_merge_request]),
        )
        .await;
        assert_eq!(ready(&harness, cx).merge_request, None);
        assert_eq!(diff_base_override(&harness, cx), None);
    }

    #[gpui::test]
    async fn falls_back_to_merged(cx: &mut TestAppContext) {
        let handler: Handler = Arc::new(|uri: &str| {
            if uri.contains("/merge_requests") && query_value(uri, "state") == Some("merged") {
                return respond(200, json!([merge_request_json(4, "feature", "main", "merged", "success")]));
            }
            respond(200, json!([]))
        });
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;
        let info = ready(&harness, cx);
        assert_eq!(info.merge_request.as_ref().map(|mr| mr.iid), Some(4));
        assert_eq!(diff_base_override(&harness, cx), None);
    }

    #[gpui::test]
    async fn branch_pipeline_without_merge_request(cx: &mut TestAppContext) {
        let handler: Handler = Arc::new(|uri: &str| {
            if uri.contains("/pipelines") && query_value(uri, "ref") == Some("feature") {
                return respond(200, json!([{
                    "id": 9,
                    "status": "failed",
                    "web_url": format!("https://{HOST}/group/project/-/pipelines/9")
                }]));
            }
            respond(200, json!([]))
        });
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;
        let info = ready(&harness, cx);
        assert_eq!(info.merge_request, None);
        assert_eq!(info.pipeline.as_ref().map(|pipeline| pipeline.id), Some(9));
    }

    #[gpui::test]
    async fn stale_response_dropped(cx: &mut TestAppContext) {
        let (release_sender, release_receiver) = futures::channel::oneshot::channel::<()>();
        let release_receiver = Arc::new(Mutex::new(Some(release_receiver)));
        let handler: Handler = Arc::new(move |uri: &str| {
            let branch = query_value(uri, "source_branch").map(str::to_string);
            let state = query_value(uri, "state").map(str::to_string);
            let gate = if branch.as_deref() == Some("feature") && state.as_deref() == Some("opened") {
                release_receiver.lock().expect("lock").take()
            } else {
                None
            };
            async move {
                if let Some(gate) = gate {
                    gate.await.ok();
                    return (
                        200,
                        json!([merge_request_json(5, "feature", "main", "opened", "running")]).to_string(),
                    );
                }
                if branch.as_deref() == Some("other") && state.as_deref() == Some("opened") {
                    return (
                        200,
                        json!([merge_request_json(6, "other", "main", "opened", "success")]).to_string(),
                    );
                }
                (200, "[]".to_string())
            }
            .boxed()
        });
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;

        harness.fs.set_branch_name(Path::new(DOT_GIT), Some("other"));
        cx.run_until_parked();
        release_sender.send(()).ok();
        cx.run_until_parked();

        let info = ready(&harness, cx);
        assert_eq!(info.branch.as_ref(), "other");
        assert_eq!(info.merge_request.as_ref().map(|mr| mr.iid), Some(6));
    }

    #[gpui::test]
    async fn sets_and_clears_override(cx: &mut TestAppContext) {
        let harness = setup(
            cx,
            "git@gitlab.example.com:group/project.git",
            standard_handler(vec![merge_request_json(5, "feature", "main", "opened", "success")]),
        )
        .await;
        assert_eq!(diff_base_override(&harness, cx).as_deref(), Some("origin/main"));

        harness.fs.set_branch_name(Path::new(DOT_GIT), Some("other"));
        cx.run_until_parked();
        assert_eq!(ready(&harness, cx).branch.as_ref(), "other");
        assert_eq!(diff_base_override(&harness, cx), None);
    }

    #[gpui::test]
    async fn missing_target_ref(cx: &mut TestAppContext) {
        let harness = setup(
            cx,
            "git@gitlab.example.com:group/project.git",
            standard_handler(vec![merge_request_json(5, "feature", "release", "opened", "success")]),
        )
        .await;
        assert_eq!(diff_base_override(&harness, cx), None);
        let info = ready(&harness, cx);
        assert!(info.target_ref_missing);
        assert_eq!(info.fetched_target_ref(), None);
    }

    #[gpui::test]
    async fn error_keeps_last_state(cx: &mut TestAppContext) {
        let fail = Arc::new(Mutex::new(false));
        let handler: Handler = Arc::new({
            let fail = fail.clone();
            move |uri: &str| {
                if *fail.lock().expect("lock") {
                    return futures::future::ready((500, "boom".to_string())).boxed();
                }
                standard_handler(vec![merge_request_json(5, "feature", "main", "opened", "success")])(uri)
            }
        });
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;
        *fail.lock().expect("lock") = true;
        harness.store.update(cx, |store, cx| store.refresh(cx));
        cx.run_until_parked();

        let info = ready(&harness, cx);
        assert_eq!(info.merge_request.as_ref().map(|mr| mr.iid), Some(5));
        assert!(info.last_error.is_some());
    }

    #[gpui::test]
    async fn unauthorized_drops_token(cx: &mut TestAppContext) {
        let handler: Handler = Arc::new(|_: &str| respond(401, json!({"message": "401 Unauthorized"})));
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;
        harness.store.read_with(cx, |store, _| {
            assert!(matches!(store.state(), BranchState::NoToken { .. }));
        });
    }

    #[gpui::test]
    async fn poll_interval_follows_pipeline(cx: &mut TestAppContext) {
        let pipeline_status = Arc::new(Mutex::new("running"));
        let handler: Handler = Arc::new({
            let pipeline_status = pipeline_status.clone();
            move |uri: &str| {
                let status = *pipeline_status.lock().expect("lock");
                standard_handler(vec![merge_request_json(5, "feature", "main", "opened", status)])(uri)
            }
        });
        let harness = setup(cx, "git@gitlab.example.com:group/project.git", handler).await;
        harness.store.update(cx, |store, cx| store.set_window_active(true, cx));
        cx.run_until_parked();
        let request_count = || harness.requests.lock().expect("lock").len();

        let before = request_count();
        cx.executor().advance_clock(Duration::from_secs(16));
        cx.run_until_parked();
        assert!(request_count() > before, "running pipeline polls after 15 s");

        *pipeline_status.lock().expect("lock") = "success";
        cx.executor().advance_clock(Duration::from_secs(16));
        cx.run_until_parked();
        let after_success = request_count();
        cx.executor().advance_clock(Duration::from_secs(16));
        cx.run_until_parked();
        assert_eq!(request_count(), after_success, "finished pipeline does not poll at 15 s");
        cx.executor().advance_clock(Duration::from_secs(110));
        cx.run_until_parked();
        assert!(request_count() > after_success, "finished pipeline polls at 120 s");
    }

    #[gpui::test]
    async fn no_polling_while_inactive(cx: &mut TestAppContext) {
        let harness = setup(
            cx,
            "git@gitlab.example.com:group/project.git",
            standard_handler(vec![merge_request_json(5, "feature", "main", "opened", "running")]),
        )
        .await;
        let before = harness.requests.lock().expect("lock").len();
        cx.executor().advance_clock(Duration::from_secs(60));
        cx.run_until_parked();
        assert_eq!(harness.requests.lock().expect("lock").len(), before);

        harness.store.update(cx, |store, cx| store.set_window_active(true, cx));
        cx.run_until_parked();
        assert!(harness.requests.lock().expect("lock").len() > before);
    }

    #[gpui::test]
    async fn not_gitlab_remote(cx: &mut TestAppContext) {
        let harness = setup(
            cx,
            "git@github.com:owner/repo.git",
            standard_handler(Vec::new()),
        )
        .await;
        harness.store.read_with(cx, |store, _| {
            assert!(matches!(store.state(), BranchState::NotGitlab));
        });
        assert!(harness.requests.lock().expect("lock").is_empty());
    }
}
