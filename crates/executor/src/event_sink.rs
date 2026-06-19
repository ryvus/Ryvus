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
                    invocation_id = %log.invocation_id,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Debug => tracing::debug!(
                    invocation_id = %log.invocation_id,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Info => tracing::info!(
                    invocation_id = %log.invocation_id,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Warn => tracing::warn!(
                    invocation_id = %log.invocation_id,
                    fields = %log.fields,
                    "{}", log.message
                ),
                LogLevel::Error => tracing::error!(
                    invocation_id = %log.invocation_id,
                    fields = %log.fields,
                    "{}", log.message
                ),
            },
            InvocationEvent::Metric(metric) => {
                tracing::info!(
                    invocation_id = %metric.invocation_id,
                    name = %metric.name,
                    value = metric.value,
                    unit = %metric.unit,
                    "action metric"
                );
            }
        }
    }
}
