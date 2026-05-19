//! Per-key request limiting for downstream relay keys.

use crate::{current_unix_secs, ServerState};
use modelwire_core::error::{Error, ErrorKind};

#[derive(Debug, Clone, Copy, Default)]
pub struct LimitEnforcement {
    pub in_flight_reserved: bool,
}

/// Enforce per-key limits and reserve one in-flight slot for this request.
///
/// The caller must invoke [`decrement_in_flight`] when request handling finishes
/// if `in_flight_reserved` is true.
pub fn enforce_key_limits(
    state: &ServerState,
    key_hash: &str,
    requests_per_minute: Option<u32>,
    max_concurrency: Option<u32>,
) -> Result<LimitEnforcement, Error> {
    let now_secs = current_unix_secs();
    let mut counters = state
        .key_limiter_counters
        .entry(key_hash.to_string())
        .or_default();
    let mut reserved = false;

    if let Some(limit) = requests_per_minute {
        if limit == 0 {
            return Err(Error::new(
                ErrorKind::RateLimited,
                "Relay key request rate limit exceeded",
            ));
        }
        if now_secs.saturating_sub(counters.rate.window_started_at_secs) >= 60 {
            counters.rate.window_started_at_secs = now_secs;
            counters.rate.request_count = 0;
        }
        if counters.rate.request_count >= limit {
            return Err(Error::new(
                ErrorKind::RateLimited,
                "Relay key request rate limit exceeded",
            ));
        }
        counters.rate.request_count = counters.rate.request_count.saturating_add(1);
    }

    if let Some(limit) = max_concurrency {
        if limit == 0 || counters.in_flight >= limit {
            return Err(Error::new(
                ErrorKind::RateLimited,
                "Relay key concurrency limit exceeded",
            ));
        }
        counters.in_flight = counters.in_flight.saturating_add(1);
        reserved = true;
    }

    Ok(LimitEnforcement {
        in_flight_reserved: reserved,
    })
}

/// Decrement in-flight counter for a key after request completion.
pub fn decrement_in_flight(state: &ServerState, key_hash: &str) {
    if let Some(mut counters) = state.key_limiter_counters.get_mut(key_hash) {
        counters.in_flight = counters.in_flight.saturating_sub(1);
    }
}

/// Enforce per-IP request rate limit (requests per minute).
pub fn enforce_ip_rate_limit(
    state: &ServerState,
    ip_identity: &str,
    requests_per_minute: Option<u32>,
) -> Result<(), Error> {
    let Some(limit) = requests_per_minute else {
        return Ok(());
    };
    if limit == 0 {
        return Err(Error::new(
            ErrorKind::RateLimited,
            "IP request rate limit exceeded",
        ));
    }

    let now_secs = current_unix_secs();
    let mut window = state
        .ip_limiter_counters
        .entry(ip_identity.to_string())
        .or_default();
    if now_secs.saturating_sub(window.window_started_at_secs) >= 60 {
        window.window_started_at_secs = now_secs;
        window.request_count = 0;
    }
    if window.request_count >= limit {
        return Err(Error::new(
            ErrorKind::RateLimited,
            "IP request rate limit exceeded",
        ));
    }
    window.request_count = window.request_count.saturating_add(1);
    Ok(())
}
