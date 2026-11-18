// SPDX-License-Identifier: CC0-1.0

//! The `ColumnMetadata` the Data API reports for a column — of a statement's
//! result set (`GetStatementResult`, `GetStatementResultV2`) and of a table in
//! the database's catalog (`DescribeTable`) — and the catalog read that feeds
//! the second of those.
//!
//! One projection serves both: a column is described from its TYPE (the name,
//! the precision, the scale and the signedness that type carries) and from its
//! ORIGIN (the relation it is read from, and the nullability that relation
//! declares for it), whichever operation is reporting it.

use super::*;

/// A resolved column type: the name `ColumnMetadata.typeName` reports plus the
/// size facts derived from the declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnType {
    /// The value of `ColumnMetadata.typeName`.
    pub name: String,
    /// `ColumnMetadata.length` / `precision`, from a declared `(n)` or `(p,s)`.
    pub length: i32,
    /// `ColumnMetadata.scale`, from a declared `(p,s)`.
    pub scale: i32,
}

/// `ColumnMetadata.typeName` is "The database-specific data type of the column",
/// and Redshift answers with the database's own (PostgreSQL) type name rather
/// than the SQL keyword the column was declared with. This table is that
/// mapping, for the type keywords in the Amazon Redshift *Data types*
/// documentation
/// (<https://docs.aws.amazon.com/redshift/latest/dg/c_Supported_data_types.html>),
/// whose "Data type" / "Aliases" columns give the left-hand side and whose
/// internal names give the right.
///
/// Longest keyword first is not required: lookup is an exact match on the
/// declared keyword with any `(..)` size suffix already stripped.
pub(super) const DECLARED_TYPE_NAMES: &[(&str, &str)] = &[
    ("smallint", "int2"),
    ("int2", "int2"),
    ("integer", "int4"),
    ("int", "int4"),
    ("int4", "int4"),
    ("bigint", "int8"),
    ("int8", "int8"),
    ("decimal", "numeric"),
    ("numeric", "numeric"),
    ("real", "float4"),
    ("float4", "float4"),
    ("double precision", "float8"),
    ("float8", "float8"),
    ("float", "float8"),
    ("boolean", "bool"),
    ("bool", "bool"),
    ("char", "bpchar"),
    ("character", "bpchar"),
    ("nchar", "bpchar"),
    ("bpchar", "bpchar"),
    ("varchar", "varchar"),
    ("character varying", "varchar"),
    ("nvarchar", "varchar"),
    ("text", "varchar"),
    ("date", "date"),
    ("time", "time"),
    ("time without time zone", "time"),
    ("timetz", "timetz"),
    ("time with time zone", "timetz"),
    ("timestamp", "timestamp"),
    ("timestamp without time zone", "timestamp"),
    ("timestamptz", "timestamptz"),
    ("timestamp with time zone", "timestamptz"),
    ("varbyte", "varbyte"),
    ("varbinary", "varbyte"),
    ("binary varying", "varbyte"),
    ("super", "super"),
];

/// The type names [`ColumnType`] reports for a numeric column, used for
/// `ColumnMetadata.isSigned`. Every Redshift numeric type is signed.
pub(super) const SIGNED_TYPE_NAMES: &[&str] = &["int2", "int4", "int8", "numeric", "float4", "float8"];

/// The type names [`ColumnType`] reports for an EXACT-numeric column whose
/// values carry a declared scale: `DECIMAL` and its alias `NUMERIC`, which the
/// Amazon Redshift *Numeric types* documentation
/// (<https://docs.aws.amazon.com/redshift/latest/dg/r_Numeric_types201.html>)
/// separates from the approximate `REAL`/`DOUBLE PRECISION`. The exact INTEGER
/// types are not here: they declare no scale, and `longValue` carries them
/// without loss.
pub(super) const EXACT_NUMERIC_TYPE_NAMES: &[&str] = &["numeric"];

/// The greatest scale an exact-numeric column can declare: the same
/// documentation gives `DECIMAL` a maximum precision of 38 digits, so no more
/// than 38 of them can sit to the right of the decimal point.
pub(super) const MAX_NUMERIC_SCALE: i32 = 38;

/// The type names [`ColumnType`] reports for a CURRENCY column: the warehouse's
/// money type, whose values are amounts of money. It is what
/// `ColumnMetadata.isCurrency` reports, and the type this emulator implements in
/// [`warehouse_types`].
pub(super) const CURRENCY_TYPE_NAMES: &[&str] = &[warehouse_types::MONEY_TYPE_NAME];

