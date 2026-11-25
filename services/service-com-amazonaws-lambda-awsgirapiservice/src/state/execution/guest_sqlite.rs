// SPDX-License-Identifier: CC0-1.0

//! The DATABASE half of this runtime's standard library: the `sqlite3` module a Python
//! handler imports, and the in-process SQLite engine behind it.
//!
//! A `python3.x` runtime is a promise about an interpreter AND about the standard library
//! that comes with it: a handler whose module body says `import sqlite3` imports cleanly on
//! the runtime it was written for, and the statements it then runs produce real answers out of
//! a real database. A module of the standard library this emulator registers no extension for
//! is SUPPLIED on the import path instead of being absent from it, exactly as the socket
//! interface, the TLS layer, the waiting interface and the signal interface are
//! ([`super::guest_http::runtime_stdlib_modules`], [`super::guest_tls`],
//! [`super::guest_select`], [`super::guest_signal`], which this module's source is resolved
//! beside).
//!
//! What is this module's own:
//!
//! * the `sqlite3` PACKAGE — `sqlite3` and the `sqlite3.dbapi2` it re-exports, laid out as the
//!   standard library lays them out — with the DB-API 2.0 surface a program reads off it: the
//!   `connect` call, the connection and cursor objects, the row object, the exception
//!   hierarchy, the module's own constants and the adapter and converter registries;
//! * the ENGINE those statements run on, which is the same in-process SQLite library every
//!   other database endpoint of this emulator serves its statements with (`rusqlite`,
//!   `bundled`): a statement is COMPILED and RUN by that engine, so what a `SELECT` answers is
//!   what the handler's own writes put in the database and nothing this module made up;
//! * the CONTAINMENT of that engine, which is this module's own: every database is opened in
//!   memory with its temp store in memory, so no statement reaches a host file; a second
//!   database can never be attached; the functions a statement may call are the one reviewed
//!   list every data plane of this emulator confines caller SQL to
//!   (`service_common::sqlite_confinement`), under a default-deny authorizer; and one
//!   statement is bounded in how long it may run, how many rows it may answer and how many
//!   bytes those rows may hold.
//!
//! A database a handler names by PATH is held in this execution environment's own memory
//! under that path, so two connections to it in one environment read and write one database
//! and nothing of one environment's databases is visible to another. The connections live as
//! long as the environment does, which is what a handler holding one in a module global
//! relies on, and a `close()` gives one up. The path itself is confined to the function's own
//! WRITABLE storage — the ephemeral storage the runtime's filesystem serves at `/tmp`, which
//! is the one part of that filesystem a database can be written to — so a name resolving
//! anywhere else is refused the way the engine refuses a database it cannot open.

use ::std::collections::BTreeMap;
use ::std::sync::{Mutex, MutexGuard, PoisonError};
use ::std::time::{Duration, Instant};

use ::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use ::rusqlite::limits::Limit;
use ::rusqlite::types::{Value as SqlValue, ValueRef};
use ::rusqlite::{Connection, OpenFlags, Statement};
use ::serde_json::{json, Value};

use super::package::PackageModule;

/// The modules of the standard library this half of the runtime supplies: the `sqlite3`
/// package.
///
/// Resolved with the socket interface, the TLS layer, the waiting interface and the signal
/// interface it is used beside, before the frozen standard library and after the deployment
/// package's own entries — so this IS the `sqlite3` a handler imports, and a package shipping
/// its own still wins.
#[must_use]
pub fn runtime_stdlib_modules() -> Vec<PackageModule> {
    [
        ("sqlite3/__init__.py", PACKAGE_SOURCE.to_string()),
        (
            "sqlite3/dbapi2.py",
            DBAPI2_SOURCE.replace(ENGINE_VERSION_MARK, ::rusqlite::version()),
        ),
    ]
    .into_iter()
    .map(|(path, source)| PackageModule {
        path: path.to_string(),
        source,
    })
    .collect()
}

/// Where the ENGINE's own version is written into the module source, so the version a program
/// reads off `sqlite3.sqlite_version` is the version of the library its statements really run
/// on rather than a number written out by hand.
const ENGINE_VERSION_MARK: &str = "@@SQLITE_VERSION@@";

/// How many databases ONE execution environment may hold open at a time.
///
/// CHOSEN, like every other bound this crate holds a guest-to-host seam to: a handler decides
/// how many connections it opens, and each one is a database held in this process's memory, so
/// the count is bounded here where no guest code can reach it. It is far above the one or two
/// connections a handler keeps.
const MAX_OPEN_DATABASES: usize = 32;

/// How many NAMED databases one execution environment may hold.
///
/// A named database outlives the connections a handler opens on it, so what bounds the memory
/// this environment holds for them is their number: CHOSEN, far above the one or two a handler
/// keeps and low enough that a program cannot fill this process by naming databases.
const MAX_NAMED_DATABASES: usize = 16;

/// The largest database IMAGE this seam answers, in bytes.
///
/// The image is what the file a handler named a database by holds, and it is built in this
/// process's memory before the guest writes it: CHOSEN, well above any database a handler keeps
/// in a function's ephemeral storage for one invocation's work, and bounded so that mirroring a
/// database to its file cannot double an unbounded one.
const MAX_IMAGE_BYTES: usize = 8 * 1024 * 1024;

/// The longest ONE statement may run, whatever the invocation has left of its own deadline.
///
/// CHOSEN: a bound on the work one statement can ask of this process, checked by the engine's
/// own progress handler. A statement that reaches it is interrupted, which is what the engine
/// answers a statement it stopped. The invocation's remaining deadline bounds it further, so
/// no statement outlives the run that asked for it.
const STATEMENT_DEADLINE: Duration = Duration::from_secs(5);

/// How many engine steps pass between two checks of that deadline.
const PROGRESS_STEPS: ::std::os::raw::c_int = 1_000;

/// The most rows one statement may answer with.
///
/// CHOSEN, for the reason the deadline is: the rows of one answer are held in this process's
/// memory before the guest reads them.
const MAX_RESULT_ROWS: usize = 100_000;

/// The most bytes the values of one answer may hold together, charged as each value is read
/// out of the engine — so a statement that would build a larger answer is refused instead of
/// allocating it.
const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// The most bytes one statement may carry.
const MAX_SQL_BYTES: i32 = 256 * 1024;

/// The most bytes one value may hold.
const MAX_VALUE_BYTES: i32 = 1024 * 1024;

/// The most columns one answer may carry.
const MAX_RESULT_COLUMNS: i32 = 256;

/// The most pages one database may hold: what bounds the database a handler can grow in this
/// process.
const MAX_DATABASE_PAGES: i32 = 32 * 1024;

/// The database name a program gives for the private in-memory database, which is the name the
/// standard library documents for one.
const MEMORY_DATABASE: &str = ":memory:";

