use ryvus_protocol::InvocationEvent;

pub trait InvocationEventSink: Send + Sync {
    fn record(&self, event: &InvocationEvent);
}

#[derive(Debug, Clone, Default)]
pub struct ConsoleInvocationEventSink;

impl InvocationEventSink for ConsoleInvocationEventSink {
    fn record(&self, event: &InvocationEvent) {
        match event {
            InvocationEvent::Log(_) => {}
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