/// The type names [`ColumnType`] reports for a character column. Redshift
/// compares `CHAR` and `VARCHAR` values case-sensitively, which is what
/// `ColumnMetadata.isCaseSensitive` reports.
pub(super) const CASE_SENSITIVE_TYPE_NAMES: &[&str] = &["bpchar", "varchar", "super"];

impl ColumnType {
    /// Whether this column is an EXACT-numeric one, whose values are reported
    /// as decimal text so that neither the declared scale nor an exact digit is
    /// lost to binary floating point.
    pub(super) fn is_exact_numeric(&self) -> bool {
        EXACT_NUMERIC_TYPE_NAMES.contains(&self.name.as_str())
    }

    /// How many digits this column declares after the decimal point, held to
    /// the greatest scale an exact-numeric column can have
    /// ([`MAX_NUMERIC_SCALE`]) so that a declaration naming more than Redshift
    /// allows cannot widen a rendered value without bound.
    pub(super) fn declared_scale(&self) -> usize {
        usize::try_from(self.scale.clamp(0, MAX_NUMERIC_SCALE)).unwrap_or(0)
    }

    /// Resolve a column's declared type keyword — `VARCHAR(16)`,
    /// `NUMERIC(10,2)`, `BIGINT` — through [`DECLARED_TYPE_NAMES`]. A keyword
    /// the table does not carry keeps its own spelling: it is still the
    /// database-specific name of that column's type.
    pub(super) fn from_declared(declared: &str) -> Self {
        let declared = declared.trim().to_ascii_lowercase();
        let (keyword, args) = match declared.split_once('(') {
            Some((keyword, rest)) => (keyword.trim(), rest.trim_end_matches(')')),
            None => (declared.as_str(), ""),
        };
        let mut sizes = args
            .split(',')
            .map(|size| size.trim().parse::<i32>().unwrap_or(0));
        let length = sizes.next().unwrap_or(0);
        let scale = sizes.next().unwrap_or(0);
        // A keyword outside the table is reported verbatim: it is still the
        // database-specific name of that column's type, and inventing a
        // different one would misreport it.
        let name = DECLARED_TYPE_NAMES
            .iter()
            .find(|(candidate, _)| *candidate == keyword)
            .map_or_else(|| keyword.to_string(), |(_, name)| (*name).to_string());
        Self {
            name,
            length,
            scale,
        }
    }

    /// Resolve a column with no declared type — an expression such as
    /// `SELECT 1` — from the value the engine produced for it.
    ///
    /// An INTEGER value takes the NARROWEST Redshift integer type whose range
    /// holds it, which is how a Redshift (PostgreSQL-derived) integer constant is
    /// typed: `int4` while the value fits the 4-byte `INTEGER` range
    /// (-2147483648 to +2147483647) and `int8` for anything past it, the ranges
    /// the Amazon Redshift *Data types* documentation publishes for `INTEGER` and
    /// `BIGINT`.
    pub(super) fn from_value(value: rusqlite::types::ValueRef<'_>) -> Option<Self> {
        use rusqlite::types::ValueRef;
        let name = match value {
            // NULL says nothing about the column's type.
            ValueRef::Null => return None,
            ValueRef::Integer(integer) => {
                if i32::try_from(integer).is_ok() {
                    "int4"
                } else {
                    "int8"
                }
            }
            ValueRef::Real(_) => "float8",
            ValueRef::Text(_) => "varchar",
            ValueRef::Blob(_) => "varbyte",
        };
        Some(Self {
            name: name.to_string(),
            length: 0,
            scale: 0,
        })
    }

    /// The type an expression column must report once it has produced one more
    /// `value`: the type that holds EVERY value the column carried.
    ///
    /// Only widening happens, and only within the integer types, because that is
    /// the only family [`Self::from_value`] can infer differently for two values
    /// of one column: a column whose values all fit `INTEGER` is `int4`, and one
    /// value past that range makes the whole column `int8`, whichever row it
    /// arrives in. The first non-NULL value decides anything else, and a NULL
    /// says nothing.
    pub(super) fn widened_with(current: Option<Self>, value: rusqlite::types::ValueRef<'_>) -> Option<Self> {
        let observed = Self::from_value(value);
        match (current, observed) {
            (None, observed) => observed,
            (Some(current), None) => Some(current),
            (Some(current), Some(observed)) => {
                match (integer_width(&current.name), integer_width(&observed.name)) {
                    (Some(held), Some(seen)) if seen > held => Some(observed),
                    _ => Some(current),
                }
            }
        }
    }
}

