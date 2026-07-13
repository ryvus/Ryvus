use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ryvus_protocol::InvocationRequest;

use crate::{ExecutorError, ExecutorResult};

pub fn assign_attempt_deadline(
    request: &mut InvocationRequest,
    timeout: Duration,
) -> ExecutorResult<()> {
    let budget =
        u64::try_from(timeout.as_millis()).map_err(|_| ExecutorError::DeadlineOutOfRange)?;
    let budget_i64 = i64::try_from(budget).map_err(|_| ExecutorError::DeadlineOutOfRange)?;
    let deadline = unix_time_ms()?
        .checked_add(budget_i64)
        .ok_or(ExecutorError::DeadlineOutOfRange)?;
    request.set_deadline(deadline, budget);
    Ok(())
}

pub fn refresh_transport_budget(request: &mut InvocationRequest) -> ExecutorResult<()> {
    request.refresh_remaining_budget(unix_time_ms()?);
    Ok(())
}

pub fn unix_time_ms() -> ExecutorResult<i64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| ExecutorError::SystemClockBeforeUnixEpoch)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| ExecutorError::DeadlineOutOfRange)
}