/// The DATABASES one execution environment holds open, and the engine behind them.
///
/// Held by the environment's session ([`super::python`]), so a connection a handler opened in
/// its module body is still open for the next invocation that environment serves — which is
/// what a handler keeping a connection in a module global relies on — and every database goes
/// when the environment does.
#[derive(Debug, Default)]
pub struct Databases {
    held: Mutex<Open>,
}

/// The open connections, the handles they are addressed by, the databases this environment
/// holds by NAME, and the name those are keyed under.
#[derive(Debug, Default)]
struct Open {
    /// Every connection this environment has open, keyed by the handle the guest addresses it
    /// by.
    connections: BTreeMap<u64, Connection>,
    /// The connection the environment itself holds on each NAMED database, which is what makes
    /// such a database outlive the connections the handler opened on it, the way the file it
    /// would be written to does. Nothing runs on these.
    named: BTreeMap<String, Connection>,
    /// How many connections this environment has opened in all, which is where the next
    /// handle comes from: a handle is never reused, so a call on a connection the handler
    /// closed is refused rather than reaching another database.
    opened: u64,
    /// What this environment's named databases are keyed under, drawn once: a database one
    /// environment holds under a path is not the database another environment holds under the
    /// same path.
    scope: String,
}

impl Open {
    /// Where this environment holds the database `named`: its own memory, under a key of this
    /// environment's own.
    ///
    /// The name travels as its own BYTES, so no character of a path can be read as part of the
    /// engine's URI grammar, and the key carries this environment's own prefix, so a database
    /// one environment holds under a path is not the one another holds under the same path.
    fn uri_of(&mut self, named: &str) -> String {
        if self.scope.is_empty() {
            self.scope = ::uuid::Uuid::new_v4().simple().to_string();
        }
        format!(
            "file:{}-{}?mode=memory&cache=shared",
            self.scope,
            ::hex::encode(named.as_bytes())
        )
    }
}

/// One more connection to the database `uri` names, in this environment's own memory.
fn open_named(uri: &str) -> Result<Connection, Failure> {
    Connection::open_with_flags(
        uri,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_URI
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(engine_failure)
}

/// How many pages one step of a page-level copy moves.
///
/// The copy is driven a step at a time rather than in one call, so the work of making one is
/// bounded between steps the way every other unit of engine work here is.
const COPY_PAGES_PER_STEP: ::std::os::raw::c_int = 256;

/// Copy `source` over `destination` with the ENGINE's own page-level copy.
///
/// `sqlite3_backup_*` between two OPEN connections, so no path is named and no file is opened;
/// it also runs beneath SQL, so it copies every table, index, view and trigger of the source
/// exactly, with no dialect or quoting of its own to get wrong.
fn copy_pages(source: &Connection, destination: &mut Connection) -> Result<(), Failure> {
    let deadline = Instant::now() + STATEMENT_DEADLINE;
    let backup = ::rusqlite::backup::Backup::new(source, destination).map_err(engine_failure)?;
    loop {
        if Instant::now() >= deadline {
            return Err(Failure::operational(
                "the database could not be copied inside the time one statement is given"
                    .to_string(),
            ));
        }
        if backup.step(COPY_PAGES_PER_STEP).map_err(engine_failure)?
            == ::rusqlite::backup::StepResult::Done
        {
            return Ok(());
        }
    }
}

/// Why one call on this seam could not be served: the DB-API exception class the standard
/// library raises for it, and the sentence it carries.
struct Failure {
    /// The class the guest raises, by the name the module declares it under.
    class: &'static str,
    /// What the guest's exception carries, which for a failure the engine reported is the
    /// engine's own sentence.
    message: String,
}

impl Failure {
    /// A call the handler made wrongly: a closed connection, a parameter it did not supply.
    fn programming(message: String) -> Self {
        Self {
            class: "ProgrammingError",
            message,
        }
    }

    /// A call this seam could not serve for a bound of its own.
    fn operational(message: String) -> Self {
        Self {
            class: "OperationalError",
            message,
        }
    }

    /// The document the guest reads it back as.
    fn document(&self) -> Value {
        json!({
            "error": {
                "class": self.class,
                "message": self.message,
            }
        })
    }
}

impl Databases {
    /// Answer ONE call the guest made on this seam: `verb` with `document`, within `within` —
    /// what the run has left of its deadline, which bounds every statement this call runs.
    pub fn answered(&self, verb: &str, document: &str, within: Duration) -> String {
        let answered = match ::serde_json::from_str::<Value>(document) {
            Ok(asked) => match verb {
                "open" => self.opened(&asked),
                "close" => self.closed(&asked),
                "image" => self.image_of(&asked),
                "execute" => self.executed(&asked, within),
                "script" => self.scripted(&asked, within),
                other => Err(Failure::programming(format!(
                    "{other} is not a call a handler makes on a database"
                ))),
            },
            Err(error) => Err(Failure::programming(format!(
                "the database call carried no document: {error}"
            ))),
        };
        match answered {
            Ok(answer) => answer.to_string(),
            Err(failed) => failed.document().to_string(),
        }
    }

    /// How many databases are open right now, for the tests of this seam.
    #[must_use]
    pub fn open(&self) -> usize {
        self.locked().connections.len()
    }

    /// Open ONE database: the private in-memory one, or this environment's own database of the
    /// name the handler gave.
    fn opened(&self, asked: &Value) -> Result<Value, Failure> {
        let database = asked
            .get("database")
            .and_then(Value::as_str)
            .unwrap_or(MEMORY_DATABASE)
            .to_string();
        let mut held = self.locked();
        if held.connections.len() >= MAX_OPEN_DATABASES {
            return Err(Failure::operational(format!(
                "one execution environment may hold {MAX_OPEN_DATABASES} databases open at a \
                 time"
            )));
        }
        let mut connection = if database.is_empty() || database == MEMORY_DATABASE {
            // "A private temporary in-memory database" — one per connection, as the standard
            // library documents the name.
            Connection::open_in_memory().map_err(engine_failure)?
        } else {
            // A database the handler NAMED: this environment's own, held in its memory under
            // that name, so two connections to it in one environment are two connections to
            // one database — and so that it OUTLIVES them, exactly as the file a managed
            // runtime would write it to outlives the connections that wrote it.
            if held.named.len() >= MAX_NAMED_DATABASES && !held.named.contains_key(&database) {
                return Err(Failure::operational(format!(
                    "one execution environment may hold {MAX_NAMED_DATABASES} named databases"
                )));
            }
            let uri = held.uri_of(&database);
            // A FILE the handler removed is a database that is gone: the next connection to
            // that name opens a fresh one, exactly as it does on a filesystem.
            if !asked
                .get("exists")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                held.named.remove(&database);
            }
            if !held.named.contains_key(&database) {
                // The connection that HOLDS the database: a database of this name has a
                // connection of the environment's own open on it from the first `connect` on,
                // so what was written to it is still there after every connection the handler
                // itself opened has been closed. Nothing runs on it.
                let keeper = open_named(&uri)?;
                held.named.insert(database.clone(), keeper);
            }
            open_named(&uri)?
        };
        confine(&mut connection)?;
        held.opened = held.opened.saturating_add(1);
        let handle = held.opened;
        held.connections.insert(handle, connection);
        Ok(json!({ "handle": handle, "database": database }))
    }

    /// Give up one connection, which is what closes the database it was the only connection
    /// to.
    fn closed(&self, asked: &Value) -> Result<Value, Failure> {
        let handle = handle_of(asked)?;
        let mut held = self.locked();
        held.connections.remove(&handle);
        Ok(json!({ "closed": true }))
    }

    /// The BYTE IMAGE of one connection's database, which is what the file a handler named it
    /// by holds.
    ///
    /// It is the engine's own serialization of that database, so the file carries the database
    /// itself rather than anything written out here. An image past
    /// [`MAX_IMAGE_BYTES`] is not answered: the file a handler reads the SIZE of is then left
    /// as it was rather than this process holding a second copy of a large database.
    fn image_of(&self, asked: &Value) -> Result<Value, Failure> {
        let handle = handle_of(asked)?;
        let held = self.locked();
        let connection = held.connections.get(&handle).ok_or_else(closed_connection)?;
        // The image is taken off a COPY of the database, made with the engine's own page-level
        // copy between two open connections (`sqlite3_backup_*`, which names no path and opens
        // no file): the database a handler named by a path is one this environment holds for
        // every connection on it, and the engine serializes a database of its own memory
        // rather than a shared one.
        let mut copied = Connection::open_in_memory().map_err(engine_failure)?;
        copy_pages(connection, &mut copied)?;
        // The engine's own serialization of the MAIN database, which is the schema every
        // statement of a connection of this runtime runs against.
        let image = copied.serialize("main").map_err(engine_failure)?;
        let bytes: &[u8] = &image;
        if bytes.len() > MAX_IMAGE_BYTES {
            return Ok(json!({ "image": Value::Null }));
        }
        Ok(json!({
            "image": ::base64::Engine::encode(
                &::base64::engine::general_purpose::STANDARD,
                bytes,
            ),
        }))
    }

    /// Run ONE statement on one connection and answer what the engine produced.
    fn executed(&self, asked: &Value, within: Duration) -> Result<Value, Failure> {
        let handle = handle_of(asked)?;
        let sql = text_of(asked, "sql")?;
        let mut held = self.locked();
        let connection = held
            .connections
            .get_mut(&handle)
            .ok_or_else(closed_connection)?;
        let deadline = Instant::now() + within.min(STATEMENT_DEADLINE);
        bound_to(connection, deadline)?;
        let answered = statement_answer(connection, &sql, asked.get("params"));
        unbind(connection);
        answered
    }

    /// Run a SCRIPT of statements on one connection, which is what `executescript` performs.
    fn scripted(&self, asked: &Value, within: Duration) -> Result<Value, Failure> {
        let handle = handle_of(asked)?;
        let sql = text_of(asked, "sql")?;
        let mut held = self.locked();
        let connection = held
            .connections
            .get_mut(&handle)
            .ok_or_else(closed_connection)?;
        let deadline = Instant::now() + within.min(STATEMENT_DEADLINE);
        bound_to(connection, deadline)?;
        let answered = connection
            .execute_batch(&sql)
            .map_err(engine_failure)
            .map(|()| connection_state(connection));
        unbind(connection);
        answered
    }

    /// The open connections, locked. A poisoned lock is read through: the map holds this
    /// environment's own databases, and a call that panicked left no half-written entry in it.
    fn locked(&self) -> MutexGuard<'_, Open> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Install the bound one statement runs under: the engine stops it once `deadline` has passed.
fn bound_to(connection: &mut Connection, deadline: Instant) -> Result<(), Failure> {
    connection
        .progress_handler(PROGRESS_STEPS, Some(move || Instant::now() >= deadline))
        .map_err(engine_failure)
}

/// Take that bound off again, so a connection standing idle between statements holds no
/// deadline of a statement already answered.
///
/// The statement it bounded has already answered by now, so a connection that could not be
/// told is one the next statement installs its own bound on anyway.
fn unbind(connection: &mut Connection) {
    let _ = connection.progress_handler(PROGRESS_STEPS, None::<fn() -> bool>);
}

/// Said of a call on a connection this environment no longer holds.
fn closed_connection() -> Failure {
    Failure::programming("Cannot operate on a closed database.".to_string())
}

/// The handle one call addresses its connection by.
fn handle_of(asked: &Value) -> Result<u64, Failure> {
    asked
        .get("handle")
        .and_then(Value::as_u64)
        .ok_or_else(closed_connection)
}

/// The text member `name` of one call.
fn text_of(asked: &Value, name: &str) -> Result<String, Failure> {
    let text = asked
        .get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| Failure::programming(format!("the database call named no {name}")))?;
    if text.len() > MAX_SQL_BYTES as usize {
        return Err(Failure::operational(format!(
            "one statement may carry at most {MAX_SQL_BYTES} bytes"
        )));
    }
    Ok(text.to_string())
}

