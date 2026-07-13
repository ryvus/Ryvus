use ryvus_protocol::{InvocationEvent, LogLevel};

pub trait InvocationEventSink: Send + Sync {
    fn record(&self, event: &InvocationEvent);
}

#[derive(Debug, Clone, Default)]
pub struct ConsoleInvocationEventSink;

impl InvocationEventSink for ConsoleInvocationEventSink {
    fn record(&self, event: &InvocationEvent) {
        match event {
            InvocationEvent::Log(log) => match log.level {
                LogLevel::Trace => tracing::trace!(
                    execution_id = %log.execution_id,
                    attempt_id = %log.attempt_id,
                    attempt_number = log.attempt_number,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Debug => tracing::debug!(
                    execution_id = %log.execution_id,
                    attempt_id = %log.attempt_id,
                    attempt_number = log.attempt_number,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Info => tracing::info!(
                    execution_id = %log.execution_id,
                    attempt_id = %log.attempt_id,
                    attempt_number = log.attempt_number,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Warn => tracing::warn!(
                    execution_id = %log.execution_id,
                    attempt_id = %log.attempt_id,
                    attempt_number = log.attempt_number,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Error => tracing::error!(
                    execution_id = %log.execution_id,
                    attempt_id = %log.attempt_id,
                    attempt_number = log.attempt_number,
                    fields = %log.fields,
                    "{}", log.message
                ),
            },
            InvocationEvent::Metric(metric) => {
                tracing::info!(
                    execution_id = %metric.execution_id,
                    attempt_id = %metric.attempt_id,
                    attempt_number = metric.attempt_number,
                    name = %metric.name,
                    value = metric.value,
                    unit = %metric.unit,
                    "action metric"
                );
            }
        }
    }
}
