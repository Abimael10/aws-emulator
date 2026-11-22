// SPDX-License-Identifier: CC0-1.0

//! The SQL engine ONE database resource runs: an in-process SQLite database,
//! confined to itself, that every statement arriving on that resource's
//! endpoint is planned and run by.
//!
//! In memory and per resource, so one database's rows never reach another and
//! nothing is written to a host file. A client's statement is run here as it
//! stands — nothing matches on its text and nothing is synthesised to look
//! computed — and what the engine answers (its rows, its counts and its own
//! error messages) is what the wire reports.
//!
//! What a client's SQL may reach is DENIED BY DEFAULT
//! ([`confine_to_the_database`]): the tables of this one database, the DDL that
//! defines them, transactions, and pure computation over the values in them.
//! The functions are an allow-list of their own
//! ([`crate::sqlite_confinement`]) plus the few server functions a database
//! engine answers from the calling session's own view ([`EngineView`]).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use ::rusqlite::functions::FunctionFlags;
use ::rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use ::rusqlite::limits::Limit;
use ::rusqlite::types::ValueRef;
use ::rusqlite::Connection;

/// How long one statement may run before the engine is interrupted.
///
/// CHOSEN: a bound on the work one connection can ask of this process, not a
/// published engine setting. A statement that reaches it is reported as
/// cancelled, which is what an engine answers a statement it stopped.
pub const STATEMENT_DEADLINE: Duration = Duration::from_secs(5);

/// How many engine steps pass between two checks of that deadline.
const PROGRESS_STEPS: std::os::raw::c_int = 1_000;

/// The most rows one statement may answer with.
///
/// CHOSEN, for the same reason the deadline is: the rows of one answer are held
/// in this process's memory before they are written to the client.
pub const MAX_RESULT_ROWS: usize = 10_000;

/// The most bytes the values of one answer may hold together, charged as each
/// value is read out of the engine — so a statement that would build a larger
/// answer is refused instead of allocating it.
pub const MAX_RESULT_BYTES: usize = 8 * 1024 * 1024;

/// The most bytes one statement may carry.
const MAX_SQL_BYTES: i32 = 256 * 1024;

/// The most bytes one value may hold.
const MAX_VALUE_BYTES: i32 = 1024 * 1024;

/// The most columns one answer may carry.
///
/// SQLite's own default is 2000, which with the value ceiling above would let a
/// single row hold far more than [`MAX_RESULT_BYTES`]; this keeps one row's
/// worst case inside the answer budget.
const MAX_RESULT_COLUMNS: i32 = 256;

/// The most pages one database may hold: SQLite's own page ceiling, which is
/// what bounds the database a client can grow in this process.
const MAX_DATABASE_PAGES: i32 = 32 * 1024;

/// What one value of an answer is, in the engine's own storage classes.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl Cell {
    /// How many bytes of this process's memory this value holds, for the bound
    /// on one answer.
    fn byte_cost(&self) -> usize {
        match self {
            Cell::Null => 1,
            Cell::Integer(_) | Cell::Real(_) => 8,
            Cell::Text(text) => text.len(),
            Cell::Blob(bytes) => bytes.len(),
        }
    }
}

/// One column of an answer: the name the engine reports it under and the type
/// its declaration names, which is what each wire maps to its own type code.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    /// The declared type of the column, where it has one — a computed column
    /// has none, and then the value's own storage class is what the wire
    /// describes it by.
    pub declared_type: Option<String>,
}

/// What one statement answered: its columns and rows, how many rows it changed,
/// and the row id its insert minted.
#[derive(Debug, Clone, Default)]
pub struct ResultSet {
    pub columns: Vec<Column>,
    pub rows: Vec<Vec<Cell>>,
    pub affected: u64,
    pub last_insert_id: i64,
}

/// Which class of failure the engine reported, so each wire can report it under
/// its own protocol's number, code and SQLSTATE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineErrorKind {
    Syntax,
    UndefinedObject,
    UniqueViolation,
    NotNullViolation,
    CheckViolation,
    ForeignKeyViolation,
    DataError,
    Cancelled,
    LimitExceeded,
    FeatureNotSupported,
    InsufficientPrivilege,
    Internal,
}

/// A statement the engine refused, carrying the engine's OWN message: a client
/// is told what the engine said, never a sentence invented here.
#[derive(Debug, Clone)]
pub struct EngineError {
    pub kind: EngineErrorKind,
    pub message: String,
}

