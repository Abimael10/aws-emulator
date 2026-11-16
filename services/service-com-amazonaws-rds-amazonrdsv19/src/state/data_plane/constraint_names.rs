// SPDX-License-Identifier: CC0-1.0

//! The CONSTRAINT a refused row violated, named as the engine the client is talking to
//! names it.
//!
//! The engines refuse a row that violates a unique, primary-key or foreign-key constraint by
//! NAMING that constraint: PostgreSQL answers `duplicate key value violates unique
//! constraint "<name>"` — and carries the name in a FIELD of the error, which is where a
//! client reads it — while MySQL answers `Duplicate entry '<value>' for key '<index>'`,
//! quoting the value that was already there and the index that holds it. A client reads
//! either to tell WHICH rule it broke: two unique constraints on one table are two different
//! conditions to a client, and the embedded engine's own sentence (`UNIQUE constraint failed:
//! <table>.<column>`) names neither the constraint nor, for a foreign key, anything at all.
//!
//! So the name is resolved from the engine's OWN CATALOG: the `CREATE TABLE` statement the
//! database holds for the table is read back and parsed ([`sqlparser`]), and the constraint
//! whose columns the refusal names is the one that refused the row. A constraint the client
//! named with `CONSTRAINT <name>` is reported under that name; one it did not name is
//! reported under the name the client's own engine would have given it — PostgreSQL's
//! `<table>_<column(s)>_<kind>` with `pkey` for a primary key, `key` for a unique constraint
//! and `fkey` for a foreign key (PostgreSQL, "Constraint Naming"), and MySQL's `PRIMARY` for
//! the primary key and the first column's name for any other index. Nothing is invented: a
//! constraint the catalog does not account for leaves the engine's own sentence as it was.
//!
//! The VALUE a MySQL refusal quotes comes from the statement that was refused — the row it
//! tried to write, with the values the client bound to it — because that is the entry that
//! was already there.

