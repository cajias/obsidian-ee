//! Connection state machine for auto-connect on startup.
//!
//! Provides a synchronous, runtime-agnostic connection state machine that can
//! be driven by any async runtime. See [Issue #3](https://github.com/cajias/obsidian-ee/issues/3).
//!
//! # Design
//!
//! The state machine emits [`ConnectionAction`] values telling the caller what
//! to do next, without performing any I/O itself. This keeps the module
//! testable and free of async runtime dependencies.
//!
//! # State transitions
//!
//! ```text
//! Disconnected --(connect/auto_connect)--> Connecting
//! Connecting --(on_connected)--> Connected
//! Connecting --(on_error)--> Reconnecting | Failed
//! Connected --(on_disconnected)--> Reconnecting | Failed
//! Reconnecting --(on_retry_tick)--> Connecting
//! ```
//!
//! The retry budget is refilled only via [`ConnectionStateMachine::on_stable_connection`]
//! (once a connection proves stable), never on `on_connected` alone — otherwise
//! an accept-then-immediately-drop server would reconnect forever.

use std::time::Duration;

use crate::{DocumentId, UserId};

/// Possible states of the connection state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionState {
    /// Not connected and not attempting to connect.
    Disconnected,
    /// Actively attempting to connect.
    Connecting,
    /// Successfully connected to the relay.
    Connected,
    /// Reconnecting after a disconnection.
    Reconnecting {
        /// The current reconnection attempt number (1-based).
        attempt: u32,
    },
    /// Connection has permanently failed.
    Failed {
        /// Human-readable reason for the failure.
        reason: String,
    },
}

/// Exponential backoff retry policy.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Maximum number of retry attempts before giving up.
    max_retries: u32,
    /// Delay before the first retry attempt.
    initial_delay: Duration,
    /// Maximum delay between retry attempts.
    max_delay: Duration,
    /// Multiplier applied to the delay after each attempt.
    backoff_multiplier: f64,
    /// Maximum jitter factor (0.0 = no jitter, 1.0 = full jitter).
    ///
    /// When non-zero, the actual delay should be chosen uniformly at random
    /// from the range returned by [`delay_range_for_attempt`](Self::delay_range_for_attempt).
    jitter_factor: f64,
}

/// Actions the caller should perform based on the current state.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnectionAction {
    /// Initiate a connection to the relay server.
    Connect {
        /// The WebSocket URL of the relay server.
        relay_url: String,
    },
    /// Wait for the specified delay, then retry.
    WaitAndRetry {
        /// How long to wait before retrying.
        delay: Duration,
        /// Which attempt number this retry represents.
        attempt: u32,
    },
    /// Send identification and subscribe to a document.
    IdentifyAndSubscribe {
        /// The user identifier.
        user_id: UserId,
        /// The document identifier.
        doc_id: DocumentId,
    },
    /// Stop retrying; the connection has permanently failed.
    GiveUp {
        /// Human-readable reason for giving up.
        reason: String,
    },
    /// No action required.
    DoNothing,
}

/// Configuration for the connection state machine.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// WebSocket URL of the relay server.
    relay_url: String,
    /// Identifier of the local user.
    user_id: UserId,
    /// Identifier of the document to collaborate on.
    doc_id: DocumentId,
    /// Whether to automatically connect on creation.
    auto_connect: bool,
    /// Retry policy for reconnection attempts.
    retry_policy: RetryPolicy,
}

/// A synchronous connection state machine.
///
/// Tracks connection state and emits actions for the caller to execute.
/// Does not perform any I/O itself, making it runtime-agnostic.
pub struct ConnectionStateMachine {
    config: ConnectionConfig,
    state: ConnectionState,
    retry_count: u32,
}

// -- RetryPolicy implementation ----------------------------------------------

impl RetryPolicy {
    /// Create a new retry policy.
    #[must_use]
    pub fn new(
        max_retries: u32,
        initial_delay: Duration,
        max_delay: Duration,
        backoff_multiplier: f64,
        jitter_factor: f64,
    ) -> Self {
        debug_assert!(backoff_multiplier > 0.0, "backoff_multiplier must be positive");
        debug_assert!(initial_delay <= max_delay, "initial_delay must not exceed max_delay");
        debug_assert!(
            (0.0..=1.0).contains(&jitter_factor),
            "jitter_factor must be between 0.0 and 1.0"
        );
        Self { max_retries, initial_delay, max_delay, backoff_multiplier, jitter_factor }
    }

