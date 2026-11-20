// SPDX-License-Identifier: CC0-1.0

//! What each statement a session ran COST this engine, recorded as the session activity the
//! monitoring services report on.
//!
//! Amazon RDS publishes its engines' load as ACTIVE SESSION HISTORY: for each unit of work a
//! session does, how long the session was active for it and what it was waiting on while it
//! was. Performance Insights reports that over a window — the top wait events, statements,
//! users and hosts of the window, each with the load it accounted for — and measures none of
//! it itself: the sessions run here, on this endpoint's engine, so this is the only place
//! those facts exist, and a window in the past can only be reported if the activity was
//! recorded as it happened.
//!
//! So every statement this endpoint runs leaves one or two samples in the SHARED activity
//! store ([`::service_common::db_activity_samples`]), keyed by the IMMUTABLE resource id of
//! the data source the session's endpoint belongs to — the same id the data-source directory
//! keys a resource by, and the id a monitoring request names:
//!
//! * the time the statement spent WAITING for a row another transaction held
//!   ([`super::row_locks`]), under the wait event the engine publishes for that wait; and
//! * the rest of its time, which the engine spent running it, under the event both engines
//!   report for a session that was waiting for nothing: `CPU`.
//!
//! Nothing here is sampled on a timer and nothing is invented: a sample weighs the time this
//! process actually spent, so a resource whose engine ran nothing holds no activity at all.

use std::time::Duration;

use ::service_common::db_activity_samples::{
    ActivitySample, DbActivitySamples, WaitEvent, MAX_STATEMENT_BYTES,
};

use super::wire_session::{SessionFacts, SessionTarget};
use super::EngineWire;

/// The SHARED activity store on the engine this service is mounted on, or `None` when no such
/// store is reachable from it — in which case the endpoint records nothing, exactly as it
/// delivers no log lines without a CloudWatch Logs service to deliver them to.
pub fn sample_store<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<DbActivitySamples>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<DbActivitySamples>)
}

/// A handle on that store, held by every session of one endpoint.
///
/// Cloneable, because each session carries its own handle onto the one store, and it is kept
/// on the session's facts — so a statement running on the blocking pool records what it cost
/// without reaching back into the listener.
#[derive(Clone)]
pub struct ActivityStore {
    samples: ::engine::ServiceState<DbActivitySamples>,
}

/// The handle has no formatted form of its own; the session facts it hangs on print as a
/// whole.
impl std::fmt::Debug for ActivityStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ActivityStore").finish()
    }
}

impl ActivityStore {
    /// The store the engine `engine` holds, for an endpoint whose sessions are attributable
    /// to a data source at all.
    pub fn of<Eng: Send + Sync + 'static>(engine: &Eng) -> Option<Self> {
        sample_store(engine).map(|samples| Self { samples })
    }
}

/// The data source a session on `target` is attributable to: the immutable resource id its
/// create minted.
///
/// `None` for a DB PROXY's endpoint, which is no data source of its own — a session through a
/// proxy runs on the database of the target the proxy resolved, which is what the session's
/// own facts name ([`SessionFacts::data_source_id`]).
#[must_use]
pub fn data_source_of(target: &SessionTarget) -> Option<&str> {
    match target {
        SessionTarget::DbInstance { resource_id, .. }
        | SessionTarget::DbCluster { resource_id, .. } => Some(resource_id.as_str()),
        SessionTarget::DbProxy { .. } => None,
    }
}

