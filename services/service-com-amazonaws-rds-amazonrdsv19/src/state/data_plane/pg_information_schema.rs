// SPDX-License-Identifier: CC0-1.0

//! The SQL-standard `information_schema` views a PostgreSQL DB instance's database
//! publishes: what a driver, an ORM, a migration tool or an admin client reads the shape of
//! the database out of before it does anything else.
//!
//! "The information schema consists of a set of views that contain information about the
//! objects defined in the current database", and PostgreSQL creates that schema in EVERY
//! database. So the views here are views OF THE DATABASE the session is on: every row is a
//! relation that database really holds and a column that relation really declares, read out
//! of the engine's own catalog. Nothing is answered from the statement that asked, and a
//! relation nothing created is in no answer.
//!
//! They are served the way the server's other catalogs are ([`super::pg_server`]): as REAL
//! relations of the session's own database, rebuilt from the engine's catalog when a
//! statement names one, so a client's `SELECT … FROM information_schema.columns WHERE
//! table_name = $1` is a query the engine plans and runs like any other. A statement that
//! names none pays nothing.
//!
//! WHERE they are held: under the `pg_` prefix PostgreSQL reserves for the relations its
//! system catalog is made of ([`HELD_PREFIX`]), because they belong to the SERVER rather than
//! to the client's data — which is what keeps them out of an export of the database
//! ([`crate::state::export_objects::is_engine_catalog_relation`]) and what the reference a
//! statement writes is resolved to ([`super::dialect`]). What they REPORT themselves as is
//! the views they are: relations of the `information_schema` schema, so a client reading
//! `information_schema.tables` sees them where PostgreSQL has them and never as tables of its
//! own schema.
//!
//! The MySQL family's own `information_schema` is a SCHEMA OF THE SERVER rather than of each
//! database, and is served as such beside this ([`super::mysql_server`]).

use ::sqlparser::ast::{ColumnDef, ColumnOption, Statement};
use ::sqlparser::dialect::PostgreSqlDialect;
use ::sqlparser::parser::Parser;
use ::sqlparser::tokenizer::Token;

use super::sql_engine::{Cell, EngineError, EngineErrorKind, InstanceDatabase};
use super::wire_session::SessionFacts;
use super::EngineWire;

/// The schema the standard catalog views live in, which is the name a client qualifies them
/// with.
pub const CATALOG_SCHEMA: &str = "information_schema";

/// The schema a relation the database holds under an unqualified name is reported in:
/// `public`, "the default schema, in which user objects are created".
const DEFAULT_SCHEMA: &str = "public";

/// The schema the SERVER's own catalog relations are reported in — "table names beginning
/// with `pg_` are reserved for system catalogs" — which is where a client finds `pg_settings`
/// and the rest of [`super::pg_server`]'s catalogs.
const SYSTEM_SCHEMA: &str = "pg_catalog";

/// The views this module serves, under the names PostgreSQL gives them.
///
/// A statement that names one has that view rebuilt from the engine's catalog before it runs
/// ([`refresh_views`]). A view of the standard this module does not serve is left as the
/// client wrote it, which the engine answers as the relation the database holds none of.
pub const VIEWS: [&str; 2] = ["tables", "columns"];

/// What the relation holding one of those views is named in the database that publishes it:
/// this prefix and the view's own name.
///
/// Under the prefix PostgreSQL reserves for its system catalog, for the reason this module's
/// own notes give — the relation belongs to the server, not to the client's data.
pub const HELD_PREFIX: &str = "pg_information_schema_";

/// The relation the view `view` of [`CATALOG_SCHEMA`] is HELD as, or `None` for a name this
/// module does not serve.
///
/// Compared without regard to case, because PostgreSQL folds an undelimited identifier to
/// lower case and the catalog's own relation names are lower case there — so
/// `information_schema.COLUMNS` and `information_schema.columns` name one view.
#[must_use]
pub fn held_as(view: &str) -> Option<String> {
    VIEWS
        .iter()
        .find(|known| known.eq_ignore_ascii_case(view))
        .map(|known| format!("{HELD_PREFIX}{known}"))
}