impl EngineError {
    /// The failure `kind`, reported with `message`.
    pub fn new(kind: EngineErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

/// What the server functions of a session answer from: the database's own
/// version and the session that is asking.
///
/// Set before each statement runs ([`DatabaseEngine::execute_as`]) and read by
/// the functions registered on the connection, so `version()` answers the
/// resource's engine version and `database()` the database this session is on
/// rather than a value fixed when the engine came up.
#[derive(Debug, Clone, Default)]
pub struct EngineView {
    /// The engine version the resource publishes.
    pub server_version: String,
    /// What `version()` answers: the sentence the resource's own server
    /// identifies itself with, which is not always the bare version. Empty
    /// falls back to [`EngineView::server_version`].
    pub server_banner: String,
    /// The database the session is using.
    pub database: String,
    /// The user the session was admitted as.
    pub user: String,
    /// The connection id the session was given.
    pub connection_id: u32,
    /// The row id the session's last insert minted, which is what
    /// `last_insert_id()` answers for THIS session.
    pub last_insert_id: i64,
}

/// The names [`DatabaseEngine`] registers on each database's connection, which
/// a statement may call beside the confinement list's own.
///
/// Each is a function BOTH engines publish — MySQL's `version()`, `database()`,
/// `user()`, `connection_id()` and `last_insert_id()`, and PostgreSQL's
/// `version()`, `current_database()` and `current_user()` — and each answers
/// from [`EngineView`] and nothing else.
const SERVER_FUNCTIONS: &[&str] = &[
    "version",
    "database",
    "current_database",
    "user",
    "current_user",
    "session_user",
    "connection_id",
    "last_insert_id",
    "lastval",
];

/// Whether `name` is one of [`SERVER_FUNCTIONS`].
fn is_server_function(name: &str) -> bool {
    SERVER_FUNCTIONS
        .iter()
        .any(|server| name.eq_ignore_ascii_case(server))
}

/// The in-process SQL engine of one database resource.
pub struct DatabaseEngine {
    /// The one connection the database's statements run on, so two sessions of
    /// the same database read and write the same tables.
    connection: Mutex<Connection>,
    /// What the server functions answer from while a statement runs.
    view: Arc<Mutex<EngineView>>,
}

/// Written by hand rather than derived: a live engine connection carries no
/// `Debug`, and the stored record this hangs off derives one.
impl std::fmt::Debug for DatabaseEngine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("DatabaseEngine").finish()
    }
}

impl DatabaseEngine {
    /// Start one database's engine: an in-memory SQLite database, bounded and
    /// confined, with this emulator's server functions registered on it.
    pub fn open() -> Result<Self, EngineError> {
        let view = Arc::new(Mutex::new(EngineView::default()));
        let connection = open_confined_database(&view)?;
        Ok(Self {
            connection: Mutex::new(connection),
            view,
        })
    }

    /// Run `sql` — ONE statement — under the session view `view`, and answer
    /// what the engine answered.
    ///
    /// The view is what the server functions answer from while the statement
    /// runs; the row id an insert minted is written back into the view the
    /// caller holds, which is what the session's own `last_insert_id()` reports
    /// next.
    ///
    /// BLOCKING: it takes the engine's connection and runs the statement on the
    /// calling thread, so a caller on an async runtime calls it from the
    /// blocking pool ([`super::session`] does).
    pub fn execute_as(&self, view: &mut EngineView, sql: &str) -> Result<ResultSet, EngineError> {
        let mut connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *self.view.lock().unwrap_or_else(PoisonError::into_inner) = view.clone();
        let deadline = Instant::now() + STATEMENT_DEADLINE;
        // The engine is interrupted when the statement outlives its budget: the
        // handler is asked every `PROGRESS_STEPS` steps, so a statement that
        // runs long is stopped by the engine itself rather than held here.
        let _ =
            connection.progress_handler(PROGRESS_STEPS, Some(move || Instant::now() >= deadline));
        let answer = run_statement(&connection, sql);
        let _ = connection.progress_handler(PROGRESS_STEPS, None::<fn() -> bool>);
        if let Ok(result) = &answer {
            if result.last_insert_id != 0 {
                view.last_insert_id = result.last_insert_id;
            }
        }
        answer
    }

    /// Whether the engine has NO transaction open — which is what tells a
    /// session's `COMMIT` that there is nothing to commit, and a client's own
    /// status flag what to say.
    ///
    /// BLOCKING, for the same reason [`Self::execute_as`] is.
    pub fn is_autocommit(&self) -> bool {
        self.connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_autocommit()
    }

    /// The tables this database holds, in name order: the engine's own catalog,
    /// which is what a `SHOW TABLES` reports.
    ///
    /// BLOCKING, for the same reason [`Self::execute_as`] is.
    pub fn table_names(&self) -> Vec<String> {
        let connection = self
            .connection
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut names = Vec::new();
        if let Ok(mut statement) = connection.prepare(
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        ) {
            if let Ok(rows) = statement.query_map([], |row| row.get::<_, String>(0)) {
                names.extend(rows.flatten());
            }
        }
        names
    }
}

