// SPDX-License-Identifier: CC0-1.0

//! The ROW LOCKS the transactions open on one database hold, and the refusals a client reads
//! off them.
//!
//! Both of the SQL engines this emulator serves lock at the level of the ROW: a transaction
//! that writes a row, or that asks for it with `SELECT … FOR UPDATE`, holds that row until it
//! ends, and another transaction that wants the same row waits for it. What the second
//! transaction is told when it cannot have the row is the engine's own condition:
//!
//! * it asked not to wait (`NOWAIT`) — PostgreSQL `lock_not_available`, MySQL
//!   `ER_LOCK_NOWAIT`;
//! * waiting would close a CYCLE of transactions each waiting for the other, which no
//!   amount of waiting resolves: the engine breaks the cycle by refusing exactly ONE of
//!   them — PostgreSQL `deadlock_detected`, MySQL `ER_LOCK_DEADLOCK` — and lets the rest
//!   proceed.
//!
//! This module owns only WHO HOLDS WHAT and who waits for whom; which rows a statement locks
//! is the reading of that statement, which belongs to the surface the statement arrived on.
//! The waiting is done on a condition variable, so a transaction that ends wakes every
//! transaction waiting for one of its rows, and every wait is bounded by the deadline the
//! caller's statement already has — the engine never parks a request forever.
//!
//! ONE TABLE PER DATABASE, not per surface. A resource whose data a client can reach through
//! more than one door — a DB cluster's engine endpoints and its Data API HTTP endpoint, which
//! address one database ([`crate::cluster_sql_volumes`]) — has ONE table for all of them, so a
//! row a transaction opened through one door holds is a row a transaction on another door
//! waits for. Two resources never share one, so no transaction can see another resource's
//! locks.
//!
//! The holders are named by a number each surface draws from its own end of the space: a wire
//! session's own connection id (a PostgreSQL backend pid, a MySQL connection id, drawn from 1
//! upward as connections arrive) and, for a surface whose transactions are not connections,
//! [`LockTable::mint_holder`], which draws from the top downward.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Which row a lock is on: the database, the table (both lowercased, as the engines resolve
/// those names case-insensitively here) and the engine's own row id.
pub type RowKey = (String, String, i64);

/// How long one wait for a row sleeps before it looks at the lock table again. The wake-up
/// a released row sends is what normally ends a wait; this only bounds the wait when the
/// holder went away without one.
const WAIT_SLICE: Duration = Duration::from_millis(25);

/// How many waiting transactions a cycle search walks before it gives up: a bound on the
/// search, not on the number of transactions.
const MAX_WAIT_CHAIN: usize = 1024;

/// How many ROWS the transactions on one database may hold locked at once.
///
/// Every locked row is an entry in the table below, and a transaction keeps its rows until
/// it ends, so a session that never commits would otherwise grow this store with every
/// statement it runs. This is what bounds it: a statement that would take the table past the
/// ceiling is refused ([`LockOutcome::TooMany`]) instead of growing the process, which is the
/// same answer the engine gives a statement that asks for more than this endpoint serves.
pub const MAX_HELD_ROWS: usize = 200_000;

/// What a lock is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockMode {
    /// A read that must not be written under it: `FOR SHARE`, `LOCK IN SHARE MODE`.
    Shared,
    /// A row this transaction is writing, or has reserved for writing: `FOR UPDATE`,
    /// `UPDATE`, `DELETE`.
    Exclusive,
}

/// What the statement asked to happen when the row is already held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitPolicy {
    /// Wait for the holder, which is what a statement that names nothing does.
    Wait,
    /// `NOWAIT`: refuse instead of waiting.
    NoWait,
    /// `SKIP LOCKED`: leave the held rows out of the result.
    SkipLocked,
}