/// The definitions of those relations, with the columns this server has state behind.
///
/// Source: PostgreSQL's "The Information Schema" — `information_schema.tables` ("one row for
/// each table or view in the current database") and `information_schema.columns` ("one row
/// for each column in a table or view of the current database"), reduced to the columns the
/// engine's own catalog answers for truthfully. A column of the standard view that is a fact
/// about storage this engine does not keep is not carried at all rather than filled in with a
/// guess.
const VIEW_SCHEMA: [&str; 2] = [
    "CREATE TABLE IF NOT EXISTS pg_information_schema_tables (\
       table_catalog text, table_schema text, table_name text, table_type text, \
       PRIMARY KEY (table_schema, table_name))",
    "CREATE TABLE IF NOT EXISTS pg_information_schema_columns (\
       table_catalog text, table_schema text, table_name text, column_name text, \
       ordinal_position integer, column_default text, is_nullable text, data_type text, \
       character_maximum_length integer, numeric_precision integer, numeric_scale integer, \
       PRIMARY KEY (table_schema, table_name, column_name))",
];

/// How `information_schema.tables.table_type` classifies a relation that holds data, and one
/// that is a query over others.
const BASE_TABLE: &str = "BASE TABLE";
const VIEW: &str = "VIEW";

/// What `information_schema.columns.is_nullable` reports for a column whose declaration
/// admits a null, and for one that refuses one.
const NULLABLE: &str = "YES";
const NOT_NULLABLE: &str = "NO";

/// How many relations of the session's database one rebuild reads: a bound on what a
/// statement that reads the catalog costs, whatever the database holds. Well above the
/// relations a client of an emulated database creates, and a database holding more is told so
/// ([`relations_of`]) rather than answered a view that silently leaves relations out.
const MAX_CATALOGUED_RELATIONS: usize = 2_048;

/// How many rows of a view one `INSERT` carries.
///
/// Every value is BOUND rather than written into the statement, and a connection admits
/// [`super::sql_engine::MAX_BOUND_PARAMETERS`] of those per statement — so the rows a rebuild
/// writes go in batches that stay inside that bound however many columns the database holds.
const ROWS_PER_INSERT: usize = 32;

/// How many ROWS one rebuild of a view may materialise: the bound on the PRODUCT of the two
/// factors a client's own schema sets — the relations the database holds and the columns each
/// of them declares — and so on what one statement that reads a catalog view costs, whatever
/// that schema is.
///
/// Well above the columns a client of an emulated database declares (it is the relation cap
/// times a wide table's column count), and a database holding more is told so rather than
/// answered a view that silently leaves columns out ([`insert_in_batches`]).
const MAX_CATALOGUED_ROWS: usize = 65_536;

/// The views a statement names, in the order [`VIEWS`] lists them. A statement of a session
/// on the other wire names none.
///
/// A view is named the way a client reaches one: QUALIFIED by the schema it lives in
/// (`information_schema.columns`), which is the only way PostgreSQL resolves it — that schema
/// is not on a session's search path, so a bare `columns` is a relation of the client's own.
#[must_use]
pub fn views_named(facts: &SessionFacts, tokens: &[Token]) -> Vec<&'static str> {
    if facts.wire != EngineWire::Postgres {
        return Vec::new();
    }
    VIEWS
        .into_iter()
        .filter(|view| {
            tokens.iter().enumerate().any(|(at, token)| match token {
                Token::Word(word) if word.value.eq_ignore_ascii_case(view) => {
                    super::dialect::qualified_by_schema(tokens, at, CATALOG_SCHEMA)
                }
                _ => false,
            })
        })
        .collect()
}

/// Create the relations holding those views if this database has not served one yet. Called
/// once per session, before any statement of it runs, exactly as the server's other catalogs
/// are ([`super::pg_server::ensure_catalogs`]) — so a client that DESCRIBES a statement over
/// one before running it is described against the relation it will read.
pub fn ensure_views(database: &InstanceDatabase, name: &str) -> Result<(), EngineError> {
    for definition in VIEW_SCHEMA {
        database.execute(name, definition, &[])?;
    }
    Ok(())
}

