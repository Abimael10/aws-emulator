// SPDX-License-Identifier: CC0-1.0

//! The sessions one DB instance's engine endpoint holds OPEN, and what each of them is
//! doing.
//!
//! The engines report their own live connections — PostgreSQL through `pg_stat_activity`,
//! MySQL through its connection count — and they BOUND them: `max_connections` is the
//! parameter that decides how many a server admits at once. Both need the same fact, the
//! set of sessions this endpoint currently serves, so it is kept once, by the endpoint
//! itself, rather than inferred from anything.
//!
//! A session registers when it is admitted and is removed when it ends, on every path
//! including a severed connection, because the entry is dropped by the session's own
//! teardown guard. Nothing here is reported for a session that is not open.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, PoisonError};

/// One live session on an engine endpoint, as the engine reports it.
#[derive(Debug, Clone)]
pub struct LiveSession {
    /// The PostgreSQL backend pid / MySQL connection id of this session.
    pub pid: u32,
    /// The role or account the session authenticated as.
    pub user: String,
    /// The database the session is connected to.
    pub database: String,
    /// The `application_name` the client's startup carried, empty where it sent none.
    pub application_name: String,
    /// The address the client connected from, as the session's own socket gives it.
    pub client_address: String,
    /// When the session was admitted, in the engine's timestamp text form.
    pub backend_start: String,
    /// When the session entered the state it is in NOW, as epoch seconds: what MySQL's
    /// `information_schema.PROCESSLIST.TIME` counts from — "the time in seconds that the
    /// thread has been in its current state".
    pub state_since: f64,
    /// What the session is doing: `active` while a statement of it runs, `idle` between
    /// statements — PostgreSQL's own `pg_stat_activity.state` values.
    pub state: String,
    /// The statement the session is running, or the last one it ran.
    pub query: String,
    /// When that statement started.
    pub query_start: Option<String>,
}

/// The sessions open on one endpoint, by pid, shared by the endpoint and every session on
/// it.
#[derive(Debug, Clone, Default)]
pub struct SessionRegistry {
    sessions: Arc<Mutex<BTreeMap<u32, LiveSession>>>,
    /// The sessions a client has ENDED from another session of the same endpoint (MySQL's
    /// `KILL`), which each of them discovers for itself: a server that kills a connection
    /// closes its socket, and the killed client learns of it on its next statement.
    killed: Arc<Mutex<BTreeSet<u32>>>,
}

/// `pg_stat_activity.state` for a session that is running a statement.
pub const STATE_ACTIVE: &str = "active";
/// `pg_stat_activity.state` for a session between statements.
pub const STATE_IDLE: &str = "idle";

impl SessionRegistry {
    /// Take the entries under the lock, recovering one a panicking session poisoned: a
    /// long-lived emulator has to keep serving every later session.
    fn entries(&self) -> std::sync::MutexGuard<'_, BTreeMap<u32, LiveSession>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// How many sessions are open right now, which is what `max_connections` bounds.
    #[must_use]
    pub fn open_count(&self) -> usize {
        self.entries().len()
    }

    /// Record a session that has just been admitted.
    pub fn admitted(&self, session: LiveSession) {
        self.entries().insert(session.pid, session);
    }

    /// Record what the session `pid` is running now.
    pub fn running(&self, pid: u32, query: &str, started: String) {
        if let Some(session) = self.entries().get_mut(&pid) {
            session.state = STATE_ACTIVE.to_string();
            session.query = query.to_string();
            session.query_start = Some(started);
            session.state_since = super::engine_time::now_epoch_seconds();
        }
    }

    /// Record that the session `pid` has finished its statement.
    pub fn idle(&self, pid: u32) {
        if let Some(session) = self.entries().get_mut(&pid) {
            session.state = STATE_IDLE.to_string();
            session.state_since = super::engine_time::now_epoch_seconds();
        }
    }

    /// Forget the session `pid`, which has ended.
    pub fn ended(&self, pid: u32) {
        self.entries().remove(&pid);
        self.killed().remove(&pid);
    }

    /// The sessions a `KILL` has ended, under the lock.
    fn killed(&self) -> std::sync::MutexGuard<'_, BTreeSet<u32>> {
        self.killed.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// End the session `pid`: it is severed, which it discovers when it next asks this
    /// endpoint for anything. Answers whether this endpoint holds such a session at all,
    /// which is what tells a `KILL` of an id nothing is connected under from one that
    /// killed a session.
    pub fn kill(&self, pid: u32) -> bool {
        if !self.entries().contains_key(&pid) {
            return false;
        }
        self.killed().insert(pid);
        true
    }

    /// Whether the session `pid` has been killed, so its connection is closed rather than
    /// answered.
    #[must_use]
    pub fn is_killed(&self, pid: u32) -> bool {
        self.killed().contains(&pid)
    }

    /// Every session open right now, ordered by pid — what `pg_stat_activity` reports.
    #[must_use]
    pub fn open_sessions(&self) -> Vec<LiveSession> {
        self.entries().values().cloned().collect()
    }
}
