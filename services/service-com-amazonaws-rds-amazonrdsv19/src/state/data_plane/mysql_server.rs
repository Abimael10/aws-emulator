// SPDX-License-Identifier: CC0-1.0

//! The MySQL SERVER surface behind a MySQL-family DB instance's endpoint: the system
//! catalogs a session reads, the Amazon RDS stored procedures it calls, and the statements
//! that end another session.
//!
//! None of it is a canned answer. Every row here is derived from this service's own state:
//! the schema list is the databases the instance holds, the process list is the sessions this
//! endpoint is serving right now, and the configuration a procedure reports is the one a
//! procedure of the same instance set.
//!
//! The catalogs are served as REAL TABLES in the `information_schema` system schema every
//! MySQL server has, rebuilt from that state when a statement names one — so a client's
//! `SELECT … FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = ?` is a query the engine
//! plans and runs like any other, rather than a result set assembled from the statement's
//! text. The PostgreSQL half of the same idea is [`super::pg_server`], and the accounts a
//! MySQL server keeps are [`super::mysql_accounts`].

use ::sqlparser::tokenizer::Token;

use super::sql_engine::{Cell, ColumnInfo, EngineError, EngineErrorKind, InstanceDatabase, ResultSet};
use super::wire_session::{SessionFacts, StatementAnswer};
use super::EngineWire;

/// The system schema a MySQL server serves its catalogs from, which every one of them lives
/// in.
const CATALOG_DATABASE: &str = "information_schema";

/// The system schema a MySQL server keeps its own tables in, which is where the Amazon RDS
/// procedures' configuration lives — `mysql.rds_configuration`, the table RDS keeps it in.
const RDS_DATABASE: &str = "mysql";

/// The system catalogs this server serves, under the names MySQL gives them.
///
/// A statement that names one has that catalog rebuilt from state before it runs
/// ([`refresh_catalogs`]); a statement that names none pays nothing.
pub const CATALOGS: [&str; 3] = ["SCHEMATA", "PROCESSLIST", "COLUMNS"];

/// The catalog tables' definitions, with the columns MySQL publishes for each.
///
/// Source: the MySQL Reference Manual's "The INFORMATION_SCHEMA SCHEMATA Table"
/// (`CATALOG_NAME`, `SCHEMA_NAME`, `DEFAULT_CHARACTER_SET_NAME`, `DEFAULT_COLLATION_NAME`,
/// `SQL_PATH`) and "The INFORMATION_SCHEMA PROCESSLIST Table" (`ID`, `USER`, `HOST`, `DB`,
/// `COMMAND`, `TIME`, `STATE`, `INFO`), reduced to the columns this server has state behind.
/// A column it has no state for is left out of the view rather than filled with a value
/// nothing stands behind.
const CATALOG_SCHEMA: [&str; 3] = [
    "CREATE TABLE IF NOT EXISTS SCHEMATA (\
       CATALOG_NAME text, SCHEMA_NAME text PRIMARY KEY, \
       DEFAULT_CHARACTER_SET_NAME text, DEFAULT_COLLATION_NAME text, SQL_PATH text)",
    "CREATE TABLE IF NOT EXISTS PROCESSLIST (\
       ID integer PRIMARY KEY, USER text, HOST text, DB text, COMMAND text, \
       TIME integer, STATE text, INFO text)",
    "CREATE TABLE IF NOT EXISTS COLUMNS (\
       TABLE_SCHEMA text, TABLE_NAME text, COLUMN_NAME text, ORDINAL_POSITION integer, \
       IS_NULLABLE text, DATA_TYPE text, COLUMN_TYPE text, CHARACTER_SET_NAME text, \
       COLLATION_NAME text, PRIMARY KEY (TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME))",
];

/// `SCHEMATA.CATALOG_NAME` — MySQL has one catalog and always reports it as `def`.
const CATALOG_NAME: &str = "def";

/// `PROCESSLIST.COMMAND` for a session that is running a statement, and for one between
/// statements: "the type of command the thread is executing". A connection waiting for its
/// next statement is `Sleep`.
const COMMAND_QUERY: &str = "Query";
const COMMAND_SLEEP: &str = "Sleep";