    /// Compute the deterministic base delay for a given attempt (0-based).
    ///
    /// Returns `None` if `attempt >= max_retries`.
    ///
    /// This returns the base delay without jitter applied. For the jittered
    /// range, use [`delay_range_for_attempt`](Self::delay_range_for_attempt).
    /// The [`ConnectionStateMachine::next_action`] method uses this base delay
    /// in [`ConnectionAction::WaitAndRetry`]; callers that want jitter should
    /// use `delay_range_for_attempt` and pick a random value in the range.
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_retries {
            return None;
        }

        let multiplier = self.backoff_multiplier.powi(attempt.try_into().unwrap_or(0));
        let delay = self.initial_delay.mul_f64(multiplier);

        Some(delay.min(self.max_delay))
    }

    /// Compute the jittered delay range for a given attempt (0-based).
    ///
    /// Returns `None` if `attempt >= max_retries`.
    /// The returned range is `(min_delay, max_delay)` where the caller
    /// should pick a random value uniformly in this range.
    #[must_use]
    pub fn delay_range_for_attempt(&self, attempt: u32) -> Option<(Duration, Duration)> {
        let base = self.delay_for_attempt(attempt)?;
        let jitter_amount = base.mul_f64(self.jitter_factor);
        let min = base.saturating_sub(jitter_amount);
        Some((min, base))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
            jitter_factor: 0.25,
        }
    }
}

// -- ConnectionConfig implementation -----------------------------------------

impl ConnectionConfig {
    /// Create a new connection configuration.
    ///
    /// Auto-connect is enabled by default. Use [`with_auto_connect`](Self::with_auto_connect)
    /// and [`with_retry_policy`](Self::with_retry_policy) to customize.
    #[must_use]
    pub fn new(
        relay_url: impl Into<String>,
        user_id: impl Into<String>,
        doc_id: impl Into<String>,
    ) -> Self {
        Self {
            relay_url: relay_url.into(),
            user_id: user_id.into(),
            doc_id: doc_id.into(),
            auto_connect: true,
            retry_policy: RetryPolicy::default(),
        }
    }

    /// Set whether to automatically connect on creation.
    #[must_use]
    pub const fn with_auto_connect(mut self, auto_connect: bool) -> Self {
        self.auto_connect = auto_connect;
        self
    }

    /// Set the retry policy for reconnection attempts.
    #[must_use]
    pub const fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    /// Returns the relay URL.
    #[must_use]
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    /// Returns the user ID.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// Returns the document ID.
    #[must_use]
    pub fn doc_id(&self) -> &str {
        &self.doc_id
    }
}

// -- ConnectionStateMachine implementation -----------------------------------

impl ConnectionStateMachine {
    /// Create a new connection state machine.
    ///
    /// If `config.auto_connect` is true the initial state is `Connecting`;
    /// otherwise it is `Disconnected`.
    #[must_use]
    pub const fn new(config: ConnectionConfig) -> Self {
        let state = if config.auto_connect {
            ConnectionState::Connecting
        } else {
            ConnectionState::Disconnected
        };

        Self { config, state, retry_count: 0 }
    }

    /// Return a reference to the current connection state.
    #[must_use]
    pub const fn state(&self) -> &ConnectionState {
        &self.state
    }

    /// Determine the next action the caller should take.
    #[must_use]
    pub fn next_action(&self) -> ConnectionAction {
        match &self.state {
            ConnectionState::Disconnected => ConnectionAction::DoNothing,
            ConnectionState::Connecting => {
                ConnectionAction::Connect { relay_url: self.config.relay_url().to_owned() }
            }
            ConnectionState::Connected => ConnectionAction::IdentifyAndSubscribe {
                user_id: self.config.user_id().to_owned(),
                doc_id: self.config.doc_id().to_owned(),
            },
            ConnectionState::Reconnecting { attempt } => {
                // attempt is 1-based, delay_for_attempt uses 0-based index
                self.config.retry_policy.delay_for_attempt(attempt - 1).map_or_else(
                    || ConnectionAction::GiveUp { reason: "max retries exceeded".into() },
                    |delay| ConnectionAction::WaitAndRetry { delay, attempt: *attempt },
                )
            }
            ConnectionState::Failed { reason } => {
                ConnectionAction::GiveUp { reason: reason.clone() }
            }
        }
    }