/// Open one database's own containment: in memory, bounded, and confined to the database
/// itself.
///
/// The settings are the ones every other in-process database of this emulator is opened under:
/// the temp store in memory FIRST, so nothing the engine spills reaches a host file, then the
/// ceilings on the database, the statement and the values, then the authorizer — installed
/// LAST, because the settings above are themselves pragmas.
fn confine(connection: &mut Connection) -> Result<(), Failure> {
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(engine_failure)?;
    for schema in ["main", "temp"] {
        connection
            .pragma_update(Some(schema), "max_page_count", MAX_DATABASE_PAGES)
            .map_err(engine_failure)?;
    }
    for (limit, value) in [
        // No second database, ever: this also takes `VACUUM INTO` away, which the engine
        // implements by attaching its output file.
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES),
        (Limit::SQLITE_LIMIT_COLUMN, MAX_RESULT_COLUMNS),
    ] {
        connection.set_limit(limit, value).map_err(engine_failure)?;
    }
    for (config, enabled) in [
        (
            ::rusqlite::config::DbConfig::SQLITE_DBCONFIG_TRUSTED_SCHEMA,
            false,
        ),
        (
            ::rusqlite::config::DbConfig::SQLITE_DBCONFIG_DEFENSIVE,
            true,
        ),
    ] {
        connection
            .set_db_config(config, enabled)
            .map_err(engine_failure)?;
    }
    connection
        .authorizer(Some(confine_to_the_database))
        .map_err(engine_failure)?;
    Ok(())
}

/// The pragmas a handler's statement may reach: the engine's own schema introspection, which
/// answers the columns a table declares and nothing about this process, its files or its
/// settings.
const SCHEMA_PRAGMAS: &[&str] = &["table_info", "table_xinfo", "table_list", "index_list"];

