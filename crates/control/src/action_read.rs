use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{rejection::QueryRejection, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use ryvus_execution::{
    action_revision, ExecutionAggregate, ExecutionHistoryQuery, ExecutionScopeId, ExecutionState,
    ExecutionStateStore,
};
use ryvus_logging::{ExecutionLogStore, LogStreamQuery};
use ryvus_protocol::{ActionDefinition, RuntimeKind};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ControlService;

const DETAIL_HISTORY_LIMIT: usize = 100;
const RECENT_EXECUTION_LIMIT: usize = 5;
const REVISION_HISTORY_PAGES: usize = 10;
const LOG_HISTORY_LIMIT: usize = 1_000;

pub struct ActionReadService {
    control: Arc<ControlService>,
    executions: Arc<dyn ExecutionStateStore>,
    logs: Arc<dyn ExecutionLogStore>,
    scope: ExecutionScopeId,
}

impl ActionReadService {
    pub fn new(
        control: Arc<ControlService>,
        executions: Arc<dyn ExecutionStateStore>,
        logs: Arc<dyn ExecutionLogStore>,
        scope: ExecutionScopeId,
    ) -> Self {
        Self {
            control,
            executions,
            logs,
            scope,
        }
    }

    pub fn detail(&self, requested_action_id: &str) -> Result<ActionDetail, ActionReadError> {
        let action = self.current_action(requested_action_id)?;
        let current_revision = action_revision(action).map_err(|_| ActionReadError::Unavailable)?;
        let history = self
            .executions
            .list_history(execution_query(
                self.scope.clone(),
                requested_action_id,
                None,
                DETAIL_HISTORY_LIMIT,
            ))
            .map_err(|_| ActionReadError::Unavailable)?;
        let recent_health = recent_health(&history.items);
        let recent_executions = history
            .items
            .into_iter()
            .take(RECENT_EXECUTION_LIMIT)
            .collect();

        Ok(ActionDetail {
            action_id: requested_action_id.to_string(),
            display_name: action
                .name
                .clone()
                .unwrap_or_else(|| action.entrypoint.clone()),
            current_revision,
            definition: action.clone(),
            recent_health,
            recent_executions,
        })
    }

    pub fn revisions(
        &self,
        requested_action_id: &str,
    ) -> Result<ObservedRevisionPage, ActionReadError> {
        let action = self.current_action(requested_action_id)?;
        let current_revision = action_revision(action).map_err(|_| ActionReadError::Unavailable)?;
        let mut revisions = BTreeMap::<String, RevisionAccumulator>::new();
        revisions
            .entry(current_revision.clone())
            .or_default()
            .runtime = Some(action.runtime.clone());

        let mut cursor = None;
        let mut execution_history_truncated = false;
        for page_index in 0..REVISION_HISTORY_PAGES {
            let page = self
                .executions
                .list_history(ExecutionHistoryQuery {
                    cursor,
                    ..execution_query(
                        self.scope.clone(),
                        requested_action_id,
                        None,
                        DETAIL_HISTORY_LIMIT,
                    )
                })
                .map_err(|_| ActionReadError::Unavailable)?;
            for execution in page.items {
                let revision = revisions
                    .entry(execution.action_revision.clone())
                    .or_default();
                revision.execution_count += 1;
                revision.observe(system_time_unix_nanos(execution.created_at));
                revision.observe(system_time_unix_nanos(execution.updated_at));
            }
            cursor = page.next_cursor;
            if cursor.is_none() {
                break;
            }
            if page_index + 1 == REVISION_HISTORY_PAGES {
                execution_history_truncated = true;
            }
        }

        let log_page = self
            .logs
            .list_streams(LogStreamQuery {
                execution_scope: self.scope.clone(),
                action_key_id: Some(requested_action_id.to_string()),
                action_revision: None,
                runtime_host_id: None,
                execution_id: None,
                attempt_id: None,
                severity: None,
                message_contains: None,
                cursor: None,
                limit: LOG_HISTORY_LIMIT,
            })
            .map_err(|_| ActionReadError::Unavailable)?;
        let log_history_truncated = log_page.next_cursor.is_some();
        for summary in log_page.streams {
            let is_current = summary.stream.action_revision == current_revision;
            let revision = revisions.entry(summary.stream.action_revision).or_default();
            revision.runtime_host_stream_count += 1;
            if !is_current || revision.runtime.is_none() {
                revision.runtime = Some(summary.stream.runtime_language);
            }
            revision.observe(i128::from(summary.stream.started_at_unix_nanos));
            if let Some(ended_at) = summary.ended_at_unix_nanos {
                revision.observe(i128::from(ended_at));
            }
        }

        let mut revisions = revisions.into_iter().collect::<Vec<_>>();
        revisions.sort_by(|(left_revision, left), (right_revision, right)| {
            (right_revision == &current_revision)
                .cmp(&(left_revision == &current_revision))
                .then_with(|| right.last.cmp(&left.last))
                .then_with(|| right_revision.cmp(left_revision))
        });
        let revisions = revisions
            .into_iter()
            .map(|(revision, observed)| ObservedRevision {
                status: if revision == current_revision {
                    ObservedRevisionStatus::Current
                } else {
                    ObservedRevisionStatus::Observed
                },
                revision,
                first_observed_at_unix_nanos: observed.first.map(|value| value.to_string()),
                last_observed_at_unix_nanos: observed.last.map(|value| value.to_string()),
                runtime: observed.runtime,
                execution_count: observed.execution_count,
                runtime_host_stream_count: observed.runtime_host_stream_count,
            })
            .collect::<Vec<_>>();

        Ok(ObservedRevisionPage {
            revisions,
            execution_history_truncated,
            log_history_truncated,
        })
    }

    fn current_action(
        &self,
        requested_action_id: &str,
    ) -> Result<&ActionDefinition, ActionReadError> {
        if requested_action_id.trim().is_empty() {
            return Err(ActionReadError::InvalidQuery);
        }
        self.control
            .action_catalog()
            .all()
            .find(|action| action_id(action) == requested_action_id)
            .ok_or(ActionReadError::NotFound)
    }
}

#[derive(Debug, Serialize)]
pub struct ActionDetail {
    pub action_id: String,
    pub display_name: String,
    pub current_revision: String,
    pub definition: ActionDefinition,
    pub recent_health: RecentHealth,
    pub recent_executions: Vec<ExecutionAggregate>,
}

#[derive(Debug, Serialize)]
pub struct RecentHealth {
    pub window: usize,
    pub sample_size: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub active: usize,
    pub success_rate: Option<f64>,
    pub average_duration_ms: Option<f64>,
    pub p95_duration_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ObservedRevisionPage {
    pub revisions: Vec<ObservedRevision>,
    pub execution_history_truncated: bool,
    pub log_history_truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct ObservedRevision {
    pub revision: String,
    pub status: ObservedRevisionStatus,
    pub first_observed_at_unix_nanos: Option<String>,
    pub last_observed_at_unix_nanos: Option<String>,
    pub runtime: Option<RuntimeKind>,
    pub execution_count: usize,
    pub runtime_host_stream_count: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservedRevisionStatus {
    Current,
    Observed,
}

#[derive(Debug, thiserror::Error)]
pub enum ActionReadError {
    #[error("invalid action query")]
    InvalidQuery,
    #[error("action not found")]
    NotFound,
    #[error("action projection unavailable")]
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct ActionQuery {
    action_id: String,
}

pub fn action_read_routes(service: Arc<ActionReadService>) -> Router {
    Router::new()
        .route("/internal/actions/detail", get(action_detail))
        .route("/internal/actions/revisions", get(action_revisions))
        .with_state(service)
}

async fn action_detail(
    State(service): State<Arc<ActionReadService>>,
    query: Result<Query<ActionQuery>, QueryRejection>,
) -> Result<Json<ActionDetail>, ActionReadError> {
    let Query(query) = query.map_err(|_| ActionReadError::InvalidQuery)?;
    service.detail(&query.action_id).map(Json)
}

async fn action_revisions(
    State(service): State<Arc<ActionReadService>>,
    query: Result<Query<ActionQuery>, QueryRejection>,
) -> Result<Json<ObservedRevisionPage>, ActionReadError> {
    let Query(query) = query.map_err(|_| ActionReadError::InvalidQuery)?;
    service.revisions(&query.action_id).map(Json)
}

impl IntoResponse for ActionReadError {
    fn into_response(self) -> Response {
        let (status, code) = match self {
            Self::InvalidQuery => (StatusCode::BAD_REQUEST, "action_invalid_query"),
            Self::NotFound => (StatusCode::NOT_FOUND, "action_not_found"),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "action_projection_unavailable",
            ),
        };
        (
            status,
            Json(json!({ "error": code, "message": self.to_string() })),
        )
            .into_response()
    }
}

#[derive(Default)]
struct RevisionAccumulator {
    first: Option<i128>,
    last: Option<i128>,
    runtime: Option<RuntimeKind>,
    execution_count: usize,
    runtime_host_stream_count: usize,
}

impl RevisionAccumulator {
    fn observe(&mut self, timestamp: i128) {
        self.first = Some(self.first.map_or(timestamp, |first| first.min(timestamp)));
        self.last = Some(self.last.map_or(timestamp, |last| last.max(timestamp)));
    }
}

fn action_id(action: &ActionDefinition) -> &str {
    action.name.as_deref().unwrap_or(&action.entrypoint)
}

fn execution_query(
    scope: ExecutionScopeId,
    action_id: &str,
    cursor: Option<ryvus_protocol::ExecutionId>,
    limit: usize,
) -> ExecutionHistoryQuery {
    ExecutionHistoryQuery {
        execution_scope_id: scope,
        action_id: Some(action_id.to_string()),
        action_revision: None,
        state: None,
        trigger: None,
        created_after: None,
        created_before: None,
        execution_id_prefix: None,
        cursor,
        limit,
    }
}

fn recent_health(executions: &[ExecutionAggregate]) -> RecentHealth {
    let succeeded = executions
        .iter()
        .filter(|execution| execution.state == ExecutionState::Succeeded)
        .count();
    let failed = executions
        .iter()
        .filter(|execution| {
            matches!(
                execution.state,
                ExecutionState::Failed | ExecutionState::TimedOut
            )
        })
        .count();
    let active = executions
        .iter()
        .filter(|execution| {
            matches!(
                execution.state,
                ExecutionState::Pending
                    | ExecutionState::Running
                    | ExecutionState::CancellationRequested
            )
        })
        .count();
    let terminal = executions
        .iter()
        .filter(|execution| {
            matches!(
                execution.state,
                ExecutionState::Succeeded
                    | ExecutionState::Failed
                    | ExecutionState::Cancelled
                    | ExecutionState::TimedOut
            )
        })
        .count();
    let mut durations = executions
        .iter()
        .filter_map(terminal_duration_ms)
        .collect::<Vec<_>>();
    durations.sort_unstable();
    let average_duration_ms = (!durations.is_empty()).then(|| {
        durations
            .iter()
            .map(|duration| *duration as f64)
            .sum::<f64>()
            / durations.len() as f64
    });
    let index = durations
        .len()
        .saturating_mul(95)
        .div_ceil(100)
        .saturating_sub(1);

    RecentHealth {
        window: DETAIL_HISTORY_LIMIT,
        sample_size: executions.len(),
        succeeded,
        failed,
        active,
        success_rate: (terminal > 0).then(|| succeeded as f64 / terminal as f64),
        average_duration_ms,
        p95_duration_ms: durations.get(index).copied(),
    }
}

fn terminal_duration_ms(execution: &ExecutionAggregate) -> Option<u64> {
    let attempt_id = execution.terminal_state.as_ref()?.attempt_id.as_ref()?;
    let duration = execution
        .attempts
        .iter()
        .find(|attempt| &attempt.attempt.attempt_id == attempt_id)?
        .result
        .as_ref()?
        .duration;
    Some(duration_ms(duration))
}

fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn system_time_unix_nanos(time: SystemTime) -> i128 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(duration) => i128::try_from(duration.as_nanos()).unwrap_or(i128::MAX),
        Err(error) => -i128::try_from(error.duration().as_nanos()).unwrap_or(i128::MAX),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, path::PathBuf, time::Duration};

    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use ryvus_execution::{
        action_revision, AttemptRecord, CreateExecutionResult, ExecutionDataReferences,
        ExecutionHistoryPage, ExecutionMutation, ExecutionPolicy, ExecutionResult,
        ExecutionStateStore, ExecutionTrigger, MemoryExecutionStateStore, NewExecution,
        RetryPolicy, StateStoreError, StateStoreResult, TerminalState, TransitionResult,
    };
    use ryvus_logging::{
        ExecutionLogRecord, ExecutionLogStore, InMemoryExecutionLogStore, LogBatch, LogRecordPage,
        LogRecordQuery, LogStoreError, LogStreamId, LogStreamMetadata, LogStreamPage,
        LogStreamTransition, MemoryLogStoreConfig,
    };
    use ryvus_protocol::{
        ActionExecutionPolicy, ActionKind, ActionManifest, ApiAction, AttemptOutcome, ExecutionId,
        ExecutionScopeId, InvocationError, InvocationRequest, InvocationResult, LogLevel,
        RuntimeHostId,
    };
    use tower::ServiceExt;

    use super::*;

    #[test]
    fn action_read_projects_detail_health_and_observed_revisions(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let action = test_action();
        let control = Arc::new(test_control(&action)?);
        let scope = ExecutionScopeId::new("action-read-test")?;
        let executions = Arc::new(MemoryExecutionStateStore::default());
        let logs = Arc::new(InMemoryExecutionLogStore::default());
        let current_revision = action_revision(&action)?;

        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "success",
            60,
            FixtureState::Terminal(AttemptOutcome::Succeeded, 10),
        )?;
        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "failed",
            50,
            FixtureState::Terminal(AttemptOutcome::Failed, 20),
        )?;
        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "timed-out",
            40,
            FixtureState::Terminal(AttemptOutcome::TimedOut, 30),
        )?;
        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "cancelled",
            30,
            FixtureState::Terminal(AttemptOutcome::Cancelled, 40),
        )?;
        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "running",
            20,
            FixtureState::Running,
        )?;
        create_execution_with_state(
            &executions,
            &action,
            &scope,
            &current_revision,
            "pending",
            10,
            FixtureState::Pending,
        )?;
        append_stream(
            &logs,
            &scope,
            "current-host",
            &current_revision,
            RuntimeKind::Node,
            15,
            Some(35),
        )?;
        append_stream(
            &logs,
            &scope,
            "old-host",
            "historical-revision",
            RuntimeKind::Node,
            5,
            Some(40),
        )?;
        let other_scope = ExecutionScopeId::new("other-action-read-test")?;
        create_execution_with_state(
            &executions,
            &action,
            &other_scope,
            "other-scope-revision",
            "other-scope-execution",
            1,
            FixtureState::Pending,
        )?;
        append_stream(
            &logs,
            &other_scope,
            "other-scope-host",
            "other-scope-revision",
            RuntimeKind::Rust,
            1,
            Some(100),
        )?;

        let service = ActionReadService::new(control, executions, logs, scope);
        let detail = service.detail("inventory")?;
        assert_eq!(detail.action_id, "inventory");
        assert_eq!(detail.current_revision, action_revision(&action)?);
        assert_eq!(detail.recent_health.sample_size, 6);
        assert_eq!(detail.recent_health.succeeded, 1);
        assert_eq!(detail.recent_health.failed, 2);
        assert_eq!(detail.recent_health.active, 2);
        assert_eq!(detail.recent_health.success_rate, Some(0.25));
        assert_eq!(detail.recent_health.average_duration_ms, Some(25.0));
        assert_eq!(detail.recent_health.p95_duration_ms, Some(40));
        assert_eq!(detail.recent_executions.len(), 5);

        let observed = service.revisions("inventory")?;
        assert_eq!(observed.revisions.len(), 2);
        assert!(matches!(
            observed.revisions[0].status,
            ObservedRevisionStatus::Current
        ));
        assert_eq!(observed.revisions[0].execution_count, 6);
        assert_eq!(observed.revisions[0].runtime_host_stream_count, 1);
        assert_eq!(observed.revisions[0].runtime, Some(RuntimeKind::Python));
        assert_eq!(
            observed.revisions[0]
                .first_observed_at_unix_nanos
                .as_deref(),
            Some("10")
        );
        assert!(observed.revisions[0].last_observed_at_unix_nanos.is_some());
        assert_eq!(observed.revisions[1].revision, "historical-revision");
        assert_eq!(observed.revisions[1].execution_count, 0);
        assert_eq!(observed.revisions[1].runtime_host_stream_count, 1);
        assert_eq!(
            observed.revisions[1]
                .first_observed_at_unix_nanos
                .as_deref(),
            Some("5")
        );
        assert_eq!(
            observed.revisions[1].last_observed_at_unix_nanos.as_deref(),
            Some("40")
        );
        assert!(observed
            .revisions
            .iter()
            .all(|revision| revision.revision != "other-scope-revision"));
        assert!(!observed.execution_history_truncated);
        assert!(!observed.log_history_truncated);
        Ok(())
    }

    #[test]
    fn action_read_bounds_observed_history_and_marks_truncation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let action = test_action();
        let control = Arc::new(test_control(&action)?);
        let scope = ExecutionScopeId::new("action-read-bounds")?;
        let executions = Arc::new(MemoryExecutionStateStore::default());
        let logs = Arc::new(InMemoryExecutionLogStore::new(MemoryLogStoreConfig {
            max_streams: 1_001,
            max_records: 1,
            max_tombstones: 1,
        })?);
        let revision = action_revision(&action)?;

        for index in 0_u64..1_001 {
            create_execution_with_state(
                &executions,
                &action,
                &scope,
                &revision,
                &format!("execution-{index:04}"),
                index,
                FixtureState::Pending,
            )?;
            append_stream(
                &logs,
                &scope,
                &format!("host-{index:04}"),
                &revision,
                RuntimeKind::Python,
                i64::try_from(index)?,
                None,
            )?;
        }

        let observed =
            ActionReadService::new(control, executions, logs, scope).revisions("inventory")?;
        assert_eq!(observed.revisions[0].execution_count, 1_000);
        assert_eq!(observed.revisions[0].runtime_host_stream_count, 1_000);
        assert!(observed.execution_history_truncated);
        assert!(observed.log_history_truncated);
        Ok(())
    }

    #[tokio::test]
    async fn action_read_routes_return_stable_safe_errors() -> Result<(), Box<dyn std::error::Error>>
    {
        let action = test_action();
        let control = Arc::new(test_control(&action)?);
        let scope = ExecutionScopeId::new("action-read-routes")?;
        let app = action_read_routes(Arc::new(ActionReadService::new(
            control.clone(),
            Arc::new(MemoryExecutionStateStore::default()),
            Arc::new(InMemoryExecutionLogStore::default()),
            scope.clone(),
        )));

        for uri in [
            "/internal/actions/detail",
            "/internal/actions/detail?action_id=%20",
        ] {
            let (status, body) = get_json(app.clone(), uri).await?;
            assert_eq!(status, StatusCode::BAD_REQUEST);
            assert_eq!(body["error"], "action_invalid_query");
        }

        let (status, body) =
            get_json(app, "/internal/actions/revisions?action_id=does-not-exist").await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"], "action_not_found");

        let execution_failure = action_read_routes(Arc::new(ActionReadService::new(
            control.clone(),
            Arc::new(UnavailableExecutionStore),
            Arc::new(InMemoryExecutionLogStore::default()),
            scope.clone(),
        )));
        let (status, body) = get_json(
            execution_failure,
            "/internal/actions/detail?action_id=inventory",
        )
        .await?;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "action_projection_unavailable");
        assert!(!body.to_string().contains("execution-provider-secret"));

        let log_failure = action_read_routes(Arc::new(ActionReadService::new(
            control,
            Arc::new(MemoryExecutionStateStore::default()),
            Arc::new(UnavailableLogStore),
            scope,
        )));
        let (status, body) = get_json(
            log_failure,
            "/internal/actions/revisions?action_id=inventory",
        )
        .await?;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "action_projection_unavailable");
        assert!(!body.to_string().contains("log-provider-secret"));
        Ok(())
    }

    async fn get_json(
        app: Router,
        uri: &str,
    ) -> Result<(StatusCode, serde_json::Value), Box<dyn std::error::Error>> {
        let response = app.oneshot(Request::get(uri).body(Body::empty())?).await?;
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await?;
        Ok((status, serde_json::from_slice(&body)?))
    }

    struct UnavailableExecutionStore;

    impl ExecutionStateStore for UnavailableExecutionStore {
        fn create(&self, _: NewExecution) -> StateStoreResult<ExecutionAggregate> {
            Err(execution_provider_error())
        }

        fn create_idempotent(&self, _: NewExecution) -> StateStoreResult<CreateExecutionResult> {
            Err(execution_provider_error())
        }

        fn load(&self, _: &ExecutionId) -> StateStoreResult<Option<ExecutionAggregate>> {
            Err(execution_provider_error())
        }

        fn compare_and_set(
            &self,
            _: &ExecutionId,
            _: u64,
            _: ExecutionMutation,
        ) -> StateStoreResult<TransitionResult> {
            Err(execution_provider_error())
        }

        fn reconcilable_cancellations(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
            Err(execution_provider_error())
        }

        fn active_executions(&self) -> StateStoreResult<Vec<ExecutionAggregate>> {
            Err(execution_provider_error())
        }

        fn list_history(&self, _: ExecutionHistoryQuery) -> StateStoreResult<ExecutionHistoryPage> {
            Err(execution_provider_error())
        }
    }

    fn execution_provider_error() -> StateStoreError {
        StateStoreError::Backend("execution-provider-secret".into())
    }

    struct UnavailableLogStore;

    impl ExecutionLogStore for UnavailableLogStore {
        fn append_batch(&self, _: LogBatch) -> Result<(), LogStoreError> {
            Err(log_provider_error())
        }

        fn list_streams(&self, _: LogStreamQuery) -> Result<LogStreamPage, LogStoreError> {
            Err(log_provider_error())
        }

        fn list_records(&self, _: LogRecordQuery) -> Result<LogRecordPage, LogStoreError> {
            Err(log_provider_error())
        }
    }

    fn log_provider_error() -> LogStoreError {
        LogStoreError::InvalidQuery("log-provider-secret".into())
    }

    fn test_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ApiAction {
                method: "GET".into(),
                path: "/inventory".into(),
                consumes: vec!["application/json".into()],
                produces: vec!["application/json".into()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: PathBuf::from("src/inventory.py"),
            entrypoint: "inventory_handler".into(),
            name: Some("inventory".into()),
            policy: ActionExecutionPolicy::default(),
        }
    }

    fn test_control(
        action: &ActionDefinition,
    ) -> Result<ControlService, Box<dyn std::error::Error>> {
        static NEXT_PROJECT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "ryvus-action-read-{}-{}",
            std::process::id(),
            NEXT_PROJECT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join(".ryvus"))?;
        fs::write(
            root.join(".ryvus/action-manifest.json"),
            serde_json::to_vec(&ActionManifest {
                actions: vec![action.clone()],
            })?,
        )?;
        Ok(ControlService::load_local(crate::LocalControlConfig {
            project_root: root,
            manifest_path: PathBuf::from(".ryvus/action-manifest.json"),
        })?)
    }

    #[derive(Clone, Copy)]
    enum FixtureState {
        Pending,
        Running,
        Terminal(AttemptOutcome, u64),
    }

    fn create_execution_with_state(
        store: &MemoryExecutionStateStore,
        action: &ActionDefinition,
        scope: &ExecutionScopeId,
        revision: &str,
        id: &str,
        created_at_nanos: u64,
        state: FixtureState,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut request = InvocationRequest::new(json!({}));
        request.execution_id = ryvus_protocol::ExecutionId::from(id);
        let aggregate = store.create(NewExecution {
            action: action.clone(),
            action_revision: revision.into(),
            execution_scope_id: scope.clone(),
            action_id: "inventory".into(),
            trigger: ExecutionTrigger::Api,
            creation_fingerprint: format!("fingerprint-{id}"),
            data_refs: ExecutionDataReferences::default(),
            request,
            policy: ExecutionPolicy {
                timeout: Duration::from_secs(3),
                retry: RetryPolicy {
                    max_attempts: 1,
                    initial_delay: Duration::from_secs(1),
                    backoff: 2.0,
                },
            },
            created_at: UNIX_EPOCH + Duration::from_nanos(created_at_nanos),
        })?;
        let attempt = AttemptRecord::pending(aggregate.request.attempt(), 1_000);
        if matches!(state, FixtureState::Running | FixtureState::Terminal(_, _)) {
            let running = applied(store.compare_and_set(
                &aggregate.execution_id,
                aggregate.execution_version,
                ExecutionMutation::StartAttempt {
                    attempt: attempt.clone(),
                },
            )?);
            if let FixtureState::Terminal(outcome, duration_ms) = state {
                let state = outcome_state(outcome);
                let invocation_result = if outcome == AttemptOutcome::Succeeded {
                    InvocationResult::success(&aggregate.request, json!({}))
                } else {
                    InvocationResult::failed(
                        &aggregate.request,
                        InvocationError::new("fixture_failure", "fixture failure", false),
                    )
                };
                store.compare_and_set(
                    &aggregate.execution_id,
                    running.execution_version,
                    ExecutionMutation::FinishAttempt {
                        attempt_id: attempt.attempt.attempt_id.clone(),
                        outcome,
                        result: Some(ExecutionResult {
                            invocation_result,
                            events: Vec::new(),
                            stdout: String::new(),
                            stderr: String::new(),
                            duration: Duration::from_millis(duration_ms),
                            exit_code: None,
                        }),
                        retry: None,
                        terminal: Some(TerminalState::new(state, Some(attempt.attempt.attempt_id))),
                    },
                )?;
            }
        }
        Ok(())
    }

    fn applied(result: TransitionResult) -> ExecutionAggregate {
        match result {
            TransitionResult::Applied { aggregate } | TransitionResult::Unchanged { aggregate } => {
                aggregate
            }
            TransitionResult::Conflict { current_version } => {
                panic!("unexpected fixture transition conflict at version {current_version}")
            }
        }
    }

    fn outcome_state(outcome: AttemptOutcome) -> ExecutionState {
        match outcome {
            AttemptOutcome::Succeeded => ExecutionState::Succeeded,
            AttemptOutcome::Failed | AttemptOutcome::InfrastructureFailed => ExecutionState::Failed,
            AttemptOutcome::Cancelled => ExecutionState::Cancelled,
            AttemptOutcome::TimedOut => ExecutionState::TimedOut,
        }
    }

    fn append_stream(
        store: &InMemoryExecutionLogStore,
        scope: &ExecutionScopeId,
        host: &str,
        revision: &str,
        runtime: RuntimeKind,
        started_at: i64,
        ended_at: Option<i64>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        store.append_batch(LogBatch {
            stream: LogStreamMetadata {
                stream_id: LogStreamId::new(scope.clone(), RuntimeHostId::from(host)),
                action_key_id: "inventory".into(),
                action_revision: revision.into(),
                runtime_language: runtime.clone(),
                started_at_unix_nanos: started_at,
            },
            batch_id: format!("batch-{host}"),
            records: Vec::new(),
            loss_ranges: Vec::new(),
            transition: Some(LogStreamTransition::Active),
        })?;
        if let Some(ended_at) = ended_at {
            let stream = LogStreamMetadata {
                stream_id: LogStreamId::new(scope.clone(), RuntimeHostId::from(host)),
                action_key_id: "inventory".into(),
                action_revision: revision.into(),
                runtime_language: runtime,
                started_at_unix_nanos: started_at,
            };
            store.append_batch(LogBatch {
                records: vec![ExecutionLogRecord {
                    timestamp_unix_nanos: ended_at,
                    observed_timestamp_unix_nanos: ended_at,
                    stream_sequence: 1,
                    stream_id: stream.stream_id.clone(),
                    action_key_id: stream.action_key_id.clone(),
                    action_revision: stream.action_revision.clone(),
                    runtime_language: stream.runtime_language.clone(),
                    runtime_session_id: None,
                    correlation: None,
                    severity: LogLevel::Info,
                    message: "stream completed".into(),
                    attributes: BTreeMap::new(),
                    trace_id: None,
                    span_id: None,
                }],
                stream,
                batch_id: format!("end-{host}"),
                loss_ranges: Vec::new(),
                transition: Some(LogStreamTransition::Complete),
            })?;
        }
        Ok(())
    }
}