/// Rebuild the views `named` from the engine's own catalog, so a statement that reads one
/// reads the database as it is at this moment.
pub fn refresh_views(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    named: &[&str],
) -> Result<(), EngineError> {
    if named.is_empty() {
        return Ok(());
    }
    for definition in VIEW_SCHEMA {
        database.execute_in(facts.connection_id, &facts.database, definition, &[])?;
    }
    let relations = relations_of(database, facts)?;
    for view in named {
        match *view {
            "tables" => refresh_tables(database, facts, &relations)?,
            "columns" => refresh_columns(database, facts, &relations)?,
            _ => {}
        }
    }
    Ok(())
}

/// One relation of the database, as the engine's own catalog holds it.
#[derive(Debug, Clone)]
struct Relation {
    /// The name the engine holds it under.
    held: String,
    /// The schema it is REPORTED in, and the name it is reported under: the server's own
    /// catalog relations belong to the system schema, the views of this module to the
    /// information schema under the view's own name, and everything else to the schema a
    /// client's objects live in.
    schema: &'static str,
    reported: String,
    /// `BASE TABLE` or `VIEW`, as the standard view classifies it.
    kind: &'static str,
    /// The statement the engine stored for it, where it holds one: the declaration a table's
    /// columns are read out of.
    declaration: Option<String>,
}

/// The relations the session's own database holds, each classified as the catalog reports it.
///
/// The engine's OWN objects (`sqlite_`) are relations of no database's catalog, so they are
/// in no view built from this.
///
/// A database holding more relations than one rebuild reads ([`MAX_CATALOGUED_RELATIONS`]) is
/// REFUSED rather than answered a shortened view: a client introspecting the catalog would
/// read a missing relation as one the database does not hold, which is a wrong answer where
/// this is an answer the endpoint cannot give.
fn relations_of(
    database: &InstanceDatabase,
    facts: &SessionFacts,
) -> Result<Vec<Relation>, EngineError> {
    let held = database.execute_in(
        facts.connection_id,
        &facts.database,
        "SELECT name, type, sql FROM sqlite_master WHERE type IN ('table', 'view') \
         AND name NOT LIKE 'sqlite_%' ORDER BY name LIMIT ?1",
        &[Some((MAX_CATALOGUED_RELATIONS + 1).to_string())],
    )?;
    if held.rows.len() > MAX_CATALOGUED_RELATIONS {
        return Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!(
                "this endpoint reports at most {MAX_CATALOGUED_RELATIONS} relations of a \
                 database in a catalog view"
            ),
        ));
    }
    Ok(held
        .rows
        .iter()
        .filter_map(|row| {
            let (Some(Cell::Text(name)), Some(Cell::Text(kind))) = (row.first(), row.get(1)) else {
                return None;
            };
            let declaration = match row.get(2) {
                Some(Cell::Text(sql)) => Some(sql.clone()),
                _ => None,
            };
            let (schema, reported, kind) = match name.strip_prefix(HELD_PREFIX) {
                // A view of this module: it is reported as the view it serves, which is a
                // VIEW of the information schema — never as a table of the client's own.
                Some(view) => (CATALOG_SCHEMA, view.to_string(), VIEW),
                None if name.starts_with("pg_") => (
                    SYSTEM_SCHEMA,
                    name.clone(),
                    if kind == "view" { VIEW } else { BASE_TABLE },
                ),
                None => (
                    DEFAULT_SCHEMA,
                    name.clone(),
                    if kind == "view" { VIEW } else { BASE_TABLE },
                ),
            };
            Some(Relation {
                held: name.clone(),
                schema,
                reported,
                kind,
                declaration,
            })
        })
        .collect())
}

