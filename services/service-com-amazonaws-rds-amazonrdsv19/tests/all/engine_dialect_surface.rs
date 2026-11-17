// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ENGINE DIALECT each published endpoint speaks: the statements
//! only one engine family defines, the catalogs each of them serves, and the words each of
//! them refuses a row or a privilege in.
//!
//! The rules under test:
//!
//! * a statement written in a form the session's own dialect does not define is refused as
//!   that engine's parse error — the `@@<variable>` reference is the MySQL family's and a
//!   PostgreSQL endpoint reads no such form — so a client can tell which engine it is talking
//!   to from the answer;
//! * each endpoint evaluates the DATE AND TIME functions its own family defines, with the
//!   fractional-seconds precision a call asks for, and defines none of the other family's;
//! * a MySQL endpoint declares the table constraints MySQL's own DDL declares (a named
//!   `UNIQUE KEY`, a column character set) and serves the system catalogs a MySQL server
//!   always has (`information_schema.SCHEMATA`, `information_schema.PROCESSLIST`);
//! * a refused row names the CONSTRAINT it violated the way the client's own engine names it:
//!   MySQL quotes the duplicate value and the index, PostgreSQL carries the constraint's name
//!   in its own field of the error;
//! * a MySQL endpoint answers the Amazon RDS stored procedures that read and write the
//!   instance's binlog configuration.
//!
//! Every table below spans what its rule generalises over — the dialect, the function, the
//! statement form and the constraint kind — rather than one recorded statement.

use crate::db_instance_engine_endpoint::{
    available_instance, endpoint_of, mysql_login, postgres_login, MySqlSession, PostgresSession,
    Rows, Transport,
};
use crate::db_instances::{call, emulator, expect_ok, text, wait_for_settle, REGION};
use smithy_server_runtime::MultiServiceRouter;

/// The master user every instance here is created with, and its test password.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";
/// The `DBName` every instance here is created with.
const DATABASE: &str = "appdb";

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// A MySQL-family instance of `engine`, with a session open on its endpoint.
async fn mysql_instance(
    identifier: &str,
    engine: &str,
    version: &str,
) -> (MultiServiceRouter, MySqlSession) {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        identifier,
        engine,
        &params!["EngineVersion" => version],
    )
    .await;
    let session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    (router, session)
}

/// A PostgreSQL instance with a session open on its endpoint.
async fn postgres_instance(identifier: &str) -> (MultiServiceRouter, PostgresSession) {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        identifier,
        "postgres",
        &params!["EngineVersion" => "16.4"],
    )
    .await;
    let session = postgres_login(
        &address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    (router, session)
}

/// The rows a statement answered with.
async fn rows(session: &mut MySqlSession, sql: &str) -> Rows {
    session
        .query(sql)
        .await
        .unwrap_or_else(|refusal| panic!("{sql:?} must run, got {refusal:?}"))
}

#[tokio::test]
async fn a_system_variable_reference_is_read_by_the_family_whose_dialect_has_it() {
    let (_router, mut mysql) = mysql_instance("emu-dialect-my", "mysql", "8.0.39").await;
    let (_pg_router, mut postgres) = postgres_instance("emu-dialect-pg").await;

    // The MySQL family reads the `@@` reference and answers the variable's own value; a
    // boolean one answers the integer a `@@` reference carries.
    for (case, statement, expected) in [
        ("the engine's identity", "SELECT @@version_comment", None),
        ("a boolean variable", "SELECT @@read_only", Some("0")),
        (
            "a boolean variable in the global scope",
            "SELECT @@global.read_only",
            Some("0"),
        ),
    ] {
        let read = rows(&mut mysql, statement).await;
        let answered = read.only().map(ToString::to_string);
        match expected {
            Some(expected) => assert_eq!(
                answered.as_deref(),
                Some(expected),
                "case {case:?}: expected {expected:?}, got {read:?}"
            ),
            // The engine's own description of itself: a value the server carries, whatever
            // it is, rather than nothing at all.
            None => assert!(
                answered.is_some_and(|value| !value.is_empty()),
                "case {case:?}: the variable must carry the server's own value, got {read:?}"
            ),
        }
    }

    // The same references on a PostgreSQL endpoint are a parse error: its grammar has no
    // such form, which is how a client tells the two engines apart.
    for (case, statement, offending) in [
        (
            "the engine's identity",
            "SELECT @@version_comment",
            "@@version_comment",
        ),
        ("a boolean variable", "SELECT @@read_only", "@@read_only"),
        (
            "a scoped reference",
            "SELECT @@global.read_only",
            "@@global",
        ),
    ] {
        let refusal = postgres
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            (
                "42601",
                format!("syntax error at or near \"{offending}\"").as_str()
            ),
            "case {case:?}: expected the PostgreSQL parse error, got {refusal:?}"
        );
    }

    // PostgreSQL's OWN `@@` operator is not that form: a statement written in its grammar is
    // the statement it is, whichever operand it carries, and the engine answers for it.
    for (case, statement) in [
        ("a literal on the left", "SELECT 'a' @@ 'b'"),
        (
            "a column on the left and a call on the right",
            "SELECT 1 FROM pg_database WHERE datname @@ to_tsquery('x')",
        ),
        ("a parenthesised operand", "SELECT (1) @@ 'b'"),
    ] {
        let refusal = postgres
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: this engine has no such operator"));
        assert!(
            !refusal.message.starts_with("syntax error at or near \"@@"),
            "case {case:?}: the operator must not be read as a system-variable reference, \
             got {refusal:?}"
        );
    }
}