/// `PROCESSLIST.STATE` for a session that is running a statement — "an action, event, or
/// state that indicates what the thread is doing". A thread that is not executing anything
/// is in no such state, which MySQL reports as the empty one.
const STATE_EXECUTING: &str = "executing";

/// The system catalogs a statement names, in the order [`CATALOGS`] lists them. A statement
/// of a session on the other wire names none.
///
/// A catalog is named the way a client reaches one: QUALIFIED by the schema it lives in
/// (`information_schema.SCHEMATA`), or bare on a session connected to that schema. A table,
/// alias or column of a client's own that happens to share the name is not a catalog
/// reference, and rebuilding one costs nothing for a statement that names none.
#[must_use]
pub fn catalogs_named(facts: &SessionFacts, tokens: &[Token]) -> Vec<&'static str> {
    if facts.wire != EngineWire::MySql {
        return Vec::new();
    }
    let on_the_schema = facts.database.eq_ignore_ascii_case(CATALOG_DATABASE);
    CATALOGS
        .into_iter()
        .filter(|catalog| {
            tokens.iter().enumerate().any(|(at, token)| match token {
                Token::Word(word) if word.value.eq_ignore_ascii_case(catalog) => {
                    on_the_schema || qualified_by_the_catalog_schema(tokens, at)
                }
                _ => false,
            })
        })
        .collect()
}

/// Whether the word at `at` is qualified by the schema the catalogs live in: the two tokens
/// before it are `information_schema` and a period ([`super::dialect::qualified_by_schema`],
/// the one reading of that qualification).
fn qualified_by_the_catalog_schema(tokens: &[Token], at: usize) -> bool {
    super::dialect::qualified_by_schema(tokens, at, CATALOG_DATABASE)
}

/// Rebuild the catalogs `named` from the state `facts` carries, so a statement that reads
/// one reads the instance as it is at this moment.
pub fn refresh_catalogs(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    named: &[&str],
) -> Result<(), EngineError> {
    if named.is_empty() {
        return Ok(());
    }
    ensure_catalogs(database, facts)?;
    for catalog in named {
        match *catalog {
            "SCHEMATA" => refresh_schemata(database, facts)?,
            "PROCESSLIST" => refresh_processlist(database, facts)?,
            "COLUMNS" => refresh_columns(database, facts)?,
            _ => {}
        }
    }
    Ok(())
}

/// The CHARACTER SET and COLLATION each column was DECLARED with, which is this server's own
/// bookkeeping: the engine declares neither, so what the client wrote is recorded here when
/// the table is created and read back by the column catalog.
const COLUMN_CHARSET_TABLE: &str = "column_charsets";

/// The definition of that table.
const COLUMN_CHARSET_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS column_charsets (\
       TABLE_SCHEMA text NOT NULL, TABLE_NAME text NOT NULL, COLUMN_NAME text NOT NULL, \
       CHARACTER_SET_NAME text, COLLATION_NAME text, \
       PRIMARY KEY (TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME))";

/// `utf8`, which MySQL 8.0 renamed: "utf8mb3 … the old name for this character set is utf8",
/// and the catalogs report the new name.
const UTF8MB3: &str = "utf8mb3";

/// The collation each character set is used with where the declaration named none: the
/// charset's own default, as the MySQL Reference Manual's "Character Sets and Collations"
/// tables publish it. A character set this does not carry reports no collation rather than a
/// guessed one.
const DEFAULT_COLLATIONS: [(&str, &str); 4] = [
    ("utf8mb4", "utf8mb4_0900_ai_ci"),
    ("utf8mb3", "utf8_general_ci"),
    ("latin1", "latin1_swedish_ci"),
    ("ascii", "ascii_general_ci"),
];