/// Record what one statement of `facts` cost: `elapsed` of session time, of which `waited`
/// was spent waiting for a row another transaction held.
///
/// Called for every statement a session runs, whether it succeeded or was refused: a
/// statement that failed still occupied the session while it ran, which is activity the
/// engine had.
pub fn record_statement(facts: &SessionFacts, sql: &str, waited: Duration, elapsed: Duration) {
    let Some(store) = &facts.activity else {
        return;
    };
    if facts.data_source_id.is_empty() {
        return;
    }
    let at = super::engine_time::now_epoch_seconds();
    let statement = truncated(sql);
    let digest = truncated(&tokenized(facts.wire, sql));
    // The waiting half first: the time a statement spent blocked is the engine's own wait
    // event, and the rest of its time is the time it was running.
    let waiting = waited.min(elapsed);
    let running = elapsed.saturating_sub(waiting);
    let mut samples = Vec::new();
    if waiting > Duration::ZERO {
        samples.push((row_lock_wait_event(facts.wire), waiting));
    }
    if running > Duration::ZERO {
        samples.push((WaitEvent::on_cpu(), running));
    }
    if samples.is_empty() {
        return;
    }
    let mut store = store.samples.lock();
    for (wait_event, active) in samples {
        store.record(
            &facts.data_source_id,
            ActivitySample {
                at_epoch_seconds: at,
                active_seconds: active.as_secs_f64(),
                wait_event,
                session_id: facts.connection_id,
                user: facts.user.clone(),
                host: facts.endpoint_host.clone(),
                database: facts.database.clone(),
                application: facts
                    .settings
                    .get(APPLICATION_NAME_SETTING)
                    .cloned()
                    .unwrap_or_default(),
                statement: statement.clone(),
                statement_digest: digest.clone(),
            },
        );
    }
}

/// The session setting carrying the name the client gave its application, which PostgreSQL
/// keeps as a session parameter and a client sets with `SET application_name`.
const APPLICATION_NAME_SETTING: &str = "application_name";

/// The wait event an engine publishes for a session WAITING FOR A ROW LOCK.
///
/// SOURCE: the wait events the Amazon RDS user guide's wait-event tuning chapters publish for
/// each engine — `Lock:transactionid` ("a session is waiting for a row-level lock", the event
/// PostgreSQL itself reports under the `Lock` type in `pg_stat_activity`) for the PostgreSQL
/// engines, and `synch/cond/mysys/my_thread_var::suspend` (the condition a MySQL thread is
/// suspended on while it waits for a row lock, which the Performance Schema groups under
/// `synch`) for the MySQL ones. The TYPE is the class each engine groups the event under,
/// which is what a request grouping by wait-event type reports.
#[must_use]
fn row_lock_wait_event(wire: EngineWire) -> WaitEvent {
    let (name, event_type) = match wire {
        EngineWire::Postgres => ("Lock:transactionid", "Lock"),
        EngineWire::MySql => ("synch/cond/mysys/my_thread_var::suspend", "synch"),
    };
    WaitEvent {
        name: name.to_string(),
        event_type: event_type.to_string(),
    }
}

/// `sql` with its LITERAL values replaced by a placeholder: the digest form, which is what
/// groups the statements that differ only in the values they were run with.
///
/// The statement's own tokens decide it (the dialect of the wire the session speaks), so the
/// digest of a statement is the statement itself with every number and string literal
/// replaced. A statement whose text does not tokenize at all — which the engine refused for
/// that — is its own digest.
#[must_use]
fn tokenized(wire: EngineWire, sql: &str) -> String {
    use ::sqlparser::tokenizer::Token;

    let Ok(tokens) = super::wire_session::significant_tokens(wire, sql) else {
        return sql.to_string();
    };
    let mut digest = String::new();
    for token in tokens {
        let rendered = match token {
            Token::Number(..)
            | Token::SingleQuotedString(_)
            | Token::DoubleQuotedString(_)
            | Token::NationalStringLiteral(_)
            | Token::HexStringLiteral(_)
            | Token::EscapedStringLiteral(_) => "?".to_string(),
            other => other.to_string(),
        };
        if !digest.is_empty() && !rendered.is_empty() {
            digest.push(' ');
        }
        digest.push_str(&rendered);
    }
    match digest.is_empty() {
        true => sql.to_string(),
        false => digest,
    }
}

/// `text` cut to the longest prefix of at most [`MAX_STATEMENT_BYTES`] bytes that is still
/// text: a sample carries no more of a statement than any reader may report, and a cut
/// through a multi-byte character would be no text at all.
#[must_use]
fn truncated(text: &str) -> String {
    if text.len() <= MAX_STATEMENT_BYTES {
        return text.to_string();
    }
    let mut end = MAX_STATEMENT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_string()
}