#[tokio::test]
async fn each_endpoint_evaluates_the_time_functions_its_own_family_defines() {
    let (_router, mut mysql) = mysql_instance("emu-time-my", "mysql", "8.0.39").await;
    let (_pg_router, mut postgres) = postgres_instance("emu-time-pg").await;

    // The MySQL date and time functions, each answering a value of the shape its own
    // documentation gives it. The shapes are asserted rather than the instant: the value is
    // the clock's, and what the rule is about is that the statement RUNS and answers a
    // timestamp of that form.
    for (case, statement, shape) in [
        ("the UTC timestamp", "SELECT UTC_TIMESTAMP()", Shape::Seconds),
        (
            "the UTC timestamp to microseconds",
            "SELECT UTC_TIMESTAMP(6)",
            Shape::Microseconds,
        ),
        (
            "the UTC timestamp to milliseconds",
            "SELECT UTC_TIMESTAMP(3)",
            Shape::Milliseconds,
        ),
        ("the session's timestamp", "SELECT NOW()", Shape::Seconds),
        ("the server's timestamp", "SELECT SYSDATE()", Shape::Seconds),
        ("the UTC date", "SELECT UTC_DATE()", Shape::Date),
        ("the current date", "SELECT CURDATE()", Shape::Date),
        ("the UTC time of day", "SELECT UTC_TIME()", Shape::Clock),
        ("the current time of day", "SELECT CURTIME()", Shape::Clock),
        (
            "the epoch seconds",
            "SELECT UNIX_TIMESTAMP()",
            Shape::Digits,
        ),
    ] {
        let read = rows(&mut mysql, statement).await;
        let value = read.only().unwrap_or_default().to_string();
        assert!(
            shape.matches(&value),
            "case {case:?}: {statement:?} must answer a {shape:?} value, got {value:?}"
        );
        // And under the TYPE its own engine reports for it, which is what a typed driver
        // decodes it as: the `MYSQL_TYPE_*` code of the protocol's column definition.
        assert_eq!(
            read.type_oids,
            vec![shape.mysql_type_code()],
            "case {case:?}: {statement:?} must report the type a driver decodes by, got {:?}",
            read.type_oids
        );
    }

    // A function only the MySQL family defines is undefined on a PostgreSQL endpoint, and
    // the other way round — each refused in its own engine's words for a function it does
    // not have.
    for (case, statement, expected) in [
        (
            "the MySQL UTC timestamp",
            "SELECT UTC_TIMESTAMP(6)",
            "function UTC_TIMESTAMP() does not exist",
        ),
        (
            "the MySQL current date",
            "SELECT CURDATE()",
            "function CURDATE() does not exist",
        ),
        (
            "the MySQL epoch seconds",
            "SELECT UNIX_TIMESTAMP()",
            "function UNIX_TIMESTAMP() does not exist",
        ),
    ] {
        let refusal = postgres
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            ("42883", expected),
            "case {case:?}: expected PostgreSQL's undefined-function refusal, got {refusal:?}"
        );
    }
    for (case, statement, expected) in [
        (
            "a PostgreSQL-only clock",
            "SELECT clock_timestamp()",
            "FUNCTION appdb.clock_timestamp does not exist",
        ),
        (
            "a PostgreSQL server function",
            "SELECT current_setting('server_version')",
            "FUNCTION appdb.current_setting does not exist",
        ),
        (
            "a PostgreSQL catalog function",
            "SELECT pg_backend_pid()",
            "FUNCTION appdb.pg_backend_pid does not exist",
        ),
    ] {
        let refusal = mysql
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            ("1305", expected),
            "case {case:?}: expected MySQL's undefined-function refusal, got {refusal:?}"
        );
    }

    // `length` is defined by BOTH families and measures a different thing in each: a
    // PostgreSQL session counts characters where a MySQL one counts bytes, and
    // `octet_length` is the bytes on either.
    let counted = postgres
        .query("SELECT length('😀'), octet_length('😀')")
        .await
        .expect("both lengths must be readable");
    assert_eq!(
        counted.rows,
        vec![vec![Some("1".to_string()), Some("4".to_string())]],
        "PostgreSQL's length counts characters and octet_length bytes, got {counted:?}"
    );

    // PostgreSQL's own: `now()` and the timestamps of the transaction and the statement,
    // each rendering the offset the server runs at.
    for (case, statement) in [
        ("the transaction's timestamp", "SELECT now()"),
        ("the statement's timestamp", "SELECT statement_timestamp()"),
        ("the wall clock", "SELECT clock_timestamp()"),
    ] {
        let read = postgres
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run, got {refusal:?}"));
        let value = read.only().unwrap_or_default().to_string();
        assert!(
            Shape::Instant.matches(&value),
            "case {case:?}: expected an instant with its offset, got {value:?}"
        );
    }
}

/// The SHAPE a time function's value has, which is what each of them is asserted against:
/// the instant itself comes from the clock.
#[derive(Debug, Clone, Copy)]
enum Shape {
    /// `YYYY-MM-DD HH:MM:SS`.
    Seconds,
    /// The same, with three digits of the second.
    Milliseconds,
    /// The same, with six.
    Microseconds,
    /// The same, with six digits and the UTC offset PostgreSQL prints.
    Instant,
    /// `YYYY-MM-DD`.
    Date,
    /// `HH:MM:SS`, with or without a fraction.
    Clock,
    /// Whole digits.
    Digits,
}

impl Shape {
    /// The `MYSQL_TYPE_*` code the protocol's column definition carries for a value of this
    /// shape: `DATETIME` (12) for a timestamp, `DATE` (10), `TIME` (11), and `LONGLONG` (8)
    /// for the epoch seconds, which are an integer.
    fn mysql_type_code(self) -> i32 {
        match self {
            Self::Seconds | Self::Milliseconds | Self::Microseconds | Self::Instant => 12,
            Self::Date => 10,
            Self::Clock => 11,
            Self::Digits => 8,
        }
    }

    /// Whether `value` is of this shape.
    fn matches(self, value: &str) -> bool {
        let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
        let (date, clock) = match value.split_once(' ') {
            Some((date, clock)) => (date, clock),
            None => ("", value),
        };
        let date_is_a_date = |text: &str| {
            let parts: Vec<&str> = text.split('-').collect();
            matches!(parts.as_slice(), [year, month, day]
                if digits(year) && year.len() == 4 && digits(month) && digits(day))
        };
        let (seconds, fraction) = clock
            .trim_end_matches("+00")
            .split_once('.')
            .unwrap_or((clock.trim_end_matches("+00"), ""));
        let clock_is_a_clock = seconds.split(':').count() == 3
            && seconds.split(':').all(|part| digits(part) && part.len() == 2);
        match self {
            Self::Digits => digits(value),
            Self::Date => date_is_a_date(value),
            Self::Clock => clock_is_a_clock,
            Self::Seconds => date_is_a_date(date) && clock_is_a_clock && fraction.is_empty(),
            Self::Milliseconds => {
                date_is_a_date(date) && clock_is_a_clock && fraction.len() == 3
            }
            Self::Microseconds => {
                date_is_a_date(date) && clock_is_a_clock && fraction.len() == 6
            }
            Self::Instant => {
                date_is_a_date(date)
                    && clock_is_a_clock
                    && fraction.len() == 6
                    && value.ends_with("+00")
            }
        }
    }
}