/// The integer type names [`ColumnType::from_value`] infers, NARROWEST FIRST — the
/// order of the ranges the Amazon Redshift *Data types* documentation publishes
/// for `INTEGER` (`int4`, 4 bytes) and `BIGINT` (`int8`, 8 bytes).
///
/// The ordering lives HERE rather than in a pairwise name comparison, so widening
/// an inferred column keeps working when another inferred width joins the list.
pub(super) const INFERRED_INTEGER_WIDTHS: &[&str] = &["int4", "int8"];

/// How wide an inferred integer type is, as its position in
/// [`INFERRED_INTEGER_WIDTHS`]; `None` for a type that is not one of them.
pub(super) fn integer_width(name: &str) -> Option<usize> {
    INFERRED_INTEGER_WIDTHS
        .iter()
        .position(|candidate| *candidate == name)
}

/// What a relation declares about ONE of its columns, beside the type the
/// engine itself reports for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredColumn {
    /// Whether the relation allows a NULL in the column: false for one it
    /// declares `NOT NULL`.
    pub nullable: bool,
    /// `ColumnMetadata.columnDefault`, "the default value of the column": the
    /// expression the column's `DEFAULT` names, or `None` for a column that
    /// declares none.
    pub column_default: Option<String>,
}

impl Default for DeclaredColumn {
    /// What is reported for a column no declaration was read for: the weakest
    /// claim — a nullable column with no default.
    fn default() -> Self {
        Self {
            nullable: true,
            column_default: None,
        }
    }
}

/// Where a reported column COMES FROM: the relation that includes it, and what
/// that relation declares about it.
///
/// A column of a stored table has one; a column a statement computes — a
/// literal, an expression, an aggregate — has none, and reports neither an
/// origin nor anything its relation would have declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnOrigin {
    /// `ColumnMetadata.tableName`: "the name of the table that includes the
    /// column". Its schema is the database's default one ([`DEFAULT_SCHEMA`]),
    /// which `schemaName` reports.
    pub table: String,
    /// What that relation declares about the column.
    pub declared: DeclaredColumn,
}

/// How wide the widest value of a type that declares NO size of its own is, in
/// characters — the "column length" `precision` reports for such a column.
///
/// Each width is that type's longest rendering, from the ranges and the
/// resolutions the Amazon Redshift *Data types* documentation
/// (<https://docs.aws.amazon.com/redshift/latest/dg/c_Supported_data_types.html>)
/// publishes for it.
const FIXED_TYPE_WIDTHS: &[(&str, i32)] = &[
    // The integer ranges, in digits: -32768 to +32767, -2147483648 to
    // +2147483647, and -9223372036854775808 to +9223372036854775807.
    ("int2", 5),
    ("int4", 10),
    ("int8", 19),
    // A boolean renders as one character, `t` or `f`.
    ("bool", 1),
    // The significant digits each approximate-numeric type carries: "6
    // significant digits of precision" for REAL and "15 significant digits of
    // precision" for DOUBLE PRECISION.
    ("float4", 6),
    ("float8", 15),
    // `4713-01-01 BC`: the widest date, with its era.
    ("date", 13),
    // `00:00:00.000000`: a time of day at the microsecond resolution the
    // datetime types hold.
    ("time", 15),
    // The same, with the `+00:00` offset a time-zone-aware value carries.
    ("timetz", 21),
    // `4713-01-01 00:00:00.000000 BC`: the widest date, a time of day at that
    // resolution, and the era.
    ("timestamp", 29),
    // The same with the offset: `4713-01-01 00:00:00.000000+00:00 BC`.
    ("timestamptz", 35),
];

/// The size a type that CAN declare one reports when the declaration named
/// none: the default the same documentation gives that type — a default length
/// of 1 character for `CHAR` and 256 for `VARCHAR`, a default precision of 18
/// digits for `DECIMAL`, and a default size of 64000 bytes for `VARBYTE`.
const DEFAULT_DECLARED_SIZES: &[(&str, i32)] = &[
    ("bpchar", 1),
    ("varchar", 256),
    ("numeric", 18),
    ("varbyte", 64000),
];

/// The type names of the columns whose `scale` is a FRACTIONAL-SECONDS
/// precision rather than a declared number of decimal places.
const DATETIME_TYPE_NAMES: &[&str] = &["time", "timetz", "timestamp", "timestamptz"];