/// How long a statement spent WAITING for a row another transaction held, kept on the
/// session's facts so the statement path can report it to the sampler above without
/// threading it back out of every step.
///
/// Shared with the clones of the facts a statement runs under, and reset at the start of each
/// statement, so what the sampler reads is this statement's own wait.
#[derive(Debug, Clone, Default)]
pub struct LockWaitClock {
    micros: ::std::sync::Arc<::std::sync::atomic::AtomicU64>,
}

impl LockWaitClock {
    /// Start a fresh statement: nothing has been waited for yet.
    pub fn reset(&self) {
        self.micros
            .store(0, ::std::sync::atomic::Ordering::Relaxed);
    }

    /// Add the time one wait for a set of rows took.
    pub fn add(&self, waited: Duration) {
        let micros = u64::try_from(waited.as_micros()).unwrap_or(u64::MAX);
        self.micros
            .fetch_add(micros, ::std::sync::atomic::Ordering::Relaxed);
    }

    /// What this statement has waited for so far.
    #[must_use]
    pub fn waited(&self) -> Duration {
        Duration::from_micros(self.micros.load(::std::sync::atomic::Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_statements_digest_replaces_the_values_it_was_run_with() {
        // Each row is a statement and the digest its values collapse to, over both wires'
        // dialects. The digest is the statement's own TOKEN stream, so its punctuation
        // stands apart from its words: what groups two statements is that they differ only
        // in the values they carried.
        let rows: &[(&str, EngineWire, &str, &str)] = &[
            (
                "a predicate on a number",
                EngineWire::MySql,
                "SELECT * FROM orders WHERE id = 42",
                "SELECT * FROM orders WHERE id = ?",
            ),
            (
                "a predicate on a string",
                EngineWire::Postgres,
                "UPDATE accounts SET name = 'ada' WHERE id = 7",
                "UPDATE accounts SET name = ? WHERE id = ?",
            ),
            (
                "two statements that differ only in their values",
                EngineWire::MySql,
                "SELECT * FROM orders WHERE id = 99",
                "SELECT * FROM orders WHERE id = ?",
            ),
            (
                "no literal at all",
                EngineWire::MySql,
                "SELECT COUNT(*) FROM orders",
                "SELECT COUNT ( * ) FROM orders",
            ),
        ];
        for (case, wire, sql, expected) in rows {
            let digest = tokenized(*wire, sql);
            assert_eq!(
                digest,
                expected.to_string(),
                "case {case}: expected {expected:?}, got {digest:?}"
            );
        }
    }

    #[test]
    fn a_sample_carries_no_more_of_a_statement_than_a_reader_may_report() {
        let long = format!("SELECT '{}'", "x".repeat(MAX_STATEMENT_BYTES * 2));
        let cut = truncated(&long);
        assert_eq!(
            cut.len(),
            MAX_STATEMENT_BYTES,
            "case a statement past the cap: expected {MAX_STATEMENT_BYTES} bytes, got {}",
            cut.len()
        );
        // A cut that would fall inside a character steps back to the boundary before it.
        let multibyte = format!("SELECT '{}'", "é".repeat(MAX_STATEMENT_BYTES));
        let cut = truncated(&multibyte);
        assert!(
            cut.len() <= MAX_STATEMENT_BYTES && !cut.is_empty(),
            "case a multi-byte statement: expected a cut at a character boundary, got {} bytes",
            cut.len()
        );
    }

    #[test]
    fn the_lock_wait_clock_counts_one_statements_waits() {
        let clock = LockWaitClock::default();
        assert_eq!(
            clock.waited(),
            Duration::ZERO,
            "case a fresh clock: expected no wait recorded"
        );
        clock.add(Duration::from_millis(30));
        clock.add(Duration::from_millis(12));
        assert_eq!(
            clock.waited(),
            Duration::from_millis(42),
            "case two waits of one statement: expected their sum, got {:?}",
            clock.waited()
        );
        clock.reset();
        assert_eq!(
            clock.waited(),
            Duration::ZERO,
            "case the next statement: expected the clock reset, got {:?}",
            clock.waited()
        );
    }
}