#[tokio::test]
async fn a_mysql_endpoint_declares_the_table_constraints_mysqls_own_ddl_declares() {
    let (_router, mut session) = mysql_instance("emu-ddl-my", "mysql", "8.0.39").await;

    // The DDL forms MySQL writes a table constraint and a column's character set in, each
    // of which the engine declares differently.
    for (case, statement) in [
        (
            "a named unique key over one column",
            "CREATE TABLE people (id int PRIMARY KEY, email varchar(120), \
             UNIQUE KEY people_email_uq (email))",
        ),
        (
            "a named unique key over several columns",
            "CREATE TABLE pairs (left_id int, right_id int, \
             UNIQUE KEY pairs_uq (left_id, right_id))",
        ),
        (
            "a unique index written with INDEX",
            "CREATE TABLE tags (id int PRIMARY KEY, label varchar(32), \
             UNIQUE INDEX tags_label_uq (label))",
        ),
        (
            "a plain index, which constrains no row",
            "CREATE TABLE events (id int PRIMARY KEY, at_hour int, KEY events_hour (at_hour))",
        ),
        (
            "a column character set and collation",
            "CREATE TABLE notes (id int PRIMARY KEY, \
             body varchar(100) CHARACTER SET utf8mb4 COLLATE utf8mb4_bin NOT NULL)",
        ),
        (
            "a constraint named with CONSTRAINT",
            "CREATE TABLE badges (id int PRIMARY KEY, code varchar(16), \
             CONSTRAINT badges_code_uq UNIQUE (code))",
        ),
        (
            "a table with options after its column list",
            "CREATE TABLE ledger (id int PRIMARY KEY, memo varchar(40), \
             UNIQUE KEY ledger_memo_uq (memo)) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        ),
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run, got {refusal:?}"));
    }

    // A round trip through a table whose column carries a character set: what the client
    // wrote is what it reads back, multi-byte characters included — counted as CHARACTERS
    // by `CHAR_LENGTH` and as BYTES by MySQL's own `LENGTH`.
    rows(
        &mut session,
        "INSERT INTO notes (id, body) VALUES (1, 'Ada Lovelace — ✓')",
    )
    .await;
    let held = rows(&mut session, "SELECT body FROM notes WHERE id = 1").await;
    assert_eq!(
        held.only(),
        Some("Ada Lovelace — ✓"),
        "the row read back must be the row written, got {held:?}"
    );
    let measured = rows(
        &mut session,
        "SELECT CHAR_LENGTH('😀'), LENGTH('😀'), OCTET_LENGTH('😀')",
    )
    .await;
    assert_eq!(
        measured.rows,
        vec![vec![
            Some("1".to_string()),
            Some("4".to_string()),
            Some("4".to_string())
        ]],
        "a 4-byte character is one character and four bytes, got {measured:?}"
    );

    // Each unique rule REFUSES a duplicate, named as MySQL names it: the duplicate value
    // and the index that holds it, which on this engine's version is qualified by its
    // table.
    rows(
        &mut session,
        "INSERT INTO people (id, email) VALUES (1, 'ada@example.com')",
    )
    .await;
    for (case, statement, expected) in [
        (
            "a duplicate primary key",
            "INSERT INTO people (id, email) VALUES (1, 'grace@example.com')",
            "Duplicate entry '1' for key 'people.PRIMARY'",
        ),
        (
            "a duplicate on a named unique key",
            "INSERT INTO people (id, email) VALUES (2, 'ada@example.com')",
            "Duplicate entry 'ada@example.com' for key 'people.people_email_uq'",
        ),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            ("1062", expected),
            "case {case:?}: expected MySQL's duplicate-entry refusal, got {refusal:?}"
        );
    }

    // A FOREIGN KEY refusal names the constraint the row broke, inside the parentheses
    // MySQL quotes the whole rule in.
    for statement in [
        "CREATE TABLE parents (id int PRIMARY KEY)",
        "CREATE TABLE children (id int PRIMARY KEY, parent_id int, \
         CONSTRAINT children_parent_fk FOREIGN KEY (parent_id) REFERENCES parents (id))",
        "INSERT INTO parents (id) VALUES (1)",
        "INSERT INTO children (id, parent_id) VALUES (1, 1)",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }
    for (case, statement, expected) in [
        (
            "a child row with no parent",
            "INSERT INTO children (id, parent_id) VALUES (2, 99)",
            (
                "1452",
                "Cannot add or update a child row: a foreign key constraint fails \
                 (`appdb`.`children`, CONSTRAINT `children_parent_fk` FOREIGN KEY \
                 (`parent_id`) REFERENCES `parents` (`id`))",
            ),
        ),
        (
            "a parent row another table still refers to",
            "DELETE FROM parents WHERE id = 1",
            (
                "1451",
                "Cannot delete or update a parent row: a foreign key constraint fails \
                 (`appdb`.`children`, CONSTRAINT `children_parent_fk` FOREIGN KEY \
                 (`parent_id`) REFERENCES `parents` (`id`))",
            ),
        ),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            expected,
            "case {case:?}: expected MySQL's foreign-key refusal, got {refusal:?}"
        );
    }
}

#[tokio::test]
async fn a_mariadb_endpoint_names_itself_and_its_indexes_as_mariadb_does() {
    let (_router, mut session) = mysql_instance("emu-ddl-maria", "mariadb", "11.4.4").await;

    // The server's own version string carries the engine's name, which is how a client
    // tells MariaDB from MySQL over the one wire they share.
    assert!(
        session.server_version.contains("MariaDB"),
        "the greeting must name the engine, got {:?}",
        session.server_version
    );
    for statement in ["SELECT VERSION()", "SELECT @@version"] {
        let read = rows(&mut session, statement).await;
        let value = read.only().unwrap_or_default().to_string();
        assert!(
            value.starts_with("11.4.4") && value.contains("MariaDB"),
            "{statement:?} must report the instance's version and the engine, got {value:?}"
        );
    }

    // MariaDB names the index alone in a duplicate-key refusal, where MySQL 8 qualifies it
    // with the table.
    rows(
        &mut session,
        "CREATE TABLE accounts (id int PRIMARY KEY, label varchar(32))",
    )
    .await;
    rows(
        &mut session,
        "INSERT INTO accounts (id, label) VALUES (1, 'first')",
    )
    .await;
    let refusal = session
        .query("INSERT INTO accounts (id, label) VALUES (1, 'second')")
        .await
        .expect_err("a duplicate primary key must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("1062", "Duplicate entry '1' for key 'PRIMARY'"),
        "expected MariaDB's duplicate-entry refusal, got {refusal:?}"
    );

    // The master user is not a superuser on a managed instance, so a server-wide variable
    // write is refused however the session spells it.
    for (case, statement) in [
        ("a global write", "SET GLOBAL max_connections = 10"),
        ("a persisted write", "SET PERSIST max_connections = 10"),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            (
                "1227",
                "Access denied; you need (at least one of) the SUPER or \
                 SYSTEM_VARIABLES_ADMIN privilege(s) for this operation"
            ),
            "case {case:?}: expected the privilege refusal, got {refusal:?}"
        );
    }
}