/// Run one statement on `connection`.
fn run_statement(connection: &Connection, sql: &str) -> Result<ResultSet, EngineError> {
    let mut statement = connection.prepare(sql).map_err(engine_error)?;
    let columns: Vec<Column> = statement
        .columns()
        .map(|column| Column {
            name: column.name().to_string(),
            declared_type: column.decl_type().map(str::to_string),
        })
        .collect();
    if columns.is_empty() {
        let affected = statement.raw_execute().map_err(engine_error)?;
        return Ok(ResultSet {
            columns,
            rows: Vec::new(),
            affected: u64::try_from(affected).unwrap_or_default(),
            last_insert_id: connection.last_insert_rowid(),
        });
    }
    let mut rows = Vec::new();
    let mut bytes_held = 0usize;
    // Read before the rows, because the cursor borrows the statement: whether
    // the statement WRITES is what decides whether the connection's counters
    // belong to it.
    let writes = !statement.readonly();
    let mut answered = statement.raw_query();
    while let Some(row) = answered.next().map_err(engine_error)? {
        if rows.len() >= MAX_RESULT_ROWS {
            return Err(too_large());
        }
        let mut cells = Vec::with_capacity(columns.len());
        for index in 0..columns.len() {
            let cell = match row.get_ref(index).map_err(engine_error)? {
                ValueRef::Null => Cell::Null,
                ValueRef::Integer(value) => Cell::Integer(value),
                ValueRef::Real(value) => Cell::Real(value),
                ValueRef::Text(bytes) => Cell::Text(String::from_utf8_lossy(bytes).into_owned()),
                ValueRef::Blob(bytes) => Cell::Blob(bytes.to_vec()),
            };
            bytes_held = bytes_held.saturating_add(cell.byte_cost());
            // Charged and tested per VALUE, before the next one is read out of
            // the engine, so the answer this process holds can never be a whole
            // row past the budget.
            if bytes_held > MAX_RESULT_BYTES {
                return Err(too_large());
            }
            cells.push(cell);
        }
        rows.push(cells);
    }
    // The counts belong to a statement that WROTE: a read-only statement
    // changed nothing and minted no row id, and the connection's own counters
    // still carry the last write's.
    let (affected, last_insert_id) = if writes {
        (connection.changes(), connection.last_insert_rowid())
    } else {
        (0, 0)
    };
    Ok(ResultSet {
        columns,
        rows,
        affected,
        last_insert_id,
    })
}

/// The refusal of an answer larger than this endpoint serves.
fn too_large() -> EngineError {
    EngineError::new(
        EngineErrorKind::LimitExceeded,
        format!(
            "the answer is larger than this endpoint serves ({MAX_RESULT_ROWS} rows or {} MiB); \
             add a LIMIT to the statement",
            MAX_RESULT_BYTES / (1024 * 1024)
        ),
    )
}

