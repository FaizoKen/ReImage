use std::time::Duration;

/// Error types that are retryable
pub fn is_retryable_error(error: &reqwest::Error, status_code: Option<u16>) -> bool {
    // Retry on timeout errors
    if error.is_timeout() {
        return true;
    }

    // Retry on connection errors
    if error.is_connect() {
        return true;
    }

    // Retry on request errors (network issues)
    if error.is_request() {
        return true;
    }

    // Retry on server errors (5xx)
    if let Some(status) = status_code {
        if status >= 500 && status < 600 {
            return true;
        }
        // Retry on rate limiting
        if status == 429 {
            return true;
        }
    }

    false
}

/// Check if an error is permanent (should not retry)
pub fn is_permanent_error(error_msg: &str) -> bool {
    let permanent_errors = [
        "Invalid content type",
        "Image too large",
        "Invalid image magic bytes",
        "unsupported",
        "corrupt",
    ];

    permanent_errors.iter().any(|e| error_msg.contains(e))
}

/// Calculate backoff delay for retry attempt
pub fn calculate_backoff(
    attempt: u32,
    initial_delay: Duration,
    multiplier: f64,
) -> Duration {
    let delay_ms = initial_delay.as_millis() as f64 * multiplier.powi(attempt as i32);
    Duration::from_millis(delay_ms.round() as u64)
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub initial_delay: Duration,
    pub backoff_multiplier: f64,
}

impl RetryConfig {
    pub fn new(max_retries: u32, initial_delay: Duration, backoff_multiplier: f64) -> Self {
        Self {
            max_retries,
            initial_delay,
            backoff_multiplier,
        }
    }

    pub fn get_delay(&self, attempt: u32) -> Duration {
        calculate_backoff(attempt, self.initial_delay, self.backoff_multiplier)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_calculation() {
        let initial = Duration::from_millis(500);
        let multiplier = 1.5;

        // First retry: 500ms
        let d1 = calculate_backoff(0, initial, multiplier);
        assert_eq!(d1.as_millis(), 500);

        // Second retry: 750ms
        let d2 = calculate_backoff(1, initial, multiplier);
        assert_eq!(d2.as_millis(), 750);

        // Third retry: 1125ms
        let d3 = calculate_backoff(2, initial, multiplier);
        assert_eq!(d3.as_millis(), 1125);
    }
}