/// The fractional-seconds precision Redshift's datetime types hold: the same
/// *Data types* documentation gives `TIME`, `TIMETZ`, `TIMESTAMP` and
/// `TIMESTAMPTZ` a resolution of microseconds, which is six digits after the
/// decimal point. It is the scale such a column reports, and the most one can
/// declare.
const MICROSECOND_SCALE: i32 = 6;

/// `ColumnMetadata.precision` for one column: "the precision value of a decimal
/// number column, or the column length for a non-numeric column".
///
/// A type that declares a size carries the DECLARED one — the precision of a
/// `DECIMAL(p,s)`, the width of a `VARCHAR(n)` — or that type's default size
/// ([`DEFAULT_DECLARED_SIZES`]) when the declaration named none. A type that
/// declares no size of its own carries the width of its widest value
/// ([`FIXED_TYPE_WIDTHS`]).
fn precision_of(column_type: &ColumnType) -> i32 {
    let name = column_type.name.as_str();
    if let Some((_, width)) = FIXED_TYPE_WIDTHS
        .iter()
        .find(|(candidate, _)| *candidate == name)
    {
        return *width;
    }
    if column_type.length > 0 {
        return column_type.length;
    }
    DEFAULT_DECLARED_SIZES
        .iter()
        .find(|(candidate, _)| *candidate == name)
        .map_or(0, |(_, size)| *size)
}

/// `ColumnMetadata.scale` for one column: "the scale value of a decimal number
/// column".
///
/// A datetime column's scale is its fractional-seconds precision — the digits
/// its declaration names, and [`MICROSECOND_SCALE`] when it names none, which is
/// the resolution those types hold. Every other column reports the scale its
/// declaration gives it, which only an exact-numeric `(p,s)` has.
fn scale_of(column_type: &ColumnType) -> i32 {
    if DATETIME_TYPE_NAMES.contains(&column_type.name.as_str()) {
        return match column_type.length {
            0 => MICROSECOND_SCALE,
            declared => declared.clamp(0, MICROSECOND_SCALE),
        };
    }
    column_type.scale
}

/// Build the modelled `ColumnMetadata` for one column: `name` as the result set
/// or the catalog spells it, the type it resolved to, and its `origin` when it
/// is a column of a relation.
pub(super) fn column_metadata(
    name: &str,
    column_type: Option<&ColumnType>,
    origin: Option<&ColumnOrigin>,
) -> ColumnMetadata {
    let type_name = column_type.map(|column_type| column_type.name.clone());
    let signed = column_type
        .is_some_and(|column_type| SIGNED_TYPE_NAMES.contains(&column_type.name.as_str()));
    let case_sensitive = column_type
        .is_some_and(|column_type| CASE_SENSITIVE_TYPE_NAMES.contains(&column_type.name.as_str()));
    let currency = column_type
        .is_some_and(|column_type| CURRENCY_TYPE_NAMES.contains(&column_type.name.as_str()));
    ColumnMetadata {
        // "A value that indicates whether the column is nullable": 0 for a
        // column its relation declares NOT NULL. Every other column is
        // nullable — one its relation allows a NULL in, and one a statement
        // computes, whose expression can always produce NULL.
        nullable: i32::from(origin.is_none_or(|origin| origin.declared.nullable)),
        is_case_sensitive: case_sensitive,
        // "A value that indicates whether the column contains currency
        // values": a column of the warehouse's money type does, and no other
        // type this emulator reports holds an amount of money.
        is_currency: currency,
        is_signed: signed,
        // Redshift reports the column's output name as both name and label; a
        // statement that aliases a column changes both together.
        label: Some(name.to_string()),
        name: Some(name.to_string()),
        precision: column_type.map_or(0, precision_of),
        scale: column_type.map_or(0, scale_of),
        // A column's width is what `precision` carries — "the precision value
        // of a decimal number column, or the column length for a non-numeric
        // column" — so it is reported there and `length` carries nothing.
        length: 0,
        type_name,
        // "The name of the schema that contains the table that includes the
        // column" and "the name of the table that includes the column": a
        // column read from a relation reports both — the engine holds a
        // relation of a schema under its qualified name, which is split back
        // into the two ([`split_relation`]) — and one a statement computes
        // belongs to no table and reports neither.
        schema_name: origin.map(|origin| split_relation(&origin.table).0.to_string()),
        table_name: origin.map(|origin| split_relation(&origin.table).1.to_string()),
        // "The default value of the column": what the relation that includes it
        // declares as its `DEFAULT`, and nothing for a column that declares
        // none or belongs to no relation.
        column_default: origin.and_then(|origin| origin.declared.column_default.clone()),
    }
}