/// Record the character set and collation each column of a `CREATE TABLE` was declared with.
///
/// The engine declares neither — a column's character set is not part of the declaration it
/// is handed ([`super::dialect`]) — so what the client wrote is kept here, which is what
/// `information_schema.COLUMNS` reports for the column afterwards. A statement that declared
/// none records nothing, and such a column reports the SERVER's own character set, which is
/// what MySQL gives a column declared without one.
pub fn record_column_charsets(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
) -> Result<(), EngineError> {
    let declared = declared_charsets(tokens);
    if declared.is_empty() {
        return Ok(());
    }
    database.create_database(RDS_DATABASE)?;
    database.execute_in(facts.connection_id, RDS_DATABASE, COLUMN_CHARSET_SCHEMA, &[])?;
    for (table, column, charset, collation) in declared {
        let charset = charset.map(|named| {
            if named.eq_ignore_ascii_case("utf8") {
                UTF8MB3.to_string()
            } else {
                named
            }
        });
        let collation = collation.or_else(|| {
            let charset = charset.as_deref()?;
            DEFAULT_COLLATIONS
                .iter()
                .find(|(known, _)| charset.eq_ignore_ascii_case(known))
                .map(|(_, collation)| (*collation).to_string())
        });
        database.execute_in(
            facts.connection_id,
            RDS_DATABASE,
            "INSERT OR REPLACE INTO column_charsets (TABLE_SCHEMA, TABLE_NAME, COLUMN_NAME, \
             CHARACTER_SET_NAME, COLLATION_NAME) VALUES (?, ?, ?, ?, ?)",
            &[
                Some(facts.database.clone()),
                Some(table),
                Some(column),
                charset,
                collation,
            ],
        )?;
    }
    Ok(())
}

/// The character set and collation each column of a `CREATE TABLE` names, as
/// `(table, column, character set, collation)` — a column that names neither is left out.
///
/// Read off the statement's own tokens: the table it creates, then the column list split on
/// its own commas, each clause starting with the column's name.
fn declared_charsets(tokens: &[Token]) -> Vec<(String, String, Option<String>, Option<String>)> {
    let significant: Vec<&Token> = tokens
        .iter()
        .filter(|token| !matches!(token, Token::Whitespace(_)))
        .collect();
    let word = |token: Option<&&Token>| match token {
        Some(Token::Word(word)) => Some(word.value.clone()),
        _ => None,
    };
    let keyword = |token: Option<&&Token>| match token {
        Some(Token::Word(word)) if word.quote_style.is_none() => {
            Some(word.value.to_ascii_uppercase())
        }
        _ => None,
    };
    // The table's own name: the word after `TABLE`, past the optional `IF NOT EXISTS`.
    let mut at = match significant
        .iter()
        .position(|token| keyword(Some(token)).as_deref() == Some("TABLE"))
    {
        Some(at) => at + 1,
        None => return Vec::new(),
    };
    while matches!(
        keyword(significant.get(at)).as_deref(),
        Some("IF" | "NOT" | "EXISTS")
    ) {
        at += 1;
    }
    let Some(table) = word(significant.get(at)) else {
        return Vec::new();
    };
    at += 1;
    // A qualified name: the table is the part after the period.
    let table = if matches!(significant.get(at), Some(Token::Period)) {
        let Some(named) = word(significant.get(at + 1)) else {
            return Vec::new();
        };
        at += 2;
        named
    } else {
        table
    };
    if !matches!(significant.get(at), Some(Token::LParen)) {
        return Vec::new();
    }
    let mut declared = Vec::new();
    let mut depth = 0usize;
    let mut clause: Vec<&&Token> = Vec::new();
    let close = |clause: &[&&Token], declared: &mut Vec<(String, String, Option<String>, Option<String>)>| {
        let first = clause.first().copied();
        let Some(column) = word(first) else {
            return;
        };
        // A table-constraint clause names no column of its own.
        if matches!(
            keyword(first).as_deref(),
            Some("PRIMARY" | "UNIQUE" | "KEY" | "INDEX" | "CONSTRAINT" | "FOREIGN" | "CHECK"
                | "FULLTEXT" | "SPATIAL")
        ) {
            return;
        }
        let mut charset = None;
        let mut collation = None;
        for (position, token) in clause.iter().enumerate() {
            match keyword(Some(token)).as_deref() {
                Some("CHARACTER")
                    if keyword(clause.get(position + 1).copied()).as_deref() == Some("SET") =>
                {
                    charset = named_value(clause, position + 2);
                }
                Some("CHARSET") => charset = named_value(clause, position + 1),
                Some("COLLATE") => collation = named_value(clause, position + 1),
                _ => {}
            }
        }
        if charset.is_some() || collation.is_some() {
            declared.push((table.clone(), column, charset, collation));
        }
    };
    for token in significant.iter().skip(at) {
        match token {
            Token::LParen => {
                depth += 1;
                if depth == 1 {
                    continue;
                }
            }
            Token::RParen => {
                depth -= 1;
                if depth == 0 {
                    close(&clause, &mut declared);
                    break;
                }
            }
            Token::Comma if depth == 1 => {
                close(&clause, &mut declared);
                clause.clear();
                continue;
            }
            _ => {}
        }
        clause.push(token);
    }
    declared
}

