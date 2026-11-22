// SPDX-License-Identifier: CC0-1.0

//! The TRANSACTION one connection has open, and what the other connections see
//! while it is.
//!
//! A session is a database connection a caller's later statements reuse, so a
//! transaction a statement opens on it stays open after that statement reports
//! and is ended by a later statement of the SAME session. Everything that
//! follows from that is this module's:
//!
//! * the work done inside an open transaction belongs to the connection that
//!   opened it and to nobody else, so a statement on any other connection does
//!   not see it until it is committed — the isolation a connection gives a
//!   caller;
//! * the transaction's own statements DO see it, each one reading what the ones
//!   before it in the same transaction wrote;
//! * a `COMMIT` makes all of it visible at once, and a `ROLLBACK` makes none of
//!   it visible ever.
//!
//! How: a transaction gets a CONNECTION OF ITS OWN. The engine holds one
//! connection per database — the COMMITTED one, which every statement outside a
//! transaction runs on — and a session that opens a transaction is given a
//! second connection over a copy of that database, taken at the instant the
//! transaction opened with the engine's own page-level copy (the same copy a
//! snapshot is taken with: see [`super::capture`]). The transaction's statements
//! run there, and the engine's own `BEGIN` is open there, so its uncommitted
//! work is in a database nothing else can reach. A `COMMIT` ends that engine
//! transaction and copies the result back over the committed database; a
//! `ROLLBACK` drops the copy, which is what makes everything it held disappear.
//!
//! Two transactions that overlap on one database are held apart the same way,
//! and so is a transaction that overlaps a statement committed outside one — so
//! the one that commits LAST is refused rather than landed over work that is
//! already committed. Amazon Redshift refuses a transaction for the same reason,
//! because it runs transactions serializably, and reports the refusal on the
//! statement that tried to commit, which is where this one is reported too.

use std::time::{Duration, Instant};

use super::{
    bound_reached, capture, redshift_sql::SchemaSet, ConnectionScope, Database, ExecutionFailure,
    MAX_ENGINE_DEADLINE,
};

/// How many transactions may be open on ONE database at once.
///
/// Each open transaction holds a copy of the database it was opened on, so this
/// bounds the engine memory sessions that open transactions and never end them
/// can make the emulator hold, exactly as every other thing a caller can
/// multiply here is bounded. A `BEGIN` that would take a database past it is
/// REFUSED, rather than reported as having opened a transaction nothing holds.
pub const MAX_OPEN_TRANSACTIONS: usize = 8;

/// How long one copy may take. The copy is the engine's own page copy of a
/// single database, whose size the engine bounds by `max_page_count`; this is
/// the wall clock it is given, so it can never run unbounded on the thread an
/// execution runs on.
const COPY_DEADLINE: Duration = MAX_ENGINE_DEADLINE;

/// One open transaction: the connection it runs on, and what the committed
/// database held when it opened.
#[derive(Debug)]
pub(super) struct OpenTransaction {
    /// Either this transaction's own connection or the COMMITTED one, depending
    /// on which of the two is installed on the [`Database`]: while a statement
    /// of the owning connection runs, that statement's connection is the
    /// database's and this field holds the committed one; between submissions it
    /// is the other way round. [`activate`] and [`deactivate`] are the only
    /// things that swap them, and they swap in pairs.
    connection: rusqlite::Connection,
    /// The schemas the database's catalogue held when the transaction opened,
    /// which a `ROLLBACK` gives back: a `CREATE SCHEMA` inside a transaction
    /// that rolled back did not happen either.
    schemas: SchemaSet,
    /// What had been committed to the database when this transaction opened
    /// ([`Database::committed_writes`]). A commit is refused when anything has
    /// been committed since, because the copy this transaction holds was taken
    /// before that work.
    committed_writes: u64,
}