/// What came of asking for a set of rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    /// The rows this transaction now holds — every row asked for, or under `SKIP LOCKED`
    /// the ones that were free.
    Granted(Vec<RowKey>),
    /// A row is held by another transaction and the statement asked not to wait.
    Unavailable,
    /// Waiting would close a cycle, so this transaction is the one the engine refuses.
    Deadlocked,
    /// The statement's own deadline passed while it waited.
    TimedOut,
    /// The rows this statement asks to hold would take this database past
    /// [`MAX_HELD_ROWS`].
    TooMany,
}

/// Who holds one row.
#[derive(Debug, Clone)]
enum Holder {
    /// One transaction, for writing.
    Exclusive(u32),
    /// Every transaction reading it under a shared lock.
    Shared(BTreeSet<u32>),
}

impl Holder {
    /// Whether `session` may take this row in `mode` — a row it already holds exclusively
    /// is its own, and a shared row only it holds can be taken exclusively (the upgrade a
    /// transaction that reads then writes a row makes).
    fn admits(&self, session: u32, mode: LockMode) -> bool {
        match (self, mode) {
            (Self::Exclusive(holder), _) => *holder == session,
            // Two shared readers of one row do not conflict, which is what a shared lock is.
            (Self::Shared(_), LockMode::Shared) => true,
            (Self::Shared(holders), LockMode::Exclusive) => {
                holders.iter().all(|holder| *holder == session)
            }
        }
    }

    /// The transaction a waiter would be waiting for.
    fn blocker(&self, session: u32) -> Option<u32> {
        match self {
            Self::Exclusive(holder) if *holder != session => Some(*holder),
            Self::Shared(holders) => holders.iter().find(|holder| **holder != session).copied(),
            Self::Exclusive(_) => None,
        }
    }
}

/// The rows the transactions on one database hold, and the transaction each waiting one
/// waits for.
#[derive(Debug, Default)]
pub struct LockTable {
    state: Mutex<State>,
    /// Signalled whenever a row is released, which is what ends a wait.
    released: Condvar,
    /// The holder ids [`LockTable::mint_holder`] has handed out, counted down from the top of
    /// the space.
    minted: AtomicU32,
}

#[derive(Debug, Default)]
struct State {
    held: BTreeMap<RowKey, Holder>,
    /// waiter -> the transaction it is waiting for. The WAIT-FOR GRAPH: a cycle in it is a
    /// deadlock.
    waiting: BTreeMap<u32, u32>,
}

impl LockTable {
    /// The lock table, recovering a lock a panicking session poisoned: a long-lived
    /// emulator has to keep serving every later session.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// An id for a holder that is NOT a connection: a transaction a surface opened without
    /// one, which still has to be told apart from every other holder of this table.
    ///
    /// Drawn from the TOP of the space downward, where the connection ids of a wire session —
    /// drawn from 1 upward, one per connection accepted — cannot reach: a resource would have
    /// to serve four billion connections for the two to meet.
    pub fn mint_holder(&self) -> u32 {
        u32::MAX - self.minted.fetch_add(1, Ordering::Relaxed)
    }

    /// Take `rows` for `session` in `mode` WITHOUT waiting for a holder.
    ///
    /// What a statement that asked not to wait does — and the one way of asking that needs no
    /// deadline, because there is nothing to wait until. Every other way through
    /// [`LockTable::acquire`] carries the deadline the caller's statement already has.
    pub fn acquire_without_waiting(
        &self,
        session: u32,
        rows: &[RowKey],
        mode: LockMode,
    ) -> LockOutcome {
        self.acquire(session, rows, mode, WaitPolicy::NoWait, Instant::now())
    }

