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

use std::time::Duration;

/// Possible states of the connection state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

/// Actions the caller should perform based on the current state.
#[derive(Debug, Clone, PartialEq, Eq)]
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
        user_id: String,
        /// The document identifier.
        doc_id: String,
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
    pub relay_url: String,
    /// Identifier of the local user.
    pub user_id: String,
    /// Identifier of the document to collaborate on.
    pub doc_id: String,
    /// Whether to automatically connect on creation.
    pub auto_connect: bool,
    /// Retry policy for reconnection attempts.
    pub retry_policy: RetryPolicy,
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

// ── RetryPolicy implementation ──────────────────────────────────────────────

impl RetryPolicy {
    /// Create a new retry policy.
    #[must_use]
    pub const fn new(
        max_retries: u32,
        initial_delay: Duration,
        max_delay: Duration,
        backoff_multiplier: f64,
    ) -> Self {
        Self { max_retries, initial_delay, max_delay, backoff_multiplier }
    }

    /// Compute the delay for a given attempt (0-based).
    ///
    /// Returns `None` if `attempt >= max_retries`.
    #[must_use]
    pub fn delay_for_attempt(&self, attempt: u32) -> Option<Duration> {
        if attempt >= self.max_retries {
            return None;
        }

        let multiplier = self.backoff_multiplier.powi(attempt.try_into().unwrap_or(0));
        let delay = self.initial_delay.mul_f64(multiplier);

        Some(delay.min(self.max_delay))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 5,
            initial_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff_multiplier: 2.0,
        }
    }
}

// ── ConnectionStateMachine implementation ───────────────────────────────────

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
                ConnectionAction::Connect { relay_url: self.config.relay_url.clone() }
            }
            ConnectionState::Connected => ConnectionAction::IdentifyAndSubscribe {
                user_id: self.config.user_id.clone(),
                doc_id: self.config.doc_id.clone(),
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

    /// Request a connection (transitions `Disconnected` -> `Connecting`).
    pub fn connect(&mut self) {
        if self.state == ConnectionState::Disconnected {
            self.state = ConnectionState::Connecting;
        }
    }

    /// Signal that the connection has been established.
    pub fn on_connected(&mut self) {
        self.state = ConnectionState::Connected;
        self.retry_count = 0;
    }

    /// Signal that the connection has been lost.
    pub fn on_disconnected(&mut self) {
        self.retry_count += 1;
        self.state = ConnectionState::Reconnecting { attempt: self.retry_count };
    }

    /// Signal that a connection error occurred.
    pub fn on_error(&mut self, reason: &str) {
        self.retry_count += 1;

        if self.config.retry_policy.delay_for_attempt(self.retry_count - 1).is_none() {
            self.state = ConnectionState::Failed { reason: reason.to_string() };
        } else {
            self.state = ConnectionState::Reconnecting { attempt: self.retry_count };
        }
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
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── helpers ─────────────────────────────────────────────────────────

    fn default_config(auto_connect: bool) -> ConnectionConfig {
        ConnectionConfig {
            relay_url: "ws://localhost:8080".into(),
            user_id: "user-1".into(),
            doc_id: "doc-1".into(),
            auto_connect,
            retry_policy: RetryPolicy::default(),
        }
    }

    // ── RetryPolicy tests ──────────────────────────────────────────────

    #[test]
    fn retry_policy_default_values() {
        let policy = RetryPolicy::default();
        assert_eq!(policy.max_retries, 5);
        assert_eq!(policy.initial_delay, Duration::from_secs(1));
        assert_eq!(policy.max_delay, Duration::from_secs(30));
        assert!((policy.backoff_multiplier - 2.0).abs() < f64::EPSILON);
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
        let policy = RetryPolicy::new(10, Duration::from_secs(1), Duration::from_secs(5), 2.0);
        // attempt 3 would be 8s without cap, but max_delay is 5s
        assert_eq!(policy.delay_for_attempt(3), Some(Duration::from_secs(5)));
    }

    #[test]
    fn retry_policy_returns_none_when_max_retries_exceeded() {
        let policy = RetryPolicy::new(3, Duration::from_secs(1), Duration::from_secs(30), 2.0);
        assert!(policy.delay_for_attempt(3).is_none());
        assert!(policy.delay_for_attempt(4).is_none());
    }

    // ── ConnectionConfig tests ─────────────────────────────────────────

    #[test]
    fn config_with_auto_connect_enabled() {
        let config = default_config(true);
        assert!(config.auto_connect);
        assert_eq!(config.relay_url, "ws://localhost:8080");
    }

    #[test]
    fn config_with_auto_connect_disabled() {
        let config = default_config(false);
        assert!(!config.auto_connect);
    }

    // ── ConnectionStateMachine tests ───────────────────────────────────

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
    fn on_connected_resets_retry_count() {
        let mut sm = ConnectionStateMachine::new(default_config(true));
        // Simulate some retries first
        sm.on_error("timeout");
        sm.on_retry_tick();
        sm.on_connected();
        assert_eq!(*sm.state(), ConnectionState::Connected);
        // Disconnect again; retry_count should have been reset so
        // this starts at attempt 1 again.
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
        let config = ConnectionConfig {
            retry_policy: RetryPolicy::new(2, Duration::from_secs(1), Duration::from_secs(30), 2.0),
            ..default_config(true)
        };
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
}