/// Replace `information_schema.tables` with one row per relation the database holds.
fn refresh_tables(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    relations: &[Relation],
) -> Result<(), EngineError> {
    let held = held_as("tables").unwrap_or_default();
    database.execute_in(
        facts.connection_id,
        &facts.database,
        &format!("DELETE FROM {held}"),
        &[],
    )?;
    let rows: Vec<Vec<Option<String>>> = relations
        .iter()
        .map(|relation| {
            vec![
                Some(facts.database.clone()),
                Some(relation.schema.to_string()),
                Some(relation.reported.clone()),
                Some(relation.kind.to_string()),
            ]
        })
        .collect();
    insert_in_batches(database, facts, &held, 4, &rows)
}

/// Replace `information_schema.columns` with one row per column of every relation the
/// database holds.
fn refresh_columns(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    relations: &[Relation],
) -> Result<(), EngineError> {
    let held = held_as("columns").unwrap_or_default();
    database.execute_in(
        facts.connection_id,
        &facts.database,
        &format!("DELETE FROM {held}"),
        &[],
    )?;
    let mut rows: Vec<Vec<Option<String>>> = Vec::new();
    for relation in relations {
        // The rows this rebuild may materialise at all, charged as each relation's columns
        // are read rather than after they have all been built: a schema past the bound costs
        // this endpoint the reading of one more relation and nothing more.
        if rows.len() > MAX_CATALOGUED_ROWS {
            break;
        }
        for column in columns_of(database, facts, relation) {
            let declared = DeclaredType::of(&column.declared_type);
            let (precision, scale) = declared.numeric_precision_and_scale();
            rows.push(vec![
                Some(facts.database.clone()),
                Some(relation.schema.to_string()),
                Some(relation.reported.clone()),
                Some(column.name),
                Some(column.position.to_string()),
                column.default,
                Some(
                    if column.not_null {
                        NOT_NULLABLE
                    } else {
                        NULLABLE
                    }
                    .to_string(),
                ),
                Some(declared.data_type()),
                declared.character_maximum_length().map(|at| at.to_string()),
                precision.map(|digits| digits.to_string()),
                scale.map(|digits| digits.to_string()),
            ]);
        }
    }
    insert_in_batches(database, facts, &held, 11, &rows)
}

/// Write `rows` into the relation `held` of the session's own database, in batches that stay
/// inside the bound on how many values one statement may bind ([`ROWS_PER_INSERT`]).
///
/// A rebuild that would materialise more rows than one statement may is REFUSED
/// ([`MAX_CATALOGUED_ROWS`]) rather than written in part: a view holding some of a database's
/// columns would read as a database that declares only those, and the bound is what keeps the
/// cost of one catalog read — the product of the relations and the columns each declares —
/// bounded whatever schema a client created.
fn insert_in_batches(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    held: &str,
    columns: usize,
    rows: &[Vec<Option<String>>],
) -> Result<(), EngineError> {
    if rows.len() > MAX_CATALOGUED_ROWS {
        return Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!(
                "this endpoint reports at most {MAX_CATALOGUED_ROWS} rows of a database in a \
                 catalog view"
            ),
        ));
    }
    for batch in rows.chunks(ROWS_PER_INSERT) {
        super::pg_server::insert_catalog_rows(
            database,
            facts,
            &facts.database,
            held,
            columns,
            batch,
        )?;
    }
    Ok(())
}

/// One column of one relation, as the engine's catalog answers for it.
struct DeclaredColumn {
    name: String,
    /// Its position in the relation, counted from one, which is `ordinal_position`.
    position: usize,
    /// The default its declaration carries, and `None` for a column that declares none.
    default: Option<String>,
    /// Whether the declaration refuses a null.
    not_null: bool,
    /// The type the column was declared with, as the declaration wrote it.
    declared_type: String,
}

