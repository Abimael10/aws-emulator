// SPDX-License-Identifier: CC0-1.0

//! The SQL engine an Athena query execution RUNS on, and the result set it produces.
//!
//! Athena is an HTTP control plane in front of a query engine: `StartQueryExecution`
//! takes SQL and `GetQueryResults` hands back the rows that SQL produced. So the SQL is
//! really run, by a real engine in this process ([`Database`], `rusqlite`/SQLite), and
//! the rows a read reports are the rows that run computed — nothing here matches on the
//! query text to decide an answer, and nothing synthesises a result set.
//!
//! The engine is CONFINED before any request reaches it (see [`Database::open`]): it
//! holds an in-memory database that names no path, every host-reaching SQL action is
//! refused by a default-deny authorizer, and one run is bounded by a wall-clock deadline,
//! a step budget and a cap on the rows, cells and bytes its result set may retain.
//!
//! One [`Database`] per catalog and database name a query addresses, so two queries in
//! the same database see each other's writes and a query in another database does not.
//! What a statement MEANS to the AWS Glue Data Catalog — the tables and views its DDL
//! defines, and the catalog relations its reads resolve to — is not decided here: this
//! slice runs SQL and holds rows, and [`super::catalog_sql`] is what resolves a statement
//! against the catalog and hands the objects it reads to [`run_over`].

/// The EXACT-NUMERIC (`DECIMAL`) value a statement projects, read as the digits it has
/// rather than through the engine's binary double.
mod exact_decimal;

/// The type a projected column has by the submitted statement's own signature for it,
/// rather than by the values it produced.
mod projection;

/// The COLUMNS a relation alias names, which this engine's dialect names nowhere.
mod relation_columns;

/// What a CAST does with a value that is not a value of the type it names.
mod strict_casts;

/// The table functions Athena's own language reads rows from that this engine's dialect
/// has no production for.
mod table_functions;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::ops::types::{ColumnInfo, Datum, Row};

/// The `StatementType` of a statement that queries or changes DATA: Athena classifies
/// `SELECT`, `INSERT`, `UPDATE`, `DELETE` and `CREATE TABLE AS SELECT` as DML. One of
/// `STATEMENT_TYPE_VALUES` in [`crate::ops::types`] (the model's `StatementType` enum).
pub const STATEMENT_TYPE_DML: &str = "DML";
/// The `StatementType` of a statement that DEFINES an object — `CREATE`, `ALTER`,
/// `DROP` — see [`STATEMENT_TYPE_DML`].
pub const STATEMENT_TYPE_DDL: &str = "DDL";
/// The `StatementType` of everything else Athena accepts, such as `SHOW`, `DESCRIBE`,
/// `EXPLAIN` and `USE` — see [`STATEMENT_TYPE_DML`].
pub const STATEMENT_TYPE_UTILITY: &str = "UTILITY";

/// Athena's type name for a 32-bit integer column, as `ColumnInfo$Type` reports it.
///
/// Every name below is one of Athena's own SQL type names, and the `(precision, scale)`
/// beside it is that type's own width: a 32-bit integer spans 10 decimal digits, a 64-bit
/// one 19, a double carries 17 significant decimal digits, and the unbounded character and
/// binary types report their maximum length. Only the fixed-point type carries digits AFTER
/// the point, so every type named here reports a scale of zero ([`TYPE_DECIMAL`]).
const TYPE_INTEGER: (&str, i32, i32) = ("integer", 10, 0);
/// Athena's type name for a 64-bit integer column; see [`TYPE_INTEGER`].
const TYPE_BIGINT: (&str, i32, i32) = ("bigint", 19, 0);
/// Athena's type name for a double-precision column; see [`TYPE_INTEGER`].
const TYPE_DOUBLE: (&str, i32, i32) = ("double", 17, 0);
/// Athena's type name for a boolean column; see [`TYPE_INTEGER`].
const TYPE_BOOLEAN: (&str, i32, i32) = ("boolean", 0, 0);
/// Athena's type name for a character column; see [`TYPE_INTEGER`].
const TYPE_VARCHAR: (&str, i32, i32) = ("varchar", 2_147_483_647, 0);
/// HIVE's type name for a character column, which is what a statement Athena performs on
/// its Hive DDL engine reports instead of [`TYPE_VARCHAR`]: the type declares no width at
/// all, so the precision beside it is zero rather than a length.
const TYPE_STRING: (&str, i32, i32) = ("string", 0, 0);
/// Athena's type name for a binary column; see [`TYPE_INTEGER`].
const TYPE_VARBINARY: (&str, i32, i32) = ("varbinary", 1_073_741_824, 0);
/// The type name reported for a column the engine produced no typed value for — a
/// projection of `NULL`, or a column of a result set with no rows at all, neither of
/// which carries a type to report; see [`TYPE_INTEGER`].
const TYPE_UNKNOWN: (&str, i32, i32) = ("unknown", 0, 0);
/// Athena's type name for an EXACT-NUMERIC column, the one type whose width is not the
/// type's own: a `DECIMAL(precision, scale)` column reports the precision and the scale the
/// value was read to (see [`exact_decimal`]), which is also where `ColumnInfo$Scale` —
/// "meaningful for DECIMAL data types" — is anything but zero.
const TYPE_DECIMAL: &str = "decimal";

/// The `ColumnNullable` every column reports.
///
/// The engine answers nothing about whether one column of a projection admits NULL, so
/// `UNKNOWN` is what can honestly be said; it is one of `COLUMN_NULLABLE_VALUES` in
/// [`crate::ops::types`].
const COLUMN_NULLABLE_UNKNOWN: &str = "UNKNOWN";

/// The `ColumnInfo$CatalogName` of every column of every result set this service serves:
/// the name of the ENGINE that computed it.
///
/// Athena's own engine is the Hive/Trino one its `AwsDataCatalog` is read through, and that
/// is the catalog a result column belongs to whichever Data Catalog the statement's context
/// named — so it is the engine's name rather than the request's.
const RESULT_CATALOG_NAME: &str = "hive";

/// The `ColumnInfo$SchemaName` and `ColumnInfo$TableName` of a column that is not projected
/// from one named relation: the EMPTY string, which is the value Athena reports where a
/// column has no originating database or table rather than an omitted member.
const NO_ORIGIN_RELATION: &str = "";

/// How long ONE query's engine run may take, wall clock.
///
/// The EMULATOR's own bound, with no AWS counterpart (Athena's own query timeout is
/// measured in minutes): a query that is still running past this is stopped and the
/// execution reports FAILED, so no request can keep an engine thread for longer than
/// this however expensive the SQL it submitted is. Set to what a read of the largest data
/// one query may go through ([`MAX_MATERIALISED_BYTES`]) needs to finish — listing,
/// decoding and materialising a table of that many bytes, and then computing over it —
/// because a scan AWS completes must complete here too; a query that wants more than that
/// is stopped rather than given the thread.
pub const MAX_QUERY_DEADLINE: Duration = Duration::from_secs(10);
/// Step budget per second of [`MAX_QUERY_DEADLINE`], counted in SQLite virtual-machine
/// instructions. A wall-clock deadline cannot stop a CPU-bound query if the host clock
/// stalls or the process is suspended; this can. Set well above the instruction rate the
/// engine sustains, so it is the backstop and not a second, tighter limit.
const SQL_STEPS_PER_SECOND: u64 = 150_000_000;

/// How many virtual-machine instructions pass between two consecutive checks of the
/// deadline and the step budget.
const PROGRESS_HANDLER_OPS: i32 = 1_000;

/// How many rows one query's result set may retain. A result set is held for as long as
/// the execution is readable, so this bounds what one query can make the emulator keep.
pub const MAX_RESULT_ROWS: usize = 10_000;

/// How many VALUES (rows times columns) one query's result set may retain. A cell costs
/// memory whatever it holds, so a wide projection of constants is bounded by this even
/// though it charges almost nothing in bytes.
const MAX_RESULT_CELLS: usize = 200_000;

/// How many bytes of materialised result one query's result set may retain.
const MAX_RESULT_BYTES: usize = 4 * 1024 * 1024;

