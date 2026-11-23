// SPDX-License-Identifier: CC0-1.0

//! The COMPRESSION each column of a table is stored under, as its `CREATE
//! TABLE` declared it, and what the catalogue reports for one that declared
//! nothing.
//!
//! `ENCODE <encoding>` is a STORAGE attribute: it says how a column is
//! compressed and nothing about what the column holds, so the engine behind this
//! emulator is handed the table without it ([`super::warehouse_statements`] is
//! where it is stripped). What a client reads back is still a fact of the table,
//! though — `PG_TABLE_DEF` reports every column's `encoding` — so the
//! declaration is kept here, per table, and the catalogue reads it from this
//! record rather than claiming an encoding nothing declared.
//!
//! A column whose table declared no encoding for it is reported under the
//! encoding the warehouse assigns automatically, which its documentation states
//! by TYPE ([`automatic_encoding`]); a column stored uncompressed reads back as
//! `none`, which is what `RAW` is reported as.

use std::collections::HashMap;

use sqlparser::tokenizer::Token;

use super::copy_load::{self, TypeClass};
use super::redshift_sql::{is_keyword, qualified_name, significant_tokens};

/// What a `CREATE TABLE` declared about how its columns are compressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredEncodings {
    /// The table's name parts, as the statement wrote them.
    pub table: Vec<String>,
    /// The encoding each column declared one for, lower-cased, in the order the
    /// statement declares the columns.
    pub columns: Vec<(String, String)>,
}

/// The encodings the tables of one database hold, by the ENGINE name of the
/// table.
///
/// Bounded like every other map this service grows on request: a table whose
/// entry had to make room for another one reads back under the automatic
/// encoding of each column's type, which is what a table that declared none
/// reads back as.
#[derive(Debug, Default)]
pub struct ColumnEncodings {
    tables: HashMap<String, Vec<(String, String)>>,
}

impl ColumnEncodings {
    /// Record what a `CREATE TABLE` just declared, replacing whatever was held
    /// for a table of that name — a table created again declares its storage
    /// again.
    pub fn record(&mut self, table: &str, columns: Vec<(String, String)>, retained: usize) {
        let key = table.to_ascii_lowercase();
        if !self.tables.contains_key(&key) && self.tables.len() >= retained {
            if let Some(evicted) = self.tables.keys().next().cloned() {
                self.tables.remove(&evicted);
            }
        }
        self.tables.insert(key, columns);
    }

    /// The encoding the table `table` declared for the column `column`, or
    /// `None` when it declared none.
    pub fn declared(&self, table: &str, column: &str) -> Option<&str> {
        self.tables
            .get(&table.to_ascii_lowercase())?
            .iter()
            .find(|(name, _encoding)| name.eq_ignore_ascii_case(column))
            .map(|(_name, encoding)| encoding.as_str())
    }
}

/// What `PG_TABLE_DEF` reports in its `encoding` column for a column of this
/// declared type that declared this encoding.
///
/// "RAW … the data is stored as raw, uncompressed data", which the catalogue
/// reports as `none`; every other encoding is reported under its own name.
pub fn reported_encoding(declared_encoding: Option<&str>, declared_type: Option<&str>) -> String {
    let chosen = match declared_encoding {
        // "ENCODE AUTO … Amazon Redshift automatically manages compression
        // encoding", so what it reports is the encoding that management
        // assigns.
        None => automatic_encoding(declared_type),
        Some(encoding) if encoding.eq_ignore_ascii_case("auto") => {
            automatic_encoding(declared_type)
        }
        Some(encoding) => encoding,
    };
    if chosen.eq_ignore_ascii_case(RAW_ENCODING) {
        return NO_ENCODING.to_string();
    }
    chosen.to_ascii_lowercase()
}

/// The encoding the warehouse assigns a column whose table declared none.
///
/// Source: the Amazon Redshift *Database Developer Guide*, `ENCODE AUTO` — a
/// `BOOLEAN`, `REAL` or `DOUBLE PRECISION` column is assigned `RAW`
/// compression, a `SMALLINT`, `INTEGER`, `BIGINT`, `DECIMAL`, `DATE`,
/// `TIMESTAMP` or `TIMESTAMPTZ` column `AZ64`, and a `CHAR` or `VARCHAR` column
/// `LZO`.
pub fn automatic_encoding(declared_type: Option<&str>) -> &'static str {
    match TypeClass::of(declared_type.unwrap_or_default()) {
        TypeClass::Integer | TypeClass::Decimal | TypeClass::Date | TypeClass::Timestamp => "az64",
        TypeClass::Character => "lzo",
        TypeClass::Boolean | TypeClass::Float => RAW_ENCODING,
    }
}