/// The DEFAULT schema a stored table is reported in.
///
/// Redshift creates a user table in `public` unless the statement names another
/// schema, and this emulator's engine holds a table of the default schema under
/// its bare name, so `public` is the schema the catalog reports such a table in.
/// Nothing is invented by it: a table a caller reaches through the Data API's
/// metadata operations with no schema of its own is in the database's default
/// schema.
pub const DEFAULT_SCHEMA: &str = "public";

/// The schema and the table one entry of the engine's catalog names.
///
/// This emulator's engine holds a database's relations in one namespace, under
/// the name the schema's tables are held by: one of the default schema under its
/// bare name, and one of any other schema the database holds under
/// `<schema>.<table>` (see [`super::redshift_sql::engine_table_name`]). So an
/// entry carrying a qualifier is in the schema it names, and one without is in
/// the default schema.
///
/// The one place that split is made, so every reader of the catalog — a
/// column's `schemaName`/`tableName`, `ListTables`' entries, the system view of
/// a database's tables — names a relation's schema and table the same way.
pub fn split_relation(relation: &str) -> (&str, &str) {
    match relation.split_once('.') {
        Some((schema, table)) => (schema, table),
        None => (DEFAULT_SCHEMA, relation),
    }
}

/// One table of a database's catalog: its name, the kind of relation it is, and
/// the columns it declares in declaration order.
#[derive(Debug, Clone)]
pub struct TableColumns {
    /// The table's name, as the engine's catalog spells it.
    pub table: String,
    /// What kind of relation it is, in the spelling `TableMember.type`
    /// documents: "TABLE" or "VIEW".
    pub kind: &'static str,
    /// One entry per declared column, in declaration order.
    pub columns: Vec<ColumnMetadata>,
}

/// The `TableMember.type` of a relation the engine's catalog holds as a table,
/// out of the values that member documents ("TABLE, VIEW, SYSTEM TABLE, GLOBAL
/// TEMPORARY, LOCAL TEMPORARY, ALIAS, and SYNONYM").
pub const TABLE_KIND: &str = "TABLE";

/// The same for one the catalog holds as a view.
pub const VIEW_KIND: &str = "VIEW";

/// Which relations a catalogue read describes: the names the request narrows it
/// to.
///
/// A read describes only the relations the request ASKED about, so the work one
/// metadata request costs is bounded by what it named rather than by how many
/// relations the database holds: the filter is applied to a relation's name
/// before anything is read about that relation.
#[derive(Debug, Default, Clone)]
pub struct CatalogFilter {
    /// The schema a relation must be in, matched as the engine matches an
    /// unquoted identifier — without regard to case.
    pub schema: Option<String>,
    /// The name a relation must carry, matched the same way.
    pub table: Option<String>,
    /// A pattern the relation's schema must match, with the two wild cards the
    /// Data API's catalogue patterns document (`%` any substring, `_` any one
    /// character).
    pub schema_pattern: Option<String>,
    /// The same, for the relation's own name.
    pub table_pattern: Option<String>,
}

impl CatalogFilter {
    /// Whether this filter describes `relation`, the name the engine's catalogue
    /// holds it under.
    fn admits(&self, relation: &str) -> bool {
        let (schema, table) = split_relation(relation);
        named(self.schema.as_deref(), schema)
            && named(self.table.as_deref(), table)
            && matched(self.schema_pattern.as_deref(), schema)
            && matched(self.table_pattern.as_deref(), table)
    }
}

/// Whether a name passes a filter's exact name: one the request did not give
/// narrows nothing.
fn named(wanted: Option<&str>, name: &str) -> bool {
    wanted.is_none_or(|wanted| wanted.eq_ignore_ascii_case(name))
}

/// The same for a `%`/`_` pattern, matched by the one matcher this crate has for
/// those wild cards.
fn matched(pattern: Option<&str>, name: &str) -> bool {
    pattern.is_none_or(|pattern| super::session_settings::wild_card_matches(pattern, name))
}

