use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ryvus_protocol::InvocationRequest;
use tokio::time::Instant;

use crate::RuntimeHostError;

pub const DEFAULT_CLOCK_SKEW_TOLERANCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy)]
pub struct DeadlineValidator {
    clock_skew_tolerance: Duration,
}

#[derive(Debug, Clone, Copy)]
pub struct ValidatedDeadline {
    pub monotonic: Instant,
    pub effective_budget: Duration,
}

impl DeadlineValidator {
    pub fn new(clock_skew_tolerance: Duration) -> Self {
        Self {
            clock_skew_tolerance,
        }
    }

    pub fn validate(
        &self,
        request: &InvocationRequest,
    ) -> Result<ValidatedDeadline, RuntimeHostError> {
        let wall_now = unix_time_ms()?;
        self.validate_at(request, wall_now, Instant::now())
    }

    fn validate_at(
        &self,
        request: &InvocationRequest,
        wall_now_unix_ms: i64,
        monotonic_now: Instant,
    ) -> Result<ValidatedDeadline, RuntimeHostError> {
        let sender_budget = request.remaining_budget_ms;
        if sender_budget == 0 {
            return Err(RuntimeHostError::EmptyDeadlineBudget);
        }

        let host_budget = request.deadline_unix_ms.saturating_sub(wall_now_unix_ms);
        if host_budget <= 0 {
            return Err(RuntimeHostError::DeadlineExpired);
        }
        let host_budget = u64::try_from(host_budget).map_err(|_| RuntimeHostError::ClockSkew)?;
        let tolerance = u64::try_from(self.clock_skew_tolerance.as_millis())
            .map_err(|_| RuntimeHostError::ClockSkew)?;
        if host_budget > sender_budget.saturating_add(tolerance) {
            return Err(RuntimeHostError::ClockSkew);
        }

        let effective_budget = Duration::from_millis(sender_budget.min(host_budget));
        let monotonic = monotonic_now
            .checked_add(effective_budget)
            .ok_or(RuntimeHostError::ClockSkew)?;
        Ok(ValidatedDeadline {
            monotonic,
            effective_budget,
        })
    }
}

impl Default for DeadlineValidator {
    fn default() -> Self {
        Self::new(DEFAULT_CLOCK_SKEW_TOLERANCE)
    }
}

fn unix_time_ms() -> Result<i64, RuntimeHostError> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| RuntimeHostError::ClockSkew)?;
    i64::try_from(elapsed.as_millis()).map_err(|_| RuntimeHostError::ClockSkew)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn accepts_transport_delay_and_uses_the_smaller_budget() {
        let request = request(10_800, 1_000);
        let now = Instant::now();

        let validated = DeadlineValidator::default()
            .validate_at(&request, 10_000, now)
            .unwrap();

        assert_eq!(validated.effective_budget, Duration::from_millis(800));
        assert_eq!(validated.monotonic, now + Duration::from_millis(800));
    }

    #[test]
    fn rejects_zero_expired_and_excessive_positive_skew() {
        assert!(matches!(
            DeadlineValidator::default().validate_at(&request(11_000, 0), 10_000, Instant::now()),
            Err(RuntimeHostError::EmptyDeadlineBudget)
        ));
        assert!(matches!(
            DeadlineValidator::default().validate_at(
                &request(10_000, 1_000),
                10_000,
                Instant::now()
            ),
            Err(RuntimeHostError::DeadlineExpired)
        ));
        assert!(matches!(
            DeadlineValidator::default().validate_at(
                &request(12_001, 1_000),
                10_000,
                Instant::now()
            ),
            Err(RuntimeHostError::ClockSkew)
        ));
    }

    fn request(deadline_unix_ms: i64, remaining_budget_ms: u64) -> InvocationRequest {
        let mut request = InvocationRequest::new(json!({}));
        request.set_deadline(deadline_unix_ms, remaining_budget_ms);
        request
    }
}