/// The statements `sql` carries, in the order they are written, as a client of
/// `wire` wrote them.
///
/// A client may send several statements in one message (the PostgreSQL simple
/// query protocol carries them separated by `;`), so the engine is asked for
/// one at a time: SQLite compiles ONE statement per prepare. The split is
/// quote-aware — a `;` inside a string, an identifier or a comment is part of
/// the statement, not a separator — and a trailing empty statement is dropped.
///
/// What counts as quoting is the WIRE's, because the two engines differ on it:
/// MySQL ends a line comment at `#` as well as at `--` and reads a backslash
/// inside a string as an escape, while PostgreSQL has neither and instead
/// carries dollar-quoted strings (`$tag$ … $tag$`), inside which nothing at all
/// is special. Both read a doubled quote as the quote itself.
pub fn statements(wire: super::EngineWire, sql: &str) -> Vec<String> {
    let mysql = wire == super::EngineWire::MySql;
    let mut statements = Vec::new();
    let mut current = String::new();
    let mut characters = sql.chars().peekable();
    // Which quoting the scan is inside, as the character that ends it.
    let mut quoted: Option<char> = None;
    // The tag of the dollar-quoted string the scan is inside (`$tag$`), which
    // ends only at that same tag.
    let mut dollar_quoted: Option<String> = None;
    let mut line_comment = false;
    let mut block_comment = false;
    while let Some(character) = characters.next() {
        if line_comment {
            current.push(character);
            if character == '\n' {
                line_comment = false;
            }
            continue;
        }
        if block_comment {
            current.push(character);
            if character == '*' && characters.peek() == Some(&'/') {
                current.push(characters.next().unwrap_or('/'));
                block_comment = false;
            }
            continue;
        }
        if let Some(tag) = &dollar_quoted {
            current.push(character);
            // The closing sequence is `$tag$`, read ahead of the scan so
            // anything else beginning with `$` stays part of the body.
            if character == '$' {
                let mut ahead = characters.clone();
                let closes = tag.chars().all(|want| ahead.next() == Some(want))
                    && ahead.next() == Some('$');
                if closes {
                    for _ in 0..tag.chars().count() + 1 {
                        if let Some(next) = characters.next() {
                            current.push(next);
                        }
                    }
                    dollar_quoted = None;
                }
            }
            continue;
        }
        if let Some(closing) = quoted {
            current.push(character);
            // MySQL reads a backslash inside a string as an escape, so the
            // character after it is part of the string whatever it is.
            if mysql && character == '\\' {
                if let Some(escaped) = characters.next() {
                    current.push(escaped);
                }
                continue;
            }
            if character == closing {
                // A doubled quote is the quote itself, so the scan stays inside.
                if characters.peek() == Some(&closing) {
                    current.push(characters.next().unwrap_or(closing));
                } else {
                    quoted = None;
                }
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => {
                quoted = Some(character);
                current.push(character);
            }
            // A dollar-quoted string, which only PostgreSQL has: `$tag$` opens
            // it and the same `$tag$` closes it.
            '$' if !mysql => {
                current.push(character);
                let mut tag = String::new();
                while let Some(next) = characters.peek() {
                    if *next == '$' {
                        break;
                    }
                    if !next.is_alphanumeric() && *next != '_' {
                        tag.clear();
                        break;
                    }
                    tag.push(*next);
                    current.push(characters.next().unwrap_or('$'));
                    if tag.len() > MAX_DOLLAR_TAG_CHARS {
                        tag.clear();
                        break;
                    }
                }
                if characters.peek() == Some(&'$') {
                    current.push(characters.next().unwrap_or('$'));
                    dollar_quoted = Some(tag);
                }
            }
            '-' if characters.peek() == Some(&'-') => {
                line_comment = true;
                current.push(character);
            }
            // `#` begins a comment on MySQL only; on PostgreSQL it is an
            // operator character.
            '#' if mysql => {
                line_comment = true;
                current.push(character);
            }
            '/' if characters.peek() == Some(&'*') => {
                block_comment = true;
                current.push(character);
            }
            ';' => {
                if !current.trim().is_empty() {
                    statements.push(current.trim().to_string());
                }
                current.clear();
            }
            other => {
                // A WORD of the statement — not text inside a string, a quoted
                // identifier, a comment or a dollar-quoted body, each of which
                // the branches above have already consumed. This is therefore
                // the ONE place that knows a word is a word, which is where the
                // niladic keywords PostgreSQL admits without parentheses are
                // read as the calls they denote ([`NILADIC_KEYWORDS`]).
                current.push(other);
                if mysql || !(other.is_alphabetic() || other == '_') {
                    continue;
                }
                let mut word = String::from(other);
                while characters
                    .peek()
                    .is_some_and(|next| next.is_alphanumeric() || *next == '_')
                {
                    if let Some(next) = characters.next() {
                        word.push(next);
                        current.push(next);
                    }
                }
                if !NILADIC_KEYWORDS
                    .iter()
                    .any(|keyword| word.eq_ignore_ascii_case(keyword))
                {
                    continue;
                }
                // A word ALREADY followed by `(` is a call and is left alone.
                // Read ahead on a clone, as the dollar-quote scan above does,
                // so the scan itself does not move.
                let mut ahead = characters.clone();
                let called = loop {
                    match ahead.next() {
                        Some(next) if next.is_whitespace() => continue,
                        Some('(') => break true,
                        _ => break false,
                    }
                };
                if !called {
                    current.push_str("()");
                }
            }
        }
    }
    if !current.trim().is_empty() {
        statements.push(current.trim().to_string());
    }
    statements
}

/// The niladic functions PostgreSQL lets a statement write WITHOUT parentheses,
/// each read by [`statements`] as the call it is.
///
/// `current_user` and `session_user` are reserved words of the PostgreSQL
/// grammar that denote a function call — "note that these are not written with
/// trailing parentheses" (PostgreSQL's "System Information Functions") — so a
/// client writes `SELECT current_user` and expects the session's own user. The
/// engine behind this endpoint reads a bare one as a column reference, so each
/// is read as the call this endpoint registers for it ([`SERVER_FUNCTIONS`]).
/// Being reserved words, neither can be the unquoted name of a column, so
/// nothing else can be read differently by mistake.
const NILADIC_KEYWORDS: [&str; 2] = ["current_user", "session_user"];


/// The longest tag a dollar-quoted string may name, so a `$` a statement
/// carries for another reason cannot make the scan read ahead without bound.
const MAX_DOLLAR_TAG_CHARS: usize = 64;

/// Open one database's connection: in memory, bounded, confined, with the
/// server functions registered on it.
fn open_confined_database(view: &Arc<Mutex<EngineView>>) -> Result<Connection, EngineError> {
    let mut connection = Connection::open_in_memory().map_err(unavailable)?;
    // The temp database in memory FIRST, so nothing the engine spills reaches a
    // host file, then the ceiling on the pages this database may hold.
    connection
        .pragma_update(None, "temp_store", "MEMORY")
        .map_err(unavailable)?;
    for schema in ["main", "temp"] {
        connection
            .pragma_update(Some(schema), "max_page_count", MAX_DATABASE_PAGES)
            .map_err(unavailable)?;
    }
    for (limit, value) in [
        // No second database, ever: this also takes `VACUUM INTO` away, which
        // the engine implements by attaching its output file.
        (Limit::SQLITE_LIMIT_ATTACHED, 0),
        (Limit::SQLITE_LIMIT_LENGTH, MAX_VALUE_BYTES),
        (Limit::SQLITE_LIMIT_SQL_LENGTH, MAX_SQL_BYTES),
        (Limit::SQLITE_LIMIT_COLUMN, MAX_RESULT_COLUMNS),
    ] {
        connection.set_limit(limit, value).map_err(unavailable)?;
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
            .map_err(unavailable)?;
    }
    register_server_functions(&connection, view)?;
    // Installed LAST, so the settings above — themselves pragmas — are not
    // refused by it.
    connection
        .authorizer(Some(confine_to_the_database))
        .map_err(unavailable)?;
    Ok(connection)
}

/// What a server function answers from the session view: a text value, or a
/// number.
type TextFunction = fn(&EngineView) -> String;
type NumberFunction = fn(&EngineView) -> i64;

/// Register the server functions both engines publish, each answering from the
/// session view the statement is running under.
fn register_server_functions(
    connection: &Connection,
    view: &Arc<Mutex<EngineView>>,
) -> Result<(), EngineError> {
    let read = |view: &Arc<Mutex<EngineView>>| -> EngineView {
        view.lock().unwrap_or_else(PoisonError::into_inner).clone()
    };
    let text_functions: [(&str, TextFunction); 6] = [
        ("version", |view| {
            if view.server_banner.is_empty() {
                view.server_version.clone()
            } else {
                view.server_banner.clone()
            }
        }),
        ("database", |view| view.database.clone()),
        ("current_database", |view| view.database.clone()),
        ("user", |view| view.user.clone()),
        ("current_user", |view| view.user.clone()),
        ("session_user", |view| view.user.clone()),
    ];
    for (name, answer) in text_functions {
        let held = Arc::clone(view);
        connection
            .create_scalar_function(name, 0, FunctionFlags::SQLITE_UTF8, move |_| {
                Ok(answer(&read(&held)))
            })
            .map_err(unavailable)?;
    }
    let number_functions: [(&str, NumberFunction); 3] = [
        ("connection_id", |view| i64::from(view.connection_id)),
        ("last_insert_id", |view| view.last_insert_id),
        ("lastval", |view| view.last_insert_id),
    ];
    for (name, answer) in number_functions {
        let held = Arc::clone(view);
        connection
            .create_scalar_function(name, 0, FunctionFlags::SQLITE_UTF8, move |_| {
                Ok(answer(&read(&held)))
            })
            .map_err(unavailable)?;
    }
    Ok(())
}

/// What a client's SQL may do inside ONE database.
///
/// DEFAULT DENY: an action not named here is refused, so an engine capability
/// this emulator has not considered — including one a later SQLite build adds —
/// cannot be reached from a session. What is allowed is what the data plane
/// exists for: reading and writing this database's own tables, the DDL that
/// defines them, transactions, and pure computation over the values in them.
///
/// `ATTACH`/`DETACH` would make a host path visible to SQL (and `VACUUM INTO`
/// is implemented by attaching its output file), `PRAGMA` reaches engine
/// configuration including temp-file paths, and a virtual table is arbitrary
/// registered code — and none of them is something a MySQL or PostgreSQL client
/// can ask for over its own wire anyway.
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
        // Pure computation over the values in those tables, as an ALLOW-LIST: a
        // function this emulator has not reviewed — including one a future
        // engine build defines — falls through to the deny arm below.
        AuthAction::Function { function_name } => {
            if crate::sqlite_confinement::sql_function_is_permitted(function_name)
                || is_server_function(function_name)
            {
                Authorization::Allow
            } else {
                Authorization::Deny
            }
        }
        // Everything that could reach outside this database, and everything
        // this emulator has not considered.
        _ => Authorization::Deny,
    }
}