/// The type name `PG_TABLE_DEF` reports for a column declared with this type:
/// the warehouse's catalogue spells a type the way PostgreSQL's does, so a
/// `VARCHAR(64)` column reads back as `character varying(64)`.
///
/// Source: the Amazon Redshift *Data types* reference, which gives each type's
/// name and its aliases.
pub fn reported_type(declared_type: Option<&str>) -> String {
    let declared = declared_type.unwrap_or_default();
    let class = TypeClass::of(declared);
    let head = copy_load::type_head(declared);
    let parameters = declared.split_once('(').map(|(_name, rest)| {
        let (parameters, _) = rest.split_once(')').unwrap_or((rest, ""));
        parameters.trim().to_string()
    });
    let named = |name: &str| match &parameters {
        Some(parameters) if !parameters.is_empty() => format!("{name}({parameters})"),
        _ => name.to_string(),
    };
    match (class, head.as_str()) {
        (TypeClass::Integer, "smallint" | "int2") => "smallint".to_string(),
        (TypeClass::Integer, "bigint" | "int8") => "bigint".to_string(),
        (TypeClass::Integer, _) => "integer".to_string(),
        (TypeClass::Decimal, _) => named("numeric"),
        (TypeClass::Float, "real" | "float4") => "real".to_string(),
        (TypeClass::Float, _) => "double precision".to_string(),
        (TypeClass::Boolean, _) => "boolean".to_string(),
        (TypeClass::Date, _) => "date".to_string(),
        (TypeClass::Timestamp, "timestamptz") => "timestamp with time zone".to_string(),
        (TypeClass::Timestamp, _) => "timestamp without time zone".to_string(),
        (TypeClass::Character, "char" | "character" | "nchar" | "bpchar") => named("character"),
        (TypeClass::Character, "") => "character varying".to_string(),
        (TypeClass::Character, "text") => "character varying".to_string(),
        (TypeClass::Character, _) => named("character varying"),
    }
}

/// `RAW`, the encoding of a column that is not compressed at all.
const RAW_ENCODING: &str = "raw";

/// The keyword a statement that declares storage starts with.
const CREATE: &str = "create";

/// What the catalogue reports for a column stored that way.
const NO_ENCODING: &str = "none";

/// Read `sql` as a `CREATE TABLE` and report the table it creates with the
/// encoding each of its columns declares, or `None` for every other statement.
///
/// A statement that creates a TEMPORARY table is not read: a temporary relation
/// belongs to the connection that created it and is held under that
/// connection's own name, which is not a table of the catalogue this record is
/// read for.
///
/// Read from the TOKENS the dialect's own tokenizer produced, so a keyword
/// inside a string literal or a quoted identifier declares nothing.
pub fn declared(sql: &str) -> Option<DeclaredEncodings> {
    // Only a `CREATE` statement declares storage, so nothing else is read at
    // all: this is called for every statement that ran.
    if !sql
        .trim_start()
        .get(..CREATE.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(CREATE))
    {
        return None;
    }
    let tokens = significant_tokens(sql)?;
    let mut rest = creating_table(&tokens)?;
    if starts_with(rest, &["if", "not", "exists"]) {
        rest = rest.get(3..)?;
    }
    let (table, rest) = qualified_name(rest)?;
    Some(DeclaredEncodings {
        table,
        columns: column_encodings(rest),
    })
}

/// What follows the `CREATE TABLE` at the head of `tokens`, or `None` when the
/// statement is not one — a temporary table included.
fn creating_table(tokens: &[Token]) -> Option<&[Token]> {
    if !is_keyword(tokens.first()?, "create") {
        return None;
    }
    if !is_keyword(tokens.get(1)?, "table") {
        return None;
    }
    tokens.get(2..)
}

/// Whether `tokens` starts with these keywords, in this order.
fn starts_with(tokens: &[Token], words: &[&str]) -> bool {
    tokens.len() >= words.len()
        && words
            .iter()
            .zip(tokens)
            .all(|(word, token)| is_keyword(token, word))
}

/// The `ENCODE` each column of a column-definition list declares, read from the
/// list's own tokens: a column is named at the start of each element of the
/// list, and the encoding is the word its `ENCODE` names.
fn column_encodings(tokens: &[Token]) -> Vec<(String, String)> {
    let mut encodings: Vec<(String, String)> = Vec::new();
    let mut depth = 0_usize;
    let mut column: Option<String> = None;
    let mut expecting = false;
    for (index, token) in tokens.iter().enumerate() {
        match token {
            Token::LParen => {
                depth += 1;
                expecting = depth == 1;
            }
            Token::RParen => {
                if depth == 1 {
                    break;
                }
                depth = depth.saturating_sub(1);
            }
            Token::Comma if depth == 1 => expecting = true,
            Token::Word(word) if depth == 1 => {
                if expecting {
                    column = Some(word.value.clone());
                    expecting = false;
                } else if is_keyword(token, "encode") {
                    if let (Some(name), Some(Token::Word(encoding))) =
                        (column.as_ref(), tokens.get(index + 1))
                    {
                        encodings.push((name.clone(), encoding.value.to_ascii_lowercase()));
                    }
                }
            }
            _ => {}
        }
    }
    encodings
}