/// The columns of `relation`, in the order it declares them.
///
/// A TABLE's columns come from the declaration the engine stored for it, which carries
/// everything the catalog reports: the name, the type as it was written, the default and
/// whether a null is admitted. A VIEW declares none of that — its columns are the columns of
/// the query behind it — so those are read from the engine itself, by asking it to describe
/// the view without producing a row; a view column admits a null, as PostgreSQL reports for
/// one.
fn columns_of(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    relation: &Relation,
) -> Vec<DeclaredColumn> {
    if relation.kind == BASE_TABLE {
        if let Some(declaration) = relation.declaration.as_deref() {
            return declared_columns(declaration);
        }
    }
    let described = database.execute_in(
        facts.connection_id,
        &facts.database,
        &format!("SELECT * FROM \"{}\" LIMIT 0", relation.held.replace('"', "\"\"")),
        &[],
    );
    let Ok(described) = described else {
        return Vec::new();
    };
    described
        .columns
        .into_iter()
        .enumerate()
        .map(|(at, column)| DeclaredColumn {
            name: column.name,
            position: at + 1,
            default: None,
            not_null: false,
            declared_type: column.declared_type.unwrap_or_default(),
        })
        .collect()
}

/// The columns the stored declaration `ddl` declares, read by the parser this endpoint reads
/// a PostgreSQL statement with. A declaration it cannot read declares nothing this reports.
fn declared_columns(ddl: &str) -> Vec<DeclaredColumn> {
    let Ok(mut statements) = Parser::parse_sql(&PostgreSqlDialect {}, ddl) else {
        return Vec::new();
    };
    if statements.len() != 1 {
        return Vec::new();
    }
    let Statement::CreateTable(created) = statements.remove(0) else {
        return Vec::new();
    };
    // The columns a TABLE-LEVEL primary key covers, which admit no null either — "the
    // primary key constraint ... implies NOT NULL" — whichever of the two forms the
    // declaration wrote it in.
    let key_columns = primary_key_columns(&created.constraints);
    created
        .columns
        .iter()
        .enumerate()
        .map(|(at, column)| DeclaredColumn {
            name: column.name.value.clone(),
            position: at + 1,
            default: column_default(column),
            not_null: refuses_a_null(column)
                || key_columns
                    .iter()
                    .any(|covered| covered.eq_ignore_ascii_case(&column.name.value)),
            declared_type: column.data_type.to_string(),
        })
        .collect()
}

/// Whether a column's OWN options refuse a null: the `NOT NULL` clause, and the primary key
/// written on the column itself.
fn refuses_a_null(column: &ColumnDef) -> bool {
    column.options.iter().any(|option| {
        matches!(
            option.option,
            ColumnOption::NotNull
                | ColumnOption::Unique {
                    is_primary: true,
                    ..
                }
        )
    })
}