/// How many rows one query may MATERIALISE from the Data Catalog: the rows of every
/// catalog table it reads, held in the engine database for the length of the run.
///
/// The EMULATOR's own bound, with no AWS counterpart (Athena scans whatever the table's
/// location holds): without it the objects a caller put at one location would decide how
/// much this process holds for one query. Raised with the byte bound below only as far as a
/// table of [`MAX_MATERIALISED_BYTES`] of narrow records needs, so that it is still a bound
/// an ordinary read meets rather than a formality.
pub const MAX_MATERIALISED_ROWS: usize = 750_000;

/// How many BYTES of object data one query may read while materialising the catalog
/// tables it reads; the byte counterpart of [`MAX_MATERIALISED_ROWS`].
///
/// Set above the smallest data usage cutoff a workgroup may configure
/// (`WorkGroupConfiguration$BytesScannedCutoffPerQuery`, `@range` min 10000000), so a table
/// a caller built to exercise that cutoff is one a query with NO cutoff still reads whole:
/// what bounds what such a read HOLDS is the rows and the values one query may materialise
/// ([`MAX_MATERIALISED_ROWS`], [`MAX_MATERIALISED_CELLS`]), which this does not raise.
pub const MAX_MATERIALISED_BYTES: usize = 24 * 1024 * 1024;

/// The CEILING on the object data one query may read, whatever data usage cutoff the
/// workgroup it runs in configures.
///
/// A workgroup cutoff is a limit the caller sets, so a query in such a workgroup reads up to
/// its cutoff rather than stopping at this emulator's default cap, which would pre-empt the
/// limit the caller asked to be held to. The cutoff itself is a caller-chosen number of no
/// upper bound, so this is where that raising stops: the EMULATOR's own ceiling on what one
/// query can make this process read. What a read then holds is bounded as before, by the
/// rows and the values one query may materialise.
pub const MAX_SCAN_BYTES: usize = 32 * 1024 * 1024;

/// How many VALUES (rows times columns) one query may materialise from the Data Catalog.
///
/// The EMULATOR's own bound, and the one that bounds the PRODUCT the row count alone does
/// not: how many rows an object holds and how many columns a table declares are both
/// caller-chosen, and one value costs memory whatever it holds, so a row of a wide table
/// is charged for its width.
pub const MAX_MATERIALISED_CELLS: usize = 2_000_000;

/// How many RELATIONS one statement may resolve through the data catalog, counted over
/// the whole statement and every level of view it reads through.
///
/// The EMULATOR's own bound, and the one that caps the PRODUCT rather than each factor:
/// a statement names at most as many relations as its own parse carries, and each of
/// those may be a view over as many more, so without one total count the work would grow
/// with the product of the two. Reaching it fails the query rather than resolving on.
pub const MAX_RESOLVED_RELATIONS: usize = 256;

/// What one retained cell costs beyond its payload, charged against
/// [`MAX_RESULT_BYTES`] so a NULL cell is never free.
const RETAINED_CELL_OVERHEAD: usize = std::mem::size_of::<Datum>();

/// What one retained row costs beyond its cells: the row's own vector.
const RETAINED_ROW_OVERHEAD: usize = std::mem::size_of::<Row>();

/// Worst-case expansion of a text value rendered through `String::from_utf8_lossy`,
/// which replaces each invalid byte with a three-byte replacement character.
const LOSSY_TEXT_EXPANSION: usize = 3;

/// Worst-case expansion of a binary value, which is rendered as two hex digits a byte.
const HEX_EXPANSION: usize = 2;

/// `SQLITE_LIMIT_LENGTH`: the largest single string or blob the engine will produce, so
/// one value cannot be larger than the whole result budget.
const MAX_VALUE_BYTES: i32 = 1024 * 1024;

/// `SQLITE_LIMIT_SQL_LENGTH`: the largest statement text the engine will accept. The
/// model already bounds `QueryString` at 262144 bytes; this is the engine's own guard.
const MAX_SQL_BYTES: i32 = 1024 * 1024;

/// `SQLITE_LIMIT_COLUMN`: the widest row the engine will produce.
const MAX_COLUMNS: i32 = 1_000;

/// `SQLITE_LIMIT_EXPR_DEPTH`: the depth bound on a parsed expression tree, so a deeply
/// nested expression cannot recurse the parser off its stack.
const MAX_EXPR_DEPTH: i32 = 100;

/// `SQLITE_LIMIT_COMPOUND_SELECT`: how many `UNION`/`EXCEPT`/`INTERSECT` terms one
/// statement may chain.
const MAX_COMPOUND_SELECT: i32 = 50;

/// `PRAGMA max_page_count`, applied to BOTH schemas a statement can write (`main` and
/// `temp`). At SQLite's default 4 KiB page this is 48 MiB each, so a query that builds a
/// huge table runs out of database rather than out of the host's memory. Above what the
/// object data one query ordinarily reads materialises to, and under what the engine may
/// ALLOCATE in all ([`MAX_ENGINE_HEAP_BYTES`], a process-wide ceiling this does not raise) —
/// which is the bound that holds however many databases are open, and the one a read past
/// either page count meets first.
const MAX_DATABASE_PAGES: i64 = 12_288;

/// `PRAGMA hard_heap_limit`: the ceiling on what the SQL engine may allocate, so engine
/// memory is capped however many databases are open and whatever one query does.
const MAX_ENGINE_HEAP_BYTES: i64 = 256 * 1024 * 1024;

/// SQL functions this emulator refuses even though the engine's own build may carry
/// them: each one exists to reach something outside the database.
const DENIED_SQL_FUNCTIONS: &[&str] = &[
    "load_extension",
    "readfile",
    "writefile",
    "fsdir",
    "edit",
    "fts3_tokenizer",
];

/// One database a query runs in: a real SQL engine in this process, confined before it
/// ever sees caller SQL (see [`Database::open`]).
#[derive(Debug)]
pub struct Database {
    /// The engine connection. Private: every statement reaches it through [`run`], which
    /// is what installs the bounds one run is held to.
    connection: rusqlite::Connection,
    /// Set while a run reads STORED data: the engine's own authorizer raises it when it
    /// authorizes a read of a table, so whether a statement scanned anything is the
    /// engine's answer and not a reader of the SQL text. Cleared by [`run`] before each
    /// statement, which holds this database's lock for the whole run.
    reads_stored_data: Arc<AtomicBool>,
}

/// A database as the state holds it, so two executions in the same database run one at a
/// time on one engine and an execution never holds the service's own state lock while
/// the engine works.
pub type DatabaseHandle = Arc<Mutex<Database>>;

