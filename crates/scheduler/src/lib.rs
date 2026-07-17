pub mod http;
pub mod model;
pub mod service;
pub mod store;

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ryvus_execution::{
    ActorRef, ExecutionDataReferences, ExecutionPersistence, ExecutionScopeId, ExecutionService,
    ExecutionSubmission, ExecutionSubmissionResult, ExecutionTrigger, Executor,
    ManualExecutionSource, RuntimeResolver, ScheduleId, ScheduleTriggerId,
};
use ryvus_protocol::{ActionDefinition, ActionKind, InvocationRequest, InvocationResult};
use serde_json::json;
use thiserror::Error;

pub use model::*;
pub use service::*;
pub use store::*;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("invalid schedule expression '{expression}': expected 'every <number><s|m|h>'")]
    InvalidExpression { expression: String },

    #[error("scheduled execution failed for {action}: {message}")]
    ExecutionFailed { action: String, message: String },

    #[error("schedule not found: {selector}")]
    ScheduleNotFound { selector: String },

    #[error("schedule selector '{selector}' matched multiple schedules: {matches}")]
    AmbiguousScheduleSelector { selector: String, matches: String },

    #[error("schedule store lock is poisoned")]
    StoreLockPoisoned,

    #[error("schedule '{schedule_id}' was not found")]
    DurableScheduleNotFound { schedule_id: ScheduleId },

    #[error("schedule trigger '{trigger_id}' was not found")]
    TriggerNotFound { trigger_id: ScheduleTriggerId },

    #[error("invalid schedule cursor: {0}")]
    InvalidCursor(String),

    #[error("schedule state conflict: {0}")]
    Conflict(String),

    #[error("schedule store backend error: {0}")]
    StoreBackend(String),
}

pub type SchedulerResult<T> = Result<T, SchedulerError>;

#[derive(Debug, Clone)]
pub struct ScheduleExecution {
    pub execution_id: ryvus_protocol::ExecutionId,
    pub result: Option<InvocationResult>,
}

pub trait ScheduleExecutor: Send + Sync + 'static {
    fn submit(
        &self,
        action: &ActionDefinition,
        submission: ExecutionSubmission,
    ) -> SchedulerResult<ScheduleExecution>;
}

