// SPDX-License-Identifier: CC0-1.0

//! The values a MySQL column admits, and the refusals a server in STRICT MODE answers a
//! statement that writes one it does not with.
//!
//! A MySQL column's declared type says how much the column holds — "a VARCHAR(M) column …
//! M represents the maximum column length in characters" (Reference Manual, "String Data
//! Types"), and each integer type has a published minimum and maximum ("Integer Types (Exact
//! Value)"). What a server does with a value that does not fit is what its `sql_mode` says.
//!
//! With STRICT MODE in force — which the default `sql_mode` of an Amazon RDS for MySQL
//! instance carries — "an invalid or missing value" makes the server REFUSE the statement
//! rather than adjust the value, and the refusal is one of the two a client branches on:
//!
//! * `ER_DATA_TOO_LONG` (1406, `22001`), `Data too long for column '<name>' at row <n>`, for a
//!   string longer than the column's declared length;
//! * `ER_WARN_DATA_OUT_OF_RANGE` (1264, `22003`), `Out of range value for column '<name>' at
//!   row <n>`, for a number outside the declared type's range.
//!
//! WITHOUT strict mode the same statement runs, with the value the column does admit — the
//! string cut to the declared length, the number held to the end of the range — and the server
//! says what it did in the session's diagnostics area ([`super::mysql_diagnostics`]):
//! `ER_WARN_DATA_TRUNCATED` (1265) for the string, the same 1264 for the number. That is the
//! value the column HOLDS from then on, which is what a client reads back.
//!
//! The columns come from the table's own declaration as the engine stored it
//! ([`super::mysql_columns`]); the values come from the statement the client sent. Only a
//! value the statement itself carries is read — what an expression or a query computes is the
//! engine's, and this adds no bound of its own to it.

use sqlparser::ast::{Expr, SetExpr, Statement, Value as SqlValue};

use super::mysql_columns::{one_statement, unqualified, DeclaredColumn};
use super::sql_engine::{EngineError, EngineErrorKind, InstanceDatabase};
use super::wire_session::SessionFacts;
use super::EngineWire;

/// The `sql_mode` values that make a server REFUSE a statement whose value does not fit the
/// column rather than adjust the value: "strict mode controls how MySQL handles invalid or
/// missing values … Strict mode is in effect if either STRICT_ALL_TABLES or
/// STRICT_TRANS_TABLES is enabled".
const STRICT_MODES: [&str; 2] = ["STRICT_TRANS_TABLES", "STRICT_ALL_TABLES"];

/// Whether the session's statements are held to the column's declared type: what its own
/// `sql_mode` says, else the server's own default.
fn strict(facts: &SessionFacts) -> bool {
    strict_mode(facts.setting("sql_mode").as_deref())
}

/// Whether `mode` is a strict one, or `None` for a session that has set none — in which case
/// the server's own default decides.
///
/// The default `sql_mode` of MySQL 5.7 and later includes `STRICT_TRANS_TABLES`, so a session
/// that has set none runs in strict mode; a session that sets a mode without either strict
/// value has left it.
fn strict_mode(mode: Option<&str>) -> bool {
    let Some(mode) = mode else {
        return true;
    };
    let mode = mode.to_ascii_uppercase();
    STRICT_MODES.iter().any(|strict| mode.contains(strict))
}