/// An engine that could not be brought up or configured at all.
fn unavailable(error: ::rusqlite::Error) -> EngineError {
    EngineError::new(EngineErrorKind::Internal, error.to_string())
}

/// The class and the message a failed statement is reported under: the ENGINE's
/// own sentence, under the class its result code names.
fn engine_error(error: ::rusqlite::Error) -> EngineError {
    let message = match &error {
        ::rusqlite::Error::SqliteFailure(_, Some(message)) => message.clone(),
        // Text the engine could not PARSE: its own sentence is the `msg`, which
        // names what it stopped at, rather than the `Display` form that repeats
        // the statement back.
        ::rusqlite::Error::SqlInputError { msg, .. } => msg.clone(),
        other => other.to_string(),
    };
    let Some(code) = failure_code(&error) else {
        return EngineError::new(EngineErrorKind::Internal, message);
    };
    let kind = match code.code {
        ::rusqlite::ErrorCode::ConstraintViolation => constraint_kind(code.extended_code),
        ::rusqlite::ErrorCode::OperationInterrupted => EngineErrorKind::Cancelled,
        ::rusqlite::ErrorCode::AuthorizationForStatementDenied => {
            EngineErrorKind::InsufficientPrivilege
        }
        ::rusqlite::ErrorCode::NotADatabase | ::rusqlite::ErrorCode::TypeMismatch => {
            EngineErrorKind::DataError
        }
        ::rusqlite::ErrorCode::DiskFull | ::rusqlite::ErrorCode::OutOfMemory => {
            EngineErrorKind::LimitExceeded
        }
        ::rusqlite::ErrorCode::Unknown => undefined_or_syntax(&message),
        _ => EngineErrorKind::Internal,
    };
    EngineError::new(kind, message)
}