    /// Request a connection. No-op if not in Disconnected state.
    pub fn connect(&mut self) {
        if self.state == ConnectionState::Disconnected {
            self.state = ConnectionState::Connecting;
        }
    }

    /// Transitions to Connected.
    ///
    /// Does **not** reset the retry counter: a successful TCP/WebSocket accept
    /// is not proof the connection is useful. If it were reset here, an
    /// accept-then-immediately-drop server (shutdown, takeover, resource
    /// eviction) would reconnect forever at a fixed cadence because the budget
    /// never accumulates toward `max_retries`. Call
    /// [`on_stable_connection`](Self::on_stable_connection) once the connection
    /// has proven stable to refill the budget.
    pub fn on_connected(&mut self) {
        debug_assert!(
            !matches!(self.state, ConnectionState::Failed { .. }),
            "on_connected called on Failed state machine"
        );
        self.state = ConnectionState::Connected;
    }

    /// Reset the retry budget after a connection has proven stable.
    ///
    /// The caller decides what "stable" means (typically staying connected for
    /// at least a minimum duration). Once called, a subsequent drop gets the
    /// full retry budget again, while transient accept-then-drop cycles that
    /// never reach stability keep accumulating toward `GiveUp`.
    pub const fn on_stable_connection(&mut self) {
        self.retry_count = 0;
    }

    /// Signals connection ended. Increments retry counter; transitions to
    /// Reconnecting or Failed.
    pub fn on_disconnected(&mut self) {
        debug_assert!(
            !matches!(self.state, ConnectionState::Failed { .. }),
            "on_disconnected called on Failed state machine"
        );
        self.advance_retry("connection lost");
    }

    /// If retries remain, transitions to Reconnecting. If exhausted,
    /// transitions to Failed.
    pub fn on_error(&mut self, reason: &str) {
        debug_assert!(
            !matches!(self.state, ConnectionState::Failed { .. }),
            "on_error called on Failed state machine"
        );
        self.advance_retry(reason);
    }

    /// Advance from `Reconnecting` to `Connecting` for the next attempt.
    pub fn on_retry_tick(&mut self) {
        if matches!(self.state, ConnectionState::Reconnecting { .. }) {
            self.state = ConnectionState::Connecting;
        }
    }