impl Database {
    /// Open a fresh in-memory database and confine it.
    ///
    /// The engine is a real SQL engine, not a purpose-built tree walker, so the
    /// statements it would otherwise accept include ones that act on the HOST rather than
    /// on this database: `ATTACH DATABASE '<path>'` opens an arbitrary host file,
    /// `VACUUM INTO '<path>'` writes one, and a `PRAGMA` can turn the engine's own
    /// switches back on. Every one of those is closed here, before any request reaches
    /// `prepare`, and the order matters: the emulator's own `PRAGMA`s run first, because
    /// the authorizer installed at the end refuses `PRAGMA` from then on.
    fn open() -> rusqlite::Result<Self> {
        // Opened WITHOUT `SQLITE_OPEN_URI`: a URI filename is another way a
        // caller-chosen string could name a file, and nothing here needs it.
        // `SQLITE_OPEN_MEMORY` keeps the database off the filesystem entirely.
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
            | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
            | rusqlite::OpenFlags::SQLITE_OPEN_MEMORY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let mut connection = rusqlite::Connection::open_in_memory_with_flags(flags)?;

        // `temp_store = MEMORY`: a large sort must not spill a file into the host's temp
        // directory. `max_page_count` on BOTH `main` and `temp`, because the pragma
        // applies to the schema it is issued against and every temporary object is on the
        // process heap. `hard_heap_limit` bounds the engine's allocator itself.
        connection.execute_batch(&format!(
            "PRAGMA temp_store = MEMORY;
             PRAGMA hard_heap_limit = {MAX_ENGINE_HEAP_BYTES};
             PRAGMA main.max_page_count = {MAX_DATABASE_PAGES};
             PRAGMA temp.max_page_count = {MAX_DATABASE_PAGES};"
        ))?;

        // Extension loading stays off DELIBERATELY: the dependency's `load_extension`
        // feature is not enabled, so neither the C entry point nor its Rust wrapper is
        // reachable from this crate at all, and the authorizer below denies the
        // `load_extension()` SQL function by name as well.
        // Defensive mode: no direct writes to the schema or to shadow tables.
        connection.set_db_config(rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true)?;
        // The schema is not trusted: a view or trigger stored in the database cannot
        // reach a privileged SQL function.
        connection.set_db_config(
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
            false,
        )?;
        // A DOUBLE-QUOTED name is an IDENTIFIER, never a string literal. Athena's SQL
        // dialect quotes an identifier with double quotes and a string with single ones, so
        // a statement naming a column the relation does not carry has to fail as an unknown
        // column — which is what a reader must be told about a column its Lake Formation
        // grant leaves out. This engine's own default would instead read such a name as the
        // text inside the quotes and answer with it.
        for quoted_strings in [
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DML,
            rusqlite::config::DbConfig::SQLITE_DBCONFIG_DQS_DDL,
        ] {
            connection.set_db_config(quoted_strings, false)?;
        }

        use rusqlite::limits::Limit;
        // No database may be attached at all, which closes `ATTACH` even if the
        // authorizer below were ever bypassed.
        connection.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;
        connection.set_limit(Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES)?;
        connection.set_limit(Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES)?;
        connection.set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_COLUMNS)?;
        connection.set_limit(Limit::SQLITE_LIMIT_EXPR_DEPTH, MAX_EXPR_DEPTH)?;
        connection.set_limit(Limit::SQLITE_LIMIT_COMPOUND_SELECT, MAX_COMPOUND_SELECT)?;

        // The EXACT-NUMERIC reading, registered before any request reaches the engine: a
        // `DECIMAL` value is carried as the digits it has rather than through this engine's
        // binary double (see [`exact_decimal`]). The authorizer below admits the call as it
        // admits every other function that is not one of `DENIED_SQL_FUNCTIONS`.
        exact_decimal::register_exact_decimal(&connection)?;

        // The CHECKED reading of a cast, registered for the same reason: a value that is not
        // a value of the type a cast names fails the query rather than being read by this
        // engine's own rules (see [`strict_casts`]).
        strict_casts::register_strict_casts(&connection)?;

        // Default-deny: every action a query legitimately needs is named, and anything
        // else — including an action a future engine version introduces — is refused.
        // The authorizer also OBSERVES which actions a statement takes, so a read of
        // stored data is noted on the way past; the decision itself is `authorize`'s and
        // nothing here widens it.
        let reads_stored_data = Arc::new(AtomicBool::new(false));
        let observer = Arc::clone(&reads_stored_data);
        // The install is CHECKED: a connection whose authorizer could not be installed
        // would run a query unconfined, so the open fails instead of handing one back.
        connection.authorizer(Some(
            move |context: rusqlite::hooks::AuthContext<'_>| -> rusqlite::hooks::Authorization {
                if matches!(context.action, rusqlite::hooks::AuthAction::Read { .. }) {
                    observer.store(true, Ordering::SeqCst);
                }
                authorize(context)
            },
        ))?;

        Ok(Self {
            connection,
            reads_stored_data,
        })
    }
}

/// A confined database, ready to run queries, or the engine's own message for why one
/// could not be opened.
pub fn open_database() -> Result<DatabaseHandle, String> {
    Database::open()
        .map(|database| Arc::new(Mutex::new(database)))
        .map_err(|error| scrub_paths(&error.to_string()))
}

/// The engine's default-deny authorizer: what a submitted query may do.
fn authorize(context: rusqlite::hooks::AuthContext<'_>) -> rusqlite::hooks::Authorization {
    use rusqlite::hooks::{AuthAction, Authorization};
    match context.action {
        // The host-filesystem escapes, denied by name. `ATTACH`/`DETACH` open an
        // arbitrary path; `PRAGMA` is also how `VACUUM INTO '<path>'` is authorized, and
        // how the switches set in `Database::open` would be turned back on.
        AuthAction::Attach { .. } | AuthAction::Detach { .. } | AuthAction::Pragma { .. } => {
            Authorization::Deny
        }
        // A virtual table module is an arbitrary extension point, some of whose
        // implementations read the filesystem.
        AuthAction::CreateVtable { .. } | AuthAction::DropVtable { .. } => Authorization::Deny,
        // A function whose whole purpose is to reach outside the database.
        AuthAction::Function { function_name, .. }
            if DENIED_SQL_FUNCTIONS
                .iter()
                .any(|denied| function_name.eq_ignore_ascii_case(denied)) =>
        {
            Authorization::Deny
        }
        // The SQL a query legitimately runs: query, modify and define objects inside this
        // one database.
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Insert { .. }
        | AuthAction::Update { .. }
        | AuthAction::Delete { .. }
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::DropIndex { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::DropView { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::AlterTable { .. }
        | AuthAction::Reindex { .. }
        | AuthAction::Analyze { .. }
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::Recursive
        | AuthAction::Function { .. } => Authorization::Allow,
        // Anything else, including an action this engine version does not yet name:
        // refused rather than allowed by omission.
        _ => Authorization::Deny,
    }
}

/// The bound ONE run is held to: a deadline, a remaining step budget and an abandon
/// flag, read by the connection's progress handler.
#[derive(Debug, Default)]
struct RunGuard {
    /// When the run must stop, or `None` while no run is in flight.
    deadline: Mutex<Option<Instant>>,
    /// Engine instructions this run may still spend.
    steps_left: AtomicU64,
    /// Set once the run has been abandoned, whatever the clock says.
    abandoned: AtomicBool,
}

impl RunGuard {
    /// Arm the guard for one run.
    fn arm(&self, timeout: Duration, steps: u64) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(Instant::now() + timeout);
        self.steps_left.store(steps, Ordering::SeqCst);
    }

    /// Disarm the guard once the run has returned, so an idle connection is never
    /// interrupted.
    fn disarm(&self) {
        *self
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = None;
    }

    /// Whether the engine should abandon the running statement. Called from the progress
    /// handler every [`PROGRESS_HANDLER_OPS`] instructions.
    fn should_interrupt(&self) -> bool {
        if self.abandoned.load(Ordering::SeqCst) {
            return true;
        }
        let deadline = *self
            .deadline
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let Some(deadline) = deadline else {
            return false;
        };
        if Instant::now() >= deadline {
            return true;
        }
        let spent = u64::try_from(PROGRESS_HANDLER_OPS).unwrap_or(1);
        // Compared AFTER this callback's own spend, so the run is interrupted on the
        // callback that exhausts the budget rather than on the one after it.
        //
        // `try_update` is the current name of the atomic read-modify-write this budget has
        // always used: the standard library renamed `fetch_update` to it and deprecates the
        // old name, which this workspace's deny-on-warnings build turns into an error. Same
        // signature and orderings, and it answers the PREVIOUS value — which is what
        // `previous` is read as here, so the budget still trips on exactly the callback that
        // spends the last of it.
        let previous = self
            .steps_left
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                Some(left.saturating_sub(spent))
            })
            .unwrap_or(0);
        previous.saturating_sub(spent) == 0
    }
}

/// The step budget one run gets, derived from [`MAX_QUERY_DEADLINE`] at
/// [`SQL_STEPS_PER_SECOND`] so the two bounds scale together.
fn step_budget() -> u64 {
    let millis = u64::try_from(MAX_QUERY_DEADLINE.as_millis()).unwrap_or(u64::MAX);
    millis
        .saturating_mul(SQL_STEPS_PER_SECOND / 1_000)
        .max(u64::try_from(PROGRESS_HANDLER_OPS).unwrap_or(1))
}

/// One query's result set, as the execution that produced it stores it.
///
/// Every value is held as the text Athena reports it as (`Datum$VarCharValue`), so a
/// repeat `GetQueryResults` reports byte-for-byte what the first one did.
#[derive(Debug, Clone, Default)]
pub struct StoredResultSet {
    /// One entry per column the query projected, in projection order.
    pub column_info: Vec<ColumnInfo>,
    /// The DATA rows, without the column-label row `GetQueryResults` puts in front of
    /// them: that row is the reading operation's own rendering of `column_info`.
    pub rows: Vec<Row>,
}