/// What a handler's SQL may do inside ONE database.
///
/// DEFAULT DENY: an action not named here is refused, so an engine capability this emulator
/// has not considered — including one a later SQLite build adds — cannot be reached from a
/// handler. What is allowed is what a function's own database exists for: reading and writing
/// its tables, the DDL that defines them, transactions, and pure computation over the values
/// in them.
///
/// `ATTACH`/`DETACH` would make a host path visible to SQL (and `VACUUM INTO` is implemented
/// by attaching its output file), a configuration `PRAGMA` reaches engine configuration
/// including temp-file paths, and a virtual table is arbitrary registered code. The pragmas
/// that are allowed are the schema introspection a program reads ([`SCHEMA_PRAGMAS`]), which
/// reaches nothing but this database's own schema.
fn confine_to_the_database(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Select
        | AuthAction::Read { .. }
        | AuthAction::Insert { .. }
        | AuthAction::Update { .. }
        | AuthAction::Delete { .. }
        | AuthAction::Transaction { .. }
        | AuthAction::Savepoint { .. }
        | AuthAction::Recursive
        | AuthAction::CreateTable { .. }
        | AuthAction::CreateTempTable { .. }
        | AuthAction::CreateIndex { .. }
        | AuthAction::CreateTempIndex { .. }
        | AuthAction::CreateView { .. }
        | AuthAction::CreateTempView { .. }
        | AuthAction::CreateTrigger { .. }
        | AuthAction::CreateTempTrigger { .. }
        | AuthAction::DropTable { .. }
        | AuthAction::DropTempTable { .. }
        | AuthAction::DropIndex { .. }
        | AuthAction::DropTempIndex { .. }
        | AuthAction::DropView { .. }
        | AuthAction::DropTempView { .. }
        | AuthAction::DropTrigger { .. }
        | AuthAction::DropTempTrigger { .. }
        | AuthAction::AlterTable { .. }
        | AuthAction::Reindex { .. }
        | AuthAction::Analyze { .. } => Authorization::Allow,
        AuthAction::Pragma { pragma_name, .. }
            if SCHEMA_PRAGMAS
                .iter()
                .any(|schema| pragma_name.eq_ignore_ascii_case(schema)) =>
        {
            Authorization::Allow
        }
        // Pure computation over the values in those tables, as an ALLOW-LIST: the one reviewed
        // list every data plane of this emulator confines caller SQL to, so a function this
        // emulator has not reviewed — including one a future engine build defines — falls
        // through to the deny arm below.
        AuthAction::Function { function_name } => {
            if ::service_common::sqlite_confinement::sql_function_is_permitted(function_name) {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        // Everything that could reach outside this database, and everything this emulator has
        // not considered.
        _ => Authorization::Deny,
    }
}

/// Compile and run ONE statement, and answer the columns it declared, the rows it produced and
/// the state the connection is in afterwards.
fn statement_answer(
    connection: &Connection,
    sql: &str,
    params: Option<&Value>,
) -> Result<Value, Failure> {
    let mut statement = connection.prepare(sql).map_err(engine_failure)?;
    bind_parameters(&mut statement, params)?;
    let described: Vec<(String, Option<String>)> = statement
        .columns()
        .map(|column| {
            (
                column.name().to_string(),
                column.decl_type().map(str::to_string),
            )
        })
        .collect();
    let columns: Vec<String> = described.iter().map(|(name, _)| name.clone()).collect();
    let declared: Vec<Value> = described
        .iter()
        .map(|(_, declared)| match declared {
            Some(declared) => Value::String(declared.clone()),
            None => Value::Null,
        })
        .collect();
    let mut rows = Vec::new();
    let mut carried = 0usize;
    let mut answered = statement.raw_query();
    while let Some(row) = answered.next().map_err(engine_failure)? {
        if rows.len() >= MAX_RESULT_ROWS {
            return Err(Failure::operational(format!(
                "one statement may answer at most {MAX_RESULT_ROWS} rows"
            )));
        }
        let mut values = Vec::with_capacity(columns.len());
        for column in 0..columns.len() {
            let value = row.get_ref(column).map_err(engine_failure)?;
            carried = carried.saturating_add(value_cost(&value));
            if carried > MAX_RESULT_BYTES {
                return Err(Failure::operational(format!(
                    "one statement may answer at most {MAX_RESULT_BYTES} bytes"
                )));
            }
            values.push(encoded_value(&value));
        }
        rows.push(Value::Array(values));
    }
    drop(answered);
    drop(statement);
    let mut answer = connection_state(connection);
    if let Value::Object(members) = &mut answer {
        members.insert("columns".to_string(), json!(columns));
        members.insert("declared".to_string(), Value::Array(declared));
        members.insert("rows".to_string(), Value::Array(rows));
    }
    Ok(answer)
}

/// What the connection reports after a statement: how many rows the statement changed, the row
/// id the last insert produced, how many rows this connection has changed in all, and whether
/// a transaction is open.
fn connection_state(connection: &Connection) -> Value {
    json!({
        "changes": connection.changes(),
        "lastInsertRowid": connection.last_insert_rowid(),
        "totalChanges": connection.total_changes(),
        "inTransaction": !connection.is_autocommit(),
    })
}

/// Bind the parameters one statement was given, the way the standard library binds them.
///
/// A SEQUENCE binds by position and has to carry exactly as many values as the statement has
/// parameters; a MAPPING binds by the name each parameter was written under. A statement with
/// parameters and a call that supplied none is the same mismatch as any other.
fn bind_parameters(statement: &mut Statement<'_>, params: Option<&Value>) -> Result<(), Failure> {
    let expected = statement.parameter_count();
    match params {
        Some(Value::Object(named)) => {
            for index in 1..=expected {
                let parameter = statement
                    .parameter_name(index)
                    .map(str::to_string)
                    .ok_or_else(|| {
                        Failure::programming(
                            "Binding 1 has no name, but you supplied a dictionary (which has \
                             only names)."
                                .to_string(),
                        )
                    })?;
                let key = parameter
                    .strip_prefix(':')
                    .or_else(|| parameter.strip_prefix('@'))
                    .or_else(|| parameter.strip_prefix('$'))
                    .unwrap_or(&parameter);
                let value = named.get(key).ok_or_else(|| {
                    Failure::programming(format!(
                        "You did not supply a value for binding parameter {parameter}."
                    ))
                })?;
                statement
                    .raw_bind_parameter(index, sql_value(value)?)
                    .map_err(engine_failure)?;
            }
            Ok(())
        }
        Some(Value::Array(values)) => {
            if values.len() != expected {
                return Err(Failure::programming(format!(
                    "Incorrect number of bindings supplied. The current statement uses \
                     {expected}, and there are {} supplied.",
                    values.len()
                )));
            }
            for (index, value) in values.iter().enumerate() {
                statement
                    .raw_bind_parameter(index + 1, sql_value(value)?)
                    .map_err(engine_failure)?;
            }
            Ok(())
        }
        _ if expected == 0 => Ok(()),
        _ => Err(Failure::programming(format!(
            "Incorrect number of bindings supplied. The current statement uses {expected}, and \
             there are 0 supplied."
        ))),
    }
}

/// One parameter value, in the engine's own storage classes.
///
/// The guest writes a value as the JSON of its storage class — null, an integer, a real, text,
/// or an object carrying the base64 of a blob's bytes — so a blob is never confused for text
/// and an integer never for a real.
fn sql_value(value: &Value) -> Result<SqlValue, Failure> {
    match value {
        Value::Null => Ok(SqlValue::Null),
        Value::Number(number) => {
            if let Some(whole) = number.as_i64() {
                return Ok(SqlValue::Integer(whole));
            }
            number.as_f64().map(SqlValue::Real).ok_or_else(|| {
                Failure::programming(
                    "Python int too large to convert to SQLite INTEGER".to_string(),
                )
            })
        }
        Value::String(text) => Ok(SqlValue::Text(text.clone())),
        Value::Object(members) => {
            let encoded = members
                .get(BLOB_MEMBER)
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    Failure::programming(
                        "Error binding parameter - probably unsupported type.".to_string(),
                    )
                })?;
            let bytes = ::base64::Engine::decode(
                &::base64::engine::general_purpose::STANDARD,
                encoded.as_bytes(),
            )
            .map_err(|_| {
                Failure::programming(
                    "Error binding parameter - probably unsupported type.".to_string(),
                )
            })?;
            Ok(SqlValue::Blob(bytes))
        }
        Value::Bool(_) | Value::Array(_) => Err(Failure::programming(
            "Error binding parameter - probably unsupported type.".to_string(),
        )),
    }
}

