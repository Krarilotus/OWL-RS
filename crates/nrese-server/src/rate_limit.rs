use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use crate::error::ApiError;
use crate::policy::{PolicyAction, RateLimitConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RateLimitBucket {
    Read,
    Write,
    Admin,
}

#[derive(Debug)]
struct WindowCounter {
    started_at: Instant,
    count: usize,
}

#[derive(Debug, Default)]
pub struct RateLimiter {
    counters: Mutex<HashMap<RateLimitBucket, WindowCounter>>,
}

impl RateLimiter {
    pub fn enforce(&self, action: PolicyAction, config: RateLimitConfig) -> Result<(), ApiError> {
        let Some(limit) = config.limit_for(action) else {
            return Ok(());
        };

        let bucket = bucket_for(action);
        let now = Instant::now();
        let mut counters = self
            .counters
            .lock()
            .map_err(|_| ApiError::internal("failed to acquire rate-limit state"))?;
        let counter = counters.entry(bucket).or_insert_with(|| WindowCounter {
            started_at: now,
            count: 0,
        });

        if now.duration_since(counter.started_at) >= config.window {
            counter.started_at = now;
            counter.count = 0;
        }

        if counter.count >= limit {
            return Err(ApiError::too_many_requests(format!(
                "{:?} rate limit exceeded ({} requests per {} seconds)",
                action,
                limit,
                config.window.as_secs()
            )));
        }

        counter.count += 1;
        Ok(())
    }
}

fn bucket_for(action: PolicyAction) -> RateLimitBucket {
    match action {
        PolicyAction::QueryRead
        | PolicyAction::GraphRead
        | PolicyAction::ServiceDescriptionRead => RateLimitBucket::Read,
        PolicyAction::UpdateWrite | PolicyAction::TellWrite | PolicyAction::GraphWrite => {
            RateLimitBucket::Write
        }
        PolicyAction::OperatorRead | PolicyAction::AdminWrite | PolicyAction::MetricsRead => {
            RateLimitBucket::Admin
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use crate::policy::{PolicyAction, RateLimitConfig};

    use super::RateLimiter;

    #[test]
    fn read_limit_blocks_second_request_when_limit_is_one() {
        let limiter = RateLimiter::default();
        let config = RateLimitConfig {
            window: Duration::from_secs(60),
            read_requests_per_window: 1,
            ..RateLimitConfig::default()
        };

        assert!(limiter.enforce(PolicyAction::QueryRead, config).is_ok());
        assert!(limiter.enforce(PolicyAction::GraphRead, config).is_err());
    }
}