/// Install the connection of the transaction `scope`'s connection has open, if
/// it has one, so that this submission's statements run on it.
///
/// The two connections are SWAPPED, so that the one installed on the database is
/// always the one the current statement runs on: between submissions the entry
/// holds the transaction's own connection, and while a statement of the owning
/// connection runs it holds the committed one. [`deactivate`] swaps them back,
/// and every path out of a submission calls it.
///
/// Called with the database locked, before the submission's first statement.
pub(super) fn activate(database: &mut Database, scope: &ConnectionScope) {
    swap_installed(database, scope);
}

/// Put the committed connection back, undoing [`activate`].
///
/// A transaction the submission OPENED is installed by the open itself, and one
/// it ENDED has already given the committed connection back and holds no entry
/// to swap against — so this restores the same invariant whatever the
/// submission's statements did to the transaction.
pub(super) fn deactivate(database: &mut Database, scope: &ConnectionScope) {
    swap_installed(database, scope);
}

/// Swap the connection installed on the database with the one the connection's
/// open transaction holds, when it has one.
fn swap_installed(database: &mut Database, scope: &ConnectionScope) {
    let Some(open) = database.transactions.get_mut(scope.key()) else {
        return;
    };
    std::mem::swap(&mut database.connection, &mut open.connection);
}

/// Drop the transactions of the connections this service no longer holds a live
/// session for.
///
/// A closed session "can no longer run SQL statements", so its connection is
/// closed and a transaction it never ended is rolled back — which is what
/// closing a connection with a transaction open does. The connections are the
/// ones the state store named when it built `scope`, so only one this service
/// has really let go of is closed here.
pub(super) fn retire_closed(database: &mut Database, scope: &ConnectionScope) {
    for key in scope.retired_transactions() {
        if let Some(open) = database.transactions.remove(key) {
            // The copy goes with the connection, so nothing it held is
            // committed. The engine's transaction is ended first, so the
            // connection is dropped the way a closed one is.
            let _rolled_back = open.connection.execute_batch("ROLLBACK");
        }
    }
}

/// Open a transaction for the connection `scope` names.
///
/// The transaction is given a connection of its own over a copy of the
/// committed database taken now, and the engine's `BEGIN` is run on THAT
/// connection — so the committed connection stays in autocommit and every other
/// connection keeps reading committed data while this transaction is open.
///
/// A connection that does not outlive its submission
/// ([`ConnectionScope::is_session`]) gets no copy: no other statement can run on
/// the database while its submission holds it, so its transaction is opened on
/// the committed connection itself and ends with the submission
/// ([`close_submission`]).
pub(super) fn open(
    database: &mut Database,
    scope: &ConnectionScope,
) -> Result<(), ExecutionFailure> {
    if !scope.is_session() {
        database.connection.execute_batch("BEGIN")?;
        return Ok(());
    }
    if database.transactions.contains_key(scope.key()) {
        // The connection already has one of its own installed, and the engine
        // reports it as closed: a transaction opened again on the same
        // connection is opened there, on the database that connection holds.
        database.connection.execute_batch("BEGIN")?;
        return Ok(());
    }
    if database.transactions.len() >= MAX_OPEN_TRANSACTIONS {
        return Err(bound_reached(format!(
            "this database may hold at most {MAX_OPEN_TRANSACTIONS} open transactions at once, \
             and this statement would open one more"
        )));
    }
    let mut copy = Database::open()?;
    copy_over(&database.connection, &mut copy.connection)
        .map_err(|unavailable| ExecutionFailure::Warehouse(unavailable.to_string()))?;
    copy.connection.execute_batch("BEGIN")?;
    // The transaction's own connection becomes the database's, and the
    // committed one is held on the entry until the transaction ends.
    let committed = std::mem::replace(&mut database.connection, copy.connection);
    database.transactions.insert(
        scope.key().to_string(),
        OpenTransaction {
            connection: committed,
            schemas: database.schemas.clone(),
            committed_writes: database.committed_writes,
        },
    );
    Ok(())
}