/// The member a BLOB's bytes travel in, base64 of those bytes — the one value whose storage
/// class JSON has no form of its own for.
const BLOB_MEMBER: &str = "b64";

/// How many bytes of this process's memory one value holds, for the bound on one answer.
fn value_cost(value: &ValueRef<'_>) -> usize {
    match value {
        ValueRef::Null => 1,
        ValueRef::Integer(_) | ValueRef::Real(_) => 8,
        ValueRef::Text(text) => text.len(),
        ValueRef::Blob(bytes) => bytes.len(),
    }
}

/// One answered value, as the guest reads it back: its storage class, written as the JSON
/// [`sql_value`] reads.
fn encoded_value(value: &ValueRef<'_>) -> Value {
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(whole) => json!(whole),
        ValueRef::Real(real) => json!(real),
        // Text the engine holds is the bytes it was given; a column holding bytes that are not
        // text is answered as the blob it is rather than as mangled text.
        ValueRef::Text(text) => match ::std::str::from_utf8(text) {
            Ok(text) => Value::String(text.to_string()),
            Err(_) => blob_value(text),
        },
        ValueRef::Blob(bytes) => blob_value(bytes),
    }
}

/// A blob's bytes, as the guest reads them back.
fn blob_value(bytes: &[u8]) -> Value {
    json!({
        BLOB_MEMBER: ::base64::Engine::encode(&::base64::engine::general_purpose::STANDARD, bytes),
    })
}

/// The DB-API exception class and the message a failed statement is reported under: the
/// ENGINE's own sentence, under the class the standard library maps its result code to.
///
/// The mapping is the standard library's own — a constraint violation and a type mismatch are
/// an integrity failure, the engine's own malfunctions are internal, a value too big for the
/// engine is a data failure, a misuse of the interface is an interface failure, and the
/// operational failures (a statement the engine could not compile, a busy or interrupted one)
/// are operational — so a handler catching `sqlite3.IntegrityError` around an insert catches
/// what it catches on the runtime it was written for.
fn engine_failure(error: ::rusqlite::Error) -> Failure {
    let (code, message) = match &error {
        ::rusqlite::Error::SqliteFailure(code, Some(message)) => (Some(*code), message.clone()),
        ::rusqlite::Error::SqliteFailure(code, None) => (Some(*code), error.to_string()),
        // Text the engine could not COMPILE: its own sentence is the `msg`, which names what it
        // stopped at, rather than the `Display` form that repeats the statement back.
        ::rusqlite::Error::SqlInputError {
            error: code, msg, ..
        } => (Some(*code), msg.clone()),
        other => (None, other.to_string()),
    };
    let Some(code) = code else {
        return Failure {
            class: "DatabaseError",
            message,
        };
    };
    let class = match code.code {
        ::rusqlite::ErrorCode::InternalMalfunction | ::rusqlite::ErrorCode::NotFound => {
            "InternalError"
        }
        ::rusqlite::ErrorCode::ConstraintViolation | ::rusqlite::ErrorCode::TypeMismatch => {
            "IntegrityError"
        }
        ::rusqlite::ErrorCode::TooBig => "DataError",
        ::rusqlite::ErrorCode::ApiMisuse => "InterfaceError",
        ::rusqlite::ErrorCode::Unknown
        | ::rusqlite::ErrorCode::PermissionDenied
        | ::rusqlite::ErrorCode::OperationAborted
        | ::rusqlite::ErrorCode::DatabaseBusy
        | ::rusqlite::ErrorCode::DatabaseLocked
        | ::rusqlite::ErrorCode::OutOfMemory
        | ::rusqlite::ErrorCode::ReadOnly
        | ::rusqlite::ErrorCode::OperationInterrupted
        | ::rusqlite::ErrorCode::SystemIoFailure
        | ::rusqlite::ErrorCode::DiskFull
        | ::rusqlite::ErrorCode::CannotOpen
        | ::rusqlite::ErrorCode::FileLockingProtocolFailed
        | ::rusqlite::ErrorCode::SchemaChanged => "OperationalError",
        _ => "DatabaseError",
    };
    Failure { class, message }
}

/// `sqlite3`: the package a program imports, which is the standard library's own layout — the
/// package re-exports the DB-API module beside it.
const PACKAGE_SOURCE: &str = r#""""The DB-API 2.0 interface to this runtime's SQLite engine."""

from sqlite3.dbapi2 import *
from sqlite3.dbapi2 import (
    Connection,
    Cursor,
    DatabaseError,
    DataError,
    Error,
    IntegrityError,
    InterfaceError,
    InternalError,
    NotSupportedError,
    OperationalError,
    ProgrammingError,
    Row,
    Warning,
    apilevel,
    connect,
    paramstyle,
    register_adapter,
    register_converter,
    sqlite_version,
    sqlite_version_info,
    threadsafety,
    version,
    version_info,
)
"#;

/// `sqlite3.dbapi2`: the DB-API 2.0 surface itself, over the engine this runtime embeds.
///
/// Every statement here is COMPILED AND RUN by that engine through the host seam
/// ([`Databases`]): nothing in this module answers a query out of anything but the database the
/// handler's own statements wrote.
const DBAPI2_SOURCE: &str = r#""""The DB-API 2.0 interface to this runtime's SQLite engine."""

import base64 as _base64
import datetime as _datetime
import json as _json
import os as _os

