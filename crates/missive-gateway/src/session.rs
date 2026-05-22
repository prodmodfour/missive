//! Persistent gateway session policy helpers.
//!
//! Gateway sessions are communication continuity records: they map an inbound
//! source identity plus target agent to the currently linked A2A context.  This
//! module evaluates reset policies over the persisted store row using an
//! injectable clock so tests and future adapter workers can reason about daily
//! and idle boundaries without depending on wall-clock time.

use missive_core::{MissiveError, MissiveTimestamp, Result};
use missive_store::{GatewaySessionRecord, GatewaySessionResetMode};
use serde::Serialize;

const SECONDS_PER_DAY: i64 = 86_400;
const SECONDS_PER_HOUR: i64 = 3_600;

/// Clock abstraction used by gateway session reset evaluation.
pub trait GatewaySessionClock {
    /// Returns the current time for reset-policy evaluation.
    fn now(&self) -> MissiveTimestamp;
}

/// System UTC clock for production session policy checks.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemGatewaySessionClock;

impl GatewaySessionClock for SystemGatewaySessionClock {
    fn now(&self) -> MissiveTimestamp {
        MissiveTimestamp::now_utc()
    }
}

/// Fixed clock used by deterministic tests and local simulations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedGatewaySessionClock {
    now: MissiveTimestamp,
}

impl FixedGatewaySessionClock {
    /// Creates a fixed clock that always returns `now`.
    #[must_use]
    pub const fn new(now: MissiveTimestamp) -> Self {
        Self { now }
    }
}

impl GatewaySessionClock for FixedGatewaySessionClock {
    fn now(&self) -> MissiveTimestamp {
        self.now
    }
}

/// Reason a gateway session should rotate to a fresh A2A context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewaySessionResetReason {
    /// A configured daily boundary has passed since the session was active/reset.
    Daily,
    /// The session has been idle for longer than its configured timeout.
    Idle,
}

/// Deterministic result of evaluating one session's reset policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GatewaySessionResetEvaluation {
    /// Timestamp supplied by the clock.
    pub now: MissiveTimestamp,
    /// Whether a new context should be created and linked before next input.
    pub should_reset: bool,
    /// Reset reasons that matched the configured policy.
    pub reasons: Vec<GatewaySessionResetReason>,
}

/// Evaluates a persisted gateway session row against its reset policy.
pub fn evaluate_session_reset(
    session: &GatewaySessionRecord,
    clock: &impl GatewaySessionClock,
) -> Result<GatewaySessionResetEvaluation> {
    let now = clock.now();
    let mut reasons = Vec::new();

    match session.reset_mode {
        GatewaySessionResetMode::None => {}
        GatewaySessionResetMode::Daily => {
            if daily_reset_due(session, now)? {
                reasons.push(GatewaySessionResetReason::Daily);
            }
        }
        GatewaySessionResetMode::Idle => {
            if idle_reset_due(session, now)? {
                reasons.push(GatewaySessionResetReason::Idle);
            }
        }
        GatewaySessionResetMode::Both => {
            if idle_reset_due(session, now)? {
                reasons.push(GatewaySessionResetReason::Idle);
            }
            if daily_reset_due(session, now)? {
                reasons.push(GatewaySessionResetReason::Daily);
            }
        }
    }

    Ok(GatewaySessionResetEvaluation {
        now,
        should_reset: !reasons.is_empty(),
        reasons,
    })
}

fn idle_reset_due(session: &GatewaySessionRecord, now: MissiveTimestamp) -> Result<bool> {
    let Some(timeout_seconds) = session.idle_timeout_seconds else {
        return Err(MissiveError::validation(
            "gateway session idle reset mode requires idle_timeout_seconds",
        ));
    };
    if timeout_seconds == 0 {
        return Err(MissiveError::validation(
            "gateway session idle_timeout_seconds must be greater than zero",
        ));
    }
    let timeout_seconds = i64::try_from(timeout_seconds).map_err(|error| {
        MissiveError::validation("gateway session idle_timeout_seconds is too large")
            .with_source(error)
    })?;
    let idle_deadline = session
        .last_active_at
        .unix_timestamp()
        .saturating_add(timeout_seconds);
    Ok(now.unix_timestamp() > idle_deadline)
}

fn daily_reset_due(session: &GatewaySessionRecord, now: MissiveTimestamp) -> Result<bool> {
    let boundary = daily_reset_boundary(now, session.daily_reset_hour)?;
    Ok(daily_reference_timestamp(session) < boundary)
}

fn daily_reference_timestamp(session: &GatewaySessionRecord) -> MissiveTimestamp {
    session
        .last_reset_at
        .map(|last_reset| last_reset.max(session.last_active_at))
        .unwrap_or(session.last_active_at)
}

