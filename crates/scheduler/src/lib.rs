use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ryvus_execution_service::ExecutionService;
use ryvus_executor::{Executor, RuntimeResolver};
use ryvus_persistence::ExecutionPersistence;
use ryvus_protocol::{ActionDefinition, ActionKind, InvocationRequest, InvocationResult};
use serde_json::json;
use thiserror::Error;

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
}

pub type SchedulerResult<T> = Result<T, SchedulerError>;

pub trait ScheduleExecutor: Send + Sync + 'static {
    fn execute_scheduled(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
    ) -> SchedulerResult<InvocationResult>;
}

impl<RR, E, EP> ScheduleExecutor for ExecutionService<RR, E, EP>
where
    RR: RuntimeResolver + Send + Sync + 'static,
    E: Executor + Send + Sync + 'static,
    EP: ExecutionPersistence + Send + Sync + 'static,
{
    fn execute_scheduled(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
    ) -> SchedulerResult<InvocationResult> {
        self.execute(action, request)
            .map(|execution| execution.result.invocation_result)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
struct ScheduledAction {
    action: ActionDefinition,
    expression: String,
    interval: ScheduleInterval,
    next_run: Instant,
}

#[derive(Debug, Clone)]
pub struct Scheduler {
    actions: Vec<ScheduledAction>,
}

impl Scheduler {
    pub fn from_actions<'a>(
        actions: impl IntoIterator<Item = &'a ActionDefinition>,
    ) -> SchedulerResult<Self> {
        let mut scheduled = Vec::new();
        let now = Instant::now();

        for action in actions {
            let ActionKind::Schedule(schedule) = &action.kind else {
                continue;
            };

            let interval = ScheduleInterval::parse(&schedule.expression)?;

            scheduled.push(ScheduledAction {
                action: action.clone(),
                expression: schedule.expression.clone(),
                interval,
                next_run: now + interval.duration(),
            });
        }

        Ok(Self { actions: scheduled })
    }

    pub fn action_count(&self) -> usize {
        self.actions.len()
    }

    pub async fn run<E>(mut self, executor: Arc<E>) -> SchedulerResult<()>
    where
        E: ScheduleExecutor,
    {
        if self.actions.is_empty() {
            std::future::pending::<()>().await;
            return Ok(());
        }

        loop {
            let next_run = self
                .actions
                .iter()
                .map(|action| action.next_run)
                .min()
                .expect("scheduler should have actions");

            tokio::time::sleep_until(tokio::time::Instant::from_std(next_run)).await;

            let now = Instant::now();
            let due = self
                .actions
                .iter_mut()
                .filter(|action| action.next_run <= now)
                .map(|action| {
                    action.next_run += action.interval.duration();
                    (action.action.clone(), action.expression.clone())
                })
                .collect::<Vec<_>>();

            for (action, expression) in due {
                let executor = Arc::clone(&executor);
                let request = schedule_request(&expression);
                let action_name = action_key(&action);

                let result = tokio::task::spawn_blocking(move || {
                    executor.execute_scheduled(&action, &request)
                })
                .await;

                match result {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => eprintln!("{error}"),
                    Err(error) => {
                        eprintln!("scheduled execution failed for {action_name}: {error}")
                    }
                }
            }
        }
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
) -> SchedulerResult<InvocationResult>
where
    E: ScheduleExecutor,
{
    let action = resolve_schedule(actions, selector)?;
    let ActionKind::Schedule(schedule) = &action.kind else {
        unreachable!("resolve_schedule only returns schedule actions");
    };
    let request = manual_schedule_request(&schedule.expression);

    executor.execute_scheduled(&action, &request)
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

fn schedule_request(expression: &str) -> InvocationRequest {
    InvocationRequest::new(json!({
        "trigger": "schedule",
        "scheduled_at": unix_timestamp_millis(),
        "expression": expression,
    }))
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
        ActionDefinition, ActionKind, InvocationRequest, InvocationResult, InvocationStatus,
        RuntimeKind, ScheduleAction, PROTOCOL_VERSION,
    };
    use serde_json::json;

    use super::{
        run_schedule_once, schedule_infos, validate_schedule_actions, ScheduleExecutor,
        ScheduleInterval, Scheduler, SchedulerError, SchedulerResult,
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

        assert_eq!(result.output, Some(json!({ "ok": true })));

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
                expression: expression.to_string(),
            }),
            source: "src/schedule.py".into(),
            entrypoint: "tick".to_string(),
            name: None,
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
                expression: expression.to_string(),
            }),
            source: source.into(),
            entrypoint: entrypoint.to_string(),
            name: name.map(str::to_string),
        }
    }

    fn api_action() -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Api(ryvus_protocol::ApiAction {
                method: "GET".to_string(),
                path: "/hello".to_string(),
                request_schema: None,
                response_schema: None,
                query_params: Vec::new(),
            }),
            source: "src/hello.py".into(),
            entrypoint: "hello".to_string(),
            name: None,
        }
    }

    #[derive(Default)]
    struct RecordingScheduleExecutor {
        requests: Mutex<Vec<InvocationRequest>>,
    }

    impl ScheduleExecutor for RecordingScheduleExecutor {
        fn execute_scheduled(
            &self,
            _action: &ActionDefinition,
            request: &InvocationRequest,
        ) -> SchedulerResult<InvocationResult> {
            self.requests
                .lock()
                .expect("requests should lock")
                .push(request.clone());

            Ok(InvocationResult {
                protocol_version: PROTOCOL_VERSION.to_string(),
                invocation_id: request.invocation_id.clone(),
                status: InvocationStatus::Success,
                output: Some(json!({ "ok": true })),
                error: None,
            })
        }
    }
}
