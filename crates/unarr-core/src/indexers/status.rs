//! Escalating back-off after provider failures, after the originals'
//! `ProviderStatusServiceBase` and `EscalationBackOff`: each failure moves
//! the level up one and disables the provider for the level's period; a
//! success moves it down one. A minimum back-off (a request limit) moves
//! the level up to the first period that covers it.

use chrono::{DateTime, Duration, Utc};

use crate::store::provider_status::ProviderStatus;

/// `EscalationBackOff.Periods`, in seconds: none, a minute, 5, 15, 30
/// minutes, 1, 3, 6, 12 and 24 hours.
pub const PERIODS: [i64; 10] = [0, 60, 300, 900, 1800, 3600, 10800, 21600, 43200, 86400];

pub const MAXIMUM_ESCALATION_LEVEL: i64 = PERIODS.len() as i64 - 1;

pub fn back_off_period(level: i64) -> Duration {
    let level = level.clamp(0, MAXIMUM_ESCALATION_LEVEL) as usize;
    Duration::seconds(PERIODS[level])
}

/// `RecordSuccess`: one level down and the block lifted; a status at level
/// zero is left as is (and returns `false`: nothing to store).
pub fn record_success(status: &mut ProviderStatus) -> bool {
    if status.escalation_level == 0 {
        return false;
    }
    status.escalation_level -= 1;
    status.disabled_till = None;
    true
}

/// How a kind of provider escalates: indexers go all the way to a day and
/// are blocked from the first failure; download clients stop at an hour
/// and are not blocked within five minutes of their first failure. Within
/// fifteen minutes of the process start no failure escalates and no block
/// outlasts five minutes, so a flaky start does not bench a provider.
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub max_level: i64,
    pub minimum_time_since_initial_failure: Duration,
    /// The process start, when the startup grace applies.
    pub started_at: Option<DateTime<Utc>>,
}

/// `MinimumTimeSinceStartup`.
pub const STARTUP_GRACE: Duration = Duration::minutes(15);

impl Policy {
    /// `IndexerStatusService`.
    pub fn indexer(started_at: Option<DateTime<Utc>>) -> Policy {
        Policy {
            max_level: MAXIMUM_ESCALATION_LEVEL,
            minimum_time_since_initial_failure: Duration::zero(),
            started_at,
        }
    }

    /// `DownloadClientStatusService`.
    pub fn download_client(started_at: Option<DateTime<Utc>>) -> Policy {
        Policy {
            max_level: 5,
            minimum_time_since_initial_failure: Duration::minutes(5),
            started_at,
        }
    }
}

/// `RecordFailure`: the indexers' policy without a startup grace, for
/// tests and callers that have no process start to speak of.
pub fn record_failure(
    status: &mut ProviderStatus,
    now: DateTime<Utc>,
    minimum_back_off: Duration,
    escalate: bool,
) {
    record_failure_with(status, now, minimum_back_off, escalate, &Policy::indexer(None));
}