/// The statement `sql` as the engine is to run it, where a column of its table does not admit
/// a value it writes: `Err` for the REFUSAL a server in strict mode answers it with, and
/// `Ok(Some(..))` for the statement a server not in strict mode runs instead. `Ok(None)` where
/// every value it writes fits.
///
/// The statements read are the ones that write the value of a NAMED column as the client wrote
/// it: `INSERT` (and the `REPLACE` that is one) and `UPDATE`.
pub fn fitted(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &str,
    sql: &str,
) -> Result<Option<Adjustment>, EngineError> {
    if facts.wire != EngineWire::MySql {
        return Ok(None);
    }
    let strict = strict(facts);
    let Some(mut statement) = one_statement(sql) else {
        return Ok(None);
    };
    let Some(written) = written_table(&statement) else {
        return Ok(None);
    };
    let declared = super::mysql_columns::declared(database, facts, target, &written);
    if declared.is_empty() {
        return Ok(None);
    }
    let mut warnings: Vec<(u16, String)> = Vec::new();
    written_values(&mut statement, &declared, |column, row, value| {
        let Some(does_not_fit) = does_not_fit(value, column) else {
            return Ok(());
        };
        if strict {
            return Err(does_not_fit.refusal(column, row));
        }
        // Not strict mode: the column takes the value it admits, and the server says what it
        // did to the one it was given.
        does_not_fit.fit(value);
        warnings.push((does_not_fit.warning(), does_not_fit.sentence(column, row)));
        Ok(())
    })?;
    if warnings.is_empty() {
        return Ok(None);
    }
    Ok(Some(Adjustment {
        sql: statement.to_string(),
        warnings,
    }))
}

/// A statement that runs with a value a column DID adjust: the statement as the engine is
/// asked to run it, and what the server has to say about each value it adjusted.
#[derive(Debug, Clone)]
pub struct Adjustment {
    pub sql: String,
    /// The server error number and the sentence of each condition, in the order the values
    /// were written.
    pub warnings: Vec<(u16, String)>,
}

/// The table the statement writes rows of, or `None` for a statement that writes none — and
/// for one whose rows come from more than one table, whose columns are not this table's.
fn written_table(statement: &Statement) -> Option<String> {
    match statement {
        Statement::Insert(insert) => Some(unqualified(&insert.table.to_string())),
        Statement::Update { table, .. } if table.joins.is_empty() => {
            Some(unqualified(&table.relation.to_string()))
        }
        _ => None,
    }
}