/// The name a `CHARACTER SET`, `CHARSET` or `COLLATE` clause carries at `at`, past the `=` it
/// may be written with.
fn named_value(clause: &[&&Token], at: usize) -> Option<String> {
    let at = if matches!(clause.get(at), Some(Token::Eq)) {
        at + 1
    } else {
        at
    };
    match clause.get(at) {
        Some(Token::Word(word)) => Some(word.value.clone()),
        Some(Token::SingleQuotedString(value) | Token::DoubleQuotedString(value)) => {
            Some(value.clone())
        }
        _ => None,
    }
}

/// Create the catalog tables if this instance has not served one yet, in the system schema
/// they belong to — which is created with them, as a MySQL server comes with it.
fn ensure_catalogs(database: &InstanceDatabase, facts: &SessionFacts) -> Result<(), EngineError> {
    database.create_database(CATALOG_DATABASE)?;
    for schema in CATALOG_SCHEMA {
        database.execute_in(facts.connection_id, CATALOG_DATABASE, schema, &[])?;
    }
    Ok(())
}

/// Replace `SCHEMATA` with the databases this instance holds.
///
/// Exactly the databases a login may name — the one `DBName` created, the system schemas a
/// MySQL server comes with, and every one a client created since — so the catalog a client
/// enumerates and the set a connection is admitted against are the same set, and a database
/// the instance does not have is absent from both.
fn refresh_schemata(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    database.execute_in(
        facts.connection_id,
        CATALOG_DATABASE,
        "DELETE FROM SCHEMATA",
        &[],
    )?;
    let character_set = facts.setting("character_set_server");
    let collation = facts.setting("collation_server");
    let rows: Vec<Vec<Option<String>>> = database
        .database_names()
        .into_iter()
        .map(|name| {
            vec![
                Some(CATALOG_NAME.to_string()),
                Some(name),
                character_set.clone(),
                collation.clone(),
                None,
            ]
        })
        .collect();
    insert_rows(database, facts, "SCHEMATA", 5, &rows)
}

/// Replace `PROCESSLIST` with the sessions this endpoint holds open right now, which is what
/// the endpoint's own session registry holds.
fn refresh_processlist(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    database.execute_in(
        facts.connection_id,
        CATALOG_DATABASE,
        "DELETE FROM PROCESSLIST",
        &[],
    )?;
    let now = super::engine_time::now_epoch_seconds();
    let rows: Vec<Vec<Option<String>>> = facts
        .live_sessions()
        .into_iter()
        .map(|session| {
            let running = session.state == super::pg_sessions::STATE_ACTIVE;
            vec![
                Some(session.pid.to_string()),
                Some(session.user),
                Some(session.client_address),
                (!session.database.is_empty()).then_some(session.database),
                Some(
                    if running { COMMAND_QUERY } else { COMMAND_SLEEP }.to_string(),
                ),
                // "The time in seconds that the thread has been in its current state",
                // which the registry records the start of.
                Some(seconds_in_state(now, session.state_since).to_string()),
                Some(if running { STATE_EXECUTING } else { "" }.to_string()),
                (running && !session.query.is_empty()).then_some(session.query),
            ]
        })
        .collect();
    insert_rows(database, facts, "PROCESSLIST", 8, &rows)
}

/// How long a session has been in the state it is in, in whole seconds: what
/// `PROCESSLIST.TIME` reports. Never negative — a clock that moved backwards under a live
/// session reports the state as having just been entered.
fn seconds_in_state(now: f64, since: f64) -> i64 {
    ::aws_smithy_types::DateTime::from_secs_f64((now - since).max(0.0)).secs()
}