/// The result set of a statement whose result is the engine's TEXT output rather than
/// rows — a statement Athena classifies as DDL or UTILITY produces no result set, and
/// what a read reports for it is this one: no columns and no rows, present rather than
/// absent.
pub static TEXT_OUTPUT_RESULT_SET: StoredResultSet = StoredResultSet {
    column_info: Vec::new(),
    rows: Vec::new(),
};

/// What running one query produced.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The engine ran the statement.
    Succeeded {
        /// The rows and columns it produced; empty for a statement that returns none.
        result: StoredResultSet,
        /// How many rows a data-manipulating statement wrote, for `UpdateCount`.
        update_count: Option<i64>,
        /// How many bytes of data the engine read to answer, for `DataScannedInBytes`.
        data_scanned_bytes: i64,
        /// How many rows the run READ from the stored objects its statement named: the
        /// rows materialised in the engine database for it (see [`materialised_rows`]).
        /// Counted beside the bytes, because what a run read is reported as a row count
        /// as well as a byte count.
        input_rows: i64,
        /// The objects the statement WROTE into the object store, as their `s3://` URIs in
        /// the order they were written — what the execution's data manifest lists. `None`
        /// for a statement that writes no output of its own at all, which is every
        /// statement that answers with rows rather than writing them; a statement that DID
        /// write its output carries the objects it wrote, and none at all where what it
        /// produced was no rows (see `super::catalog_sql::query_output`).
        written_objects: Option<Vec<String>>,
    },
    /// The statement did not run to completion, with the message for why.
    Failed {
        /// The engine's message, with anything path-shaped redacted, or the emulator's
        /// own for a bound it holds a run to.
        message: String,
        /// The execution's `StateChangeReason`, where what performed the statement
        /// reports something other than [`Self::Failed::message`]; `None` where the two
        /// are the same text.
        reason: Option<String>,
        /// Whose doing the failure was, which is what the reported error category
        /// follows from.
        cause: FailureCause,
    },
    /// The run was ABORTED by a limit in force on the query rather than by anything the
    /// statement itself did, so the query is INTERRUPTED rather than failed: it reaches
    /// the cancelled state ([`crate::state::QUERY_STATE_CANCELLED`]) carrying the reason
    /// that limit publishes, and the data usage the limit accounts for.
    Aborted {
        /// The execution's `StateChangeReason`: what the limit that stopped the run
        /// publishes about it.
        reason: String,
        /// What `DataScannedInBytes` reports for the aborted run, which for a data usage
        /// limit is the limit itself — the run was stopped at it.
        data_scanned_bytes: i64,
    },
}

/// Whose doing a failed query's failure was.
///
/// Read off the error CODE the engine reports, never off its message: an authorization
/// denial and an interruption are this emulator's doing, and every other engine error is
/// the SQL submitted. The engine reports a DENIED FUNCTION as a plain statement error
/// rather than as an authorization denial, so that one is reported as the SQL — which is
/// what the engine said about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// The engine refused, interrupted or could not run the SQL submitted — the
    /// caller's own input.
    Sql,
    /// This EMULATOR stopped the run: a bound it holds a run to was reached, or its
    /// authorizer refused the statement outright. The SQL is not wrong; what it asked
    /// for is not allowed here, or it outgrew what is allowed.
    Emulator,
    /// A failure of one of Athena's OWN classes, carrying the category and the error type
    /// Athena publishes for it: a name the data catalog does not hold, an object it
    /// already holds, a view a statement addresses that is not one. The classification
    /// comes from the class of the failure, never from the engine.
    Athena {
        /// The `AthenaError$ErrorCategory` of that class.
        category: i32,
        /// The `AthenaError$ErrorType` Athena publishes for it, where it publishes one.
        error_type: Option<i32>,
    },
}

/// One run: what it produced, and how long each phase of it took.
#[derive(Debug, Clone)]
pub struct Completion {
    /// The rows, or the failure.
    pub outcome: Outcome,
    /// What the engine spent on it, reported as `EngineExecutionTimeInMillis`.
    pub engine_time: Duration,
    /// What resolving the statement against the data catalog spent before the engine ran
    /// it, reported as `QueryPlanningTimeInMillis`.
    pub planning_time: Duration,
}

/// Run `sql` on the engine `handle` guards, with `parameters` bound to its placeholders
/// in order.
///
/// `counts_rows` says whether the engine counts the rows this statement writes, which is
/// what decides whether a row count is reported at all.
///
/// BLOCKING, deliberately: the engine is synchronous, so a caller must put this on a
/// blocking pool (`tokio::task::spawn_blocking`) rather than run it on an async worker.
/// It takes only this one database's lock and never the service's state lock, so one
/// caller's slow query serialises nothing but its own database.
pub fn run(
    handle: &DatabaseHandle,
    sql: &str,
    counts_rows: bool,
    parameters: &[String],
) -> Completion {
    run_over(handle, &[], sql, counts_rows, parameters, 0)
}

/// One object a statement reads that lives OUTSIDE the engine: a catalog table's rows, or
/// a catalog view's query, held in the engine database for the length of the run.
#[derive(Debug, Clone)]
pub enum LocalObject {
    /// A table and the rows it holds.
    Table(LocalTable),
    /// A table whose rows a PREDICATE narrows: the rows are loaded into a scratch table
    /// the engine evaluates the predicate over, the kept rows are projected into the table
    /// the statement reads, and the scratch table is gone again before the statement runs
    /// — so nothing the predicate rejected, and no column the projection leaves out, is in
    /// the database while the statement is executing.
    Filtered {
        /// The table the statement reads: the name it reads it under, and the columns that
        /// name carries — a subset of `source_columns`, in their order.
        table: LocalTable,
        /// The columns the rows are loaded with, which the predicate is evaluated over:
        /// they may include a column the statement may not read.
        source_columns: Vec<LocalColumn>,
        /// The boolean SQL expression a row has to satisfy to be kept.
        predicate: String,
    },
    /// A view and the query its rows come from.
    View {
        /// The name the view is reachable under.
        name: String,
        /// The SQL its rows come from, already naming the objects it reads.
        sql: String,
    },
}

/// One table a statement reads, as the engine holds it.
#[derive(Debug, Clone)]
pub struct LocalTable {
    /// The name its rows are reachable under.
    pub name: String,
    /// Its columns, in order.
    pub columns: Vec<LocalColumn>,
    /// Its rows: one value per column, in column order.
    pub rows: Vec<Vec<rusqlite::types::Value>>,
}

/// One column of a [`LocalTable`].
#[derive(Debug, Clone)]
pub struct LocalColumn {
    /// The column name.
    pub name: String,
    /// The type the engine holds it as, which is also the type a result set reports for
    /// it.
    pub declared: &'static str,
}