/// Read the catalog of the database `handle` guards: the relations `filter`
/// describes, and the columns each one declares.
///
/// BLOCKING for the same reason [`run_on`] is, and bounded the same way: the
/// catalog is read with real SQL on the engine's own `sqlite_master`, and each
/// table's columns come from the engine's answer to a zero-row query over it,
/// so a reported column is one the engine really declares. The read stops at
/// the same bound a statement's execution does — its deadline and the
/// cancellation flag — which is read between relations as well as inside the
/// engine, because describing one relation steps no statement for the engine's
/// progress handler to run.
pub fn catalog_on(
    handle: &DatabaseHandle,
    filter: &CatalogFilter,
    timeout: Duration,
    bound: &ExecutionBound,
) -> Result<Vec<TableColumns>, String> {
    let mut database = handle
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let connection = &mut database.connection;

    let guard = std::sync::Arc::clone(&bound.0);
    let handler_guard = std::sync::Arc::clone(&guard);
    connection
        .progress_handler(
            PROGRESS_HANDLER_OPS,
            Some(move || handler_guard.should_interrupt()),
        )
        .map_err(|error| scrub_paths(&error.to_string()))?;
    guard.arm(timeout, step_budget(timeout));
    let read = read_catalog(connection, filter, &guard);
    guard.disarm();
    read
}

/// The catalog read [`catalog_on`] bounds: the engine's own list of relations,
/// narrowed to the ones `filter` describes, and the columns each of them
/// declares.
fn read_catalog(
    connection: &rusqlite::Connection,
    filter: &CatalogFilter,
    guard: &ExecutionGuard,
) -> Result<Vec<TableColumns>, String> {
    let relations = listed_relations(connection, filter)
        .map_err(|error| scrub_paths(&error.to_string()))?;

    let mut described = Vec::with_capacity(relations.len());
    for (name, definition, kind) in relations {
        // The bound this read runs under, between relations: the engine's
        // progress handler cannot see this loop, because describing a relation
        // steps no statement, so a read past its deadline or one a caller gave
        // up on stops HERE rather than working through the rest of the
        // catalogue.
        if guard.stopped() {
            return Err(CATALOG_READ_STOPPED.to_string());
        }
        // What the relation DECLARES about each of its columns, read from its
        // own definition: whether a NULL is allowed in it, and the default it
        // names.
        let declared = super::column_origins::declared_columns(definition.as_deref());
        // A zero-row query over the table: the engine answers with the table's
        // columns and their declared types, which is where every reported
        // column comes from.
        let quoted = name.replace('"', "\"\"");
        let probe = connection
            .prepare(&format!("SELECT * FROM \"{quoted}\" LIMIT 0"))
            .map_err(|error| scrub_paths(&error.to_string()))?;
        let columns = probe
            .columns()
            .map(|column| {
                let column_type = column
                    .decl_type()
                    .map(|decl| ColumnType::from_declared(&decl.to_ascii_lowercase()));
                let origin = ColumnOrigin {
                    table: name.clone(),
                    declared: declared
                        .get(&column.name().to_ascii_lowercase())
                        .cloned()
                        .unwrap_or_default(),
                };
                column_metadata(column.name(), column_type.as_ref(), Some(&origin))
            })
            .collect();
        described.push(TableColumns {
            table: name,
            kind: if kind.eq_ignore_ascii_case("view") {
                VIEW_KIND
            } else {
                TABLE_KIND
            },
            columns,
        });
    }
    Ok(described)
}

/// What a catalogue read that its bound stopped reports.
///
/// The read is bounded exactly as a statement's execution is, so a read the
/// engine's deadline or a caller's cancellation stopped answers as a read the
/// database could not finish rather than as an empty catalogue.
const CATALOG_READ_STOPPED: &str =
    "the catalog read stopped at the deadline this database's reads are bounded by";

/// The relations the engine's catalogue holds that `filter` describes, in name
/// order, each with the definition it was created by — which is where its
/// declared nullability and defaults are read from — and what kind of relation
/// it is.
///
/// Views are relations a client can read columns from, so they are listed too;
/// the engine's own internal objects are not part of the database's catalogue.
/// The filter is applied HERE, before anything is read about a relation.
fn listed_relations(
    connection: &rusqlite::Connection,
    filter: &CatalogFilter,
) -> rusqlite::Result<Vec<(String, Option<String>, String)>> {
    let mut listing = connection.prepare(
        "SELECT name, sql, type FROM sqlite_master WHERE type IN ('table', 'view') \
         AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )?;
    let mut relations: Vec<(String, Option<String>, String)> = Vec::new();
    let mut rows = listing.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(0)?;
        if filter.admits(&name) {
            relations.push((name, row.get(1)?, row.get(2)?));
        }
    }
    Ok(relations)
}