_host = __import__("_lambda_host")

# The database name a program gives for the private in-memory database, which is the name the
# standard library documents for one.
MEMORY_DATABASE = ":memory:"

# The DB-API 2.0 module-level constants, as the standard library's own module publishes them.
apilevel = "2.0"
threadsafety = 1
paramstyle = "qmark"

# The version of the DB-API wrapper, as the module reports it, and of the ENGINE behind it —
# which is the version of the library the statements really run on.
version = "2.6.0"
version_info = (2, 6, 0)
sqlite_version = "@@SQLITE_VERSION@@"
sqlite_version_info = tuple(
    int(part) for part in sqlite_version.split(".") if part.isdigit()
)

# How a connection asks for the declared types of its columns to be converted.
PARSE_DECLTYPES = 1
PARSE_COLNAMES = 2

# The three answers an authorizer can give, as the module publishes them.
SQLITE_OK = 0
SQLITE_DENY = 1
SQLITE_IGNORE = 2

# What `Connection.autocommit` is set to to keep the legacy transaction handling, which is the
# handling this module performs.
LEGACY_TRANSACTION_CONTROL = -1

# The type a blob is handed over as, as the module names it.
Binary = memoryview


class Warning(Exception):
    """Raised for important warnings, as DB-API 2.0 declares it."""


class Error(Exception):
    """The base class of every error this module raises."""


class InterfaceError(Error):
    """An error in the interface rather than in the database."""


class DatabaseError(Error):
    """An error reported by the database."""


class DataError(DatabaseError):
    """A problem with the data: a value out of range, a value too large."""


class OperationalError(DatabaseError):
    """A problem with the database's operation, outside the program's control."""


class IntegrityError(DatabaseError):
    """A constraint of the database was violated."""


class InternalError(DatabaseError):
    """The database reported an internal error."""


class ProgrammingError(DatabaseError):
    """The program used this interface wrongly."""


class NotSupportedError(DatabaseError):
    """A capability this runtime's engine does not serve was asked for."""


# The classes the engine's own result codes are reported under, by the name the host names.
_CLASSES = {
    "Warning": Warning,
    "Error": Error,
    "InterfaceError": InterfaceError,
    "DatabaseError": DatabaseError,
    "DataError": DataError,
    "OperationalError": OperationalError,
    "IntegrityError": IntegrityError,
    "InternalError": InternalError,
    "ProgrammingError": ProgrammingError,
    "NotSupportedError": NotSupportedError,
}

# The member a blob's bytes travel in on the host seam: the base64 of those bytes.
_BLOB = "b64"

# The adapters a program registered, by the type they adapt, and the converters it registered,
# by the declared type they convert. Both are the module's own registries, as the standard
# library keeps them.
_ADAPTERS = {}
_CONVERTERS = {}

# The first words of the statements that OPEN a transaction under the legacy handling this
# module performs: "Lambda" never begins one for a query or for data definition.
_TRANSACTIONAL = ("insert", "update", "delete", "replace")

# The first words of the statements whose last inserted row id a cursor reports.
_INSERTING = ("insert", "replace")


def _call(verb, document):
    """One call on the host seam, with the failure it reported raised as the error it is."""
    answered = _json.loads(_host.sqlite(verb, _json.dumps(document)))
    failed = answered.get("error")
    if failed:
        raise _CLASSES.get(failed.get("class"), DatabaseError)(failed.get("message", ""))
    return answered


def _adapted(value):
    """`value` as the storage class the engine holds it in, written for the host seam."""
    if value is None:
        return None
    if isinstance(value, bool):
        # The engine has no boolean class of its own, so a boolean is stored as the integer
        # the standard library stores it as.
        return int(value)
    if isinstance(value, int):
        if value.bit_length() > 63:
            raise OverflowError("Python int too large to convert to SQLite INTEGER")
        return value
    if isinstance(value, float):
        return value
    if isinstance(value, str):
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        return {_BLOB: _base64.b64encode(bytes(value)).decode("ascii")}
    adapter = _ADAPTERS.get(type(value))
    if adapter is None:
        for held, adapts in _ADAPTERS.items():
            if isinstance(value, held):
                adapter = adapts
                break
    if adapter is not None:
        return _adapted(adapter(value))
    conform = getattr(value, "__conform__", None)
    if conform is not None:
        adapted = conform(PrepareProtocol)
        if adapted is not None:
            return _adapted(adapted)
    raise InterfaceError(
        "Error binding parameter - probably unsupported type."
    )


def _restored(value):
    """One answered value, as the Python object of its storage class."""
    if isinstance(value, dict):
        return _base64.b64decode(value[_BLOB].encode("ascii"))
    return value


def _parameters(parameters):
    """The parameters one statement was given, written for the host seam.

    A MAPPING binds by name and a SEQUENCE by position, which is what the two parameter styles
    of this module are; a call that named neither supplied none.
    """
    if parameters is None:
        return None
    keys = getattr(parameters, "keys", None)
    if keys is not None:
        return {str(key): _adapted(parameters[key]) for key in keys()}
    return [_adapted(value) for value in parameters]


def _first_word(statement):
    """The first word of `statement`, with the comments and whitespace before it skipped."""
    text = statement
    while True:
        text = text.lstrip()
        if text.startswith("--"):
            broke = text.find("\n")
            if broke < 0:
                return ""
            text = text[broke + 1 :]
            continue
        if text.startswith("/*"):
            closed = text.find("*/")
            if closed < 0:
                return ""
            text = text[closed + 2 :]
            continue
        break
    word = ""
    for character in text:
        if character.isalpha():
            word += character
            continue
        break
    return word.lower()


class PrepareProtocol:
    """The protocol an object's own `__conform__` is asked to adapt itself for."""


def register_adapter(held, adapter):
    """Register `adapter` as how a value of the type `held` is stored."""
    _ADAPTERS[held] = adapter


def register_converter(declared, converter):
    """Register `converter` as how a column declared `declared` is read back."""
    _CONVERTERS[declared.upper()] = converter


def adapt(value, protocol=None, alternate=None):
    """`value` as the object this module would store, or `alternate` where it cannot."""
    try:
        return _restored(_adapted(value))
    except InterfaceError:
        if alternate is None:
            raise
        return alternate


class Row:
    """One answered row, readable by position and by column name."""

    def __init__(self, cursor, values):
        self._names = [column[0] for column in (cursor.description or ())]
        self._values = tuple(values)

    def keys(self):
        return list(self._names)

    def __getitem__(self, key):
        if isinstance(key, (int, slice)):
            return self._values[key]
        wanted = key.lower()
        for index, name in enumerate(self._names):
            if name.lower() == wanted:
                return self._values[index]
        raise IndexError("No item with that key")

    def __iter__(self):
        return iter(self._values)

    def __len__(self):
        return len(self._values)

    def __eq__(self, other):
        if isinstance(other, Row):
            return self._values == other._values
        if isinstance(other, tuple):
            return self._values == other
        return NotImplemented

    def __hash__(self):
        return hash(self._values)

    def __repr__(self):
        return repr(self._values)