/// Run `sql` over `objects`: materialise each one in the engine database, then run the
/// statement that reads them.
///
/// `scanned_bytes` is what reading those objects cost, which for a statement over catalog
/// tables is the data the query scanned and therefore what `DataScannedInBytes` reports;
/// a statement over nothing but the engine's own tables reports the engine's own measure.
///
/// BLOCKING, exactly as [`run`] is, and under the same bounds: materialising happens
/// inside the same armed deadline and step budget as the statement itself.
pub fn run_over(
    handle: &DatabaseHandle,
    objects: &[LocalObject],
    sql: &str,
    counts_rows: bool,
    parameters: &[String],
    scanned_bytes: i64,
) -> Completion {
    // A statement reading its rows from a table function this dialect lacks names those
    // same rows in a production it has; one carrying none is run exactly as submitted.
    let expanded = table_functions::expanded(sql);
    let sql = expanded.as_deref().unwrap_or(sql);
    // A relation whose alias names its own COLUMNS names them where this dialect names the
    // columns of a relation instead; one whose aliases name none is, again, run as written.
    let aliased = relation_columns::named_columns(sql);
    let sql = aliased.as_deref().unwrap_or(sql);
    // An exact-numeric value the statement PROJECTS is read as the digits it has rather
    // than through this engine's double; a statement projecting none is, again, run as
    // submitted.
    let exact = exact_decimal::exactly_projected(sql);
    let sql = exact.as_deref().unwrap_or(sql);
    // A cast to a number or a boolean has its argument judged before the cast reads it, so a
    // value that is not a value of that type fails the query as Athena fails it.
    let checked = strict_casts::checked(sql);
    let sql = checked.as_deref().unwrap_or(sql);
    let mut database = handle.lock().unwrap_or_else(PoisonError::into_inner);
    let guard = Arc::new(RunGuard::default());
    let handler = Arc::clone(&guard);
    if let Err(error) = database
        .connection
        .progress_handler(PROGRESS_HANDLER_OPS, Some(move || handler.should_interrupt()))
    {
        // The bound is what makes an abandoned statement stop; a statement that could only
        // run without it does not run at all, and the engine's own words say why.
        let failure = RunFailure::Engine(error);
        return Completion {
            outcome: Outcome::Failed {
                message: failure.message(),
                reason: None,
                cause: failure.cause(),
            },
            engine_time: std::time::Duration::ZERO,
            planning_time: std::time::Duration::ZERO,
        };
    }

    let input_rows = materialised_rows(objects);
    // What type each column of the stored data this run reads is DECLARED as, which is
    // the type an expression over one of those columns has whatever values it produced
    // (see [`projection`]).
    let source_types = source_column_types(objects);
    let started = Instant::now();
    guard.arm(MAX_QUERY_DEADLINE, step_budget());
    database.reads_stored_data.store(false, Ordering::SeqCst);
    let produced = materialise(&database.connection, objects)
        .and_then(|()| execute_once(&database.connection, sql, parameters, &source_types));
    guard.disarm();
    let engine_time = started.elapsed();
    let read_stored_data = database.reads_stored_data.load(Ordering::SeqCst);

    let outcome = match produced {
        Ok(Produced::Rows {
            result,
            data_scanned_bytes,
        }) => Outcome::Succeeded {
            result,
            // Rows were returned rather than written, so there is no row count to
            // report: `UpdateCount` is "the number of rows inserted".
            update_count: None,
            // `DataScannedInBytes` is "the number of bytes in the data that was
            // queried": for a statement over CATALOG tables that is the object data this
            // read scanned at their locations, and for one over nothing but this engine's
            // own tables it is what the engine read — so a statement that read no stored
            // data at all scanned nothing, however many values it computed from literals
            // and functions.
            data_scanned_bytes: if objects.is_empty() {
                if read_stored_data {
                    data_scanned_bytes
                } else {
                    0
                }
            } else {
                scanned_bytes
            },
            input_rows,
            // The engine answers with ROWS: an object of the object store is written by the
            // statement layer over this run, never by the engine itself.
            written_objects: None,
        },
        Ok(Produced::Affected(rows)) => Outcome::Succeeded {
            result: StoredResultSet::default(),
            // Reported only for a statement whose rows the engine counts: a definition
            // statement writes none to count, and the rows a `CREATE TABLE AS SELECT`
            // writes are not counted by this engine at all — so no count is reported for
            // it rather than one it did not produce.
            update_count: counts_rows.then_some(rows),
            data_scanned_bytes: 0,
            input_rows,
            written_objects: None,
        },
        Err(failure) => Outcome::Failed {
            message: failure.message(),
            reason: None,
            cause: failure.cause(),
        },
    };
    Completion {
        outcome,
        engine_time,
        // The engine was handed SQL that names no catalog relation, so this run resolved
        // nothing against the catalog; a statement whose relations WERE resolved is timed
        // by the resolution that did it (see `super::catalog_sql::execute`).
        planning_time: std::time::Duration::ZERO,
    }
}

/// How many rows one run READ from the stored objects it was handed: the rows materialised
/// in the engine database for it, which for a statement over catalog tables is the data the
/// run read.
///
/// A view contributes none of its own: its rows come from the tables it reads, which are
/// counted where they are materialised. A run handed no object read no stored rows at all.
fn materialised_rows(objects: &[LocalObject]) -> i64 {
    let rows: usize = objects
        .iter()
        .map(|object| match object {
            LocalObject::Table(table) => table.rows.len(),
            LocalObject::Filtered { table, .. } => table.rows.len(),
            LocalObject::View { .. } => 0,
        })
        .sum();
    i64::try_from(rows).unwrap_or(i64::MAX)
}

/// The type each column of the objects one run reads is DECLARED as, keyed by the column's
/// name folded to lower case — a column name resolves without regard to case.
///
/// A name two of the objects declare DIFFERENTLY carries no type here: an expression over it
/// names no one column, so nothing about its type follows from the declarations. A view
/// contributes none: its columns are the ones of the tables it reads.
fn source_column_types(objects: &[LocalObject]) -> SourceColumnTypes {
    let mut types: SourceColumnTypes = HashMap::new();
    let declared = |columns: &[LocalColumn]| -> Vec<(String, Option<ColumnType>)> {
        columns
            .iter()
            .map(|column| {
                (
                    column.name.to_ascii_lowercase(),
                    ColumnType::declared(column.declared),
                )
            })
            .collect()
    };
    let columns: Vec<(String, Option<ColumnType>)> = objects
        .iter()
        .flat_map(|object| match object {
            LocalObject::Table(table) => declared(&table.columns),
            LocalObject::Filtered { table, .. } => declared(&table.columns),
            LocalObject::View { .. } => Vec::new(),
        })
        .collect();
    for (name, column_type) in columns {
        types
            .entry(name)
            .and_modify(|held| {
                if *held != column_type {
                    *held = None;
                }
            })
            .or_insert(column_type);
    }
    types
}

/// What type each column of the stored data a run reads is declared as, by its folded name
/// ([`source_column_types`]).
type SourceColumnTypes = HashMap<String, Option<ColumnType>>;

/// Either the rows a query returned, or the row count a statement that returns no
/// columns affected.
enum Produced {
    /// A projection: its rows and the bytes the engine read for them.
    Rows {
        /// The result set itself.
        result: StoredResultSet,
        /// The engine's own size for the values it read.
        data_scanned_bytes: i64,
    },
    /// A statement with no result set, and the number of rows it changed.
    Affected(i64),
}

/// Why one run did not produce a complete result.
enum RunFailure {
    /// The engine refused, interrupted or could not run the statement.
    Engine(rusqlite::Error),
    /// The result set reached one of this emulator's retention bounds. The query FAILS
    /// rather than reporting a truncated result as a complete one: a caller must never
    /// be handed a short answer that looks whole.
    Bound(String),
}

impl From<rusqlite::Error> for RunFailure {
    fn from(error: rusqlite::Error) -> Self {
        Self::Engine(error)
    }
}

impl RunFailure {
    /// The message to report as the execution's `StateChangeReason`.
    fn message(&self) -> String {
        match self {
            Self::Engine(error) => scrub_paths(&error.to_string()),
            Self::Bound(message) => message.clone(),
        }
    }

    /// Whose doing this failure was; see [`FailureCause`].
    fn cause(&self) -> FailureCause {
        let emulator_code = |error: &rusqlite::ffi::Error| {
            matches!(
                error.code,
                // The EMULATOR stopped the run at its deadline or step budget.
                rusqlite::ErrorCode::OperationInterrupted
                    // The EMULATOR's own authorizer refused the statement: the SQL is
                    // valid, and what it asked for is what this confinement does not
                    // allow.
                    | rusqlite::ErrorCode::AuthorizationForStatementDenied
            )
        };
        match self {
            Self::Engine(rusqlite::Error::SqliteFailure(error, _)) if emulator_code(error) => {
                FailureCause::Emulator
            }
            Self::Engine(_) => FailureCause::Sql,
            Self::Bound(_) => FailureCause::Emulator,
        }
    }
}