    /// Take `rows` for `session` in `mode`, answering what the engine answers when they
    /// cannot all be had.
    ///
    /// All or nothing while waiting: the rows are granted only once NONE of them is held by
    /// another transaction, so a statement cannot hold half its rows while it waits for the
    /// rest — which is what would turn two statements over one table into a cycle of their
    /// own. `SKIP LOCKED` is the exception the clause itself defines: it takes the free rows
    /// and reports those.
    pub fn acquire(
        &self,
        session: u32,
        rows: &[RowKey],
        mode: LockMode,
        policy: WaitPolicy,
        deadline: Instant,
    ) -> LockOutcome {
        if rows.is_empty() {
            return LockOutcome::Granted(Vec::new());
        }
        let mut state = self.state();
        loop {
            if would_hold_too_many(&state, rows) {
                return LockOutcome::TooMany;
            }
            match blocked_by(&state, session, rows, mode) {
                None => {
                    grant(&mut state, session, rows, mode);
                    return LockOutcome::Granted(rows.to_vec());
                }
                Some(blocker) => match policy {
                    WaitPolicy::NoWait => return LockOutcome::Unavailable,
                    WaitPolicy::SkipLocked => {
                        let free: Vec<RowKey> = rows
                            .iter()
                            .filter(|row| {
                                state
                                    .held
                                    .get(*row)
                                    .is_none_or(|holder| holder.admits(session, mode))
                            })
                            .cloned()
                            .collect();
                        grant(&mut state, session, &free, mode);
                        return LockOutcome::Granted(free);
                    }
                    WaitPolicy::Wait => {
                        if waits_on(&state, blocker, session) {
                            return LockOutcome::Deadlocked;
                        }
                        let now = Instant::now();
                        if now >= deadline {
                            return LockOutcome::TimedOut;
                        }
                        state.waiting.insert(session, blocker);
                        let slice = (deadline - now).min(WAIT_SLICE);
                        let (waited, _) = self
                            .released
                            .wait_timeout(state, slice)
                            .unwrap_or_else(PoisonError::into_inner);
                        state = waited;
                        state.waiting.remove(&session);
                    }
                },
            }
        }
    }

    /// Let go of every row `session` holds — what COMMIT, ROLLBACK and the end of a session
    /// do — and wake whoever was waiting for one of them.
    pub fn release(&self, session: u32) {
        let mut state = self.state();
        state.held.retain(|_, holder| match holder {
            Holder::Exclusive(holder) => *holder != session,
            Holder::Shared(holders) => {
                holders.remove(&session);
                !holders.is_empty()
            }
        });
        state.waiting.remove(&session);
        drop(state);
        self.released.notify_all();
    }

    /// Let go of `rows` only, which is what a statement OUTSIDE a transaction does: its
    /// locks live for the statement, not for a transaction it is not in.
    pub fn release_rows(&self, session: u32, rows: &[RowKey]) {
        if rows.is_empty() {
            return;
        }
        let mut state = self.state();
        for row in rows {
            let drop_row = match state.held.get_mut(row) {
                Some(Holder::Exclusive(holder)) => *holder == session,
                Some(Holder::Shared(holders)) => {
                    holders.remove(&session);
                    holders.is_empty()
                }
                None => false,
            };
            if drop_row {
                state.held.remove(row);
            }
        }
        drop(state);
        self.released.notify_all();
    }

    /// How many rows are held right now, for the tests that pin that a transaction's locks
    /// go when it does.
    #[must_use]
    pub fn held_rows(&self) -> usize {
        self.state().held.len()
    }
}

/// Whether taking `rows` would take the table past [`MAX_HELD_ROWS`]. Counted over the rows
/// the table does not already hold, so a statement asking again for rows its transaction
/// already holds is not refused for them.
fn would_hold_too_many(state: &State, rows: &[RowKey]) -> bool {
    let fresh = rows
        .iter()
        .filter(|row| !state.held.contains_key(*row))
        .count();
    state.held.len().saturating_add(fresh) > MAX_HELD_ROWS
}

/// The transaction holding the first of `rows` that `session` cannot have in `mode`.
fn blocked_by(state: &State, session: u32, rows: &[RowKey], mode: LockMode) -> Option<u32> {
    rows.iter().find_map(|row| {
        state
            .held
            .get(row)
            .filter(|holder| !holder.admits(session, mode))
            .and_then(|holder| holder.blocker(session))
    })
}