/// The columns a table-level `PRIMARY KEY (…)` constraint covers, and none where the
/// declaration carries no such constraint.
fn primary_key_columns(constraints: &[sqlparser::ast::TableConstraint]) -> Vec<String> {
    constraints
        .iter()
        .find_map(|constraint| match constraint {
            sqlparser::ast::TableConstraint::PrimaryKey { columns, .. } => Some(
                columns
                    .iter()
                    .map(|column| column.column.to_string().trim_matches('"').to_string())
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default()
}

/// The DEFAULT a column's declaration carries, as the declaration wrote the expression, and
/// `None` for a column that declares none.
fn column_default(column: &ColumnDef) -> Option<String> {
    column.options.iter().find_map(|option| match &option.option {
        ColumnOption::Default(expression) => Some(expression.to_string()),
        _ => None,
    })
}

/// A type as a declaration wrote it, read into what the catalog reports for it.
struct DeclaredType<'a> {
    /// The type's own name, without the modifier its declaration carried, lower-cased.
    base: String,
    /// The text between its parentheses, where it carried one.
    modifier: Option<&'a str>,
}

impl<'a> DeclaredType<'a> {
    /// The declared type `declared` read apart.
    fn of(declared: &'a str) -> Self {
        let trimmed = declared.trim();
        let (base, modifier) = match trimmed.split_once('(') {
            Some((base, rest)) => (base, rest.rsplit_once(')').map(|(modifier, _)| modifier)),
            None => (trimmed, None),
        };
        Self {
            base: base.trim().to_ascii_lowercase(),
            modifier,
        }
    }

    /// `data_type`: the type under the name PostgreSQL's own catalog holds it under, which is
    /// what the view reports rather than the spelling the declaration used.
    ///
    /// Source: the PostgreSQL documentation's "Data Types" chapter, whose table lists the
    /// aliases each type may be declared under beside the type's own name (`int`, `int4` for
    /// `integer`; `decimal` for `numeric`; `bool` for `boolean`; `float8` for `double
    /// precision`; `timestamp` for `timestamp without time zone`), and the `serial` family,
    /// which is "not a true type" but "a notational convenience for creating" an integer
    /// column of the matching width. A name this list does not carry is reported as it was
    /// declared, lower-cased: a name nothing here recognises is the client's own and is not
    /// replaced by a guess.
    fn data_type(&self) -> String {
        match self.base.as_str() {
            "smallint" | "int2" | "smallserial" | "serial2" => "smallint",
            "integer" | "int" | "int4" | "serial" | "serial4" => "integer",
            "bigint" | "int8" | "bigserial" | "serial8" => "bigint",
            "decimal" | "numeric" => "numeric",
            "real" | "float4" => "real",
            "double precision" | "float8" => "double precision",
            "boolean" | "bool" => "boolean",
            "character" | "char" | "bpchar" => "character",
            "character varying" | "varchar" => "character varying",
            "time" | "time without time zone" => "time without time zone",
            "timetz" | "time with time zone" => "time with time zone",
            "timestamp" | "timestamp without time zone" => "timestamp without time zone",
            "timestamptz" | "timestamp with time zone" => "timestamp with time zone",
            other => other,
        }
        .to_string()
    }

    /// `character_maximum_length`: "if `data_type` identifies a character or bit string type,
    /// the declared maximum length", and null for every other type.
    fn character_maximum_length(&self) -> Option<i64> {
        let data_type = self.data_type();
        if !matches!(data_type.as_str(), "character" | "character varying") {
            return None;
        }
        self.modifier?.trim().parse().ok()
    }

    /// `numeric_precision` and `numeric_scale`: the two parts of an exact numeric type's
    /// modifier, and null for every other type.
    fn numeric_precision_and_scale(&self) -> (Option<i64>, Option<i64>) {
        if self.data_type() != "numeric" {
            return (None, None);
        }
        let Some(modifier) = self.modifier else {
            return (None, None);
        };
        let (precision, scale) = match modifier.split_once(',') {
            Some((precision, scale)) => (precision, Some(scale)),
            None => (modifier, None),
        };
        (
            precision.trim().parse().ok(),
            scale.and_then(|scale| scale.trim().parse().ok()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the type table: the type as a declaration wrote it, the name the catalog
    /// holds it under, its character length, and its numeric precision and scale.
    type TypeRow = (
        &'static str,
        &'static str,
        Option<i64>,
        Option<i64>,
        Option<i64>,
    );

    /// RULE: a type is reported under the name PostgreSQL's catalog holds it under, with the
    /// length or the precision and scale its declaration carried read off it; a type nothing
    /// here recognises is reported as it was declared.
    ///
    /// The rows span what the reading generalises over: each width of integer, the aliases
    /// and the `serial` notation of one type, the character types with and without a length,
    /// the exact numeric with both parts of its modifier and with one, the two spellings of a
    /// timestamp, and a type of the client's own.
    #[test]
    fn a_declared_type_is_reported_under_the_catalogs_own_name() {
        // (declared, data_type, character_maximum_length, numeric_precision, numeric_scale)
        let rows: &[TypeRow] = &[
            ("INT", "integer", None, None, None),
            ("int4", "integer", None, None, None),
            ("INTEGER", "integer", None, None, None),
            ("serial", "integer", None, None, None),
            ("BIGINT", "bigint", None, None, None),
            ("int8", "bigint", None, None, None),
            ("bigserial", "bigint", None, None, None),
            ("smallint", "smallint", None, None, None),
            ("TEXT", "text", None, None, None),
            ("VARCHAR(64)", "character varying", Some(64), None, None),
            ("varchar", "character varying", None, None, None),
            ("CHAR(2)", "character", Some(2), None, None),
            ("NUMERIC(9,2)", "numeric", None, Some(9), Some(2)),
            ("decimal(18)", "numeric", None, Some(18), None),
            ("numeric", "numeric", None, None, None),
            ("bool", "boolean", None, None, None),
            ("float8", "double precision", None, None, None),
            (
                "TIMESTAMP",
                "timestamp without time zone",
                None,
                None,
                None,
            ),
            (
                "timestamp with time zone",
                "timestamp with time zone",
                None,
                None,
                None,
            ),
            ("bytea", "bytea", None, None, None),
            // A type of the client's own, which this list does not carry.
            ("geometry", "geometry", None, None, None),
        ];
        for (declared, data_type, length, precision, scale) in rows {
            let read = DeclaredType::of(declared);
            let actual = (
                read.data_type(),
                read.character_maximum_length(),
                read.numeric_precision_and_scale(),
            );
            assert_eq!(
                actual,
                (
                    (*data_type).to_string(),
                    *length,
                    (*precision, *scale)
                ),
                "case {declared:?}: expected {data_type:?} / {length:?} / {precision:?},{scale:?}, \
                 got {actual:?}"
            );
        }
    }

    /// RULE: the columns of a table are the columns its stored declaration declares, at the
    /// position it declares them, with the default it carries and whether it admits a null —
    /// which a primary key refuses whichever of the two forms declared it.
    #[test]
    fn a_tables_columns_are_the_ones_its_declaration_declares() {
        let declared = declared_columns(
            "CREATE TABLE widgets (id BIGINT PRIMARY KEY, label TEXT NOT NULL, \
             qty INT DEFAULT 0, note VARCHAR(16))",
        );
        let actual: Vec<(String, usize, bool, Option<String>, String)> = declared
            .into_iter()
            .map(|column| {
                (
                    column.name,
                    column.position,
                    column.not_null,
                    column.default,
                    column.declared_type,
                )
            })
            .collect();
        assert_eq!(
            actual,
            vec![
                ("id".to_string(), 1, true, None, "BIGINT".to_string()),
                ("label".to_string(), 2, true, None, "TEXT".to_string()),
                (
                    "qty".to_string(),
                    3,
                    false,
                    Some("0".to_string()),
                    "INT".to_string()
                ),
                (
                    "note".to_string(),
                    4,
                    false,
                    None,
                    "VARCHAR(16)".to_string()
                ),
            ],
            "the declaration's own columns must be read back: got {actual:?}"
        );

        // The same key declared as a CONSTRAINT of the table rather than on its columns: both
        // of those columns refuse a null too, and the one outside the key still admits one.
        let composite = declared_columns(
            "CREATE TABLE pairs (year INT, seq INT, note TEXT, PRIMARY KEY (year, seq))",
        );
        let nullable: Vec<(String, bool)> = composite
            .into_iter()
            .map(|column| (column.name, column.not_null))
            .collect();
        assert_eq!(
            nullable,
            vec![
                ("year".to_string(), true),
                ("seq".to_string(), true),
                ("note".to_string(), false),
            ],
            "case \"a table-level primary key\": its columns admit no null, got {nullable:?}"
        );
    }

    /// RULE: the relation holding a view is named for the view it serves, and a name of the
    /// standard this module does not serve resolves to nothing.
    #[test]
    fn only_a_served_view_is_held_as_a_relation() {
        for (case, view, expected) in [
            ("the tables view", "tables", Some("pg_information_schema_tables")),
            (
                "the columns view",
                "columns",
                Some("pg_information_schema_columns"),
            ),
            (
                "a name PostgreSQL folds to the same view",
                "COLUMNS",
                Some("pg_information_schema_columns"),
            ),
            ("a view of the standard this module does not serve", "routines", None),
        ] {
            let actual = held_as(view);
            assert_eq!(
                actual.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
