//! Circuit breaker that decides, per dictation, whether to try Deepgram at all.
//!
//! ```text
//!   Closed ──failure──▶ Open (cooldown) ──cooldown over──▶ Half-open: try Deepgram once
//!     ▲                    │                                   │ success      │ failure
//!     └────────────────────┴───────────────────────────────────┘              ▼
//!                                                             Open, cooldown doubled
//! ```
//!
//! While open, dictations go straight to local Whisper, so a dead network costs nothing: no
//! connection attempt and no timeout wait. Network failures back off from 30 s, doubling up to
//! 5 min; a rejected key or exhausted credit waits an hour. One success closes the breaker.
//! Time is passed in by the caller so this is deterministic and unit-testable.

use super::deepgram::FailureKind;
use std::time::{Duration, Instant};

const FIRST_COOLDOWN: Duration = Duration::from_secs(30);
const MAX_COOLDOWN: Duration = Duration::from_secs(300);
const AUTH_COOLDOWN: Duration = Duration::from_secs(3600);

#[derive(Debug, Default)]
pub struct Breaker {
    consecutive_failures: u32,
    open_until: Option<Instant>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Transition {
    /// Nothing the user needs to hear about.
    None,
    /// Just switched to offline.
    WentOffline(Duration),
    /// Deepgram works again after having failed.
    BackOnline,
}

impl Breaker {
    /// Whether the next dictation should try Deepgram.
    pub fn allows(&self, now: Instant) -> bool {
        self.open_until.is_none_or(|until| now >= until)
    }

    pub fn is_open(&self, now: Instant) -> bool {
        !self.allows(now)
    }

    pub fn record_success(&mut self) -> Transition {
        let was_failing = self.consecutive_failures > 0;
        self.consecutive_failures = 0;
        self.open_until = None;
        if was_failing {
            Transition::BackOnline
        } else {
            Transition::None
        }
    }

    pub fn record_failure(&mut self, kind: FailureKind, now: Instant) -> Transition {
        let cooldown = match kind {
            FailureKind::Cancelled => return Transition::None,
            FailureKind::Auth => AUTH_COOLDOWN,
            FailureKind::Network => {
                let doublings = self.consecutive_failures.min(8);
                (FIRST_COOLDOWN * 2u32.pow(doublings)).min(MAX_COOLDOWN)
            }
        };
        self.consecutive_failures += 1;
        self.open_until = Some(now + cooldown);
        Transition::WentOffline(cooldown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_until_first_failure() {
        let b = Breaker::default();
        assert!(b.allows(Instant::now()));
    }

    #[test]
    fn network_failures_back_off_exponentially_and_cap() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        let mut cooldowns = Vec::new();
        for _ in 0..6 {
            if let Transition::WentOffline(c) = b.record_failure(FailureKind::Network, t0) {
                cooldowns.push(c.as_secs());
            }
        }
        assert_eq!(cooldowns, [30, 60, 120, 240, 300, 300]);
    }

    #[test]
    fn open_skips_deepgram_then_half_opens() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        b.record_failure(FailureKind::Network, t0);
        assert!(!b.allows(t0 + Duration::from_secs(29)));
        assert!(
            b.allows(t0 + Duration::from_secs(30)),
            "half-open after cooldown"
        );
    }

    #[test]
    fn success_closes_and_reports_recovery() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        assert_eq!(b.record_success(), Transition::None);
        b.record_failure(FailureKind::Network, t0);
        assert_eq!(b.record_success(), Transition::BackOnline);
        assert!(b.allows(t0));
        // Backoff restarts from the beginning after recovery.
        assert_eq!(
            b.record_failure(FailureKind::Network, t0),
            Transition::WentOffline(FIRST_COOLDOWN)
        );
    }

    #[test]
    fn auth_failures_wait_an_hour() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        assert_eq!(
            b.record_failure(FailureKind::Auth, t0),
            Transition::WentOffline(AUTH_COOLDOWN)
        );
        assert!(!b.allows(t0 + Duration::from_secs(3599)));
    }

    #[test]
    fn cancellation_is_not_a_failure() {
        let t0 = Instant::now();
        let mut b = Breaker::default();
        assert_eq!(
            b.record_failure(FailureKind::Cancelled, t0),
            Transition::None
        );
        assert!(b.allows(t0));
    }
}