#[tokio::test]
async fn a_mysql_endpoint_serves_the_system_catalogs_a_mysql_server_always_has() {
    let (_router, mut session) = mysql_instance("emu-catalogs-my", "mysql", "8.0.39").await;

    // Every database the instance holds is in `SCHEMATA`: the one it was created with and
    // the system schemas a MySQL server comes with.
    let schemata = rows(
        &mut session,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA ORDER BY SCHEMA_NAME",
    )
    .await;
    let held: Vec<String> = schemata
        .rows
        .iter()
        .filter_map(|row| row.first().cloned().flatten())
        .collect();
    for expected in [DATABASE, "information_schema", "mysql", "performance_schema", "sys"] {
        assert!(
            held.iter().any(|name| name == expected),
            "the catalog must list {expected:?}, got {held:?}"
        );
    }
    // A database a client creates is there for the next statement that enumerates them.
    rows(&mut session, "CREATE DATABASE reporting").await;
    let after = rows(
        &mut session,
        "SELECT SCHEMA_NAME FROM information_schema.SCHEMATA WHERE SCHEMA_NAME = 'reporting'",
    )
    .await;
    assert_eq!(
        after.only(),
        Some("reporting"),
        "a database the client created must be in the catalog, got {after:?}"
    );
    // A table no MySQL server has is refused in MySQL's own words for one.
    let refusal = session
        .query("SELECT * FROM information_schema.NOT_A_CATALOG")
        .await
        .expect_err("a table the server does not have must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        (
            "1146",
            "Table 'information_schema.NOT_A_CATALOG' doesn't exist"
        ),
        "expected MySQL's missing-table refusal, got {refusal:?}"
    );

    // The COLUMNS catalog reports the columns of the session's own tables, as the client
    // declared them, with the SERVER's own character set on each character column — the one
    // the instance runs with, which is where the value comes from.
    let server_character_set = rows(&mut session, "SELECT @@character_set_server")
        .await
        .only()
        .unwrap_or_default()
        .to_string();
    rows(
        &mut session,
        "CREATE TABLE invoices (id int PRIMARY KEY, memo varchar(40) NOT NULL, total int)",
    )
    .await;
    let columns = rows(
        &mut session,
        "SELECT COLUMN_NAME, ORDINAL_POSITION, IS_NULLABLE, DATA_TYPE, COLUMN_TYPE, \
         CHARACTER_SET_NAME FROM information_schema.COLUMNS \
         WHERE TABLE_NAME = 'invoices' ORDER BY ORDINAL_POSITION",
    )
    .await;
    assert_eq!(
        columns.rows,
        vec![
            vec![
                Some("id".to_string()),
                Some("1".to_string()),
                Some("NO".to_string()),
                Some("int".to_string()),
                Some("int".to_string()),
                None,
            ],
            vec![
                Some("memo".to_string()),
                Some("2".to_string()),
                Some("NO".to_string()),
                Some("varchar".to_string()),
                Some("varchar(40)".to_string()),
                Some(server_character_set),
            ],
            vec![
                Some("total".to_string()),
                Some("3".to_string()),
                Some("YES".to_string()),
                Some("int".to_string()),
                Some("int".to_string()),
                None,
            ],
        ],
        "the catalog must report the table's own columns, got {columns:?}"
    );

    // A column DECLARED with a character set carries that one rather than the server's, and
    // `utf8` is reported under the name MySQL renamed it to.
    for (case, statement, table, expected) in [
        (
            "a column declared utf8mb4",
            "CREATE TABLE notes4 (id int PRIMARY KEY, body text CHARACTER SET utf8mb4)",
            "notes4",
            ("utf8mb4", "utf8mb4_0900_ai_ci"),
        ),
        (
            "a column declared with the old name of the 3-byte set",
            "CREATE TABLE notes3 (id int PRIMARY KEY, body text CHARACTER SET utf8)",
            "notes3",
            ("utf8mb3", "utf8_general_ci"),
        ),
        (
            "a column declared with a collation of its own",
            "CREATE TABLE notes5 (id int PRIMARY KEY, \
             body text CHARACTER SET utf8mb4 COLLATE utf8mb4_bin)",
            "notes5",
            ("utf8mb4", "utf8mb4_bin"),
        ),
    ] {
        rows(&mut session, statement).await;
        let read = rows(
            &mut session,
            &format!(
                "SELECT CHARACTER_SET_NAME, COLLATION_NAME FROM information_schema.COLUMNS \
                 WHERE TABLE_NAME = '{table}' AND COLUMN_NAME = 'body'"
            ),
        )
        .await;
        assert_eq!(
            read.rows,
            vec![vec![
                Some(expected.0.to_string()),
                Some(expected.1.to_string())
            ]],
            "case {case:?}: the column must carry the character set it was declared with, \
             got {read:?}"
        );
    }

    // This session is in the process list, with the connection id it was handed.
    let connection = rows(&mut session, "SELECT CONNECTION_ID()").await;
    let id = connection.only().unwrap_or_default().to_string();
    let listed = rows(
        &mut session,
        &format!(
            "SELECT ID, USER, DB, COMMAND FROM information_schema.PROCESSLIST WHERE ID = {id}"
        ),
    )
    .await;
    assert_eq!(
        listed.rows,
        vec![vec![
            Some(id.clone()),
            Some(MASTER_USER.to_string()),
            Some(DATABASE.to_string()),
            Some("Query".to_string()),
        ]],
        "the process list must hold this session, got {listed:?}"
    );
    // A connection id nothing is connected under cannot be ended.
    let refusal = session
        .query("KILL 999999")
        .await
        .expect_err("a thread id nothing holds must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("1094", "Unknown thread id: 999999"),
        "expected MySQL's unknown-thread refusal, got {refusal:?}"
    );
}