/// Define each object a statement reads in this database, so the statement that follows
/// reads the rows the CATALOG holds for it.
///
/// A table is replaced on every run rather than kept: the objects at a table's location
/// are what a query reads, so each run reads them again and no earlier run's rows can
/// answer a later query. The names are the qualified ones the resolution chose, so
/// nothing an earlier statement defined in this database is overwritten.
fn materialise(
    connection: &rusqlite::Connection,
    objects: &[LocalObject],
) -> Result<(), RunFailure> {
    for object in objects {
        let name = match object {
            LocalObject::Table(table) | LocalObject::Filtered { table, .. } => &table.name,
            LocalObject::View { name, .. } => name,
        };
        let quoted = quoted_name(name);
        // The name may hold either kind of object from an earlier run, and dropping the
        // wrong kind is the engine's own error rather than this one's, so both are tried.
        let _ = connection.execute_batch(&format!("DROP VIEW IF EXISTS {quoted};"));
        let _ = connection.execute_batch(&format!("DROP TABLE IF EXISTS {quoted};"));
        match object {
            LocalObject::Table(table) => define_table(connection, &quoted, table)?,
            LocalObject::Filtered {
                table,
                source_columns,
                predicate,
            } => {
                // The rows go into a scratch table of their own, the engine decides which
                // of them the predicate keeps, and the scratch table is dropped again —
                // whether or not the predicate could be evaluated, so a statement never
                // runs in a database holding rows it may not read.
                let scratch = quoted_name(&scratch_name(&table.name));
                let _ = connection.execute_batch(&format!("DROP TABLE IF EXISTS {scratch};"));
                let source = LocalTable {
                    name: table.name.clone(),
                    columns: source_columns.clone(),
                    rows: table.rows.clone(),
                };
                let filtered = define_table(connection, &scratch, &source).and_then(|()| {
                    declare_table(connection, &quoted, &table.columns)?;
                    let projection: Vec<String> = table
                        .columns
                        .iter()
                        .map(|column| quoted_name(&column.name))
                        .collect();
                    // ONE statement, prepared and run on its own: the predicate is a
                    // boolean expression this engine evaluates, and preparing a single
                    // statement is what keeps text that is more than an expression from
                    // running as a second statement beside it.
                    connection.execute(
                        &format!(
                            "INSERT INTO {quoted} SELECT {} FROM {scratch} WHERE {predicate}",
                            projection.join(", ")
                        ),
                        [],
                    )?;
                    Ok(())
                });
                let _ = connection.execute_batch(&format!("DROP TABLE IF EXISTS {scratch};"));
                filtered?;
            }
            LocalObject::View { sql, .. } => {
                connection.execute_batch(&format!("CREATE VIEW {quoted} AS {sql};"))?;
            }
        }
    }
    Ok(())
}

/// Define `table` under the already-quoted name `quoted` and insert every row it holds.
///
/// The rows go in under ONE transaction of the engine's own: an in-memory database still
/// commits every statement that is not inside one, and a catalog table is materialised a row
/// at a time — so for a table of many rows the commits, and not the inserts, would be most
/// of the run. The transaction is rolled back if any row cannot be inserted, which is the
/// same outcome a failed materialisation had before: the run reports the engine's error and
/// the table it was building is not read.
fn define_table(
    connection: &rusqlite::Connection,
    quoted: &str,
    table: &LocalTable,
) -> Result<(), RunFailure> {
    declare_table(connection, quoted, &table.columns)?;
    let placeholders = vec!["?"; table.columns.len()].join(", ");
    let rows = connection.unchecked_transaction()?;
    {
        let mut insert =
            rows.prepare(&format!("INSERT INTO {quoted} VALUES ({placeholders})"))?;
        for row in &table.rows {
            insert.execute(rusqlite::params_from_iter(row.iter()))?;
        }
    }
    rows.commit()?;
    Ok(())
}

/// Declare a table of `columns` under the already-quoted name `quoted`.
fn declare_table(
    connection: &rusqlite::Connection,
    quoted: &str,
    columns: &[LocalColumn],
) -> Result<(), RunFailure> {
    let declarations: Vec<String> = columns
        .iter()
        .map(|column| format!("{} {}", quoted_name(&column.name), column.declared))
        .collect();
    connection.execute_batch(&format!(
        "CREATE TABLE {quoted} ({});",
        declarations.join(", ")
    ))?;
    Ok(())
}

/// The name the unnarrowed rows of `name` are loaded under while the engine applies a
/// predicate to them.
///
/// It carries a character the Data Catalog's own `NameString` pattern admits in no database
/// or table name, so it is a name no catalog relation resolves to — and it exists only
/// inside [`materialise`], which drops it again before the statement runs.
fn scratch_name(name: &str) -> String {
    format!("{name}\u{1}unfiltered")
}