class Cursor:
    """One cursor on a connection: the statement it ran and the rows it answered."""

    def __init__(self, connection):
        self.connection = connection
        self.arraysize = 1
        self.row_factory = None
        self._description = None
        self._rows = []
        self._at = 0
        self._rowcount = -1
        self._lastrowid = None
        self._closed = False

    @property
    def description(self):
        return self._description

    @property
    def rowcount(self):
        return self._rowcount

    @property
    def lastrowid(self):
        return self._lastrowid

    def _usable(self):
        if self._closed:
            raise ProgrammingError("Cannot operate on a closed cursor.")
        self.connection._usable()

    def execute(self, sql, parameters=()):
        """Run ONE statement and hold what it answered."""
        self._usable()
        answered = self.connection._statement(sql, _parameters(parameters))
        self._held(sql, answered)
        return self

    def executemany(self, sql, seq_of_parameters):
        """Run ONE statement once per set of parameters, and hold what they changed in all."""
        self._usable()
        changed = 0
        answered = None
        for parameters in seq_of_parameters:
            answered = self.connection._statement(sql, _parameters(parameters))
            if answered.get("columns"):
                raise ProgrammingError(
                    "executemany() can only execute DML statements."
                )
            changed += int(answered.get("changes") or 0)
        if answered is None:
            # Nothing was run, so there is nothing of a statement to report; the DB-API has
            # the cursor report no rows changed.
            self._description = None
            self._rows = []
            self._at = 0
            self._rowcount = -1
            return self
        self._held(sql, answered)
        self._rowcount = changed
        return self

    def executescript(self, sql):
        """Run a SCRIPT of statements, which commits whatever was open first."""
        self._usable()
        self.connection.commit()
        answered = self.connection._script(sql)
        self._held(sql, answered)
        return self

    def _held(self, sql, answered):
        """Hold what one statement answered, as this cursor reports it."""
        columns = answered.get("columns") or []
        rows = answered.get("rows") or []
        declared = answered.get("declared") or []
        if columns:
            converters = self.connection._converters(columns, declared)
            self._description = [
                (
                    self.connection._column_name(name),
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                for name in columns
            ]
            self._rows = [self._row(row, converters) for row in rows]
            # "For SELECT statements, [rowcount] is -1": the number of rows a query answered
            # is not a count of rows it changed.
            self._rowcount = -1
        else:
            self._description = None
            self._rows = []
            self._rowcount = int(answered.get("changes") or 0)
        self._at = 0
        if _first_word(sql) in _INSERTING:
            self._lastrowid = int(answered.get("lastInsertRowid") or 0)
        else:
            self._lastrowid = None

    def _row(self, values, converters):
        """One answered row, converted and then handed to whatever factory is in force."""
        restored = []
        for index, value in enumerate(values):
            held = _restored(value)
            convert = converters[index] if index < len(converters) else None
            if convert is not None and held is not None:
                if isinstance(held, str):
                    held = held.encode("utf-8")
                held = convert(held)
            restored.append(held)
        restored = tuple(restored)
        factory = self.row_factory or self.connection.row_factory
        if factory is None:
            return restored
        return factory(self, restored)

    def fetchone(self):
        self._usable()
        if self._at >= len(self._rows):
            return None
        row = self._rows[self._at]
        self._at += 1
        return row

    def fetchmany(self, size=None):
        self._usable()
        if size is None:
            size = self.arraysize
        if size <= 0:
            return []
        taken = self._rows[self._at : self._at + size]
        self._at += len(taken)
        return taken

    def fetchall(self):
        self._usable()
        taken = self._rows[self._at :]
        self._at = len(self._rows)
        return taken

    def setinputsizes(self, sizes):
        """Required by DB-API 2.0 and does nothing here, as it does in the standard library."""

    def setoutputsize(self, size, column=None):
        """Required by DB-API 2.0 and does nothing here, as it does in the standard library."""

    def close(self):
        self._closed = True
        self._rows = []
        self._at = 0

    def __iter__(self):
        return self

    def __next__(self):
        row = self.fetchone()
        if row is None:
            raise StopIteration
        return row


# The WRITABLE storage of a function: the one directory of its filesystem a database can be
# written to, which is the ephemeral storage the function's `EphemeralStorage.Size` sizes.
# Everything else of that filesystem is read-only, as it is on the runtime a handler was
# written for.
_WRITABLE_ROOT = "/tmp"


def _writable_path(database):
    """`database` as the file of the function's writable storage it names.

    ``None`` for a name that resolves anywhere else: a relative name (which resolves against
    the read-only task root), an absolute one outside the writable root, or one that climbs
    out of it. The name is NORMALISED first, so `..` and `.` cannot carry it out of the root
    after the check.
    """
    named = str(database)
    if not named.startswith("/"):
        return None
    resolved = _os.path.normpath(named)
    if resolved != _WRITABLE_ROOT and not resolved.startswith(_WRITABLE_ROOT + "/"):
        return None
    if resolved == _WRITABLE_ROOT:
        return None
    return resolved


class Connection:
    """One connection to one database of this execution environment."""

    def __init__(self, database, detect_types=0, isolation_level="", **options):
        self.row_factory = None
        self.text_factory = str
        self._detect_types = int(detect_types or 0)
        self._isolation_level = isolation_level
        self._in_transaction = False
        self._total_changes = 0
        self._closed = False
        # The FILE a database named by a path is held in, which is a file of the function's
        # own WRITABLE storage and nowhere else: a database named `:memory:` has none, and a
        # name that resolves outside that storage is refused below.
        self._path = None
        if database not in ("", MEMORY_DATABASE):
            self._path = _writable_path(database)
            if self._path is None:
                # Everything of a function's filesystem except its ephemeral storage is
                # read-only, so a database named anywhere else is one the engine cannot open —
                # which is the sentence the engine gives for exactly that.
                raise OperationalError("unable to open database file")
        named = database if self._path is None else self._path
        held_file = self._path is not None and _os.path.exists(self._path)
        opened = _call("open", {"database": named, "exists": bool(held_file)})
        self._handle = opened["handle"]
        self.database = opened.get("database", named)
        if self._path is not None and not held_file:
            # The engine creates the file when it opens a database that has none, so the path
            # a handler named is a file from the first connection on — empty until something is
            # written, exactly as the engine leaves it.
            try:
                with open(self._path, "ab"):
                    pass
            except OSError:
                _call("close", {"handle": self._handle})
                self._closed = True
                raise OperationalError("unable to open database file") from None

    def _mirror(self):
        """Write what the engine holds into the file this database is named by.

        The bytes are the engine's own image of the database, so the file carries the database
        itself. It is written where a database reaches a consistent state — a commit, a
        rollback, a script, a close — which is where the engine itself writes one out.
        """
        if self._path is None:
            return
        answered = _call("image", {"handle": self._handle})
        image = answered.get("image")
        if image is None:
            return
        try:
            with open(self._path, "wb") as held:
                held.write(_base64.b64decode(image.encode("ascii")))
        except OSError:
            # The file the database is named by is gone or cannot be written. The database
            # itself is unaffected: what the handler wrote is still in it.
            pass

    def _usable(self):
        if self._closed:
            raise ProgrammingError("Cannot operate on a closed database.")

    @property
    def in_transaction(self):
        return self._in_transaction

    @property
    def total_changes(self):
        return self._total_changes

    @property
    def isolation_level(self):
        return self._isolation_level

    @isolation_level.setter
    def isolation_level(self, level):
        if level is None:
            self.commit()
        elif str(level).strip().upper() not in ("", "DEFERRED", "IMMEDIATE", "EXCLUSIVE"):
            raise ValueError(
                "isolation_level string must be '', 'DEFERRED', 'IMMEDIATE', or 'EXCLUSIVE'"
            )
        self._isolation_level = level

    def _statement(self, sql, parameters):
        """Run one statement, opening a transaction first where this module's handling does."""
        self._usable()
        if (
            self._isolation_level is not None
            and not self._in_transaction
            and _first_word(sql) in _TRANSACTIONAL
        ):
            level = str(self._isolation_level or "").strip()
            self._run("BEGIN " + level if level else "BEGIN", None)
        return self._run(sql, parameters)

    def _run(self, sql, parameters):
        """One statement on the host seam, with what it reported about the connection held."""
        document = {"handle": self._handle, "sql": sql}
        if parameters is not None:
            document["params"] = parameters
        answered = _call("execute", document)
        self._noticed(answered)
        return answered

    def _script(self, sql):
        answered = _call("script", {"handle": self._handle, "sql": sql})
        self._noticed(answered)
        self._mirror()
        return answered

    def _noticed(self, answered):
        self._in_transaction = bool(answered.get("inTransaction"))
        self._total_changes = int(answered.get("totalChanges") or 0)

    def _column_name(self, name):
        """The name a column is described by: the part before a type the query named."""
        if self._detect_types & PARSE_COLNAMES and "[" in name:
            return name.split("[")[0].strip()
        return name

    def _converters(self, columns, declared):
        """The converter for each answered column, where the connection asked for them."""
        converters = []
        for index, name in enumerate(columns):
            convert = None
            if self._detect_types & PARSE_COLNAMES and "[" in name and "]" in name:
                named = name[name.find("[") + 1 : name.find("]")].strip()
                convert = _CONVERTERS.get(named.upper())
            if convert is None and self._detect_types & PARSE_DECLTYPES:
                held = declared[index] if index < len(declared) else None
                if held:
                    convert = _CONVERTERS.get(str(held).split("(")[0].strip().upper())
            converters.append(convert)
        return converters

    def cursor(self, factory=None):
        self._usable()
        if factory is None:
            return Cursor(self)
        return factory(self)

    def execute(self, sql, parameters=()):
        return self.cursor().execute(sql, parameters)

    def executemany(self, sql, seq_of_parameters):
        return self.cursor().executemany(sql, seq_of_parameters)

    def executescript(self, sql):
        return self.cursor().executescript(sql)

    def commit(self):
        """Commit whatever this connection has open, and nothing where it has nothing."""
        self._usable()
        if self._in_transaction:
            self._run("COMMIT", None)
        self._mirror()

    def rollback(self):
        """Roll back whatever this connection has open."""
        self._usable()
        if self._in_transaction:
            self._run("ROLLBACK", None)
        self._mirror()

    def close(self):
        """Give up this connection, rolling back whatever it had open."""
        if self._closed:
            return
        if self._in_transaction:
            try:
                self._run("ROLLBACK", None)
            except Error:
                pass
        self._mirror()
        _call("close", {"handle": self._handle})
        self._closed = True

    def create_function(self, name, narg, func, **options):
        """Register a function of the program's own, which this engine does not run."""
        raise NotSupportedError(
            "this runtime's SQLite engine runs only its own library functions, so a function "
            "registered by a handler is not available to a statement"
        )

    def create_aggregate(self, name, n_arg, aggregate_class):
        raise NotSupportedError(
            "this runtime's SQLite engine runs only its own library functions, so an aggregate "
            "registered by a handler is not available to a statement"
        )

    def create_collation(self, name, callable_):
        raise NotSupportedError(
            "this runtime's SQLite engine collates with its own collations, so a collation "
            "registered by a handler is not available to a statement"
        )

    def enable_load_extension(self, enable):
        """Allow the engine's own code loader, which this runtime has none of."""
        if enable:
            raise NotSupportedError(
                "this runtime's SQLite engine loads no extensions"
            )

    def load_extension(self, path, **options):
        raise NotSupportedError("this runtime's SQLite engine loads no extensions")

    def backup(self, target, **options):
        raise NotSupportedError(
            "this runtime's SQLite engine serves no backup of a database"
        )

    def iterdump(self):
        raise NotSupportedError(
            "this runtime's SQLite engine serves no dump of a database"
        )

    def interrupt(self):
        """Stop the statement running on this connection, of which a handler runs one at a
        time: by the time this call is reached the statement it would stop has answered."""

    def set_authorizer(self, authorizer):
        raise NotSupportedError(
            "this runtime's SQLite engine confines a statement with its own authorizer"
        )

    def set_progress_handler(self, handler, n):
        raise NotSupportedError(
            "this runtime's SQLite engine bounds a statement with its own progress handler"
        )

    def set_trace_callback(self, handler):
        raise NotSupportedError(
            "this runtime's SQLite engine traces no statement to a handler's callback"
        )

    def __enter__(self):
        return self

    def __exit__(self, kind, value, trace):
        """Commit what the block did, or roll it back where the block raised."""
        if kind is None:
            self.commit()
        else:
            self.rollback()
        return False


def connect(
    database=":memory:",
    timeout=5.0,
    detect_types=0,
    isolation_level="",
    check_same_thread=True,
    factory=None,
    cached_statements=128,
    uri=False,
    **options
):
    """Open a connection to `database`.

    `":memory:"` is a private database of this connection, as the standard library documents
    it; any other name is this execution environment's own database of that name, so two
    connections to it in one environment read and write one database.
    """
    held = factory or Connection
    return held(
        str(database),
        detect_types=detect_types,
        isolation_level=isolation_level,
    )


# The default adapters the standard library registers for the two calendar types, which store
# a date and a timestamp as the ISO 8601 text of each.
register_adapter(_datetime.date, lambda value: value.isoformat())
register_adapter(_datetime.datetime, lambda value: value.isoformat(" "))

# ... and the converters it registers for the two declared types that name them.
register_converter(
    "date", lambda value: _datetime.date.fromisoformat(value.decode("utf-8"))
)
register_converter(
    "timestamp", lambda value: _datetime.datetime.fromisoformat(value.decode("utf-8"))
)
"#;
