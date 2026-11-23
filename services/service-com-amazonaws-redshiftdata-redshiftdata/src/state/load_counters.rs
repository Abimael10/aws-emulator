// SPDX-License-Identifier: CC0-1.0

//! What the last `COPY` of a SESSION loaded, and the function that reports it.
//!
//! A `COPY` does not report the rows it ingested on the statement record: the
//! Data API's `ResultRows` is "either the number of rows returned from the SQL
//! statement or the number of rows affected", and a load's ingested count is
//! neither — Amazon Redshift keeps it in the SESSION, where
//! `PG_LAST_COPY_COUNT()` reports it ("returns the number of rows that were
//! loaded by the last COPY command executed in the current session").
//!
//! So the count lives here, per CONNECTION ([`SessionLoadCounts`]), and the
//! function that reads it is registered on the engine connection the statement
//! about to run uses ([`install`]) with that connection's own count — which is
//! what makes the value a session's own rather than the database's: a `COPY` on
//! one connection changes nothing the next statement of another connection
//! reads.

use std::collections::HashMap;

/// What the last `COPY` of each connection loaded, by that connection's key.
///
/// Bounded like every other map this service grows on request: a connection
/// whose entry had to make room for another one reads what a connection that
/// has run no load reads.
#[derive(Debug, Default)]
pub struct SessionLoadCounts {
    counts: HashMap<String, i64>,
}

impl SessionLoadCounts {
    /// What the last `COPY` on the connection `key` loaded, and zero for a
    /// connection that has run none — which is what a session reports before
    /// its first load.
    pub fn loaded(&self, key: &str) -> i64 {
        self.counts.get(key).copied().unwrap_or(NO_LOAD_COUNT)
    }

    /// Record what a `COPY` on the connection `key` just loaded.
    pub fn record(&mut self, key: &str, rows: i64, retained: usize) {
        if !self.counts.contains_key(key) && self.counts.len() >= retained {
            if let Some(evicted) = self.counts.keys().next().cloned() {
                self.counts.remove(&evicted);
            }
        }
        self.counts.insert(key.to_string(), rows);
    }

    /// Forget the connection `key`: a connection that has closed holds no
    /// session state, so the next statement on a connection of its own reads
    /// what a fresh one reads.
    pub fn forget(&mut self, key: &str) {
        self.counts.remove(key);
    }
}

/// What a session that has run no `COPY` reports.
const NO_LOAD_COUNT: i64 = 0;

/// Register `PG_LAST_COPY_COUNT()` on `connection`, reporting `loaded`.
///
/// Called before each statement runs, with the count of the connection that
/// statement runs on: the engine holds one connection per database and swaps in
/// the connection of an open transaction, so which session's count the function
/// reports is decided per statement rather than when a connection is opened.
pub fn install(connection: &rusqlite::Connection, loaded: i64) -> rusqlite::Result<()> {
    connection.create_scalar_function(
        "pg_last_copy_count",
        0,
        rusqlite::functions::FunctionFlags::SQLITE_UTF8,
        move |_arguments| Ok(loaded),
    )
}