/// Record `session` as the holder of `rows` in `mode`.
fn grant(state: &mut State, session: u32, rows: &[RowKey], mode: LockMode) {
    for row in rows {
        match mode {
            LockMode::Exclusive => {
                state.held.insert(row.clone(), Holder::Exclusive(session));
            }
            LockMode::Shared => match state.held.get_mut(row) {
                Some(Holder::Shared(holders)) => {
                    holders.insert(session);
                }
                Some(Holder::Exclusive(_)) => {}
                None => {
                    state
                        .held
                        .insert(row.clone(), Holder::Shared([session].into()));
                }
            },
        }
    }
}

/// Whether `from` is already waiting, directly or through other waiters, for `target`:
/// that is the cycle a new wait by `target` on `from` would close.
fn waits_on(state: &State, from: u32, target: u32) -> bool {
    let mut at = from;
    for _ in 0..MAX_WAIT_CHAIN {
        if at == target {
            return true;
        }
        match state.waiting.get(&at) {
            Some(next) => at = *next,
            None => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the table below.
    fn row(table: &str, id: i64) -> RowKey {
        ("appdata".to_string(), table.to_string(), id)
    }

    /// RULE: a row one holder has taken exclusively is held against EVERY other holder,
    /// whichever surface that holder came from, until it lets go — and a row nobody holds is
    /// granted at once.
    ///
    /// The rows differ along who asks and for what: another holder for the same row, the same
    /// holder again, another holder for another row, and the same row once the holder has let
    /// it go.
    #[test]
    fn a_row_one_holder_took_is_held_against_every_other_holder() {
        let locks = LockTable::default();
        let wire_session = 7u32;
        let other_surface = locks.mint_holder();
        assert_eq!(
            locks.acquire_without_waiting(
                wire_session,
                &[row("seats", 1)],
                LockMode::Exclusive,
            ),
            LockOutcome::Granted(vec![row("seats", 1)]),
            "case {:?}: a free row is granted at once",
            "the first holder"
        );
        // (case, who asks, the row, what the table answers)
        let rows: Vec<(&str, u32, RowKey, LockOutcome)> = vec![
            (
                "another holder asking for the held row",
                other_surface,
                row("seats", 1),
                LockOutcome::Unavailable,
            ),
            (
                "the holder itself asking again",
                wire_session,
                row("seats", 1),
                LockOutcome::Granted(vec![row("seats", 1)]),
            ),
            (
                "another holder asking for a neighbouring row",
                other_surface,
                row("seats", 2),
                LockOutcome::Granted(vec![row("seats", 2)]),
            ),
            (
                "another holder asking for the same row id of another table",
                other_surface,
                row("tickets", 1),
                LockOutcome::Granted(vec![row("tickets", 1)]),
            ),
        ];
        for (case, holder, asked, expected) in rows {
            let answered =
                locks.acquire_without_waiting(holder, &[asked], LockMode::Exclusive);
            assert_eq!(
                answered, expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }

        // And once the holder lets go, the row is the next asker's.
        locks.release(wire_session);
        let answered = locks.acquire_without_waiting(
            other_surface,
            &[row("seats", 1)],
            LockMode::Exclusive,
        );
        assert_eq!(
            answered,
            LockOutcome::Granted(vec![row("seats", 1)]),
            "case {:?}: expected the released row to be granted, got {answered:?}",
            "the row after its holder let go"
        );
    }

    /// RULE: a minted holder id is each time a NEW one, and never one a connection id of a
    /// wire session can be.
    #[test]
    fn a_minted_holder_id_is_out_of_the_reach_of_a_connection_id() {
        let locks = LockTable::default();
        let minted: Vec<u32> = (0..4).map(|_| locks.mint_holder()).collect();
        let mut unique = minted.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(
            unique.len(),
            minted.len(),
            "case {:?}: every minted id is its own, got {minted:?}",
            "four minted ids"
        );
        // A connection id is drawn from 1 upward; a minted one from the top downward, so no
        // emulated resource's connections can reach one.
        assert!(
            minted.iter().all(|id| *id > u32::MAX / 2),
            "case {:?}: a minted id comes from the top of the space, got {minted:?}",
            "where a minted id comes from"
        );
    }
}