impl<RR, E, EP> ScheduleExecutor for ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver + Send + Sync + 'static,
    E: Executor + Send + Sync + 'static,
    EP: ExecutionPersistence + Send + Sync + 'static,
{
    fn submit(
        &self,
        action: &ActionDefinition,
        submission: ExecutionSubmission,
    ) -> SchedulerResult<ScheduleExecution> {
        self.execute_submission(action, submission)
            .map(|outcome| match outcome {
                ExecutionSubmissionResult::Executed(record) => ScheduleExecution {
                    execution_id: record.attempt.execution_id,
                    result: Some(record.result.invocation_result),
                },
                ExecutionSubmissionResult::Existing(aggregate)
                | ExecutionSubmissionResult::AlreadyActive(aggregate) => ScheduleExecution {
                    execution_id: aggregate.execution_id,
                    result: aggregate
                        .attempts
                        .last()
                        .and_then(|attempt| attempt.result.as_ref())
                        .map(|result| result.invocation_result.clone()),
                },
            })
            .map_err(|error| SchedulerError::ExecutionFailed {
                action: action_key(action),
                message: error.to_string(),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleInfo {
    pub id: String,
    pub name: String,
    pub source: String,
    pub entrypoint: String,
    pub expression: String,
    pub action_key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScheduleInterval(Duration);

impl ScheduleInterval {
    pub fn parse(expression: &str) -> SchedulerResult<Self> {
        let Some(raw) = expression.strip_prefix("every ") else {
            return Err(SchedulerError::InvalidExpression {
                expression: expression.to_string(),
            });
        };

        let (number, unit) = raw.split_at(raw.len().saturating_sub(1));
        let value = number
            .parse::<u64>()
            .map_err(|_| SchedulerError::InvalidExpression {
                expression: expression.to_string(),
            })?;

        if value == 0 {
            return Err(SchedulerError::InvalidExpression {
                expression: expression.to_string(),
            });
        }

        let seconds = match unit {
            "s" => value,
            "m" => value * 60,
            "h" => value * 60 * 60,
            _ => {
                return Err(SchedulerError::InvalidExpression {
                    expression: expression.to_string(),
                })
            }
        };

        Ok(Self(Duration::from_secs(seconds)))
    }

    pub fn duration(self) -> Duration {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    action_count: usize,
}

impl Scheduler {
    pub fn from_actions<'a>(
        actions: impl IntoIterator<Item = &'a ActionDefinition>,
    ) -> SchedulerResult<Self> {
        let mut action_count = 0;

        for action in actions {
            let ActionKind::Schedule(schedule) = &action.kind else {
                continue;
            };

            ScheduleInterval::parse(&schedule.expression)?;
            action_count += 1;
        }

        Ok(Self { action_count })
    }

    pub fn action_count(&self) -> usize {
        self.action_count
    }
}

pub fn schedule_infos<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> SchedulerResult<Vec<ScheduleInfo>> {
    let schedules = actions
        .into_iter()
        .filter_map(|action| {
            let ActionKind::Schedule(schedule) = &action.kind else {
                return None;
            };

            if let Err(error) = ScheduleInterval::parse(&schedule.expression) {
                return Some(Err(error));
            }

            let name = action
                .name
                .clone()
                .unwrap_or_else(|| action.entrypoint.clone());

            Some(Ok((
                action,
                schedule.expression.clone(),
                name,
                action.source.display().to_string(),
                action_key(action),
            )))
        })
        .collect::<SchedulerResult<Vec<_>>>()?;

    let mut base_ids = HashMap::<String, usize>::new();
    for (_, _, name, _, _) in &schedules {
        *base_ids.entry(sanitize_id(name)).or_default() += 1;
    }

    Ok(schedules
        .into_iter()
        .map(|(action, expression, name, source, action_key)| {
            let base_id = sanitize_id(&name);
            let id = if base_ids.get(&base_id).copied().unwrap_or_default() > 1 {
                format!("{base_id}_{}", sanitize_id(&source))
            } else {
                base_id
            };

            ScheduleInfo {
                id,
                name,
                source,
                entrypoint: action.entrypoint.clone(),
                expression,
                action_key,
            }
        })
        .collect())
}

pub fn run_schedule_once<'a, E>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
    selector: &str,
    executor: Arc<E>,
) -> SchedulerResult<ScheduleExecution>
where
    E: ScheduleExecutor,
{
    let action = resolve_schedule(actions, selector)?;
    let ActionKind::Schedule(schedule) = &action.kind else {
        unreachable!("resolve_schedule only returns schedule actions");
    };
    let request = manual_schedule_request(&schedule.expression);

    let policy =
        ryvus_execution::ExecutionPolicy::from_action_policy(&action.policy).map_err(|error| {
            SchedulerError::ExecutionFailed {
                action: action_key(&action),
                message: error.to_string(),
            }
        })?;
    let scope = ExecutionScopeId::new("local")
        .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?;
    let actor = ActorRef::new("local-user")
        .map_err(|error| SchedulerError::StoreBackend(error.to_string()))?;
    executor.submit(
        &action,
        ExecutionSubmission {
            scope,
            action_id: action_key(&action),
            trigger: ExecutionTrigger::Manual {
                actor,
                source: ManualExecutionSource::Direct,
            },
            request,
            policy,
            data_refs: ExecutionDataReferences::default(),
        },
    )
}

pub fn validate_schedule_actions<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> SchedulerResult<()> {
    Scheduler::from_actions(actions).map(|_| ())
}

fn resolve_schedule<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
    selector: &str,
) -> SchedulerResult<ActionDefinition> {
    let all_actions = actions.into_iter().cloned().collect::<Vec<_>>();
    let infos = schedule_infos(&all_actions)?;
    let matching_keys = infos
        .iter()
        .filter(|info| {
            info.id == selector
                || info.name == selector
                || info.action_key == selector
                || info.source == selector
        })
        .map(|info| info.action_key.clone())
        .collect::<Vec<_>>();

    let schedules = all_actions
        .into_iter()
        .filter(|action| matching_keys.iter().any(|key| key == &action_key(action)))
        .collect::<Vec<_>>();

    match schedules.len() {
        0 => Err(SchedulerError::ScheduleNotFound {
            selector: selector.to_string(),
        }),
        1 => Ok(schedules
            .into_iter()
            .next()
            .expect("one schedule should exist")),
        _ => Err(SchedulerError::AmbiguousScheduleSelector {
            selector: selector.to_string(),
            matches: schedules
                .iter()
                .map(action_key)
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

fn manual_schedule_request(expression: &str) -> InvocationRequest {
    InvocationRequest::new(json!({
        "trigger": "schedule",
        "scheduled_at": unix_timestamp_millis(),
        "expression": expression,
        "manual": true,
    }))
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn action_key(action: &ActionDefinition) -> String {
    format!("{}::{}", action.source.display(), action.entrypoint)
}

fn sanitize_id(value: &str) -> String {
    let mut output = String::new();

    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if !output.ends_with('_') {
            output.push('_');
        }
    }

    output.trim_matches('_').to_string()
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use ryvus_protocol::{
        ActionDefinition, ActionKind, InvocationRequest, InvocationResult, RuntimeKind,
        ScheduleAction,
    };
    use serde_json::json;

    use super::{
        run_schedule_once, schedule_infos, validate_schedule_actions, ExecutionSubmission,
        ScheduleExecution, ScheduleExecutor, ScheduleInterval, Scheduler, SchedulerError,
        SchedulerResult,
    };

    #[test]
    fn parses_interval_expressions() {
        assert_eq!(
            ScheduleInterval::parse("every 10s")
                .unwrap()
                .duration()
                .as_secs(),
            10
        );
        assert_eq!(
            ScheduleInterval::parse("every 5m")
                .unwrap()
                .duration()
                .as_secs(),
            300
        );
        assert_eq!(
            ScheduleInterval::parse("every 1h")
                .unwrap()
                .duration()
                .as_secs(),
            3600
        );
    }

    #[test]
    fn rejects_invalid_interval_expressions() {
        assert!(ScheduleInterval::parse("10s").is_err());
        assert!(ScheduleInterval::parse("every 0s").is_err());
        assert!(ScheduleInterval::parse("every xs").is_err());
        assert!(ScheduleInterval::parse("every 1d").is_err());
    }

    #[test]
    fn builds_scheduler_from_schedule_actions_only() {
        let scheduler = Scheduler::from_actions([&schedule_action("every 10s")]).unwrap();

        assert_eq!(scheduler.action_count(), 1);
    }

    #[test]
    fn validates_schedule_expressions() {
        let action = schedule_action("daily");

        assert!(validate_schedule_actions([&action]).is_err());
    }

    #[test]
    fn lists_schedule_infos_only_for_schedule_actions() {
        let schedule = named_schedule_action(
            Some("restock"),
            "src/schedules/restock.py",
            "tick",
            "every 10s",
        );
        let api = api_action();

        let schedules = schedule_infos([&schedule, &api]).expect("schedule info should build");

        assert_eq!(schedules.len(), 1);
        assert_eq!(schedules[0].id, "restock");
        assert_eq!(schedules[0].name, "restock");
        assert_eq!(schedules[0].source, "src/schedules/restock.py");
        assert_eq!(schedules[0].entrypoint, "tick");
        assert_eq!(schedules[0].expression, "every 10s");
        assert_eq!(schedules[0].action_key, "src/schedules/restock.py::tick");
    }

    #[test]
    fn schedule_ids_get_source_suffix_when_names_collide() {
        let first = named_schedule_action(Some("sync"), "src/a.py", "tick", "every 10s");
        let second = named_schedule_action(Some("sync"), "src/b.py", "tick", "every 10s");

        let schedules = schedule_infos([&first, &second]).expect("schedule info should build");

        assert_eq!(schedules[0].id, "sync_src_a_py");
        assert_eq!(schedules[1].id, "sync_src_b_py");
    }

    #[test]
    fn manual_run_resolves_by_id_name_action_key_and_source() {
        let action = named_schedule_action(
            Some("restock"),
            "src/modules/petstore/schedules/restock_report.py",
            "restock_report",
            "every 10s",
        );
        let executor = Arc::new(RecordingScheduleExecutor::default());

        run_schedule_once([&action], "restock", Arc::clone(&executor)).expect("id should run");
        run_schedule_once([&action], "restock", Arc::clone(&executor)).expect("name should run");
        run_schedule_once(
            [&action],
            "src/modules/petstore/schedules/restock_report.py::restock_report",
            Arc::clone(&executor),
        )
        .expect("action key should run");
        run_schedule_once(
            [&action],
            "src/modules/petstore/schedules/restock_report.py",
            Arc::clone(&executor),
        )
        .expect("source should run");

        assert_eq!(
            executor
                .requests
                .lock()
                .expect("requests should lock")
                .len(),
            4
        );
    }

    #[test]
    fn manual_run_marks_event_as_manual() {
        let action = named_schedule_action(Some("restock"), "src/restock.py", "tick", "every 10s");
        let executor = Arc::new(RecordingScheduleExecutor::default());

        let result = run_schedule_once([&action], "restock", Arc::clone(&executor))
            .expect("schedule should run");

        assert_eq!(
            result.result.and_then(|result| result.output),
            Some(json!({ "ok": true }))
        );

        let requests = executor.requests.lock().expect("requests should lock");
        assert_eq!(requests[0].event["trigger"], json!("schedule"));
        assert_eq!(requests[0].event["expression"], json!("every 10s"));
        assert_eq!(requests[0].event["manual"], json!(true));
        assert!(requests[0].event["scheduled_at"].is_number());
    }

    #[test]
    fn manual_run_reports_ambiguous_selector() {
        let first = named_schedule_action(Some("sync"), "src/a.py", "tick", "every 10s");
        let second = named_schedule_action(Some("sync"), "src/b.py", "tick", "every 10s");
        let executor = Arc::new(RecordingScheduleExecutor::default());

        let error = run_schedule_once([&first, &second], "sync", executor)
            .expect_err("selector should be ambiguous");

        assert!(matches!(
            error,
            SchedulerError::AmbiguousScheduleSelector { .. }
        ));
    }

    fn schedule_action(expression: &str) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                key: "restock_report:default".to_string(),
                expression: expression.to_string(),
            }),
            source: "src/schedule.py".into(),
            entrypoint: "tick".to_string(),
            name: None,
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    fn named_schedule_action(
        name: Option<&str>,
        source: &str,
        entrypoint: &str,
        expression: &str,
    ) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                key: "restock_report:default".to_string(),
                expression: expression.to_string(),
            }),
            source: source.into(),
            entrypoint: entrypoint.to_string(),
            name: name.map(str::to_string),
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    fn api_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ryvus_protocol::ApiAction {
                method: "GET".to_string(),
                path: "/hello".to_string(),
                consumes: vec!["application/json".to_string()],
                produces: vec!["application/json".to_string()],
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
                authorizer: None,
            }),
            source: "src/hello.py".into(),
            entrypoint: "hello".to_string(),
            name: None,
            policy: ryvus_protocol::ActionExecutionPolicy::default(),
        }
    }

    #[derive(Default)]
    struct RecordingScheduleExecutor {
        requests: Mutex<Vec<InvocationRequest>>,
    }

    impl ScheduleExecutor for RecordingScheduleExecutor {
        fn submit(
            &self,
            _action: &ActionDefinition,
            submission: ExecutionSubmission,
        ) -> SchedulerResult<ScheduleExecution> {
            let request = submission.request;
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request.clone());

            Ok(ScheduleExecution {
                execution_id: request.execution_id.clone(),
                result: Some(InvocationResult::success(&request, json!({ "ok": true }))),
            })
        }
    }
}
