// SPDX-License-Identifier: CC0-1.0

//! The schemas ONE database of a data warehouse holds — its own catalogue of
//! schemas, which `ListSchemas` reports.
//!
//! The engine behind a database has no notion of a schema: the catalogue is
//! [`Database::schemas`], which `CREATE SCHEMA` adds to, `DROP SCHEMA` removes
//! from and a `CREATE EXTERNAL SCHEMA` binds one of, and which decides what a
//! qualified table name reaches (see [`super::redshift_sql::engine_table_name`]).
//! So the schemas a database holds are read from HERE, while the relations each
//! one contains are read from the engine ([`super::catalog_on`]).

use super::*;

/// The schemas the database `handle` guards holds, in name order, narrowed to
/// the ones `pattern` matches.
///
/// BLOCKING for the same reason [`super::catalog_on`] is: the database's lock is
/// held for a whole execution, so a reporting read of a database that is running
/// a statement waits for it, and the caller runs this where blocking is allowed.
///
/// Reads only — a reporting operation must not add a schema a later caller would
/// then find in the catalogue.
pub fn schemas_on(handle: &DatabaseHandle, pattern: Option<&str>) -> Vec<String> {
    let database = handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    narrowed(database.schemas.iter().map(String::as_str), pattern)
}

/// The schemas a database of the warehouse that no statement has OPENED holds.
///
/// "PUBLIC schema … Every database created in Amazon Redshift has a schema named
/// PUBLIC", so a database the warehouse holds carries that schema from the
/// moment it exists — which is exactly what [`Database::open`] starts a
/// database's catalogue with. A reporting read of such a database reports that
/// one schema rather than opening an engine a later caller would then find.
pub fn unopened_schemas(pattern: Option<&str>) -> Vec<String> {
    narrowed(std::iter::once(DEFAULT_SCHEMA), pattern)
}

/// The names `pattern` admits, in the order they were given.
///
/// A pattern the request did not carry narrows nothing; one it did is matched
/// with the two wild cards the Data API's catalogue patterns document ("`%`
/// means match any substring of 0 or more characters and `_` means match any one
/// character"), by the one matcher this crate has for them.
///
/// Each name is reported as a CALLER spells it: a schema the catalogue holds
/// under a stored spelling of a delimited identifier ([`identifier_case`]) is
/// listed under the name it was created with, and matched as that name.
fn narrowed<'a>(names: impl Iterator<Item = &'a str>, pattern: Option<&str>) -> Vec<String> {
    names
        .map(identifier_case::spelled)
        .filter(|name| {
            pattern.is_none_or(|pattern| session_settings::wild_card_matches(pattern, name))
        })
        .collect()
}