/// `ProviderStatusServiceBase.RecordFailure`: the first failure sets level
/// one; later ones move a level up (when `escalate`, outside the grace
/// periods), at least far enough to cover `minimum_back_off`, and the
/// provider is disabled for the level's period unless it is still within
/// the policy's grace since its first failure.
pub fn record_failure_with(
    status: &mut ProviderStatus,
    now: DateTime<Utc>,
    minimum_back_off: Duration,
    escalate: bool,
    policy: &Policy,
) {
    let max_level = policy.max_level.clamp(0, MAXIMUM_ESCALATION_LEVEL);
    let mut escalate = escalate;
    status.most_recent_failure = Some(now);
    if status.escalation_level == 0 {
        status.initial_failure = Some(now);
        status.escalation_level = 1;
        escalate = false;
    }
    let in_startup_grace = policy.started_at.is_some_and(|s| s + STARTUP_GRACE > now);
    let in_grace = status
        .initial_failure
        .is_some_and(|f| f + policy.minimum_time_since_initial_failure > now);
    if escalate && !in_grace && !in_startup_grace {
        status.escalation_level = (status.escalation_level + 1).min(max_level);
    }
    if minimum_back_off > Duration::zero() {
        while status.escalation_level < max_level
            && back_off_period(status.escalation_level) < minimum_back_off
        {
            status.escalation_level += 1;
        }
    }
    if !in_grace || minimum_back_off > Duration::zero() {
        status.disabled_till = Some(now + back_off_period(status.escalation_level));
    }
    if in_startup_grace && minimum_back_off == Duration::zero() {
        if let Some(till) = status.disabled_till {
            let cap = now + Duration::seconds(PERIODS[2]);
            if cap < till {
                status.disabled_till = Some(cap);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 7, 12, minute, 0).unwrap()
    }

    #[test]
    fn failures_escalate_and_successes_step_down() {
        let mut s = ProviderStatus::default();
        record_failure(&mut s, at(0), Duration::zero(), true);
        assert_eq!(s.escalation_level, 1);
        assert_eq!(s.initial_failure, Some(at(0)));
        assert_eq!(s.disabled_till, Some(at(1)));
        assert!(s.is_disabled(at(0)));
        assert!(!s.is_disabled(at(1)));

        record_failure(&mut s, at(2), Duration::zero(), true);
        assert_eq!(s.escalation_level, 2);
        assert_eq!(s.initial_failure, Some(at(0)), "the first failure stays");
        assert_eq!(s.most_recent_failure, Some(at(2)));
        assert_eq!(s.disabled_till, Some(at(7)));

        assert!(record_success(&mut s));
        assert_eq!(s.escalation_level, 1);
        assert_eq!(s.disabled_till, None);
        assert!(record_success(&mut s));
        assert!(!record_success(&mut s), "level zero has nothing to store");
        assert_eq!(s.escalation_level, 0);

        // A new failure after recovery starts a new initial failure.
        record_failure(&mut s, at(30), Duration::zero(), true);
        assert_eq!(s.initial_failure, Some(at(30)));
    }

    #[test]
    fn a_request_limit_jumps_to_the_covering_period() {
        let mut s = ProviderStatus::default();
        record_failure(&mut s, at(0), Duration::hours(1), true);
        assert_eq!(s.escalation_level, 5, "the first period of at least an hour");
        assert_eq!(s.disabled_till, Some(at(0) + Duration::hours(1)));
        // Without escalation, the first failure still sets level one and
        // later ones stay there.
        let mut t = ProviderStatus::default();
        record_failure(&mut t, at(0), Duration::zero(), false);
        assert_eq!(t.escalation_level, 1);
        assert_eq!(t.disabled_till, Some(at(1)));
        assert_eq!(t.initial_failure, Some(at(0)));
        record_failure(&mut t, at(5), Duration::zero(), false);
        assert_eq!(t.escalation_level, 1);
    }

    #[test]
    fn download_clients_wait_five_minutes_and_a_fresh_process_caps_the_block() {
        let policy = Policy::download_client(None);
        let mut s = ProviderStatus::default();
        record_failure_with(&mut s, at(0), Duration::zero(), true, &policy);
        assert_eq!(s.escalation_level, 1);
        assert_eq!(
            s.disabled_till, None,
            "no block within five minutes of the first failure"
        );
        record_failure_with(&mut s, at(3), Duration::zero(), true, &policy);
        assert_eq!(s.escalation_level, 1, "no escalation in the grace either");
        assert_eq!(s.disabled_till, None);
        record_failure_with(&mut s, at(6), Duration::zero(), true, &policy);
        assert_eq!(s.escalation_level, 2);
        assert_eq!(s.disabled_till, Some(at(6) + Duration::minutes(5)));
        // A request limit blocks even in the grace, and the level stops at an hour.
        let mut r = ProviderStatus::default();
        record_failure_with(&mut r, at(0), Duration::hours(3), true, &policy);
        assert_eq!(r.escalation_level, 5);
        assert_eq!(r.disabled_till, Some(at(0) + Duration::hours(1)));
        // Within fifteen minutes of the start, blocks last five minutes at most.
        let fresh = Policy::indexer(Some(at(0)));
        let mut i = ProviderStatus {
            escalation_level: 6,
            ..Default::default()
        };
        record_failure_with(&mut i, at(10), Duration::zero(), true, &fresh);
        assert_eq!(i.escalation_level, 6, "no escalation right after the start");
        assert_eq!(i.disabled_till, Some(at(15)));
        record_failure_with(&mut i, at(20), Duration::zero(), true, &fresh);
        assert_eq!(i.escalation_level, 7);
        assert_eq!(i.disabled_till, Some(at(20) + Duration::hours(6)));
    }

    #[test]
    fn the_level_is_capped() {
        let mut s = ProviderStatus::default();
        for _ in 0..20 {
            record_failure(&mut s, at(0), Duration::zero(), true);
        }
        assert_eq!(s.escalation_level, MAXIMUM_ESCALATION_LEVEL);
        assert_eq!(back_off_period(s.escalation_level), Duration::hours(24));
        assert_eq!(back_off_period(99), Duration::hours(24));
    }
}