use sqlparser::ast::{
    ColumnOption, Expr, Insert, SetExpr, Statement, TableConstraint, Value as SqlValue,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use super::sql_engine::{Cell, EngineError, EngineErrorKind, InstanceDatabase};
use super::wire_session::SessionFacts;
use super::EngineWire;

/// The refusal `error`, worded as the session's own engine words a constraint violation.
///
/// Only a refusal the embedded engine reported as a constraint violation is reworded; every
/// other refusal keeps the sentence it already had.
#[must_use]
pub fn as_the_engine_names_it(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &str,
    sql: &str,
    params: &[Option<String>],
    error: EngineError,
) -> EngineError {
    match (facts.wire, error.kind) {
        (EngineWire::Postgres, EngineErrorKind::UniqueViolation) => {
            unique_violation(database, target, &error)
        }
        (EngineWire::Postgres, EngineErrorKind::ForeignKeyViolation) => {
            foreign_key_violation(database, target, sql, &error)
        }
        (EngineWire::MySql, EngineErrorKind::UniqueViolation) => {
            duplicate_entry(database, facts, target, sql, params, &error)
        }
        (EngineWire::MySql, EngineErrorKind::ForeignKeyViolation) => {
            mysql_foreign_key_violation(database, target, sql, &error)
        }
        _ => error,
    }
}

/// `Cannot add or update a child row: a foreign key constraint fails (…)` — MySQL's refusal
/// of a row whose reference has no parent, and `Cannot delete or update a parent row: …` for
/// the other end of the same rule.
///
/// MySQL quotes the CONSTRAINT itself inside the parentheses — the table it is on, its name,
/// its columns and the table and columns it references, each identifier back-quoted — and a
/// client reads the constraint's name out of that. The constraint is the one the engine's own
/// catalog holds for the table the statement wrote ([`foreign_keys_in`]).
fn mysql_foreign_key_violation(
    database: &InstanceDatabase,
    target: &str,
    sql: &str,
    error: &EngineError,
) -> EngineError {
    let Some((table, removes_a_parent)) = written_table(sql) else {
        return error.clone();
    };
    // A statement that writes a row is refused for the child's own constraint; a statement
    // that removes one is refused for a constraint pointing AT it.
    let (child, declared) = if removes_a_parent {
        match referring_foreign_key(database, target, &table) {
            Some(found) => found,
            None => return error.clone(),
        }
    } else {
        match one_foreign_key_of(database, target, &table) {
            Some(found) => (table.clone(), found),
            None => return error.clone(),
        }
    };
    let name = constraint_name(&declared.named, &child, &declared.columns, "fkey");
    let columns = |columns: &[String]| {
        columns
            .iter()
            .map(|column| format!("`{column}`"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let sentence = if removes_a_parent {
        "Cannot delete or update a parent row"
    } else {
        "Cannot add or update a child row"
    };
    EngineError::new(
        error.kind,
        format!(
            "{sentence}: a foreign key constraint fails (`{target}`.`{child}`, \
             CONSTRAINT `{name}` FOREIGN KEY ({}) REFERENCES `{}` ({}))",
            columns(&declared.columns),
            declared.parent,
            columns(&declared.parent_columns),
        ),
    )
    .with_constraint(name)
    // `ER_NO_REFERENCED_ROW_2` for a child row with no parent, and `ER_ROW_IS_REFERENCED_2`
    // for a parent row another table still refers to.
    .with_mysql_error(if removes_a_parent { 1451 } else { 1452 }, "23000")
}

/// The table a statement WROTE and whether it removed a row from it, for the statements a
/// foreign-key refusal can be about.
fn written_table(sql: &str) -> Option<(String, bool)> {
    let statement = one_statement(sql)?;
    let (table, removes_a_parent) = match &statement {
        Statement::Insert(insert) => (insert.table.to_string(), false),
        Statement::Update { table, .. } => (table.to_string(), false),
        Statement::Delete(delete) => match &delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(tables)
            | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                (tables.first().map(|from| from.relation.to_string())?, true)
            }
        },
        _ => return None,
    };
    Some((unquoted(&table), removes_a_parent))
}

/// `Duplicate entry '<value>' for key '<index>'` — MySQL's refusal of a row whose key is
/// already in the table, with the value the statement tried to write and the index that
/// holds it.
fn duplicate_entry(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &str,
    sql: &str,
    params: &[Option<String>],
    error: &EngineError,
) -> EngineError {
    let Some((table, columns)) = violated_columns(&error.message) else {
        return error.clone();
    };
    let Some(violated) = violated_constraint(database, target, &table, &columns) else {
        return error.clone();
    };
    let Some(entry) = written_entry(sql, params, &columns) else {
        return error.clone();
    };
    let index = mysql_index_name(facts, &violated, &table, &columns);
    EngineError::new(
        error.kind,
        format!("Duplicate entry '{entry}' for key '{index}'"),
    )
    .with_constraint(index)
    .with_mysql_error(1062, "23000")
}

/// The name MySQL reports an index under in that refusal.
///
/// The index itself is `PRIMARY` for the primary key — "the name of a `PRIMARY KEY` is always
/// `PRIMARY`" — else the name the client gave it, else its first column, which is the name
/// MySQL gives an index declared without one.
///
/// MySQL qualifies that name with the index's TABLE from its 8.0 line on, where MariaDB (and
/// MySQL's earlier lines) report the index alone: the two engines word the same refusal
/// differently, and a client parsing the key out of the message reads what its own server
/// wrote. The engine and the version the instance runs are what decide which
/// ([`qualifies_the_index_with_its_table`]).
fn mysql_index_name(
    facts: &SessionFacts,
    violated: &ViolatedConstraint,
    table: &str,
    columns: &[String],
) -> String {
    let index = if violated.is_primary {
        "PRIMARY".to_string()
    } else {
        violated
            .named
            .as_ref()
            .map(|named| named.value.clone())
            .or_else(|| columns.first().cloned())
            .unwrap_or_default()
    };
    if qualifies_the_index_with_its_table(facts) {
        return format!("{table}.{index}");
    }
    index
}

/// The engine of an Amazon RDS for MariaDB resource, whose refusals name an index alone.
const MARIADB_ENGINE: &str = "mariadb";

/// The MySQL release line from which a duplicate-key refusal names the index QUALIFIED by its
/// table.
const MYSQL_QUALIFIED_INDEX_MAJOR_VERSION: u32 = 8;

/// Whether the session's own server qualifies the index with its table in that refusal; see
/// [`mysql_index_name`].
///
/// MariaDB never does, whatever its version — its release numbering is its own, and 10 or 11
/// is not MySQL 8. Every other MySQL-family engine does from its 8.0 line on, which is read
/// off the version the instance reports for itself.
fn qualifies_the_index_with_its_table(facts: &SessionFacts) -> bool {
    if facts.engine.eq_ignore_ascii_case(MARIADB_ENGINE) {
        return false;
    }
    facts
        .server_version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u32>().ok())
        .is_some_and(|major| major >= MYSQL_QUALIFIED_INDEX_MAJOR_VERSION)
}

/// The value of the key `columns` in the row the refused statement tried to write, as MySQL
/// quotes it: the values of those columns, joined by `-` for a key over several of them.
///
/// A statement that writes SEVERAL rows is one row of which was the duplicate: the one whose
/// value the table already holds, which is what the refusal is about.
fn written_entry(sql: &str, params: &[Option<String>], columns: &[String]) -> Option<String> {
    let statement = one_statement(sql)?;
    let rows: Vec<Vec<String>> = match &statement {
        Statement::Insert(insert) => inserted_values(insert, params, columns),
        Statement::Update { assignments, .. } => {
            let mut row = Vec::new();
            for column in columns {
                let assigned = assignments.iter().find(|assignment| {
                    names_match(&assignment.target.to_string(), column)
                })?;
                row.push(literal_text(&assigned.value, params)?);
            }
            vec![row]
        }
        _ => return None,
    };
    rows.into_iter()
        .map(|row| row.join("-"))
        .next_back()
}

/// The values of `columns` in each row an `INSERT` writes, in the order the rows were
/// written. A row that does not name every one of those columns is left out: what it wrote
/// for them is the table's own default, which the statement does not carry.
fn inserted_values(
    insert: &Insert,
    params: &[Option<String>],
    columns: &[String],
) -> Vec<Vec<String>> {
    let Some(source) = &insert.source else {
        return Vec::new();
    };
    let SetExpr::Values(values) = source.body.as_ref() else {
        return Vec::new();
    };
    let positions: Option<Vec<usize>> = columns
        .iter()
        .map(|column| {
            insert
                .columns
                .iter()
                .position(|named| names_match(&named.value, column))
        })
        .collect();
    let Some(positions) = positions else {
        return Vec::new();
    };
    values
        .rows
        .iter()
        .filter_map(|row| {
            positions
                .iter()
                .map(|at| row.get(*at).and_then(|value| literal_text(value, params)))
                .collect::<Option<Vec<String>>>()
        })
        .collect()
}

/// The text of a value a statement wrote, where it wrote one this server can read: a
/// literal, or the value the client BOUND to a placeholder.
///
/// `None` for anything computed — a function call, an expression over other columns — whose
/// value is the engine's to work out and not something the statement carries.
fn literal_text(value: &Expr, params: &[Option<String>]) -> Option<String> {
    match value {
        Expr::Value(value) => match &value.value {
            SqlValue::Number(number, _) => Some(number.clone()),
            SqlValue::SingleQuotedString(text)
            | SqlValue::DoubleQuotedString(text)
            | SqlValue::EscapedStringLiteral(text) => Some(text.clone()),
            SqlValue::Boolean(truth) => Some(i64::from(*truth).to_string()),
            // A positional placeholder: the value the client bound to it, by its own
            // position in the statement.
            SqlValue::Placeholder(written) => {
                let at = written
                    .trim_start_matches(['?', '$', ':'])
                    .parse::<usize>()
                    .ok()
                    .and_then(|position| position.checked_sub(1))?;
                params.get(at).cloned().flatten()
            }
            _ => None,
        },
        Expr::UnaryOp { op, expr } => {
            Some(format!("{op}{}", literal_text(expr, params)?))
        }
        _ => None,
    }
}

/// `duplicate key value violates unique constraint "<name>"`, for the constraint whose
/// columns the engine's own sentence names.
fn unique_violation(
    database: &InstanceDatabase,
    target: &str,
    error: &EngineError,
) -> EngineError {
    let Some((table, columns)) = violated_columns(&error.message) else {
        return error.clone();
    };
    let Some(violated) = violated_constraint(database, target, &table, &columns) else {
        return error.clone();
    };
    let name = postgres_constraint_name(&violated, &table, &columns);
    EngineError::new(
        error.kind,
        format!("duplicate key value violates unique constraint \"{name}\""),
    )
    .with_constraint(name)
}

/// The name PostgreSQL reports the violated constraint under: the one the client gave it,
/// else the one PostgreSQL itself would have given it.
fn postgres_constraint_name(
    violated: &ViolatedConstraint,
    table: &str,
    columns: &[String],
) -> String {
    let kind = if violated.is_primary { "pkey" } else { "key" };
    constraint_name(&violated.named, table, columns, kind)
}

/// The table and the columns `UNIQUE constraint failed: <table>.<column>, …` names, or
/// `None` for a sentence of another shape.
fn violated_columns(message: &str) -> Option<(String, Vec<String>)> {
    let listed = message.strip_prefix("UNIQUE constraint failed: ")?;
    let mut table = String::new();
    let mut columns = Vec::new();
    for pair in listed.split(',') {
        let (named_table, column) = pair.trim().split_once('.')?;
        if table.is_empty() {
            table = named_table.to_string();
        } else if !table.eq_ignore_ascii_case(named_table) {
            // Two tables in one refusal is not a shape this engine reports.
            return None;
        }
        columns.push(column.to_ascii_lowercase());
    }
    (!table.is_empty() && !columns.is_empty()).then_some((table, columns))
}

/// WHICH constraint of `table` over `columns` refused the row: whether it is the table's
/// primary key, and the name the client gave it if it gave one.
struct ViolatedConstraint {
    /// The name the client declared it under, where it declared one.
    named: Option<sqlparser::ast::Ident>,
    /// Whether it is the table's PRIMARY KEY, which both engines name differently from any
    /// other unique rule.
    is_primary: bool,
}

/// The unique or primary-key constraint of `table` over `columns`, as the engine's own
/// catalog holds the table's definition.
fn violated_constraint(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
    columns: &[String],
) -> Option<ViolatedConstraint> {
    let created = create_statement(database, target, table)?;
    let Statement::CreateTable(created) = created else {
        return None;
    };
    for constraint in &created.constraints {
        match constraint {
            TableConstraint::Unique {
                name,
                columns: named,
                ..
            } if same_columns(named, columns) => {
                return Some(ViolatedConstraint {
                    named: name.clone(),
                    is_primary: false,
                });
            }
            TableConstraint::PrimaryKey {
                name,
                columns: named,
                ..
            } if same_columns(named, columns) => {
                return Some(ViolatedConstraint {
                    named: name.clone(),
                    is_primary: true,
                });
            }
            _ => {}
        }
    }
    // A constraint declared on the COLUMN itself, which is the one-column case.
    let [only] = columns else {
        return None;
    };
    for column in &created.columns {
        if !names_match(&column.name.value, only) {
            continue;
        }
        for option in &column.options {
            if let ColumnOption::Unique { is_primary, .. } = option.option {
                return Some(ViolatedConstraint {
                    named: option.name.clone(),
                    is_primary,
                });
            }
        }
    }
    None
}

/// `insert or update on table "<child>" violates foreign key constraint "<name>"`, or the
/// sentence for the other end of the same constraint when the statement removed a parent row.
fn foreign_key_violation(
    database: &InstanceDatabase,
    target: &str,
    sql: &str,
    error: &EngineError,
) -> EngineError {
    let Some(statement) = one_statement(sql) else {
        return error.clone();
    };
    let (table, removes_a_parent) = match &statement {
        Statement::Insert(insert) => (insert.table.to_string(), false),
        Statement::Update { table, .. } => (table.to_string(), false),
        Statement::Delete(delete) => match &delete.from {
            sqlparser::ast::FromTable::WithFromKeyword(tables)
            | sqlparser::ast::FromTable::WithoutKeyword(tables) => {
                match tables.first().map(|from| from.relation.to_string()) {
                    Some(table) => (table, true),
                    None => return error.clone(),
                }
            }
        },
        _ => return error.clone(),
    };
    let table = unquoted(&table);
    // A statement that writes a row is refused for the child's own constraint; a statement
    // that removes one is refused for a constraint pointing AT it, which is the other end of
    // the same rule and the other sentence PostgreSQL has for it.
    if removes_a_parent {
        let Some((child, name)) = referring_constraint(database, target, &table) else {
            return error.clone();
        };
        return EngineError::new(
            error.kind,
            format!(
                "update or delete on table \"{table}\" violates foreign key constraint \
                 \"{name}\" on table \"{child}\""
            ),
        )
        .with_constraint(name);
    }
    let Some(name) = foreign_key_of(database, target, &table) else {
        return error.clone();
    };
    EngineError::new(
        error.kind,
        format!(
            "insert or update on table \"{table}\" violates foreign key constraint \"{name}\""
        ),
    )
    .with_constraint(name)
}

/// The name of the foreign-key constraint `table` carries, for the table that carries ONE:
/// which rule a row broke is only decided for a table with a single foreign key, and a table
/// with several keeps the engine's own sentence rather than being told the wrong rule.
fn foreign_key_of(database: &InstanceDatabase, target: &str, table: &str) -> Option<String> {
    let declared = one_foreign_key_of(database, target, table)?;
    Some(constraint_name(
        &declared.named,
        table,
        &declared.columns,
        "fkey",
    ))
}

/// The ONE foreign key `table` declares, with everything it declares about it; see
/// [`foreign_key_of`] for why only a table with a single one is decided.
fn one_foreign_key_of(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
) -> Option<DeclaredForeignKey> {
    let mut keys = foreign_keys_in(&create_sql(database, target, table)?);
    (keys.len() == 1).then(|| keys.remove(0))
}

/// The table that refers to `table` and the foreign key it refers with, for the one table
/// that does; the other half of [`referring_constraint`].
fn referring_foreign_key(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
) -> Option<(String, DeclaredForeignKey)> {
    let mut found: Option<(String, DeclaredForeignKey)> = None;
    for (child, ddl) in candidates_referring_to(database, target, table) {
        for declared in foreign_keys_in(&ddl) {
            if !names_match(&declared.parent, table) {
                continue;
            }
            if found.is_some() {
                // More than one table refers to this one: which rule the row broke is not
                // decided here.
                return None;
            }
            found = Some((child.clone(), declared));
        }
    }
    found
}

/// The table that refers to `table` and the name of the constraint it refers with, for the
/// one table that does.
///
/// The candidates are read in ONE statement from the engine's own catalog — the tables whose
/// definition MENTIONS this one, which is what a reference to it has to — and at most
/// [`MAX_REFERRING_CANDIDATES`] of them are read: naming the rule a row broke must not cost
/// one statement and one parse per table of the database.
fn referring_constraint(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
) -> Option<(String, String)> {
    let (child, declared) = referring_foreign_key(database, target, table)?;
    let named = constraint_name(&declared.named, &child, &declared.columns, "fkey");
    Some((child, named))
}

/// How many of a database's tables are read when a refusal has to name the constraint that
/// refers to one of them.
///
/// The catalog itself narrows the candidates to the tables whose definition mentions the one
/// the row was removed from; this bounds the work that narrowing can still leave, so a
/// refusal costs a bounded number of parses whatever the database holds. Past it the engine's
/// own sentence is what the client is told.
const MAX_REFERRING_CANDIDATES: usize = 16;

/// The tables of `target` whose own definition mentions `table`, with that definition: the
/// candidates for a constraint referring to it, as the engine's catalog holds them.
fn candidates_referring_to(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
) -> Vec<(String, String)> {
    let Ok(answer) = database.execute(
        target,
        "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND sql IS NOT NULL \
         AND name NOT LIKE 'sqlite_%' AND lower(sql) LIKE '%' || lower(?1) || '%' \
         ORDER BY name LIMIT ?2",
        &[
            Some(table.to_string()),
            Some(MAX_REFERRING_CANDIDATES.to_string()),
        ],
    ) else {
        return Vec::new();
    };
    answer
        .rows
        .iter()
        .filter_map(|row| match (row.first(), row.get(1)) {
            (Some(Cell::Text(name)), Some(Cell::Text(ddl))) => Some((name.clone(), ddl.clone())),
            _ => None,
        })
        .collect()
}

/// The foreign keys the definition `ddl` declares: the table each refers to, the columns it
/// is over, and the name the client gave it, if any.
/// One FOREIGN KEY a table declares: the name the client gave it (if any), the columns it is
/// over, and the table and columns it references.
struct DeclaredForeignKey {
    named: Option<sqlparser::ast::Ident>,
    columns: Vec<String>,
    parent: String,
    parent_columns: Vec<String>,
}

/// The foreign keys the definition `ddl` declares.
fn foreign_keys_in(ddl: &str) -> Vec<DeclaredForeignKey> {
    let Some(Statement::CreateTable(created)) = one_statement(ddl) else {
        return Vec::new();
    };
    let lowered = |columns: &[sqlparser::ast::Ident]| {
        columns
            .iter()
            .map(|column| column.value.to_ascii_lowercase())
            .collect::<Vec<String>>()
    };
    let mut keys = Vec::new();
    for constraint in &created.constraints {
        if let TableConstraint::ForeignKey {
            name,
            columns,
            foreign_table,
            referred_columns,
            ..
        } = constraint
        {
            keys.push(DeclaredForeignKey {
                named: name.clone(),
                columns: lowered(columns),
                parent: unquoted(&foreign_table.to_string()),
                parent_columns: lowered(referred_columns),
            });
        }
    }
    // A reference declared on the COLUMN itself.
    for column in &created.columns {
        for option in &column.options {
            if let ColumnOption::ForeignKey {
                foreign_table,
                referred_columns,
                ..
            } = &option.option
            {
                keys.push(DeclaredForeignKey {
                    named: option.name.clone(),
                    columns: vec![column.name.value.to_ascii_lowercase()],
                    parent: unquoted(&foreign_table.to_string()),
                    parent_columns: lowered(referred_columns),
                });
            }
        }
    }
    keys
}

/// The `CREATE TABLE` statement the database `target` holds for `table`, as its text.
fn create_sql(database: &InstanceDatabase, target: &str, table: &str) -> Option<String> {
    let answer = database
        .execute(
            target,
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND lower(name) = lower(?1)",
            &[Some(table.to_string())],
        )
        .ok()?;
    match answer.rows.first().and_then(|row| row.first()) {
        Some(Cell::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

/// The `CREATE TABLE` statement the database `target` holds for `table`, parsed.
fn create_statement(
    database: &InstanceDatabase,
    target: &str,
    table: &str,
) -> Option<Statement> {
    one_statement(&create_sql(database, target, table)?)
}

/// `sql` as the one statement it is, or `None` where it is not one statement the parser
/// reads.
fn one_statement(sql: &str) -> Option<Statement> {
    let mut statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).ok()?;
    (statements.len() == 1).then(|| statements.remove(0))
}

/// The name a constraint is reported under: the one the client gave it, else the name
/// PostgreSQL itself would have given it (see this module's own notes).
fn constraint_name(
    named: &Option<sqlparser::ast::Ident>,
    table: &str,
    columns: &[String],
    kind: &str,
) -> String {
    if let Some(named) = named {
        return named.value.clone();
    }
    if kind == "pkey" {
        return format!("{table}_pkey");
    }
    let mut name = table.to_string();
    for column in columns {
        name.push('_');
        name.push_str(column);
    }
    format!("{name}_{kind}")
}

/// Whether a constraint's column list is the one the refusal named, in the same order.
fn same_columns<T: std::fmt::Display>(named: &[T], columns: &[String]) -> bool {
    let named: Vec<String> = named
        .iter()
        .map(|column| column_of(&column.to_string()))
        .collect();
    named.len() == columns.len()
        && named
            .iter()
            .zip(columns)
            .all(|(named, column)| names_match(named, column))
}

/// The column one entry of a constraint's column list names: its own name, without the sort
/// order or the operator class an index column may carry.
fn column_of(entry: &str) -> String {
    let entry = entry.split_whitespace().next().unwrap_or(entry);
    unquoted(entry)
}

/// One identifier without the quoting a catalog entry carries.
fn unquoted(name: &str) -> String {
    name.trim_matches(|character| character == '"' || character == '`' || character == '[')
        .trim_matches(']')
        .to_string()
}

/// Whether two identifiers name the same thing: both engines resolve a name
/// case-insensitively here.
fn names_match(left: &str, right: &str) -> bool {
    unquoted(left).eq_ignore_ascii_case(&unquoted(right))
}