/// Whether this database already holds a table or view called `name`.
///
/// The read seam for a relation the data catalog holds nothing for: a statement may
/// address a table an earlier statement defined in this database, and this is what says
/// whether it did.
pub fn holds_relation(handle: &DatabaseHandle, name: &str) -> bool {
    let database = handle.lock().unwrap_or_else(PoisonError::into_inner);
    database
        .connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table', 'view') \
             AND name = ?1 COLLATE NOCASE",
            [name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
}

/// One identifier as the engine reads it: quoted, so a name carrying a separator is one
/// name and not a qualified one.
fn quoted_name(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// The `ColumnInfo` of a column of CHARACTER data the catalog itself projects: the one
/// column a `SHOW` statement answers with.
///
/// A statement Athena performs on its HIVE DDL engine — the class whose result is that
/// engine's text output — reports its columns in HIVE's own type names rather than in the
/// query engine's, so a character column of such a result is Hive's `string`
/// ([`TYPE_STRING`]): a type that declares no width, and whose values Hive compares without
/// regard to case.
pub fn hive_string_column(name: &str) -> ColumnInfo {
    column_info(name, TYPE_STRING, false)
}

/// Prepare and run one statement, keeping no more of its result than the bounds above
/// allow.
fn execute_once(
    connection: &rusqlite::Connection,
    sql: &str,
    parameters: &[String],
    source_types: &SourceColumnTypes,
) -> Result<Produced, RunFailure> {
    let mut statement = connection.prepare(sql)?;

    // `ExecutionParameters` are "applied sequentially to the parameters in the query in
    // the order in which the parameters occur", and both Athena and this engine write a
    // query parameter as `?`, so they bind positionally and in order. A count that does
    // not match the placeholders in the query is the ENGINE's error, not a scan of the
    // SQL text here.
    let bound: Vec<rusqlite::types::Value> =
        parameters.iter().map(String::as_str).map(bound_parameter).collect();

    if statement.column_count() == 0 {
        let affected = statement.execute(rusqlite::params_from_iter(bound.iter()))?;
        return Ok(Produced::Affected(i64::try_from(affected).unwrap_or(i64::MAX)));
    }

    // The column names and declared types are read before the query starts, so the
    // immutable borrow of the prepared statement ends before `query` takes it mutably.
    let columns: Vec<(String, Option<ColumnType>)> = statement
        .columns()
        .map(|column| {
            (
                column.name().to_string(),
                column.decl_type().and_then(ColumnType::declared),
            )
        })
        .collect();
    // The types the statement itself fixes, which no value of the run can decide. They are
    // used only where the projection's columns can be matched up with the result's one for
    // one: a projection carrying a wildcard stands for as many columns as the relations it
    // reads hold.
    let fixed_types = projection::fixed_column_types(sql, source_types);
    let fixed_types = if fixed_types.len() == columns.len() {
        fixed_types
    } else {
        Vec::new()
    };
    // The type each column reports BEFORE a value is looked at: the STATEMENT's own
    // signature for the projected expression where it fixes one — which is also what the
    // values are RENDERED as, so a boolean column's values are `true` and `false` rather
    // than the 1 and 0 this engine stores them as — and the engine's own declaration
    // otherwise. A column neither fixes a type for is the one the values decide.
    let fixed: Vec<Option<ColumnType>> = columns
        .iter()
        .enumerate()
        .map(|(index, (_, declared))| {
            fixed_types
                .get(index)
                .copied()
                .flatten()
                .or(*declared)
        })
        .collect();
    // The names the statement's own projection gives its columns, which the text of a
    // computed expression cannot decide.
    let projected_names = projection::projected_names(sql);

    let mut rows = statement.query(rusqlite::params_from_iter(bound.iter()))?;
    let mut records: Vec<Row> = Vec::new();
    // `scanned` is the engine's own size for the values it read, which is what
    // `DataScannedInBytes` means; `retained` is what holding the result actually costs
    // this process, which is what the bound has to measure.
    let mut scanned: usize = 0;
    let mut retained: usize = 0;
    let mut cells: usize = 0;
    let mut observed: Vec<Option<ColumnType>> = fixed.clone();

    while let Some(row) = rows.next()? {
        if records.len() >= MAX_RESULT_ROWS {
            return Err(RunFailure::Bound(format!(
                "the result set exceeded the {MAX_RESULT_ROWS} rows this emulator retains \
                 for one query"
            )));
        }
        if cells.saturating_add(columns.len()) > MAX_RESULT_CELLS {
            return Err(RunFailure::Bound(format!(
                "the result set exceeded the {MAX_RESULT_CELLS} values this emulator \
                 retains for one query"
            )));
        }

        // The row is SIZED before any of it is materialised: `get_ref` borrows the
        // engine's own buffer and copies nothing, so a row that would take the result
        // set past its budget is measured and refused rather than allocated first.
        let mut row_scanned: usize = 0;
        let mut row_retained: usize = RETAINED_ROW_OVERHEAD;
        for index in 0..columns.len() {
            let value = row.get_ref(index)?;
            row_scanned = row_scanned.saturating_add(value_bytes(value));
            row_retained = row_retained.saturating_add(retained_bytes(value));
        }
        if retained.saturating_add(row_retained) > MAX_RESULT_BYTES {
            return Err(RunFailure::Bound(format!(
                "the result set exceeded the {MAX_RESULT_BYTES} bytes this emulator \
                 retains for one query"
            )));
        }
        scanned = scanned.saturating_add(row_scanned);
        retained = retained.saturating_add(row_retained);
        cells = cells.saturating_add(columns.len());

        let mut data = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            let value = row.get_ref(index)?;
            let fixed = fixed.get(index).copied().flatten();
            // A column whose type the statement or the engine already fixes reports THAT
            // type; one without is inferred from the values it produces, and the inferred
            // type has to hold every one of them, so every row is looked at rather than
            // only the first.
            if fixed.is_none() {
                if let Some(slot) = observed.get_mut(index) {
                    *slot = ColumnType::widened(*slot, value);
                }
            }
            data.push(Datum {
                var_char_value: rendered(value, fixed),
            });
        }
        records.push(Row { data: Some(data) });
    }

    let column_info = columns
        .iter()
        .zip(observed.iter())
        .enumerate()
        .map(|(index, ((name, _), column_type))| {
            // The STATEMENT's own name for the projected column wins over the name the
            // engine answers, which for a computed column is the SQL text that produced it —
            // and only where the projection could be matched up with the result, so one that
            // could not renames nothing.
            let projected = (projected_names.len() == columns.len())
                .then(|| projected_names.get(index).cloned().flatten())
                .flatten();
            column_metadata(projected.as_deref().unwrap_or(name), *column_type)
        })
        .collect();

    Ok(Produced::Rows {
        result: StoredResultSet {
            column_info,
            rows: records,
        },
        data_scanned_bytes: i64::try_from(scanned).unwrap_or(i64::MAX),
    })
}

/// One `ExecutionParameter` as the value it binds.
///
/// A parameter arrives as the SQL literal the caller wrote (`1`, `-2.5`, `'a name'`), so
/// it is READ by the SQL parser this crate already depends on — never by a reader of
/// literal syntax written here — and binds as the value that literal denotes. Anything
/// that is not a literal binds as the text the caller sent. Nothing is interpolated into
/// the SQL: the value is BOUND, so a parameter can never add syntax to the statement.
fn bound_parameter(parameter: &str) -> rusqlite::types::Value {
    use ::sqlparser::ast::{Expr, UnaryOperator, Value as SqlValue};
    use rusqlite::types::Value;

    let text = || Value::Text(parameter.to_string());
    let dialect = ::sqlparser::dialect::GenericDialect {};
    let Ok(expression) = ::sqlparser::parser::Parser::new(&dialect)
        .try_with_sql(parameter)
        .and_then(|mut parser| parser.parse_expr())
    else {
        return text();
    };
    // A negative number parses as a negation of a positive one, so the sign is read off
    // the expression rather than off the characters.
    let (negated, value) = match expression {
        Expr::Value(value) => (false, value.value),
        Expr::UnaryOp {
            op: UnaryOperator::Minus,
            expr,
        } => match *expr {
            Expr::Value(value) => (true, value.value),
            _ => return text(),
        },
        _ => return text(),
    };
    match value {
        SqlValue::Number(number, _) => {
            let signed = if negated {
                format!("-{number}")
            } else {
                number
            };
            signed.parse::<i64>().map(Value::Integer).unwrap_or_else(|_| {
                signed
                    .parse::<f64>()
                    .map_or_else(|_| text(), Value::Real)
            })
        }
        SqlValue::SingleQuotedString(string)
        | SqlValue::DoubleQuotedString(string)
        | SqlValue::EscapedStringLiteral(string) => Value::Text(string),
        SqlValue::Boolean(flag) => Value::Integer(i64::from(flag)),
        SqlValue::Null => Value::Null,
        _ => text(),
    }
}

/// The byte size one value contributes to `DataScannedInBytes`: the engine's own size
/// for that value.
fn value_bytes(value: rusqlite::types::ValueRef<'_>) -> usize {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => 0,
        ValueRef::Integer(_) => std::mem::size_of::<i64>(),
        ValueRef::Real(_) => std::mem::size_of::<f64>(),
        ValueRef::Text(bytes) | ValueRef::Blob(bytes) => bytes.len(),
    }
}

/// What retaining one value actually costs this process: the `Datum` it is stored in
/// plus the text it is rendered as, charged at its worst-case expansion.
fn retained_bytes(value: rusqlite::types::ValueRef<'_>) -> usize {
    use rusqlite::types::ValueRef;
    let payload = match value {
        // A number is rendered as decimal text, which is never longer than the digits of
        // a 64-bit value plus a sign and an exponent.
        ValueRef::Null => 0,
        ValueRef::Integer(_) | ValueRef::Real(_) => 32,
        ValueRef::Text(bytes) => bytes.len().saturating_mul(LOSSY_TEXT_EXPANSION),
        ValueRef::Blob(bytes) => bytes.len().saturating_mul(HEX_EXPANSION),
    };
    RETAINED_CELL_OVERHEAD.saturating_add(payload)
}

/// One value as the text `Datum$VarCharValue` carries, or `None` for SQL NULL — which
/// Athena reports as a datum with no value rather than as the empty string.
///
/// `reported` is the type the column itself reports where something other than this value
/// fixes it (the statement's own signature, or the engine's declaration): a value is
/// rendered as a value of its column's type, not of its storage class — which is what makes
/// a boolean column's values `true` and `false`. An EXACT-NUMERIC value already carries its
/// own digits as text by the time it is read ([`exact_decimal`]), so it is reported as it
/// stands.
fn rendered(value: rusqlite::types::ValueRef<'_>, reported: Option<ColumnType>) -> Option<String> {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => None,
        ValueRef::Integer(integer) => {
            if reported == Some(ColumnType::Boolean) {
                // A boolean column's values are Athena's own `true`/`false`, not the 1
                // and 0 the engine stores them as.
                Some((integer != 0).to_string())
            } else {
                Some(integer.to_string())
            }
        }
        // `{:?}` keeps the fractional part of a whole double (`1.0`, not `1`), which is
        // how an approximate-numeric value is told from an exact one.
        ValueRef::Real(double) => Some(format!("{double:?}")),
        ValueRef::Text(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
        ValueRef::Blob(bytes) => Some(hex::encode(bytes)),
    }
}

/// The Athena type one column of a result set reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ColumnType {
    /// A 32-bit integer.
    Integer,
    /// A 64-bit integer.
    BigInt,
    /// A double-precision number.
    Double,
    /// An EXACT fixed-point number of `precision` significant digits, `scale` of them
    /// after the point — the type whose values carry their own digits rather than a binary
    /// approximation of them (see [`exact_decimal`]).
    Decimal {
        /// How many significant digits a value of this column carries in all.
        precision: i32,
        /// How many of them are after the decimal point.
        scale: i32,
    },
    /// A boolean.
    Boolean,
    /// Character data.
    VarChar,
    /// Binary data.
    VarBinary,
}