/// How many of the session's own tables the column catalog is rebuilt from: a bound on what
/// one statement that reads it costs, whatever the database holds.
const MAX_CATALOGUED_TABLES: usize = 256;

/// The character-data types a MySQL column carries a character set and a collation for.
///
/// Source: the MySQL Reference Manual's "The INFORMATION_SCHEMA COLUMNS Table" —
/// `CHARACTER_SET_NAME` and `COLLATION_NAME` are `NULL` "for numeric and other non-character
/// types" — and the "String Data Types" chapter's list of the character ones.
const CHARACTER_TYPES: [&str; 8] = [
    "char",
    "varchar",
    "tinytext",
    "text",
    "mediumtext",
    "longtext",
    "enum",
    "set",
];

/// Replace `COLUMNS` with the columns of the tables the session's own database holds.
///
/// The definitions come from the engine's own catalog — the `CREATE TABLE` statement it holds
/// for each table, read back and parsed — so what a client declared is what it reads back.
/// The character set and collation reported for a character column are the SERVER's, which is
/// what every table of an Amazon RDS instance is created with, and `NULL` for a column of any
/// other type, as MySQL reports.
fn refresh_columns(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    database.execute_in(
        facts.connection_id,
        CATALOG_DATABASE,
        "DELETE FROM COLUMNS",
        &[],
    )?;
    let held = database.execute_in(
        facts.connection_id,
        &facts.database,
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL \
         AND name NOT LIKE 'sqlite_%' AND name <> ?1 ORDER BY name LIMIT ?2",
        &[
            // This server's own bookkeeping is not a table of the client's.
            Some(COLUMN_CHARSET_TABLE.to_string()),
            Some(MAX_CATALOGUED_TABLES.to_string()),
        ],
    )?;
    let character_set = facts.setting("character_set_server");
    let collation = facts.setting("collation_server");
    let declared = recorded_charsets(database, facts);
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for row in &held.rows {
        let (Some(Cell::Text(table)), Some(Cell::Text(ddl))) = (row.first(), row.get(1)) else {
            continue;
        };
        for (position, column) in declared_columns(ddl).into_iter().enumerate() {
            let character = CHARACTER_TYPES
                .iter()
                .any(|known| column.data_type.eq_ignore_ascii_case(known));
            // The character set the column was DECLARED with, where it was declared with
            // one, else the server's own — which is what a column declared without one gets.
            let written = declared
                .iter()
                .find(|(named_table, named_column, _, _)| {
                    named_table.eq_ignore_ascii_case(table)
                        && named_column.eq_ignore_ascii_case(&column.name)
                })
                .map(|(_, _, charset, collation)| (charset.clone(), collation.clone()));
            let (column_charset, column_collation) = match written {
                Some((charset, named)) => {
                    let fallback = collation_of(&charset, &collation);
                    (charset.or_else(|| character_set.clone()), named.or(fallback))
                }
                None => (character_set.clone(), collation.clone()),
            };
            rows.push(vec![
                Some(facts.database.clone()),
                Some(table.clone()),
                Some(column.name),
                Some((position + 1).to_string()),
                Some(if column.nullable { "YES" } else { "NO" }.to_string()),
                Some(column.data_type),
                Some(column.column_type),
                character.then_some(column_charset).flatten(),
                character.then_some(column_collation).flatten(),
            ]);
        }
    }
    insert_rows(database, facts, "COLUMNS", 9, &rows)
}

/// The collation a column declared with a character set and no collation of its own carries:
/// that character set's default, else the server's own collation.
fn collation_of(charset: &Option<String>, server: &Option<String>) -> Option<String> {
    let charset = charset.as_deref()?;
    DEFAULT_COLLATIONS
        .iter()
        .find(|(known, _)| charset.eq_ignore_ascii_case(known))
        .map(|(_, collation)| (*collation).to_string())
        .or_else(|| server.clone())
}