#[tokio::test]
async fn a_mysql_endpoint_answers_the_rds_configuration_procedures() {
    let (_router, mut session) = mysql_instance("emu-rds-procs", "mysql", "8.0.39").await;

    // The configuration a fresh instance carries: the rows RDS publishes, with the
    // retention unset until a call sets it.
    let shown = rows(&mut session, "CALL mysql.rds_show_configuration").await;
    let held: Vec<(String, Option<String>)> = shown
        .rows
        .iter()
        .map(|row| {
            (
                row.first().cloned().flatten().unwrap_or_default(),
                row.get(1).cloned().flatten(),
            )
        })
        .collect();
    assert_eq!(
        held,
        vec![
            ("binlog retention hours".to_string(), None),
            ("source delay".to_string(), Some("0".to_string())),
            ("target delay".to_string(), Some("0".to_string())),
        ],
        "the procedure must report the instance's own configuration, got {shown:?}"
    );

    // Each value a call sets is the value a later call reports, and a call that sets NULL
    // clears it again.
    for (case, statement, name, expected) in [
        (
            "a retention in hours",
            "CALL mysql.rds_set_configuration('binlog retention hours', 24)",
            "binlog retention hours",
            Some("24"),
        ),
        (
            "another retention",
            "CALL mysql.rds_set_configuration('binlog retention hours', 168)",
            "binlog retention hours",
            Some("168"),
        ),
        (
            "a replication delay",
            "CALL mysql.rds_set_configuration('source delay', 30)",
            "source delay",
            Some("30"),
        ),
        (
            "the retention cleared",
            "CALL mysql.rds_set_configuration('binlog retention hours', NULL)",
            "binlog retention hours",
            None,
        ),
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run, got {refusal:?}"));
        let shown = rows(&mut session, "CALL mysql.rds_show_configuration").await;
        let value = shown
            .rows
            .iter()
            .find(|row| row.first().cloned().flatten().as_deref() == Some(name))
            .and_then(|row| row.get(1).cloned().flatten());
        assert_eq!(
            value.as_deref(),
            expected,
            "case {case:?}: {name:?} must read back as {expected:?}, got {shown:?}"
        );
    }

    // A configuration this instance does not have, and a procedure it does not have.
    let refusal = session
        .query("CALL mysql.rds_set_configuration('not a configuration', 1)")
        .await
        .expect_err("a configuration name the procedure does not have must be refused");
    assert_eq!(
        refusal.code.as_str(),
        "1644",
        "expected the procedure's own exception, got {refusal:?}"
    );
    let refusal = session
        .query("CALL mysql.rds_not_a_procedure")
        .await
        .expect_err("a procedure the server does not have must be refused");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("1305", "PROCEDURE mysql.rds_not_a_procedure does not exist"),
        "expected MySQL's missing-procedure refusal, got {refusal:?}"
    );
}

