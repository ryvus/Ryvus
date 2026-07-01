use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use ryvus_execution_service::ExecutionService;
use ryvus_executor::{Executor, RuntimeResolver};
use ryvus_persistence::ExecutionPersistence;
use ryvus_protocol::{ActionDefinition, ActionKind, InvocationRequest};
use serde_json::json;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("invalid schedule expression '{expression}': expected 'every <number><s|m|h>'")]
    InvalidExpression { expression: String },

    #[error("scheduled execution failed for {action}: {message}")]
    ExecutionFailed { action: String, message: String },
}

pub type SchedulerResult<T> = Result<T, SchedulerError>;

pub trait ScheduleExecutor: Send + Sync + 'static {
    fn execute_scheduled(
        &self,
        action: &ActionDefinition,
        request: &InvocationRequest,
    ) -> SchedulerResult<()>;
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
    ) -> SchedulerResult<()> {
        self.execute(action, request)
            .map(|_| ())
            .map_err(|error| SchedulerError::ExecutionFailed {
                action: action_key(action),
                message: error.to_string(),
            })
    }
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
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("{error}"),
                    Err(error) => {
                        eprintln!("scheduled execution failed for {action_name}: {error}")
                    }
                }
            }
        }
    }
}

pub fn validate_schedule_actions<'a>(
    actions: impl IntoIterator<Item = &'a ActionDefinition>,
) -> SchedulerResult<()> {
    Scheduler::from_actions(actions).map(|_| ())
}

fn schedule_request(expression: &str) -> InvocationRequest {
    InvocationRequest::new(json!({
        "trigger": "schedule",
        "scheduled_at": unix_timestamp_millis(),
        "expression": expression,
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

#[cfg(test)]
mod tests {
    use ryvus_protocol::{ActionDefinition, ActionKind, RuntimeKind, ScheduleAction};

    use super::{validate_schedule_actions, ScheduleInterval, Scheduler};

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

    fn schedule_action(expression: &str) -> ActionDefinition {
        ActionDefinition {
            runtime: RuntimeKind::Python,
            kind: ActionKind::Schedule(ScheduleAction {
                expression: expression.to_string(),
            }),
            source: "src/schedule.py".into(),
            entrypoint: "tick".to_string(),
        }
    }
}