/// The character set and collation each column of this database was DECLARED with, as
/// [`record_column_charsets`] recorded them. Empty where this instance has recorded none.
fn recorded_charsets(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Vec<(String, String, Option<String>, Option<String>)> {
    let Ok(held) = database.execute_in(
        facts.connection_id,
        RDS_DATABASE,
        "SELECT TABLE_NAME, COLUMN_NAME, CHARACTER_SET_NAME, COLLATION_NAME \
         FROM column_charsets WHERE TABLE_SCHEMA = ?",
        &[Some(facts.database.clone())],
    ) else {
        return Vec::new();
    };
    let text = |cell: Option<&Cell>| match cell {
        Some(Cell::Text(value)) => Some(value.clone()),
        _ => None,
    };
    held.rows
        .iter()
        .filter_map(|row| {
            Some((
                text(row.first())?,
                text(row.get(1))?,
                text(row.get(2)),
                text(row.get(3)),
            ))
        })
        .collect()
}

/// One column of a table, as the catalog reports it.
struct DeclaredColumn {
    name: String,
    /// `DATA_TYPE`: "the column data type … without any other type information", which is the
    /// type's own name.
    data_type: String,
    /// `COLUMN_TYPE`: "the column data type", as the declaration wrote it — the length and
    /// precision included.
    column_type: String,
    /// Whether the column admits `NULL`, which `IS_NULLABLE` reports as `YES` or `NO`.
    nullable: bool,
}

/// The columns the definition `ddl` declares, in the order it declared them.
fn declared_columns(ddl: &str) -> Vec<DeclaredColumn> {
    let Ok(mut statements) =
        ::sqlparser::parser::Parser::parse_sql(&::sqlparser::dialect::PostgreSqlDialect {}, ddl)
    else {
        return Vec::new();
    };
    if statements.len() != 1 {
        return Vec::new();
    }
    let ::sqlparser::ast::Statement::CreateTable(created) = statements.remove(0) else {
        return Vec::new();
    };
    created
        .columns
        .iter()
        .map(|column| {
            let column_type = column.data_type.to_string().to_ascii_lowercase();
            let data_type = column_type
                .split(['(', ' '])
                .next()
                .unwrap_or(&column_type)
                .to_string();
            let nullable = !column.options.iter().any(|option| {
                matches!(
                    option.option,
                    ::sqlparser::ast::ColumnOption::NotNull
                        | ::sqlparser::ast::ColumnOption::Unique { is_primary: true, .. }
                )
            });
            DeclaredColumn {
                name: column.name.value.clone(),
                data_type,
                column_type,
                nullable,
            }
        })
        .collect()
}

/// [`super::pg_server::insert_catalog_rows`] into a catalog of this server's system schema,
/// which every catalog of this module lives in.
fn insert_rows(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    table: &str,
    columns: usize,
    rows: &[Vec<Option<String>>],
) -> Result<(), EngineError> {
    super::pg_server::insert_catalog_rows(
        database,
        facts,
        CATALOG_DATABASE,
        table,
        columns,
        rows,
    )
}

/// The configuration table the Amazon RDS procedures read and write, as RDS keeps it:
/// `mysql.rds_configuration`, one row per configurable value.
const RDS_CONFIGURATION_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS rds_configuration (\
       name text PRIMARY KEY, value text, description text)";

/// The configuration an Amazon RDS MySQL instance carries, as `mysql.rds_show_configuration`
/// reports it: the name, the value and the description of each row.
///
/// Source: the Amazon RDS User Guide's "mysql.rds_show_configuration" and
/// "mysql.rds_set_configuration", which list exactly these names and print these
/// descriptions. The DEFAULT of each is the value the guide documents: binlog retention is
/// unset until a call sets it ("the default is NULL, meaning binary logs are not retained"),
/// and both replication delays start at zero.
const RDS_CONFIGURATION: [(&str, Option<&str>, &str); 3] = [
    (
        "binlog retention hours",
        None,
        "binlog retention hours specifies the duration in hours before binary logs are \
         automatically deleted.",
    ),
    (
        "source delay",
        Some("0"),
        "source delay specifies replication delay in seconds between current instance and \
         its master.",
    ),
    (
        "target delay",
        Some("0"),
        "target delay specifies replication delay in seconds between current instance and \
         its target.",
    ),
];

/// Serve the statements an Amazon RDS MySQL server answers that are the SERVER's own rather
/// than the engine's: the `mysql.rds_*` stored procedures RDS ships, and `KILL`.
///
/// `None` means the statement is not one of them, and is the engine's to run.
pub fn server_statement(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    words: &[String],
    tokens: &[Token],
) -> Result<Option<StatementAnswer>, EngineError> {
    if facts.wire != EngineWire::MySql {
        return Ok(None);
    }
    match words.first().map(String::as_str).unwrap_or_default() {
        "CALL" => call_statement(database, facts, tokens),
        "KILL" => kill_statement(facts, tokens).map(Some),
        _ => Ok(None),
    }
}

/// `CALL [mysql.]<procedure>[(<arguments>)]`: the procedure the statement names, served from
/// this server's own state.
fn call_statement(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    tokens: &[Token],
) -> Result<Option<StatementAnswer>, EngineError> {
    let Some((procedure, arguments)) = called_procedure(tokens) else {
        return Ok(None);
    };
    match procedure.as_str() {
        "rds_show_configuration" => show_configuration(database, facts).map(Some),
        "rds_set_configuration" => {
            set_configuration(database, facts, &arguments).map(Some)
        }
        // "mysql.rds_kill — ends a connection", taking the connection's own id, which is
        // what `KILL` does on a server whose users may hold the privilege for it.
        "rds_kill" => kill(facts, arguments.first().cloned().flatten().as_deref()).map(Some),
        // A procedure this server does not have: MySQL's own refusal for one, which names
        // the schema it was looked for in.
        _ => Err(EngineError::new(
            EngineErrorKind::UndefinedObject,
            format!("PROCEDURE {RDS_DATABASE}.{procedure} does not exist"),
        )
        .with_mysql_error(1305, "42000")),
    }
}

/// The procedure a `CALL` names and the arguments it passes, or `None` for a statement that
/// names none.
///
/// The procedure may be written with the schema it lives in (`mysql.rds_kill`) or without
/// it, and its arguments are the literals, numbers and `NULL`s inside its parentheses — a
/// procedure of this server takes nothing else. An argument that is `NULL` is `None`, which
/// is the value that CLEARS a configuration.
fn called_procedure(tokens: &[Token]) -> Option<(String, Vec<Option<String>>)> {
    let mut name = None;
    let mut arguments: Vec<Option<String>> = Vec::new();
    let mut inside = false;
    for token in tokens.iter().skip(1) {
        match token {
            Token::LParen => inside = true,
            Token::RParen => inside = false,
            Token::Word(word) if !inside => name = Some(word.value.to_ascii_lowercase()),
            Token::Word(word) if inside => {
                if word.quote_style.is_none() && word.value.eq_ignore_ascii_case("null") {
                    arguments.push(None);
                } else {
                    arguments.push(Some(word.value.clone()));
                }
            }
            Token::SingleQuotedString(value) | Token::DoubleQuotedString(value) if inside => {
                arguments.push(Some(value.clone()));
            }
            Token::Number(value, _) if inside => arguments.push(Some(value.clone())),
            _ => {}
        }
    }
    name.map(|name| (name, arguments))
}

/// `CALL mysql.rds_show_configuration`: one row per configurable value, with the value this
/// instance carries for it.
fn show_configuration(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<StatementAnswer, EngineError> {
    ensure_configuration(database, facts)?;
    let held = database.execute_in(
        facts.connection_id,
        RDS_DATABASE,
        "SELECT name, value, description FROM rds_configuration ORDER BY name",
        &[],
    )?;
    Ok(StatementAnswer::new(
        "CALL",
        ResultSet::of_rows(
            vec![column("name"), column("value"), column("description")],
            held.rows,
        ),
    ))
}

/// One column of a result set this server builds itself, carrying the text its value is.
fn column(name: &str) -> ColumnInfo {
    ColumnInfo {
        name: name.to_string(),
        declared_type: Some("text".to_string()),
    }
}

/// `CALL mysql.rds_set_configuration('<name>', <value>)`: the value this instance carries
/// for that configuration from now on, which a later `rds_show_configuration` reports.
fn set_configuration(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    arguments: &[Option<String>],
) -> Result<StatementAnswer, EngineError> {
    ensure_configuration(database, facts)?;
    let Some(Some(name)) = arguments.first() else {
        return Err(invalid_configuration_name());
    };
    let value = arguments.get(1).cloned().flatten();
    if !RDS_CONFIGURATION
        .iter()
        .any(|(known, ..)| known.eq_ignore_ascii_case(name))
    {
        return Err(invalid_configuration_name());
    }
    let changed = database.execute_in(
        facts.connection_id,
        RDS_DATABASE,
        "UPDATE rds_configuration SET value = ? WHERE lower(name) = lower(?)",
        &[value, Some(name.clone())],
    )?;
    Ok(StatementAnswer::new(
        "CALL",
        ResultSet::changed(changed.affected, 0),
    ))
}

/// What the procedure answers a configuration name it does not have: the exception it
/// signals, which names nothing but the name's invalidity.
fn invalid_configuration_name() -> EngineError {
    EngineError::new(
        EngineErrorKind::DataError,
        "Invalid configuration name".to_string(),
    )
    .with_mysql_error(1644, "45000")
}

/// Create the configuration table if this instance has not held it yet, with the row each
/// configurable value comes with.
fn ensure_configuration(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<(), EngineError> {
    database.create_database(RDS_DATABASE)?;
    database.execute_in(
        facts.connection_id,
        RDS_DATABASE,
        RDS_CONFIGURATION_SCHEMA,
        &[],
    )?;
    for (name, value, description) in RDS_CONFIGURATION {
        database.execute_in(
            facts.connection_id,
            RDS_DATABASE,
            "INSERT OR IGNORE INTO rds_configuration (name, value, description) \
             VALUES (?, ?, ?)",
            &[
                Some(name.to_string()),
                value.map(ToString::to_string),
                Some(description.to_string()),
            ],
        )?;
    }
    Ok(())
}

/// `KILL [CONNECTION | QUERY] <id>`: the session the statement names is ended.
fn kill_statement(facts: &SessionFacts, tokens: &[Token]) -> Result<StatementAnswer, EngineError> {
    let named = tokens.iter().skip(1).find_map(|token| match token {
        Token::Number(value, _) => Some(value.clone()),
        _ => None,
    });
    kill(facts, named.as_deref())
}

/// End the session `named` holds the connection id of.
///
/// A client cannot end a connection this endpoint does not hold: MySQL answers that with
/// `ER_NO_SUCH_THREAD`, which names the id it was asked about. The killed session is not
/// answered at all — its connection is closed, which is what its client reports as the
/// server going away mid-statement ([`super::pg_sessions::SessionRegistry::kill`]).
fn kill(facts: &SessionFacts, named: Option<&str>) -> Result<StatementAnswer, EngineError> {
    let Some(pid) = named.and_then(|named| named.trim().parse::<u32>().ok()) else {
        return Err(unknown_thread(named.unwrap_or_default()));
    };
    if !facts.sessions.kill(pid) {
        return Err(unknown_thread(&pid.to_string()));
    }
    Ok(StatementAnswer::new("KILL", ResultSet::changed(0, 0)))
}

/// `ER_NO_SUCH_THREAD`: "Unknown thread id: <id>".
fn unknown_thread(named: &str) -> EngineError {
    EngineError::new(
        EngineErrorKind::UndefinedObject,
        format!("Unknown thread id: {named}"),
    )
    .with_mysql_error(1094, "HY000")
}

/// The refusal `error`, worded as a MySQL server words a table the statement names that the
/// database does not have.
///
/// The embedded engine says `no such table: <name>`, which is its own sentence; MySQL's is
/// `Table '<database>.<table>' doesn't exist` — the database the statement ran against and
/// the table it named, which is the shape a client's own parser reads the two out of. Every
/// other refusal keeps the sentence it already had.
#[must_use]
pub fn as_mysql_words_it(wire: EngineWire, target: &str, error: EngineError) -> EngineError {
    if wire != EngineWire::MySql {
        return error;
    }
    let Some(table) = error.message.strip_prefix("no such table: ") else {
        return error;
    };
    // The engine may name the table qualified by the schema it looked in, which MySQL
    // reports as the database the statement ran against.
    let table = table.rsplit('.').next().unwrap_or(table);
    EngineError::new(
        error.kind,
        format!("Table '{target}.{table}' doesn't exist"),
    )
    .with_mysql_error(1146, "42S02")
}