/// The SQLite failure behind `error`, where it carries one.
fn failure_code(error: &::rusqlite::Error) -> Option<::rusqlite::ffi::Error> {
    match error {
        ::rusqlite::Error::SqliteFailure(code, _) => Some(*code),
        // The failure a PREPARE reported, which carries the same result code as
        // any other failure: a statement the engine could not parse is a
        // statement error and not an internal one.
        ::rusqlite::Error::SqlInputError { error, .. } => Some(*error),
        _ => None,
    }
}

/// Whether the engine's own sentence says the statement named something that
/// does not exist, which is a class of its own on both wires, or is a statement
/// the engine could not parse at all.
///
/// SQLite reports both as `SQLITE_ERROR`, and the only thing that tells them
/// apart is the sentence it wrote — which is what the client is shown either
/// way.
fn undefined_or_syntax(message: &str) -> EngineErrorKind {
    let lowered = message.to_ascii_lowercase();
    if lowered.starts_with("no such table")
        || lowered.starts_with("no such column")
        || lowered.starts_with("no such index")
        || lowered.starts_with("no such view")
        || lowered.starts_with("no such function")
    {
        return EngineErrorKind::UndefinedObject;
    }
    if lowered.contains("not authorized") {
        return EngineErrorKind::InsufficientPrivilege;
    }
    if lowered.contains("not supported") {
        return EngineErrorKind::FeatureNotSupported;
    }
    EngineErrorKind::Syntax
}

