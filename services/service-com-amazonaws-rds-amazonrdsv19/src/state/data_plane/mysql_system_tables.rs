// SPDX-License-Identifier: CC0-1.0

//! The system schema a MySQL-family server comes up WITH, held on disk like any other table.
//!
//! A MySQL server's `mysql` schema is the server's own storage: the grant tables it reads an
//! account out of and the help tables `HELP` answers from are ordinary tables in it, created
//! when the server's data directory is initialized and present before any client connects
//! (MySQL Reference Manual, "Grant Tables" and "Server-Side Help Support"). So they are part
//! of what the server HOLDS — a snapshot of the instance carries them, an export of that
//! snapshot writes them out, and a session may select from them — which is why they are
//! created where the server starts ([`start_system_schema`]) rather than when the first
//! statement happens to name one.
//!
//! What is NOT here: the two schemas a MySQL server MATERIALISES rather than stores.
//! `information_schema`'s tables are "views" the server builds from its own state, and
//! `performance_schema`'s "tables use no persistent on-disk storage", so neither holds a
//! stored table to snapshot — this emulator rebuilds the `information_schema` catalogs a
//! statement names where it serves that statement ([`super::mysql_server`]) and an export
//! leaves both schemas out ([`crate::state::export_objects`]).
//!
//! Nothing is invented here: the tables are created with the columns MySQL publishes for
//! them and no rows. A row appears when something writes one — an account a session created
//! ([`super::mysql_accounts`]), a setting an Amazon RDS procedure recorded — so a read of one
//! reports what this server really holds.

use super::sql_engine::InstanceDatabase;

/// The schema those tables live in: the one a MySQL server keeps its own storage in.
const SYSTEM_SCHEMA: &str = "mysql";

/// The HELP tables of a MySQL server, with the columns MySQL publishes for each.
///
/// Source: the MySQL Reference Manual's "Server-Side Help Support", which names the four
/// tables of the help catalog (`help_topic`, `help_category`, `help_keyword`,
/// `help_relation`) and the columns of each.
const HELP_TABLES: [&str; 4] = [
    "CREATE TABLE IF NOT EXISTS help_topic (\
       help_topic_id integer PRIMARY KEY, name text NOT NULL, help_category_id integer, \
       description text, example text, url text)",
    "CREATE TABLE IF NOT EXISTS help_category (\
       help_category_id integer PRIMARY KEY, name text NOT NULL, parent_category_id integer, \
       url text)",
    "CREATE TABLE IF NOT EXISTS help_keyword (\
       help_keyword_id integer PRIMARY KEY, name text NOT NULL)",
    "CREATE TABLE IF NOT EXISTS help_relation (\
       help_topic_id integer NOT NULL, help_keyword_id integer NOT NULL, \
       PRIMARY KEY (help_keyword_id, help_topic_id))",
];

/// Create the schema a MySQL server comes up with and the tables it holds in it, leaving
/// every table and row that is already there exactly as it is.
///
/// Called where a MySQL-family engine is STARTED ([`super::open_engine_endpoint`]), so the
/// server holds its own schema from the moment its endpoint answers — before any client has
/// connected, which is when a real server's data directory already has it. Idempotent: a
/// second call creates nothing, so starting a second endpoint over storage that already
/// exists (a DB cluster's volume) neither duplicates nor replaces anything.
///
/// What goes wrong is LOGGED rather than returned: the endpoint is the resource, and a server
/// whose help tables could not be created still serves every session and statement. The
/// account catalog is created the same way where an account statement needs it
/// ([`super::mysql_accounts`]), which is what keeps the two from drifting apart.
pub fn start_system_schema(database: &InstanceDatabase) {
    if let Err(error) = database.create_database(SYSTEM_SCHEMA) {
        ::tracing::warn!(
            error = %error.message,
            "the MySQL server's system schema could not be created"
        );
        return;
    }
    for table in HELP_TABLES
        .iter()
        .chain(super::mysql_accounts::CATALOG_SCHEMA.iter())
    {
        if let Err(error) = database.execute(SYSTEM_SCHEMA, table, &[]) {
            ::tracing::warn!(
                error = %error.message,
                "a table of the MySQL server's system schema could not be created"
            );
        }
    }
}