/// Hand `each` every value the statement writes for a column of `declared`: the column, the
/// ROW of the statement the value was written in, and the value itself, which `each` may
/// change.
///
/// An `INSERT` writes one value per column per row of its `VALUES` list, under the columns it
/// named or — where it named none — the table's own, in the order they were declared. An
/// `UPDATE` writes one value per assignment, the same value for every row it changes, which is
/// why MySQL names row 1 for it. Rows a QUERY produces are not read: what it writes into a
/// column is that query's own value and not something the statement carries.
fn written_values(
    statement: &mut Statement,
    declared: &[DeclaredColumn],
    mut each: impl FnMut(&DeclaredColumn, usize, &mut Expr) -> Result<(), EngineError>,
) -> Result<(), EngineError> {
    let named = |written: &str| {
        declared
            .iter()
            .find(|column| column.name.eq_ignore_ascii_case(written))
            .cloned()
    };
    match statement {
        Statement::Insert(insert) => {
            let columns: Vec<Option<DeclaredColumn>> = if insert.columns.is_empty() {
                declared.iter().cloned().map(Some).collect()
            } else {
                insert
                    .columns
                    .iter()
                    .map(|column| named(&column.value))
                    .collect()
            };
            let Some(source) = insert.source.as_mut() else {
                return Ok(());
            };
            let SetExpr::Values(values) = source.body.as_mut() else {
                return Ok(());
            };
            for (row, written) in values.rows.iter_mut().enumerate() {
                for (at, value) in written.iter_mut().enumerate() {
                    if let Some(Some(column)) = columns.get(at) {
                        each(column, row + 1, value)?;
                    }
                }
            }
        }
        Statement::Update { assignments, .. } => {
            for assignment in assignments.iter_mut() {
                if let Some(column) = named(&unqualified(&assignment.target.to_string())) {
                    each(&column, 1, &mut assignment.value)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// What a column's declared type admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Admits {
    /// At most this many CHARACTERS, which is the length a `CHAR`/`VARCHAR` declaration
    /// carries.
    Characters(usize),
    /// A whole number in this range, which is what an integer type admits.
    WholeNumbers(i128, i128),
}

/// The string types whose declaration carries the length in CHARACTERS: "the CHAR and VARCHAR
/// types are declared with a length that indicates the maximum number of characters you want
/// to store".
///
/// The byte-string types (`BINARY`, `VARBINARY`) are deliberately absent: their declared
/// length is in BYTES, which is a different count from the one the characters of a value give,
/// and this holds a value to no bound it does not read correctly.
const SIZED_STRING_TYPES: [&str; 2] = ["char", "varchar"];

/// The range each MySQL integer type admits, signed and unsigned.
///
/// Source: the Reference Manual's "Integer Types (Exact Value) - INTEGER, INT, SMALLINT,
/// TINYINT, MEDIUMINT, BIGINT" table, which publishes the minimum and maximum of each —
/// `INTEGER` being the published synonym of `INT`.
const INTEGER_RANGES: [(&str, i128, i128, i128); 6] = [
    // (type, signed minimum, signed maximum, unsigned maximum)
    ("tinyint", -128, 127, 255),
    ("smallint", -32_768, 32_767, 65_535),
    ("mediumint", -8_388_608, 8_388_607, 16_777_215),
    ("int", -2_147_483_648, 2_147_483_647, 4_294_967_295),
    ("integer", -2_147_483_648, 2_147_483_647, 4_294_967_295),
    (
        "bigint",
        -9_223_372_036_854_775_808,
        9_223_372_036_854_775_807,
        18_446_744_073_709_551_615,
    ),
];

/// What the declared type `declared` admits, or `None` for a type this endpoint holds a value
/// to no bound of its own for.
fn admits(declared: &str) -> Option<Admits> {
    let lowered = declared.to_ascii_lowercase();
    let base = lowered
        .split(['(', ' '])
        .next()
        .unwrap_or(&lowered)
        .trim()
        .to_string();
    if SIZED_STRING_TYPES.iter().any(|sized| *sized == base) {
        // The length the declaration carries, which is the only bound a string column has
        // here; a declaration that carries none bounds nothing.
        return lowered
            .split_once('(')
            .and_then(|(_, rest)| rest.split(')').next())
            .and_then(|length| length.trim().parse::<usize>().ok())
            .map(Admits::Characters);
    }
    let unsigned = lowered.contains("unsigned");
    INTEGER_RANGES
        .iter()
        .find(|(name, ..)| *name == base)
        .map(|(_, least, greatest, unsigned_greatest)| {
            if unsigned {
                Admits::WholeNumbers(0, *unsigned_greatest)
            } else {
                Admits::WholeNumbers(*least, *greatest)
            }
        })
}

/// HOW a value does not fit the column it was written for, and the value the column admits
/// in its place.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DoesNotFit {
    /// Longer than the column's declared length: the column holds the first characters of it.
    TooLong(String),
    /// Outside the declared type's range: the column holds the nearer end of that range.
    OutOfRange(i128),
}

impl DoesNotFit {
    /// The REFUSAL a server in strict mode answers the statement with, under the server error
    /// number and in the words this condition carries.
    fn refusal(&self, column: &DeclaredColumn, row: usize) -> EngineError {
        match self {
            // `ER_DATA_TOO_LONG`.
            Self::TooLong(_) => EngineError::new(
                EngineErrorKind::DataError,
                format!("Data too long for column '{}' at row {row}", column.name),
            )
            .with_mysql_error(1406, "22001"),
            // `ER_WARN_DATA_OUT_OF_RANGE`, which is also the number the condition carries as a
            // warning: the same condition, reported as what it stopped or as what it changed.
            Self::OutOfRange(_) => EngineError::new(
                EngineErrorKind::NumericValueOutOfRange,
                self.sentence(column, row),
            )
            .with_mysql_error(1264, "22003"),
        }
    }

    /// The server error number the WARNING carries where the value was adjusted instead:
    /// `ER_WARN_DATA_TRUNCATED` for a value cut to the column's length, and
    /// `ER_WARN_DATA_OUT_OF_RANGE` for a number held to the type's range.
    fn warning(&self) -> u16 {
        match self {
            Self::TooLong(_) => 1265,
            Self::OutOfRange(_) => 1264,
        }
    }

    /// The sentence the condition carries as a WARNING: what the server did to the value it
    /// was given.
    fn sentence(&self, column: &DeclaredColumn, row: usize) -> String {
        match self {
            Self::TooLong(_) => {
                format!("Data truncated for column '{}' at row {row}", column.name)
            }
            Self::OutOfRange(_) => format!(
                "Out of range value for column '{}' at row {row}",
                column.name
            ),
        }
    }

    /// Write the value the column DOES admit in place of the one it was given, which is what
    /// a server not in strict mode stores.
    fn fit(&self, value: &mut Expr) {
        match self {
            Self::TooLong(held) => {
                if let Expr::Value(written) = value {
                    written.value = SqlValue::SingleQuotedString(held.clone());
                }
            }
            // A negative number is written as the operator applied to a number, so the whole
            // expression is replaced by the value the column admits, sign and all.
            Self::OutOfRange(held) => match value {
                Expr::Value(written) => {
                    written.value = SqlValue::Number(held.to_string(), false);
                }
                Expr::UnaryOp { expr, .. } => {
                    if let Expr::Value(written) = expr.as_ref() {
                        let mut written = written.clone();
                        written.value = SqlValue::Number(held.to_string(), false);
                        *value = Expr::Value(written);
                    }
                }
                _ => {}
            },
        }
    }
}

/// How the value written for `column` does not fit it, or `None` where the column admits it.
fn does_not_fit(value: &Expr, column: &DeclaredColumn) -> Option<DoesNotFit> {
    // The column the engine generates keys for is declared to it as its own rowid type, so
    // its declaration no longer carries the width the client wrote: the engine's own bound on
    // a key is what holds for it.
    if super::mysql_columns::key_column(std::slice::from_ref(column)).is_some() {
        return None;
    }
    let written = written_value(value)?;
    match admits(&column.declared_type)? {
        Admits::Characters(length) => (written.chars().count() > length).then(|| {
            DoesNotFit::TooLong(written.chars().take(length).collect())
        }),
        Admits::WholeNumbers(least, greatest) => {
            let number = written.trim().parse::<i128>().ok()?;
            (number < least || number > greatest)
                .then(|| DoesNotFit::OutOfRange(number.clamp(least, greatest)))
        }
    }
}

/// The value the statement itself carries for a column, as its text, or `None` for one the
/// statement does not carry — `NULL`, and anything an expression or a query computes, which
/// is the engine's own to produce.
fn written_value(value: &Expr) -> Option<String> {
    match value {
        Expr::Value(value) => match &value.value {
            SqlValue::Number(number, _) => Some(number.clone()),
            SqlValue::SingleQuotedString(text)
            | SqlValue::DoubleQuotedString(text)
            | SqlValue::EscapedStringLiteral(text) => Some(text.clone()),
            _ => None,
        },
        // A negative number is the operator applied to the number it is written with.
        Expr::UnaryOp { op, expr } => Some(format!("{op}{}", written_value(expr)?)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one value an `INSERT` writes, as the parser reads it: how every row below names the
    /// value under test.
    fn one_written_value(written: &str) -> Expr {
        let sql = format!("INSERT INTO gauges (label) VALUES ({written})");
        let Some(Statement::Insert(insert)) = one_statement(&sql) else {
            panic!("{sql:?} must read as an insert");
        };
        let SetExpr::Values(values) = insert
            .source
            .expect("the insert carries its values")
            .body
            .as_ref()
            .clone()
        else {
            panic!("the insert carries a values list");
        };
        values
            .rows
            .first()
            .and_then(|row| row.first())
            .expect("the row carries its one value")
            .clone()
    }

    /// RULE: what a declared type admits is the length its declaration carries, for a string
    /// column, and the published range of the type, for an integer one.
    #[test]
    fn what_a_declared_type_admits_is_what_its_declaration_says() {
        let rows: &[(&str, &str, Option<Admits>)] = &[
            // (case, the declared type, what it admits)
            ("a varchar", "varchar(5)", Some(Admits::Characters(5))),
            ("a char", "char(2)", Some(Admits::Characters(2))),
            (
                "a byte string, whose length is not one this reads",
                "varbinary(16)",
                None,
            ),
            ("a string with no length", "text", None),
            (
                "a signed tinyint",
                "tinyint",
                Some(Admits::WholeNumbers(-128, 127)),
            ),
            (
                "an unsigned tinyint",
                "tinyint unsigned",
                Some(Admits::WholeNumbers(0, 255)),
            ),
            (
                "a smallint",
                "smallint",
                Some(Admits::WholeNumbers(-32_768, 32_767)),
            ),
            (
                "a mediumint",
                "mediumint",
                Some(Admits::WholeNumbers(-8_388_608, 8_388_607)),
            ),
            (
                "an int with a display width",
                "int(11)",
                Some(Admits::WholeNumbers(-2_147_483_648, 2_147_483_647)),
            ),
            (
                "the published synonym of int",
                "integer",
                Some(Admits::WholeNumbers(-2_147_483_648, 2_147_483_647)),
            ),
            (
                "an unsigned int",
                "int unsigned",
                Some(Admits::WholeNumbers(0, 4_294_967_295)),
            ),
            (
                "a bigint",
                "bigint",
                Some(Admits::WholeNumbers(
                    -9_223_372_036_854_775_808,
                    9_223_372_036_854_775_807,
                )),
            ),
            (
                "an unsigned bigint",
                "bigint unsigned",
                Some(Admits::WholeNumbers(0, 18_446_744_073_709_551_615)),
            ),
            ("a type with no bound of its own", "datetime", None),
        ];
        for (case, declared, expected) in rows {
            let actual = admits(declared);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// One row of the refusal table: the case, the declared type, the value written, the row
    /// of the statement it was written in, and the refusal it is answered with.
    type RefusalRow = (
        &'static str,
        &'static str,
        &'static str,
        usize,
        Option<(u16, &'static str)>,
    );

    /// RULE: a value the column does not admit is refused under the server error number its
    /// condition carries, naming the column and the row of the statement it was written in.
    #[test]
    fn a_value_the_column_does_not_admit_is_refused_as_its_own_condition() {
        let column = |name: &str, declared_type: &str| DeclaredColumn {
            name: name.to_string(),
            declared_type: declared_type.to_string(),
            primary_key: false,
            position: 0,
        };
        let value = one_written_value;
        let rows: &[RefusalRow] = &[
            // (case, the declared type, the value written, the row, the refusal)
            ("a string that fits", "varchar(5)", "'abcde'", 1, None),
            (
                "a string one character too long",
                "varchar(5)",
                "'abcdef'",
                1,
                Some((1406, "Data too long for column 'label' at row 1")),
            ),
            (
                "a string too long in a later row",
                "varchar(5)",
                "'abcdefgh'",
                3,
                Some((1406, "Data too long for column 'label' at row 3")),
            ),
            (
                "a multi-byte string that fits in characters",
                "varchar(5)",
                "'héllo'",
                1,
                None,
            ),
            ("a number that fits", "tinyint", "127", 1, None),
            (
                "a number past the type's maximum",
                "tinyint",
                "128",
                1,
                Some((1264, "Out of range value for column 'label' at row 1")),
            ),
            (
                "a negative number past the type's minimum",
                "tinyint",
                "-129",
                1,
                Some((1264, "Out of range value for column 'label' at row 1")),
            ),
            ("the type's own minimum", "tinyint", "-128", 1, None),
            (
                "a negative number in an unsigned column",
                "int unsigned",
                "-1",
                1,
                Some((1264, "Out of range value for column 'label' at row 1")),
            ),
            (
                "a value the statement does not carry",
                "varchar(5)",
                "NULL",
                1,
                None,
            ),
            (
                "a value an expression computes",
                "varchar(5)",
                "repeat('a', 9)",
                1,
                None,
            ),
            (
                "a column with no bound of its own",
                "text",
                "'longer than five'",
                1,
                None,
            ),
        ];
        for (case, declared_type, written, row, expected) in rows {
            let column = column("label", declared_type);
            let refusal = does_not_fit(&value(written), &column)
                .map(|condition| condition.refusal(&column, *row));
            let actual = refusal.as_ref().map(|refusal| {
                (
                    refusal.mysql_error.unwrap_or_default().0,
                    refusal.message.as_str(),
                )
            });
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// One row of the adjustment table: the case, the declared type, the value written, and
    /// the value the column admits with the warning it is reported under.
    type AdjustmentRow = (
        &'static str,
        &'static str,
        &'static str,
        Option<(&'static str, u16, &'static str)>,
    );

    /// RULE: without strict mode the column takes the value it DOES admit — the string cut to
    /// its length, the number held to the end of its range — and the server says so.
    #[test]
    fn without_strict_mode_the_column_takes_the_value_it_admits() {
        let column = |name: &str, declared_type: &str| DeclaredColumn {
            name: name.to_string(),
            declared_type: declared_type.to_string(),
            primary_key: false,
            position: 0,
        };
        let value = one_written_value;
        let rows: &[AdjustmentRow] = &[
            // (case, the declared type, the value written, (the value held, warning, sentence))
            ("a string that fits", "varchar(5)", "'abcde'", None),
            (
                "a string too long, cut to the declared length",
                "varchar(5)",
                "'abcdefgh'",
                Some((
                    "'abcde'",
                    1265,
                    "Data truncated for column 'label' at row 1",
                )),
            ),
            (
                "a multi-byte string, cut by characters",
                "varchar(2)",
                "'héllo'",
                Some((
                    "'hé'",
                    1265,
                    "Data truncated for column 'label' at row 1",
                )),
            ),
            (
                "a number past the type's maximum",
                "tinyint",
                "300",
                Some((
                    "127",
                    1264,
                    "Out of range value for column 'label' at row 1",
                )),
            ),
            (
                "a negative number past the type's minimum",
                "tinyint",
                "-300",
                Some((
                    "-128",
                    1264,
                    "Out of range value for column 'label' at row 1",
                )),
            ),
            (
                "a negative number in an unsigned column",
                "int unsigned",
                "-7",
                Some((
                    "0",
                    1264,
                    "Out of range value for column 'label' at row 1",
                )),
            ),
        ];
        for (case, declared_type, written, expected) in rows {
            let column = column("label", declared_type);
            let mut value = value(written);
            let condition = does_not_fit(&value, &column);
            let actual = condition.as_ref().map(|condition| {
                condition.fit(&mut value);
                (
                    value.to_string(),
                    condition.warning(),
                    condition.sentence(&column, 1),
                )
            });
            let actual = actual
                .as_ref()
                .map(|(held, warning, sentence)| (held.as_str(), *warning, sentence.as_str()));
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: whether the statement is held to the column's type at all is what the session's
    /// `sql_mode` says, and the server's default carries strict mode.
    #[test]
    fn whether_a_value_is_refused_follows_the_sessions_sql_mode() {
        let rows: &[(&str, Option<&str>, bool)] = &[
            // (case, the session's sql_mode, whether a value is held to the type)
            ("the server's own default", None, true),
            (
                "the default mode of a MySQL 8.0 server",
                Some(
                    "ONLY_FULL_GROUP_BY,STRICT_TRANS_TABLES,NO_ZERO_IN_DATE,NO_ZERO_DATE,\
                     ERROR_FOR_DIVISION_BY_ZERO,NO_ENGINE_SUBSTITUTION",
                ),
                true,
            ),
            (
                "strict mode over every table",
                Some("STRICT_ALL_TABLES"),
                true,
            ),
            (
                "a mode with neither strict value",
                Some("NO_ENGINE_SUBSTITUTION,NO_ZERO_DATE"),
                false,
            ),
            ("no mode at all", Some(""), false),
        ];
        for (case, mode, expected) in rows {
            let actual = strict_mode(*mode);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected {expected}, got {actual}"
            );
        }
    }
}