impl ColumnType {
    /// The type a column DECLARED as `declared` reports, or `None` when the declaration
    /// names none this engine maps — in which case the values decide (see
    /// [`ColumnType::widened`]).
    fn declared(declared: &str) -> Option<Self> {
        let declared = declared.to_ascii_lowercase();
        // The declaration is matched the way SQLite's own type affinity reads it: by the
        // substring that names the type family.
        if declared.contains("bool") {
            return Some(Self::Boolean);
        }
        if declared.contains("bigint") {
            return Some(Self::BigInt);
        }
        if declared.contains("int") {
            return Some(Self::Integer);
        }
        if declared.contains("real") || declared.contains("floa") || declared.contains("doub") {
            return Some(Self::Double);
        }
        if declared.contains("char") || declared.contains("clob") || declared.contains("text") {
            return Some(Self::VarChar);
        }
        if declared.contains("blob") || declared.contains("binary") {
            return Some(Self::VarBinary);
        }
        None
    }

    /// The type that holds both what `current` holds and `value`.
    fn widened(current: Option<Self>, value: rusqlite::types::ValueRef<'_>) -> Option<Self> {
        use rusqlite::types::ValueRef;
        let observed = match value {
            // A NULL says nothing about the column's type.
            ValueRef::Null => return current,
            ValueRef::Integer(integer) => {
                if i32::try_from(integer).is_ok() {
                    Self::Integer
                } else {
                    Self::BigInt
                }
            }
            ValueRef::Real(_) => Self::Double,
            ValueRef::Text(_) => Self::VarChar,
            ValueRef::Blob(_) => Self::VarBinary,
        };
        Some(match current {
            None => observed,
            Some(current) if current == observed => current,
            // Two different types in one column: the wider of the two holds both, and
            // the widths run narrow to wide in `rank`.
            Some(current) => {
                if current.rank() >= observed.rank() {
                    current
                } else {
                    observed
                }
            }
        })
    }

    /// How wide this type is beside the others: a type of a higher rank can carry every
    /// value one of a lower rank can.
    fn rank(self) -> u8 {
        match self {
            Self::Boolean => 0,
            Self::Integer => 1,
            Self::BigInt => 2,
            // An exact fixed-point value holds every whole number of its own precision, and
            // a double holds a value of any scale at the precision it carries.
            Self::Decimal { .. } => 3,
            Self::Double => 4,
            Self::VarBinary => 5,
            Self::VarChar => 6,
        }
    }

    /// This type's Athena name, its precision and its scale.
    fn named(self) -> (&'static str, i32, i32) {
        match self {
            Self::Integer => TYPE_INTEGER,
            Self::BigInt => TYPE_BIGINT,
            Self::Double => TYPE_DOUBLE,
            Self::Decimal { precision, scale } => (TYPE_DECIMAL, precision, scale),
            Self::Boolean => TYPE_BOOLEAN,
            Self::VarChar => TYPE_VARCHAR,
            Self::VarBinary => TYPE_VARBINARY,
        }
    }
}

/// The `ColumnInfo` one column the QUERY ENGINE computed reports, typed in that engine's
/// own type names.
fn column_metadata(name: &str, column_type: Option<ColumnType>) -> ColumnInfo {
    column_info(
        name,
        column_type.map_or(TYPE_UNKNOWN, ColumnType::named),
        // Only character data compares case-sensitively.
        column_type == Some(ColumnType::VarChar),
    )
}

/// The `ColumnInfo` one column reports: its name, the `(type name, precision, scale)`
/// triple the engine that served it names its type by, and whether its values compare
/// case-sensitively.
fn column_info(name: &str, named: (&str, i32, i32), case_sensitive: bool) -> ColumnInfo {
    let (type_name, precision, scale) = named;
    ColumnInfo {
        // "The catalog to which the query results belong": every statement here is served
        // by the one engine this service runs, so the catalog a result column belongs to
        // is that engine's own ([`RESULT_CATALOG_NAME`]).
        catalog_name: Some(RESULT_CATALOG_NAME.to_string()),
        // "The schema name (database name) to which the query results belong" and "the
        // table name for the query results": a column of a PROJECTION belongs to no one
        // relation — the engine reports no originating table for it — and the empty string
        // is what Athena reports where there is none, which is a value and not an absent
        // member.
        schema_name: Some(NO_ORIGIN_RELATION.to_string()),
        table_name: Some(NO_ORIGIN_RELATION.to_string()),
        name: name.to_string(),
        // The label is what a client renders as the heading, which is this column's name.
        label: Some(name.to_string()),
        r#type: type_name.to_string(),
        precision,
        // "For DECIMAL data types" the scale is the digits a value carries after the point;
        // every other type here carries none, and reports zero.
        scale,
        nullable: Some(COLUMN_NULLABLE_UNKNOWN.to_string()),
        case_sensitive,
    }
}

/// Redact anything path-shaped from an engine message before it goes on the wire. The
/// message is the engine's own and a caller needs it to see why a query failed, but a
/// host path in it would tell the caller about the machine the emulator runs on.
///
/// Only a token that STARTS a path is redacted — one beginning at the start of the
/// message or after whitespace or a quote, with `/`, `\\` or a `C:\` drive prefix. A
/// slash inside a token, as in the identifier of a syntax error, is left alone, so the
/// diagnostic keeps its detail.
fn scrub_paths(message: &str) -> String {
    let mut scrubbed = String::with_capacity(message.len());
    let mut characters = message.char_indices().peekable();
    let mut at_token_start = true;
    while let Some((index, character)) = characters.next() {
        let rest = message.get(index..).unwrap_or_default();
        let path_start = at_token_start
            && (character == '/'
                || rest.starts_with("\\\\")
                || (character.is_ascii_alphabetic() && rest.get(1..3) == Some(":\\")));
        if !path_start {
            scrubbed.push(character);
            at_token_start = character.is_whitespace()
                || character == '\''
                || character == '"'
                || character == '(';
            continue;
        }
        scrubbed.push_str("<path>");
        while characters
            .peek()
            .is_some_and(|(_, next)| !next.is_whitespace() && *next != '\'' && *next != '"')
        {
            characters.next();
        }
        at_token_start = false;
    }
    scrubbed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The STEP budget is the backstop for a run the wall clock cannot stop, so the
    /// progress handler must answer "interrupt" on exactly the callback that spends the
    /// last of it — not before it, and not one callback late.
    ///
    /// Armed with a deadline far in the future, so the only bound under test is the budget.
    #[test]
    fn the_step_budget_interrupts_the_run_on_the_callback_that_spends_it() {
        let spend = u64::try_from(PROGRESS_HANDLER_OPS).expect("the callback's own spend");
        // Each row: the budget one run gets, and which callback must be the interrupting
        // one — a budget of exactly one callback's spend, of several, and one that is not a
        // whole multiple of it.
        for (budget, interrupting_call) in [
            (spend, 1usize),
            (spend * 2, 2),
            (spend * 3, 3),
            (spend * 5 / 2, 3),
        ] {
            let guard = RunGuard::default();
            guard.arm(Duration::from_secs(3_600), budget);
            for call in 1..=interrupting_call {
                let interrupted = guard.should_interrupt();
                assert_eq!(
                    interrupted,
                    call == interrupting_call,
                    "case budget {budget}: callback {call} of {interrupting_call} answered \
                     interrupt={interrupted}"
                );
            }
            // Past the budget the answer stays "interrupt": the counter saturates rather
            // than wrapping back into a fresh budget.
            assert!(
                guard.should_interrupt(),
                "case budget {budget}: a spent budget stays spent"
            );
        }
    }

    /// A disarmed guard interrupts nothing, so an idle connection is never stopped.
    #[test]
    fn a_disarmed_guard_interrupts_nothing() {
        let guard = RunGuard::default();
        guard.arm(Duration::from_secs(3_600), 0);
        guard.disarm();
        assert!(
            !guard.should_interrupt(),
            "case disarmed: a guard with no run in flight must not interrupt"
        );
    }
}