/// Which constraint the engine refused the row for, from its extended result
/// code.
fn constraint_kind(extended_code: std::os::raw::c_int) -> EngineErrorKind {
    // The extended codes SQLite documents for SQLITE_CONSTRAINT: the primary
    // result code (19) in the low byte and the constraint in the high byte.
    const SQLITE_CONSTRAINT_CHECK: std::os::raw::c_int = 275;
    const SQLITE_CONSTRAINT_FOREIGNKEY: std::os::raw::c_int = 787;
    const SQLITE_CONSTRAINT_NOTNULL: std::os::raw::c_int = 1299;
    const SQLITE_CONSTRAINT_PRIMARYKEY: std::os::raw::c_int = 1555;
    const SQLITE_CONSTRAINT_UNIQUE: std::os::raw::c_int = 2067;
    match extended_code {
        SQLITE_CONSTRAINT_FOREIGNKEY => EngineErrorKind::ForeignKeyViolation,
        SQLITE_CONSTRAINT_NOTNULL => EngineErrorKind::NotNullViolation,
        SQLITE_CONSTRAINT_PRIMARYKEY | SQLITE_CONSTRAINT_UNIQUE => EngineErrorKind::UniqueViolation,
        SQLITE_CONSTRAINT_CHECK => EngineErrorKind::CheckViolation,
        _ => EngineErrorKind::CheckViolation,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view of one session, for a statement that calls a server function.
    fn view_of(database: &str) -> EngineView {
        EngineView {
            server_version: "8.4.11".to_string(),
            server_banner: String::new(),
            database: database.to_string(),
            user: "dbadmin".to_string(),
            connection_id: 7,
            last_insert_id: 0,
        }
    }

    /// RULE: a statement is RUN by the engine — its rows, its counts and its
    /// own error sentences are the engine's — and what it may reach is denied
    /// by default.
    #[test]
    fn statements_are_run_by_the_engine_and_confined_to_the_database() {
        let engine = DatabaseEngine::open().expect("the engine starts");
        let mut view = view_of("appdb");
        engine
            .execute_as(&mut view, "CREATE TABLE t (id INTEGER PRIMARY KEY, val TEXT)")
            .expect("the DDL runs");
        let written = engine
            .execute_as(&mut view, "INSERT INTO t VALUES (1, 'one'), (2, 'two')")
            .expect("the INSERT runs");
        assert_eq!(
            written.affected, 2,
            "case the INSERT: expected the engine's own count, got {written:?}"
        );
        let read = engine
            .execute_as(&mut view, "SELECT val FROM t ORDER BY id")
            .expect("the SELECT runs");
        assert_eq!(
            read.rows,
            vec![
                vec![Cell::Text("one".to_string())],
                vec![Cell::Text("two".to_string())]
            ],
            "case the SELECT: expected the rows the INSERT wrote, got {read:?}"
        );
        assert_eq!(
            read.columns.first().map(|column| column.declared_type.clone()),
            Some(Some("TEXT".to_string())),
            "case the SELECT: expected the column's declared type, got {read:?}"
        );
        // The server functions answer from the calling session's view.
        let named = engine
            .execute_as(&mut view, "SELECT database(), user(), version()")
            .expect("the server functions run");
        assert_eq!(
            named.rows,
            vec![vec![
                Cell::Text("appdb".to_string()),
                Cell::Text("dbadmin".to_string()),
                Cell::Text("8.4.11".to_string())
            ]],
            "case the server functions: expected the session's own view, got {named:?}"
        );
        // And what reaches outside this database is refused, whatever the
        // statement asks for.
        for refused in [
            "ATTACH DATABASE '/tmp/x.db' AS other",
            "PRAGMA journal_mode = WAL",
            "SELECT load_extension('x')",
            "SELECT readfile('/etc/passwd')",
            "VACUUM INTO '/tmp/copy.db'",
        ] {
            let answer = engine.execute_as(&mut view, refused);
            assert!(
                answer.is_err(),
                "case {refused:?}: a statement reaching outside the database is refused, got \
                 {answer:?}"
            );
        }
    }

    /// RULE: a message may carry several statements, and a `;` inside a string,
    /// an identifier or a comment is part of the statement rather than a
    /// separator — read as the WIRE's own client wrote it.
    #[test]
    fn a_message_is_split_into_the_statements_it_carries() {
        use super::super::EngineWire;
        // (case, the wire, the message, the statements it carries)
        let rows: &[(&str, EngineWire, &str, &[&str])] = &[
            ("one statement", EngineWire::MySql, "SELECT 1", &["SELECT 1"]),
            ("a trailing semicolon", EngineWire::MySql, "SELECT 1;", &["SELECT 1"]),
            (
                "two statements",
                EngineWire::Postgres,
                "SELECT 1; SELECT 2",
                &["SELECT 1", "SELECT 2"],
            ),
            ("nothing at all", EngineWire::Postgres, "   ", &[]),
            (
                "a semicolon inside a string",
                EngineWire::MySql,
                "INSERT INTO t VALUES ('a;b'); SELECT 1",
                &["INSERT INTO t VALUES ('a;b')", "SELECT 1"],
            ),
            (
                "a doubled quote inside a string",
                EngineWire::Postgres,
                "INSERT INTO t VALUES ('it''s; here')",
                &["INSERT INTO t VALUES ('it''s; here')"],
            ),
            (
                "a semicolon inside a quoted identifier",
                EngineWire::Postgres,
                "SELECT \"a;b\" FROM t",
                &["SELECT \"a;b\" FROM t"],
            ),
            (
                "a semicolon inside a line comment",
                EngineWire::Postgres,
                "SELECT 1 -- a; comment\n",
                &["SELECT 1 -- a; comment"],
            ),
            (
                "a semicolon inside a block comment",
                EngineWire::MySql,
                "SELECT /* a; comment */ 1",
                &["SELECT /* a; comment */ 1"],
            ),
            // MySQL's own quoting: `#` begins a comment and a backslash inside
            // a string escapes the character after it, so neither hides a
            // separator and neither ends the string early.
            (
                "a semicolon inside a MySQL hash comment",
                EngineWire::MySql,
                "SELECT 1 # a; comment\nSELECT 2",
                &["SELECT 1 # a; comment\nSELECT 2"],
            ),
            (
                "a backslash-escaped quote inside a MySQL string",
                EngineWire::MySql,
                "INSERT INTO t VALUES ('a\\'; b'); SELECT 1",
                &["INSERT INTO t VALUES ('a\\'; b')", "SELECT 1"],
            ),
            // PostgreSQL's own: `#` is an operator character rather than a
            // comment, and a dollar-quoted body carries anything at all.
            (
                "a hash on the PostgreSQL wire is not a comment",
                EngineWire::Postgres,
                "SELECT 1 # 2; SELECT 3",
                &["SELECT 1 # 2", "SELECT 3"],
            ),
            (
                "a semicolon inside a dollar-quoted body",
                EngineWire::Postgres,
                "SELECT $$a; b$$; SELECT 2",
                &["SELECT $$a; b$$", "SELECT 2"],
            ),
            (
                "a semicolon inside a tagged dollar-quoted body",
                EngineWire::Postgres,
                "SELECT $tag$a; $$ b$tag$; SELECT 2",
                &["SELECT $tag$a; $$ b$tag$", "SELECT 2"],
            ),
        ];
        for (case, wire, message, want) in rows {
            let answer = statements(*wire, message);
            assert_eq!(
                answer, *want,
                "case {case:?}: expected {want:?}, got {answer:?}"
            );
        }
    }

    /// RULE: an answer larger than this endpoint serves is refused rather than
    /// allocated — charged per value, so one row cannot overshoot the budget.
    #[test]
    fn an_answer_past_the_budget_is_refused() {
        let engine = DatabaseEngine::open().expect("the engine starts");
        let mut view = view_of("appdb");
        engine
            .execute_as(&mut view, "CREATE TABLE wide (val TEXT)")
            .expect("the DDL runs");
        // Each row holds a value of a known size, so the number of rows the
        // budget admits is known: the refusal has to arrive before the rows
        // this process holds pass it.
        let value_bytes = 64 * 1024;
        let rows_past_the_budget = MAX_RESULT_BYTES / value_bytes + 2;
        for _ in 0..rows_past_the_budget {
            engine
                .execute_as(
                    &mut view,
                    &format!("INSERT INTO wide VALUES (printf('%.{value_bytes}c', 'x'))"),
                )
                .expect("the INSERT runs");
        }
        let answer = engine.execute_as(&mut view, "SELECT val FROM wide");
        let error = answer
            .err()
            .unwrap_or_else(|| panic!("case the answer budget: an oversized answer is refused"));
        assert_eq!(
            error.kind,
            EngineErrorKind::LimitExceeded,
            "case the answer budget: expected the limit refusal, got {error:?}"
        );
        // And the same statement under a LIMIT, which is what the refusal tells
        // the client to add, is answered.
        let bounded = engine
            .execute_as(&mut view, "SELECT val FROM wide LIMIT 2")
            .expect("the bounded answer is served");
        assert_eq!(
            bounded.rows.len(),
            2,
            "case the answer budget: expected the bounded answer, got {} rows",
            bounded.rows.len()
        );
    }

    /// RULE: the niladic functions the PostgreSQL grammar lets a client write
    /// WITHOUT parentheses are read as the calls they denote, on the wires that
    /// speak that protocol and nowhere else — and only where the word is a word
    /// of the statement rather than text inside it.
    #[test]
    fn the_niladic_keywords_are_read_as_the_calls_they_denote() {
        use super::super::EngineWire;
        // (case, the wire, the statement, what the engine is handed)
        let rows: &[(&str, EngineWire, &str, &str)] = &[
            (
                "the bare keyword",
                EngineWire::Postgres,
                "SELECT current_user",
                "SELECT current_user()",
            ),
            (
                "the other keyword, on the warehouse wire",
                EngineWire::Redshift,
                "SELECT session_user",
                "SELECT session_user()",
            ),
            (
                "written in another case",
                EngineWire::Postgres,
                "SELECT CURRENT_USER",
                "SELECT CURRENT_USER()",
            ),
            (
                "already a call",
                EngineWire::Postgres,
                "SELECT current_user()",
                "SELECT current_user()",
            ),
            (
                "a call with a space before the parenthesis",
                EngineWire::Postgres,
                "SELECT current_user ()",
                "SELECT current_user ()",
            ),
            (
                "inside an expression",
                EngineWire::Postgres,
                "SELECT 'as ' || current_user || '!'",
                "SELECT 'as ' || current_user() || '!'",
            ),
            (
                "inside a string literal",
                EngineWire::Postgres,
                "SELECT 'current_user'",
                "SELECT 'current_user'",
            ),
            (
                "inside a quoted identifier",
                EngineWire::Postgres,
                "SELECT \"current_user\" FROM t",
                "SELECT \"current_user\" FROM t",
            ),
            (
                "inside a comment",
                EngineWire::Postgres,
                "SELECT 1 -- current_user",
                "SELECT 1 -- current_user",
            ),
            (
                "inside a block comment",
                EngineWire::Postgres,
                "SELECT /* current_user */ 1",
                "SELECT /* current_user */ 1",
            ),
            // A dollar-quoted body carries anything at all, so a word inside
            // one is text and not a word of the statement.
            (
                "inside a dollar-quoted string",
                EngineWire::Postgres,
                "SELECT $$current_user$$",
                "SELECT $$current_user$$",
            ),
            (
                "inside a tagged dollar-quoted string",
                EngineWire::Redshift,
                "SELECT $tag$ current_user $tag$ || current_user",
                "SELECT $tag$ current_user $tag$ || current_user()",
            ),
            (
                "a longer word that only starts with the keyword",
                EngineWire::Postgres,
                "SELECT current_users FROM t",
                "SELECT current_users FROM t",
            ),
            // The MySQL wire's grammar has no such form: there the word is a
            // column reference and is left exactly as written.
            (
                "the keyword on the MySQL wire",
                EngineWire::MySql,
                "SELECT current_user",
                "SELECT current_user",
            ),
        ];
        for (case, wire, statement, want) in rows {
            let answer = statements(*wire, statement);
            assert_eq!(
                answer,
                vec![(*want).to_string()],
                "case {case:?}: expected {want:?}, got {answer:?}"
            );
        }
    }
}
