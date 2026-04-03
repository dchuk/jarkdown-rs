//! Retry and rate-limiting utilities for async API calls.

use std::time::Duration;
use chrono::Utc;
use log::warn;
use rand::Rng;

/// Configuration for retry behavior with exponential backoff.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay: f64,
    pub max_delay: f64,
    pub jitter: bool,
    pub retryable_status_codes: Vec<u16>,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: 1.0,
            max_delay: 60.0,
            jitter: true,
            retryable_status_codes: vec![429, 503, 504],
        }
    }
}

/// Parse a Retry-After header value into seconds to wait.
pub fn parse_retry_after(header_value: &str) -> f64 {
    let trimmed = header_value.trim();
    if let Ok(seconds) = trimmed.parse::<f64>() {
        return seconds.max(0.0).min(300.0);
    }
    if let Ok(retry_time) = chrono::DateTime::parse_from_rfc2822(trimmed) {
        let now = Utc::now();
        let wait = (retry_time.with_timezone(&Utc) - now).num_milliseconds() as f64 / 1000.0;
        return wait.max(0.0).min(300.0);
    }
    5.0
}

/// Retry an async closure with exponential backoff.
pub async fn retry_with_backoff<F, Fut, T>(
    mut f: F,
    config: &RetryConfig,
) -> std::result::Result<T, reqwest::Error>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<T, reqwest::Error>>,
{
    let mut last_err = None;
    let mut rng = rand::thread_rng();

    for attempt in 0..=config.max_retries {
        match f().await {
            Ok(val) => return Ok(val),
            Err(e) => {
                let is_retryable = e.status().map_or(false, |s| {
                    config.retryable_status_codes.contains(&s.as_u16())
                });
                if !is_retryable || attempt == config.max_retries {
                    last_err = Some(e);
                    break;
                }
                let mut delay =
                    (config.base_delay * 2.0_f64.powi(attempt as i32)).min(config.max_delay);
                if config.jitter {
                    delay += rng.gen_range(0.0..delay * 0.1);
                }
                warn!(
                    "Rate limited (attempt {}/{}), retrying in {:.1}s...",
                    attempt + 1, config.max_retries, delay
                );
                tokio::time::sleep(Duration::from_secs_f64(delay)).await;
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap())
}