fn daily_reset_boundary(now: MissiveTimestamp, reset_hour: u8) -> Result<MissiveTimestamp> {
    if reset_hour > 23 {
        return Err(MissiveError::validation(format!(
            "gateway session daily_reset_hour must be between 0 and 23, got {reset_hour}"
        )));
    }
    let now_seconds = now.unix_timestamp();
    let day_start = now_seconds.div_euclid(SECONDS_PER_DAY) * SECONDS_PER_DAY;
    let candidate = day_start.saturating_add(i64::from(reset_hour) * SECONDS_PER_HOUR);
    let boundary = if now_seconds < candidate {
        candidate.saturating_sub(SECONDS_PER_DAY)
    } else {
        candidate
    };
    MissiveTimestamp::from_unix_timestamp(boundary)
}

#[cfg(test)]
mod tests {
    use missive_core::{AgentAlias, ContextId};
    use missive_store::{GatewaySessionId, GatewaySessionResetMode};

    use super::*;

    #[test]
    fn daily_reset_uses_fixed_clock_and_configured_hour() {
        let mut session = session(GatewaySessionResetMode::Daily);
        session.daily_reset_hour = 4;
        session.last_active_at = timestamp("2025-01-02T03:59:00Z");

        let before = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T03:59:59Z")),
        )
        .expect("before boundary");
        assert!(!before.should_reset);

        let after = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T04:00:01Z")),
        )
        .expect("after boundary");
        assert!(after.should_reset);
        assert_eq!(after.reasons, vec![GatewaySessionResetReason::Daily]);

        session.last_reset_at = Some(timestamp("2025-01-02T04:00:02Z"));
        let already_reset = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T05:00:00Z")),
        )
        .expect("already reset");
        assert!(!already_reset.should_reset);
    }

    #[test]
    fn idle_reset_uses_fixed_clock_and_timeout() {
        let mut session = session(GatewaySessionResetMode::Idle);
        session.idle_timeout_seconds = Some(60);
        session.last_active_at = timestamp("2025-01-02T00:00:00Z");

        let within_timeout = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T00:01:00Z")),
        )
        .expect("within timeout");
        assert!(!within_timeout.should_reset);

        let expired = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T00:01:01Z")),
        )
        .expect("expired");
        assert!(expired.should_reset);
        assert_eq!(expired.reasons, vec![GatewaySessionResetReason::Idle]);
    }

    #[test]
    fn combined_reset_policy_triggers_for_idle_or_daily_reason() {
        let mut idle_only = session(GatewaySessionResetMode::Both);
        idle_only.daily_reset_hour = 4;
        idle_only.idle_timeout_seconds = Some(60);
        idle_only.last_active_at = timestamp("2025-01-02T04:30:00Z");
        let idle_expired = evaluate_session_reset(
            &idle_only,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T04:31:01Z")),
        )
        .expect("idle expired");
        assert_eq!(idle_expired.reasons, vec![GatewaySessionResetReason::Idle]);

        let mut daily_only = session(GatewaySessionResetMode::Both);
        daily_only.daily_reset_hour = 4;
        daily_only.idle_timeout_seconds = Some(86_400);
        daily_only.last_active_at = timestamp("2025-01-02T03:59:00Z");
        let daily_expired = evaluate_session_reset(
            &daily_only,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T04:00:01Z")),
        )
        .expect("daily expired");
        assert_eq!(
            daily_expired.reasons,
            vec![GatewaySessionResetReason::Daily]
        );

        let mut both = session(GatewaySessionResetMode::Both);
        both.daily_reset_hour = 4;
        both.idle_timeout_seconds = Some(60);
        both.last_active_at = timestamp("2025-01-01T03:00:00Z");
        let both_expired = evaluate_session_reset(
            &both,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T04:00:01Z")),
        )
        .expect("both expired");
        assert_eq!(
            both_expired.reasons,
            vec![
                GatewaySessionResetReason::Idle,
                GatewaySessionResetReason::Daily
            ]
        );
    }

    #[test]
    fn missing_idle_timeout_is_reported_as_validation_error() {
        let session = session(GatewaySessionResetMode::Idle);

        let error = evaluate_session_reset(
            &session,
            &FixedGatewaySessionClock::new(timestamp("2025-01-02T00:00:00Z")),
        )
        .expect_err("missing idle timeout");

        assert_eq!(error.category(), missive_core::ErrorCategory::Validation);
        assert!(error.to_string().contains("idle_timeout_seconds"));
    }

    fn session(reset_mode: GatewaySessionResetMode) -> GatewaySessionRecord {
        GatewaySessionRecord {
            gateway_session_id: GatewaySessionId::new("session-1").expect("session id"),
            source_kind: "adapter".to_owned(),
            source_id: "stdin/user-1".to_owned(),
            agent_alias: AgentAlias::new("echo").expect("agent"),
            resume_name: "default".to_owned(),
            context_id: ContextId::new("ctx-session").expect("context"),
            reset_mode,
            daily_reset_hour: 0,
            idle_timeout_seconds: None,
            last_active_at: timestamp("2025-01-02T00:00:00Z"),
            last_reset_at: None,
            reset_count: 0,
            metadata: Default::default(),
            created_at: timestamp("2025-01-02T00:00:00Z"),
            updated_at: timestamp("2025-01-02T00:00:00Z"),
        }
    }

    fn timestamp(value: &str) -> MissiveTimestamp {
        value.parse().expect("timestamp")
    }
}