    /// Returns `true` when the state is `Connected`.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.state == ConnectionState::Connected
    }

    /// Returns the `auto_connect` setting from the config.
    #[must_use]
    pub const fn is_auto_connect(&self) -> bool {
        self.config.auto_connect
    }

    // -- private helpers -----------------------------------------------------

    /// Shared retry logic for `on_disconnected` and `on_error`.
    ///
    /// Increments the retry counter. If the retry budget is exhausted,
    /// transitions to `Failed`; otherwise transitions to `Reconnecting`.
    fn advance_retry(&mut self, reason: &str) {
        self.retry_count += 1;
        if self.config.retry_policy.delay_for_attempt(self.retry_count - 1).is_none() {
            self.state = ConnectionState::Failed { reason: reason.to_string() };
        } else {
            self.state = ConnectionState::Reconnecting { attempt: self.retry_count };
        }
    }
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // -- helpers -------------------------------------------------------------

    fn default_config(auto_connect: bool) -> ConnectionConfig {
        ConnectionConfig::new("ws://localhost:8080", "user-1", "doc-1")
            .with_auto_connect(auto_connect)
    }

    // -- RetryPolicy tests ---------------------------------------------------

    #[test]
    fn retry_policy_default_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 5);
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(30));
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
        assert!((policy.jitter_factor - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn retry_policy_first_attempt_returns_initial_delay() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.delay_for_attempt(0), Some(Duration::from_secs(1)));
    }

    #[test]
    fn retry_policy_exponential_backoff() {
        let policy = RetryPolicy::default();
        // attempt 0 -> 1s, attempt 1 -> 2s, attempt 2 -> 4s
        assert_eq!(policy.delay_for_attempt(0), Some(Duration::from_secs(1)));
        assert_eq!(policy.delay_for_attempt(1), Some(Duration::from_secs(2)));
        assert_eq!(policy.delay_for_attempt(2), Some(Duration::from_secs(4)));
    }

    #[test]
    fn retry_policy_caps_at_max_delay() {
        let policy = RetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(5), 2.0, 0.0);
        // attempt 3 would be 8s without cap, but max_delay is 5s
        assert_eq!(policy.delay_for_attempt(3), Some(Duration::from_secs(5)));
    }

    #[test]
    fn retry_policy_returns_none_when_max_retries_exceeded() {
        let policy = RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0);
        assert!(policy.delay_for_attempt(3).is_none());
        assert!(policy.delay_for_attempt(4).is_none());
    }

    // -- ConnectionConfig tests ----------------------------------------------

    #[test]
    fn config_with_auto_connect_enabled() {
        let config = default_config(true);
        assert_eq!(config.relay_url(), "ws://localhost:8080");
    }

    #[test]
    fn config_with_auto_connect_disabled() {
        let config = default_config(false);
        assert!(!config.auto_connect);
    }

    // -- ConnectionStateMachine tests ----------------------------------------

    #[test]
    fn new_with_auto_connect_starts_connecting() {
        let sm = ConnectionStateMachine::new(default_config(true));
        assert_eq!(*sm.state(), ConnectionState::Connecting);
    }

    #[test]
    fn new_without_auto_connect_starts_disconnected() {
        let sm = ConnectionStateMachine::new(default_config(false));
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
    }

    #[test]
    fn next_action_when_connecting_returns_connect() {
        let sm = ConnectionStateMachine::new(default_config(true));
        assert_eq!(
            sm.next_action(),
            ConnectionAction::Connect { relay_url: "ws://localhost:8080".into() }
        );
    }

    #[test]
    fn next_action_when_disconnected_returns_do_nothing() {
        let sm = ConnectionStateMachine::new(default_config(false));
        assert_eq!(sm.next_action(), ConnectionAction::DoNothing);
    }

    #[test]
    fn next_action_when_connected_returns_identify_and_subscribe() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_connected();
        assert_eq!(
            sm.next_action(),
            ConnectionAction::IdentifyAndSubscribe {
                user_id: "user-1".into(),
                doc_id: "doc-1".into(),
            }
        );
    }

    #[test]
    fn on_connected_transitions_to_connected() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);
    }

    #[test]
    fn on_connected_alone_does_not_reset_retry_count() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        // Simulate some retries first
        sm.on_error("timeout");
        sm.on_retry_tick();
        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);
        // Disconnect again WITHOUT signalling stability: the earlier failed
        // attempt must still count, so this is attempt 2 (not reset to 1).
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 2 });
    }

    #[test]
    fn on_stable_connection_resets_retry_count() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_error("timeout");
        sm.on_retry_tick();
        sm.on_connected();
        // Connection proved stable -> budget refilled.
        sm.on_stable_connection();
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
    }

    #[test]
    fn on_disconnected_transitions_to_reconnecting() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_connected();
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
    }

    #[test]
    fn on_error_increments_retry_count() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_error("connection refused");
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
        sm.on_retry_tick();
        sm.on_error("connection refused again");
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 2 });
    }

    #[test]
    fn on_error_transitions_to_failed_when_max_retries_exceeded() {
        let config =
            ConnectionConfig::new("ws://localhost:8080", "user-1", "doc-1").with_retry_policy(
                RetryPolicy::new(2, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0),
            );
        let mut sm = ConnectionStateMachine::new(config);

        // attempt 1
        sm.on_error("fail");
        sm.on_retry_tick();
        // attempt 2
        sm.on_error("fail");
        sm.on_retry_tick();
        // attempt 3 exceeds max_retries of 2
        sm.on_error("final fail");

        assert_eq!(*sm.state(), ConnectionState::Failed { reason: "final fail".into() });
    }

    #[test]
    fn next_action_when_reconnecting_returns_wait_and_retry() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_error("timeout");
        let action = sm.next_action();
        assert_eq!(
            action,
            ConnectionAction::WaitAndRetry { delay: Duration::from_secs(1), attempt: 1 }
        );
    }

    #[test]
    fn on_retry_tick_transitions_reconnecting_to_connecting() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_error("timeout");
        sm.on_retry_tick();
        assert_eq!(*sm.state(), ConnectionState::Connecting);
    }

    #[test]
    fn manual_connect_from_disconnected() {
        let mut sm = ConnectionStateMachine::new(default_config(false));
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
        sm.connect();
        assert_eq!(*sm.state(), ConnectionState::Connecting);
    }

    #[test]
    fn is_connected_returns_true_only_when_connected() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        assert!(!sm.is_connected());
        sm.on_connected();
        assert!(sm.is_connected());
        sm.on_disconnected();
        assert!(!sm.is_connected());
    }

    #[test]
    fn full_lifecycle_connect_disconnect_reconnect() {
        let mut sm = ConnectionStateMachine::new(default_config(true));

        // Initial state: Connecting (auto_connect = true)
        assert_eq!(*sm.state(), ConnectionState::Connecting);
        assert_eq!(
            sm.next_action(),
            ConnectionAction::Connect { relay_url: "ws://localhost:8080".into() }
        );

        // Connection established
        sm.on_connected();
        assert!(sm.is_connected());
        assert_eq!(
            sm.next_action(),
            ConnectionAction::IdentifyAndSubscribe {
                user_id: "user-1".into(),
                doc_id: "doc-1".into(),
            }
        );

        // Connection lost
        sm.on_disconnected();
        assert!(!sm.is_connected());
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
        assert_eq!(
            sm.next_action(),
            ConnectionAction::WaitAndRetry { delay: Duration::from_secs(1), attempt: 1 }
        );

        // Retry tick fires
        sm.on_retry_tick();
        assert_eq!(*sm.state(), ConnectionState::Connecting);

        // Reconnection succeeds
        sm.on_connected();
        assert!(sm.is_connected());
    }

    // -- Invalid state transition tests --------------------------------------

    #[test]
    fn connect_from_connected_is_no_op() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_connected();
        sm.connect(); // should be no-op
        assert_eq!(*sm.state(), ConnectionState::Connected);
    }

    #[test]
    fn connect_from_connecting_is_no_op() {
        let sm_auto = ConnectionStateMachine::new(default_config(true));
        // already in Connecting, can't call connect() again since it only works from Disconnected
        assert_eq!(*sm_auto.state(), ConnectionState::Connecting);
    }

    #[test]
    fn connect_from_reconnecting_is_no_op() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_error("fail");
        sm.connect(); // should be no-op
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
    }

    #[test]
    fn on_retry_tick_from_connected_is_no_op() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        sm.on_connected();
        sm.on_retry_tick(); // should be no-op
        assert_eq!(*sm.state(), ConnectionState::Connected);
    }

    #[test]
    fn on_retry_tick_from_disconnected_is_no_op() {
        let mut sm = ConnectionStateMachine::new(default_config(false));
        sm.on_retry_tick(); // should be no-op
        assert_eq!(*sm.state(), ConnectionState::Disconnected);
    }

    // -- Retry budget / boundary tests ---------------------------------------

    #[test]
    fn next_action_when_failed_returns_give_up_with_reason() {
        let config =
            ConnectionConfig::new("ws://localhost:8080", "user-1", "doc-1").with_retry_policy(
                RetryPolicy::new(1, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0),
            );
        let mut sm = ConnectionStateMachine::new(config);
        sm.on_error("first");
        sm.on_retry_tick();
        sm.on_error("fatal");
        assert_eq!(sm.next_action(), ConnectionAction::GiveUp { reason: "fatal".into() });
    }

    #[test]
    fn retry_policy_zero_max_retries_fails_immediately() {
        let policy = RetryPolicy::new(0, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0);
        assert!(policy.delay_for_attempt(0).is_none());
    }

    #[test]
    fn on_disconnected_transitions_to_failed_when_retries_exhausted() {
        let config =
            ConnectionConfig::new("ws://localhost:8080", "user-1", "doc-1").with_retry_policy(
                RetryPolicy::new(1, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0),
            );
        let mut sm = ConnectionStateMachine::new(config);
        // First disconnect
        sm.on_connected();
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
        // Second disconnect after reconnect. Without a stability signal the
        // budget is NOT reset, so this second unstable drop exhausts
        // max_retries=1 and transitions to Failed.
        sm.on_retry_tick();
        sm.on_connected();
        sm.on_disconnected();
        assert!(
            matches!(sm.state(), ConnectionState::Failed { .. }),
            "expected Failed after exhausting budget, got {:?}",
            sm.state()
        );
    }

    #[test]
    fn multi_cycle_stable_connection_resets_retry_count() {
        let mut sm = ConnectionStateMachine::new(default_config(true));

        // Cycle 1: connect -> stable -> disconnect -> reconnect
        sm.on_connected();
        sm.on_stable_connection();
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
        sm.on_retry_tick();
        sm.on_connected();
        sm.on_stable_connection();

        // Cycle 2: budget was refilled by stability -> attempt 1 again
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
        sm.on_retry_tick();
        sm.on_connected();
        sm.on_stable_connection();

        // Cycle 3: still resets
        sm.on_disconnected();
        assert_eq!(*sm.state(), ConnectionState::Reconnecting { attempt: 1 });
    }

    /// Regression: an accept-then-immediately-drop server (connects but the
    /// session never proves stable) must accumulate retries and eventually
    /// `GiveUp` instead of reconnecting forever at a fixed cadence.
    #[test]
    fn accept_then_drop_without_stability_reaches_give_up() {
        let config =
            ConnectionConfig::new("ws://localhost:8080", "user-1", "doc-1").with_retry_policy(
                RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(30), 2.0, 0.0),
            );
        let mut sm = ConnectionStateMachine::new(config);

        // Simulate connect -> drop cycles where the connection never stays up
        // long enough to be deemed stable (so on_stable_connection is NOT
        // called). With max_retries=3 the budget is exhausted on the 4th drop.
        // If the reset regression returned, retry_count would keep resetting
        // and the final assert (Failed) would catch it — the fixed loop count
        // means it can never hang.
        for _ in 0..4 {
            sm.on_connected(); // TCP accept succeeds
            sm.on_disconnected(); // ...but the session drops immediately
            sm.on_retry_tick(); // no-op once Failed, so safe to call
        }

        // Budget (max_retries=3) is now exhausted -> Failed / GiveUp reachable.
        assert!(
            matches!(sm.state(), ConnectionState::Failed { .. }),
            "expected Failed after repeated accept-then-drop, got {:?}",
            sm.state()
        );
        assert!(matches!(sm.next_action(), ConnectionAction::GiveUp { .. }));
    }

    // -- Jitter tests --------------------------------------------------------

    #[test]
    fn retry_policy_jitter_range() {
        let policy =
            RetryPolicy::new(5, Duration::from_secs(10), Duration::from_secs(60), 2.0, 0.5);
        let range = policy.delay_range_for_attempt(0).unwrap();
        // base = 10s, jitter = 50%, so range is 5s to 10s
        assert_eq!(range, (Duration::from_secs(5), Duration::from_secs(10)));
    }

    #[test]
    fn retry_policy_no_jitter() {
        let policy =
            RetryPolicy::new(5, Duration::from_secs(10), Duration::from_secs(60), 2.0, 0.0);
        let range = policy.delay_range_for_attempt(0).unwrap();
        assert_eq!(range, (Duration::from_secs(10), Duration::from_secs(10)));
    }

    #[test]
    fn retry_policy_full_jitter() {
        let policy =
            RetryPolicy::new(5, Duration::from_secs(10), Duration::from_secs(60), 2.0, 1.0);
        let range = policy.delay_range_for_attempt(0).unwrap();
        assert_eq!(range, (Duration::ZERO, Duration::from_secs(10)));
    }
}