/// End the transaction the connection `scope` names has open, committing or
/// rolling back what it did.
///
/// The engine ends its own transaction first, on the transaction's connection. A
/// `COMMIT` then LANDS by copying that database back over the committed one, so
/// what the transaction did becomes visible to every connection at once; a
/// `ROLLBACK` drops the copy, and the catalogue goes back to what it held when
/// the transaction opened.
pub(super) fn close(
    database: &mut Database,
    scope: &ConnectionScope,
    commit: bool,
) -> Result<(), ExecutionFailure> {
    let sql = if commit { "COMMIT" } else { "ROLLBACK" };
    let Some(open) = database.transactions.remove(scope.key()) else {
        // The connection has no transaction of its own, so the one being ended
        // is the committed connection's: the engine ends it and nothing is
        // copied.
        database.connection.execute_batch(sql)?;
        return Ok(());
    };
    // The entry holds the COMMITTED connection while the transaction's own one
    // is installed: the engine's transaction is ended on the installed one, and
    // the committed connection goes back on the database before anything lands.
    let ended = database.connection.execute_batch(sql);
    let transaction = std::mem::replace(&mut database.connection, open.connection);
    if !commit {
        database.schemas = open.schemas;
        return ended.map_err(ExecutionFailure::Engine);
    }
    if let Err(error) = ended {
        database.schemas = open.schemas;
        return Err(ExecutionFailure::Engine(error));
    }
    if open.committed_writes != database.committed_writes {
        // Work was committed to this database while this transaction was open,
        // so the database this transaction holds was copied before that work:
        // landing it would undo what is already committed. Nothing of this
        // transaction lands.
        database.schemas = open.schemas;
        return Err(ExecutionFailure::Warehouse(
            CONCURRENT_TRANSACTION_MESSAGE.to_string(),
        ));
    }
    if let Err(unavailable) = copy_over(&transaction, &mut database.connection) {
        database.schemas = open.schemas;
        return Err(ExecutionFailure::Warehouse(unavailable.to_string()));
    }
    database.committed_writes = database.committed_writes.saturating_add(1);
    Ok(())
}

/// The `Error` a transaction whose commit was refused reports.
///
/// Amazon Redshift runs transactions serializably, so a transaction that
/// overlapped another one is refused rather than committed over it. This
/// emulator holds a transaction's work in a copy of the whole database, so the
/// overlap it can see is that database's: the refusal names that, in this
/// emulator's own words, rather than claiming to have found the cycle of
/// transactions Redshift reports.
pub(super) const CONCURRENT_TRANSACTION_MESSAGE: &str =
    "the transaction could not be committed: work was committed to this database by another \
     connection while this transaction was open, so this transaction was rolled back";

/// Roll back a transaction the submission that opened it left open on the
/// COMMITTED connection, and report whether there was one.
///
/// Only a connection that does not outlive its submission runs a transaction
/// there ([`open`]), and closing a connection with a transaction still open
/// rolls that transaction back — so nothing it did is committed, and the next
/// statement on that database finds the connection in autocommit rather than
/// inside a transaction somebody else opened.
pub(super) fn close_submission(database: &Database) -> bool {
    if database.connection.is_autocommit() {
        return false;
    }
    let _rolled_back = database.connection.execute_batch("ROLLBACK");
    true
}

/// What a statement did to the transaction its connection has open, reported by
/// the execution and carried out where the state store's view of the connections
/// lives (see [`super::connections`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionEffect {
    /// The connection opened a transaction, which outlives this submission.
    Opened,
    /// It ended the transaction it had open.
    Closed,
}

/// Whether the connection `scope` names has a transaction open on a connection
/// of its own, so that what its statements do is not committed to the database
/// the other connections read until it ends.
pub(super) fn holds_open_transaction(database: &Database, scope: &ConnectionScope) -> bool {
    database.transactions.contains_key(scope.key())
}

/// Copy one open database over another, under the bound one copy is given.
fn copy_over(
    source: &rusqlite::Connection,
    destination: &mut rusqlite::Connection,
) -> Result<i64, capture::WarehouseUnavailable> {
    capture::copy_connection(
        source,
        destination,
        Instant::now() + COPY_DEADLINE,
        COPY_DEADLINE,
    )
}