#[tokio::test]
async fn a_blue_green_deployments_green_database_serves_its_own_read_only_endpoint() {
    let router = emulator();
    let (blue_address, blue_port) = available_instance(
        &router,
        "emu-bg-blue",
        "mysql",
        &params!["EngineVersion" => "8.0.39"],
    )
    .await;
    // A row the production database holds before the environment is cloned, which the clone
    // has to carry.
    let mut blue = mysql_login(
        &blue_address,
        blue_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the blue endpoint");
    rows(&mut blue, "CREATE TABLE orders (id int PRIMARY KEY, memo varchar(32))").await;
    rows(&mut blue, "INSERT INTO orders (id, memo) VALUES (1, 'placed')").await;

    let created = expect_ok(
        "create the deployment",
        call(
            &router,
            "CreateBlueGreenDeployment",
            &params![
                "BlueGreenDeploymentName" => "emu-bg",
                "Source" => format!("arn:aws:rds:{REGION}:000000000000:db:emu-bg-blue"),
            ],
        )
        .await,
    );
    let green = text(&created, "Target")
        .rsplit(':')
        .next()
        .expect("the target ARN names the green database")
        .to_string();
    wait_for_settle();
    let described = expect_ok(
        "describe the green database",
        call(
            &router,
            "DescribeDBInstances",
            &params!["DBInstanceIdentifier" => green.clone()],
        )
        .await,
    );
    let (green_address, green_port) = endpoint_of(&described);
    assert_ne!(
        (green_address.as_str(), green_port),
        (blue_address.as_str(), blue_port),
        "the green database must publish its OWN endpoint, not the blue database's"
    );

    let mut session = mysql_login(
        &green_address,
        green_port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted on the green endpoint");
    // The green environment is a COPY of production: the rows the blue database held are
    // there.
    let held = rows(&mut session, "SELECT memo FROM orders WHERE id = 1").await;
    assert_eq!(
        held.only(),
        Some("placed"),
        "the green database must hold the rows it was cloned from, got {held:?}"
    );
    // And it serves no writes until the deployment is switched over, which is what the
    // server's own read-only variable reports.
    let read_only = rows(&mut session, "SELECT @@read_only").await;
    assert_eq!(
        read_only.only(),
        Some("1"),
        "a green database must report itself read-only, got {read_only:?}"
    );
    for (case, statement) in [
        ("a write", "INSERT INTO orders (id, memo) VALUES (2, 'green')"),
        ("a schema change", "CREATE TABLE greenonly (id int)"),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            refusal.code.as_str(),
            "1290",
            "case {case:?}: expected MySQL's read-only refusal, got {refusal:?}"
        );
    }
    // The blue database keeps serving its writes throughout.
    let writable = rows(&mut blue, "SELECT @@read_only").await;
    assert_eq!(
        writable.only(),
        Some("0"),
        "the blue database must stay writable, got {writable:?}"
    );
    rows(&mut blue, "INSERT INTO orders (id, memo) VALUES (3, 'blue')").await;
}

#[tokio::test]
async fn a_postgresql_endpoint_names_the_constraint_a_refused_row_violated() {
    let (_router, mut session) = postgres_instance("emu-constraint-pg").await;

    for statement in [
        "CREATE TABLE members (id int PRIMARY KEY, email text, \
         CONSTRAINT members_email_uq UNIQUE (email))",
        "INSERT INTO members (id, email) VALUES (1, 'ada@example.com')",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }

    // Each unique rule is a different condition to a client, and the name is what tells
    // them apart: the one the client declared, and the one PostgreSQL itself gives a
    // primary key.
    for (case, statement, expected) in [
        (
            "a duplicate primary key",
            "INSERT INTO members (id, email) VALUES (1, 'grace@example.com')",
            "members_pkey",
        ),
        (
            "a duplicate on a declared unique constraint",
            "INSERT INTO members (id, email) VALUES (2, 'ada@example.com')",
            "members_email_uq",
        ),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        assert_eq!(
            (
                refusal.code.as_str(),
                refusal.message.as_str(),
                session.last_refusal_constraint.as_deref(),
            ),
            (
                "23505",
                format!("duplicate key value violates unique constraint \"{expected}\"").as_str(),
                Some(expected),
            ),
            "case {case:?}: expected the constraint to be named, got {refusal:?}"
        );
    }

    // A function that reads or writes the HOST the engine runs on: the RDS master user is a
    // member of no role that holds its privilege, which is what the refusal says — and the
    // same answer for each of the functions those roles hold.
    for (case, statement) in [
        (
            "reading a server file",
            "SELECT pg_read_file('/etc/passwd')",
        ),
        (
            "reading it as bytes",
            "SELECT pg_read_binary_file('/etc/passwd')",
        ),
        ("listing a server directory", "SELECT pg_ls_dir('/')"),
        ("stat-ing a server file", "SELECT pg_stat_file('/etc/passwd')"),
        (
            "importing a large object from one",
            "SELECT lo_import('/etc/passwd')",
        ),
    ] {
        let refusal = session
            .query(statement)
            .await
            .expect_err(&format!("case {case:?}: {statement:?} must be refused"));
        let function = statement
            .trim_start_matches("SELECT ")
            .split('(')
            .next()
            .unwrap_or_default();
        assert_eq!(
            (refusal.code.as_str(), refusal.message.as_str()),
            (
                "42501",
                format!("permission denied for function {function}").as_str()
            ),
            "case {case:?}: expected PostgreSQL's privilege refusal, got {refusal:?}"
        );
    }

    // The extensions this instance has a control file for, with the one it was created
    // with reported as installed and the rest as available only.
    let available = session
        .query(
            "SELECT name, default_version, installed_version FROM pg_available_extensions \
             WHERE name IN ('plpgsql', 'pgcrypto') ORDER BY name",
        )
        .await
        .expect("the extension catalog must be readable");
    assert_eq!(
        available.rows,
        vec![
            vec![
                Some("pgcrypto".to_string()),
                Some("1.3".to_string()),
                None,
            ],
            vec![
                Some("plpgsql".to_string()),
                Some("1.0".to_string()),
                Some("1.0".to_string()),
            ],
        ],
        "the catalog must report which extensions are available and which are installed, \
         got {available:?}"
    );
    session
        .query("CREATE EXTENSION pgcrypto")
        .await
        .expect("an extension this instance ships must install");
    let installed = session
        .query(
            "SELECT installed_version FROM pg_available_extensions WHERE name = 'pgcrypto'",
        )
        .await
        .expect("the extension catalog must be readable");
    assert_eq!(
        installed.only(),
        Some("1.3"),
        "an installed extension must report its version, got {installed:?}"
    );
}

/// One row of the `LAST_INSERT_ID()` table: a statement the session runs, and the key its
/// `LAST_INSERT_ID()` answers with once it has.
struct InsertIdCase {
    case: &'static str,
    /// The statement to run first, or `None` to read the value as it stands.
    runs: Option<&'static str>,
    /// What `LAST_INSERT_ID()` answers afterwards.
    reports: &'static str,
}

#[tokio::test]
async fn a_mysql_session_answers_the_auto_increment_key_it_was_assigned() {
    let router = emulator();
    let (address, port) = available_instance(
        &router,
        "emu-dialect-insert-id",
        "mysql",
        &params!["EngineVersion" => "8.0.39"],
    )
    .await;
    let mut session = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    for statement in [
        "CREATE TABLE gauges (id INT AUTO_INCREMENT PRIMARY KEY, label VARCHAR(16) NOT NULL) \
         ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
        "CREATE TABLE notes (id BIGINT AUTO_INCREMENT PRIMARY KEY, body VARCHAR(16)) \
         ENGINE=InnoDB",
        "CREATE TABLE plain (code VARCHAR(8) PRIMARY KEY, note VARCHAR(16)) ENGINE=InnoDB",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }

    // The table spans what the rule generalises over: how many rows one statement inserts,
    // which table it inserts into, the statements that insert none, and the form that SETS
    // the session's value.
    let cases: &[InsertIdCase] = &[
        InsertIdCase {
            case: "a session that has inserted nothing",
            runs: None,
            reports: "0",
        },
        InsertIdCase {
            case: "one row inserted",
            runs: Some("INSERT INTO gauges (label) VALUES ('pressure')"),
            reports: "1",
        },
        InsertIdCase {
            case: "a second single row",
            runs: Some("INSERT INTO gauges (label) VALUES ('flow')"),
            reports: "2",
        },
        InsertIdCase {
            case: "three rows in ONE statement, whose FIRST key is reported",
            runs: Some("INSERT INTO gauges (label) VALUES ('a'), ('b'), ('c')"),
            reports: "3",
        },
        InsertIdCase {
            case: "an update, which is assigned no key",
            runs: Some("UPDATE gauges SET label = 'tide' WHERE id = 1"),
            reports: "3",
        },
        InsertIdCase {
            case: "a delete, which is assigned no key",
            runs: Some("DELETE FROM gauges WHERE id = 4"),
            reports: "3",
        },
        InsertIdCase {
            case: "a read, which is assigned no key",
            runs: Some("SELECT count(*) FROM gauges"),
            reports: "3",
        },
        InsertIdCase {
            case: "an insert into another table, which has its own keys",
            runs: Some("INSERT INTO notes (body) VALUES ('first')"),
            reports: "1",
        },
        InsertIdCase {
            case: "the one-argument form, which sets the session's value",
            runs: Some("SELECT LAST_INSERT_ID(99)"),
            reports: "99",
        },
        InsertIdCase {
            case: "an insert after that form, whose own key is reported again",
            runs: Some("INSERT INTO notes (body) VALUES ('second')"),
            reports: "2",
        },
        InsertIdCase {
            case: "an insert that SUPPLIES the key itself, which generates none",
            runs: Some("INSERT INTO notes (id, body) VALUES (100, 'explicit')"),
            reports: "2",
        },
        InsertIdCase {
            case: "an insert that writes the key column as NULL, asking for one",
            runs: Some("INSERT INTO notes (id, body) VALUES (NULL, 'generated')"),
            reports: "101",
        },
        InsertIdCase {
            case: "an insert into a table with no auto-increment column",
            runs: Some("INSERT INTO plain (code, note) VALUES ('a1', 'first')"),
            reports: "101",
        },
        InsertIdCase {
            case: "the row holding the highest key deleted",
            runs: Some("DELETE FROM gauges WHERE id = 5"),
            reports: "101",
        },
        InsertIdCase {
            case: "an insert after that delete, which does not reuse the key",
            runs: Some("INSERT INTO gauges (label) VALUES ('after')"),
            reports: "6",
        },
    ];
    for row in cases {
        if let Some(statement) = row.runs {
            let answer = session
                .query(statement)
                .await
                .unwrap_or_else(|refusal| {
                    panic!("case {:?}: {statement:?} must run, got {refusal:?}", row.case)
                });
            // The one-argument form ANSWERS the value it set, as MySQL's own does.
            if statement.contains("LAST_INSERT_ID(99)") {
                assert_eq!(
                    answer.only(),
                    Some(row.reports),
                    "case {:?}: the call must answer the value it set, got {answer:?}",
                    row.case
                );
            }
        }
        // Every spelling of the read is the same value, and the column is named after the
        // function as MySQL names it.
        for read in [
            "SELECT LAST_INSERT_ID()",
            "select last_insert_id()",
            "SELECT LAST_INSERT_ID() AS assigned",
        ] {
            let answer = rows(&mut session, read).await;
            assert_eq!(
                answer.only(),
                Some(row.reports),
                "case {:?}: {read:?} must answer {:?}, got {answer:?}",
                row.case,
                row.reports
            );
        }
        let named = rows(&mut session, "SELECT LAST_INSERT_ID()").await;
        assert_eq!(
            named.columns,
            vec!["LAST_INSERT_ID()".to_string()],
            "case {:?}: the column must be named after the call, got {named:?}",
            row.case
        );
    }

    // The keys really are the rows' own: what the reads above reported is what the table
    // holds, and a key the counter has handed out is not handed out again.
    let held = rows(&mut session, "SELECT id, label FROM gauges ORDER BY id").await;
    assert_eq!(
        held.rows,
        vec![
            vec![Some("1".to_string()), Some("tide".to_string())],
            vec![Some("2".to_string()), Some("flow".to_string())],
            vec![Some("3".to_string()), Some("a".to_string())],
            vec![Some("6".to_string()), Some("after".to_string())],
        ],
        "the assigned keys must be the keys the rows carry, got {held:?}"
    );
    let noted = rows(&mut session, "SELECT id, body FROM notes ORDER BY id").await;
    assert_eq!(
        noted.rows,
        vec![
            vec![Some("1".to_string()), Some("first".to_string())],
            vec![Some("2".to_string()), Some("second".to_string())],
            vec![Some("100".to_string()), Some("explicit".to_string())],
            vec![Some("101".to_string()), Some("generated".to_string())],
        ],
        "a supplied key must be the row's own, and the next generated key must follow it, \
         got {noted:?}"
    );

    // The value belongs to the SESSION: another session on the same endpoint has its own,
    // and neither reads the other's.
    let mut other = mysql_login(
        &address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .expect("the master user must be admitted");
    let fresh = rows(&mut other, "SELECT LAST_INSERT_ID()").await;
    assert_eq!(
        fresh.only(),
        Some("0"),
        "a second session must answer its own value, got {fresh:?}"
    );
    rows(&mut other, "INSERT INTO gauges (label) VALUES ('other')").await;
    let theirs = rows(&mut other, "SELECT LAST_INSERT_ID()").await;
    assert_eq!(
        theirs.only(),
        Some("7"),
        "the second session must answer the key ITS insert was assigned, got {theirs:?}"
    );
    let mine = rows(&mut session, "SELECT LAST_INSERT_ID()").await;
    assert_eq!(
        mine.only(),
        Some("6"),
        "another session's insert must not change this one's value, got {mine:?}"
    );

    // The counter belongs to the COLUMN however the client declared its key: MySQL takes the
    // keyword before or after the key on the column itself, and takes the key as a clause of
    // its own.
    for (case, declaration, table) in [
        (
            "the key declared after the keyword",
            "CREATE TABLE forms_a (id INT AUTO_INCREMENT PRIMARY KEY, label VARCHAR(8)) \
             ENGINE=InnoDB",
            "forms_a",
        ),
        (
            "the key declared before the keyword",
            "CREATE TABLE forms_b (id INT PRIMARY KEY AUTO_INCREMENT, label VARCHAR(8)) \
             ENGINE=InnoDB",
            "forms_b",
        ),
        (
            "the key declared as a clause of its own",
            "CREATE TABLE forms_c (id INT AUTO_INCREMENT, label VARCHAR(8), PRIMARY KEY (id)) \
             ENGINE=InnoDB",
            "forms_c",
        ),
    ] {
        session
            .query(declaration)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {declaration:?} must run, got {refusal:?}"));
        for label in ["one", "two"] {
            let insert = format!("INSERT INTO {table} (label) VALUES ('{label}')");
            session
                .query(&insert)
                .await
                .unwrap_or_else(|refusal| panic!("case {case:?}: {insert:?} must run, got {refusal:?}"));
        }
        let assigned = rows(&mut session, "SELECT LAST_INSERT_ID()").await;
        assert_eq!(
            assigned.only(),
            Some("2"),
            "case {case:?}: the column must be assigned keys, got {assigned:?}"
        );
        for statement in [
            format!("DELETE FROM {table} WHERE id = 2"),
            format!("INSERT INTO {table} (label) VALUES ('three')"),
        ] {
            session
                .query(&statement)
                .await
                .unwrap_or_else(|refusal| panic!("case {case:?}: {statement:?} must run, got {refusal:?}"));
        }
        let next = rows(&mut session, "SELECT LAST_INSERT_ID()").await;
        assert_eq!(
            next.only(),
            Some("3"),
            "case {case:?}: the counter must not hand out the deleted key again, got {next:?}"
        );
    }

    // The function is the MySQL family's: a PostgreSQL endpoint has no such function and
    // refuses the call in its own words.
    let (_pg_router, mut postgres) = postgres_instance("emu-dialect-insert-id-pg").await;
    let refusal = postgres
        .query("SELECT LAST_INSERT_ID()")
        .await
        .expect_err("a PostgreSQL session must have no such function");
    assert_eq!(
        (refusal.code.as_str(), refusal.message.as_str()),
        ("42883", "function LAST_INSERT_ID() does not exist"),
        "expected PostgreSQL's undefined-function refusal, got {refusal:?}"
    );
}

/// One row of the declared-type table: a statement the session runs, and what the column's
/// declared type makes of the value it writes.
struct DeclaredTypeCase {
    case: &'static str,
    runs: &'static str,
    /// The refusal the statement is answered with, as the error number and its sentence, or
    /// `None` for a statement the column admits.
    refused: Option<(&'static str, &'static str)>,
    /// What the table holds for the row afterwards, where the statement ran.
    holds: Option<&'static str>,
    /// The condition the server reports for it: its level, number and sentence.
    reports: Option<(&'static str, &'static str, &'static str)>,
}

#[tokio::test]
async fn a_mysql_column_admits_the_values_its_declared_type_does() {
    let (_router, mut session) =
        mysql_instance("emu-dialect-strict", "mysql", "8.0.39").await;
    for statement in [
        "CREATE TABLE readings (id INT AUTO_INCREMENT PRIMARY KEY, name VARCHAR(5) NOT NULL, \
         level TINYINT, tally INT UNSIGNED, note TEXT) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
    ] {
        session
            .query(statement)
            .await
            .unwrap_or_else(|refusal| panic!("{statement:?} must run, got {refusal:?}"));
    }

    // The table spans what the rule generalises over: the kind of bound the declaration
    // carries (a length, a signed range, an unsigned one, none at all), the end of the range
    // the value passes, and which row of the statement wrote it.
    let strict: &[DeclaredTypeCase] = &[
        DeclaredTypeCase {
            case: "a string the length admits",
            runs: "INSERT INTO readings (name) VALUES ('fits5')",
            refused: None,
            holds: Some("fits5"),
            reports: None,
        },
        DeclaredTypeCase {
            case: "a string one character too long",
            runs: "INSERT INTO readings (name) VALUES ('abcdef')",
            refused: Some(("1406", "Data too long for column 'name' at row 1")),
            holds: None,
            reports: Some((
                "Error",
                "1406",
                "Data too long for column 'name' at row 1",
            )),
        },
        DeclaredTypeCase {
            case: "a string too long in the SECOND row of the statement",
            runs: "INSERT INTO readings (name) VALUES ('ok'), ('abcdefgh')",
            refused: Some(("1406", "Data too long for column 'name' at row 2")),
            holds: None,
            reports: Some((
                "Error",
                "1406",
                "Data too long for column 'name' at row 2",
            )),
        },
        DeclaredTypeCase {
            case: "a number past the signed type's maximum",
            runs: "INSERT INTO readings (name, level) VALUES ('n', 128)",
            refused: Some(("1264", "Out of range value for column 'level' at row 1")),
            holds: None,
            reports: Some((
                "Error",
                "1264",
                "Out of range value for column 'level' at row 1",
            )),
        },
        DeclaredTypeCase {
            case: "the signed type's own minimum",
            runs: "INSERT INTO readings (name, level) VALUES ('m', -128)",
            refused: None,
            holds: Some("m"),
            reports: None,
        },
        DeclaredTypeCase {
            case: "a negative number in an unsigned column",
            runs: "INSERT INTO readings (name, tally) VALUES ('u', -1)",
            refused: Some(("1264", "Out of range value for column 'tally' at row 1")),
            holds: None,
            reports: Some((
                "Error",
                "1264",
                "Out of range value for column 'tally' at row 1",
            )),
        },
        DeclaredTypeCase {
            case: "a long value in a column whose declaration bounds none",
            runs: "INSERT INTO readings (name, note) VALUES ('t', 'far longer than five')",
            refused: None,
            holds: Some("t"),
            reports: None,
        },
        DeclaredTypeCase {
            case: "an UPDATE whose value the column admits",
            runs: "UPDATE readings SET note = 'noted' WHERE name = 't'",
            refused: None,
            holds: Some("t"),
            reports: None,
        },
        DeclaredTypeCase {
            case: "an UPDATE whose value is too long for the column",
            runs: "UPDATE readings SET name = 'abcdefgh' WHERE name = 't'",
            refused: Some(("1406", "Data too long for column 'name' at row 1")),
            holds: Some("t"),
            reports: None,
        },
        DeclaredTypeCase {
            case: "an UPDATE whose number is outside the column's range",
            runs: "UPDATE readings SET level = 999 WHERE name = 't'",
            refused: Some(("1264", "Out of range value for column 'level' at row 1")),
            holds: Some("t"),
            reports: None,
        },
    ];
    for row in strict {
        let answer = session.query(row.runs).await;
        match (row.refused, &answer) {
            (Some((code, message)), Err(refusal)) => assert_eq!(
                (refusal.code.as_str(), refusal.message.as_str()),
                (code, message),
                "case {:?}: expected the server's own refusal, got {refusal:?}",
                row.case
            ),
            (Some(expected), Ok(answered)) => panic!(
                "case {:?}: {:?} must be refused with {expected:?}, got {answered:?}",
                row.case, row.runs
            ),
            (None, Err(refusal)) => panic!(
                "case {:?}: {:?} must run, got {refusal:?}",
                row.case, row.runs
            ),
            (None, Ok(_)) => {}
        }
        if let Some(held) = row.holds {
            let found = rows(
                &mut session,
                &format!("SELECT name FROM readings WHERE name = '{held}'"),
            )
            .await;
            assert_eq!(
                found.only(),
                Some(held),
                "case {:?}: the row the statement wrote must be there, got {found:?}",
                row.case
            );
        }
        // What the server has to say about the statement, which a client reads back.
        let reported = rows(&mut session, "SHOW WARNINGS").await;
        let expected: Vec<Vec<Option<String>>> = row
            .reports
            .map(|(level, code, message)| {
                vec![vec![
                    Some(level.to_string()),
                    Some(code.to_string()),
                    Some(message.to_string()),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            reported.rows, expected,
            "case {:?}: the diagnostics must be this statement's own, got {reported:?}",
            row.case
        );
        assert_eq!(
            reported.columns,
            vec![
                "Level".to_string(),
                "Code".to_string(),
                "Message".to_string()
            ],
            "case {:?}: the columns must be the ones the server reports, got {reported:?}",
            row.case
        );
    }

    // The same values WITHOUT strict mode: the statement runs, the column holds the value it
    // admits, and the server says what it did.
    session
        .query("SET sql_mode = 'NO_ENGINE_SUBSTITUTION'")
        .await
        .expect("a session may set its own sql_mode");
    let lenient: &[DeclaredTypeCase] = &[
        DeclaredTypeCase {
            case: "a string too long, cut to the declared length",
            runs: "INSERT INTO readings (name) VALUES ('abcdefgh')",
            refused: None,
            holds: Some("abcde"),
            reports: Some((
                "Warning",
                "1265",
                "Data truncated for column 'name' at row 1",
            )),
        },
        DeclaredTypeCase {
            case: "a number past the type's maximum, held to it",
            runs: "INSERT INTO readings (name, level) VALUES ('held', 300)",
            refused: None,
            holds: Some("held"),
            reports: Some((
                "Warning",
                "1264",
                "Out of range value for column 'level' at row 1",
            )),
        },
    ];
    for row in lenient {
        session
            .query(row.runs)
            .await
            .unwrap_or_else(|refusal| {
                panic!(
                    "case {:?}: {:?} must run without strict mode, got {refusal:?}",
                    row.case, row.runs
                )
            });
        let reported = rows(&mut session, "SHOW WARNINGS").await;
        let expected: Vec<Vec<Option<String>>> = row
            .reports
            .map(|(level, code, message)| {
                vec![vec![
                    Some(level.to_string()),
                    Some(code.to_string()),
                    Some(message.to_string()),
                ]]
            })
            .unwrap_or_default();
        assert_eq!(
            reported.rows, expected,
            "case {:?}: the server must report what it did to the value, got {reported:?}",
            row.case
        );
    }
    // The values the columns HOLD: the string cut to the declared length and the number held
    // to the end of its range, which is what a client reads back.
    let held = rows(
        &mut session,
        "SELECT name, level FROM readings WHERE name IN ('abcde', 'held') ORDER BY name",
    )
    .await;
    assert_eq!(
        held.rows,
        vec![
            vec![Some("abcde".to_string()), None],
            vec![Some("held".to_string()), Some("127".to_string())],
        ],
        "the columns must hold the values they admit, got {held:?}"
    );
}
