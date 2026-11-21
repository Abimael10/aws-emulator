// SPDX-License-Identifier: CC0-1.0

//! The ROW LOCKS the transactions open on one engine endpoint hold, and the two refusals a
//! client reads off them.
//!
//! The table itself is the tree's ONE lock table
//! ([`::service_common::sql_row_locks`]), re-exported here under the name this module tree
//! reaches it by: a DB cluster's data is addressed through more than one surface — the engine
//! endpoints served here and the Data API HTTP endpoint another service serves — so WHO HOLDS
//! WHAT cannot be a table of this crate's own, or a row one surface's transaction holds would
//! be a row the other's walks straight over.
//!
//! What stays here is which TABLE a given endpoint uses ([`super::sql_engine::InstanceDatabase`]):
//! a DB cluster's is the one table of that cluster's volume, shared with its other surfaces,
//! and a DB instance's is its own. Which rows a statement locks is [`super::lock_plan`]'s
//! reading of the statement, and the session ids the holders are named by are the
//! `connection_id` of [`super::wire_session::SessionFacts`] — a PostgreSQL backend pid, a
//! MySQL connection id.

pub use ::service_common::sql_row_locks::{
    LockMode, LockOutcome, LockTable, RowKey, WaitPolicy, MAX_HELD_ROWS,
};
