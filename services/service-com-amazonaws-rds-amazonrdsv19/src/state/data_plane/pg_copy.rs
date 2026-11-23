// SPDX-License-Identifier: CC0-1.0

//! `COPY <table> FROM STDIN`: the PostgreSQL bulk-load exchange, and the rows it loads.
//!
//! The statement is not a query the engine runs on its own — it puts the SESSION into the
//! protocol's copy-in mode, where the client streams the rows and the server loads them —
//! so this module owns the three parts of it: which table and columns the statement names,
//! how the text format the client streams is read, and what each row is refused with when
//! it does not fit the table.
//!
//! The rows really are loaded: each one becomes an `INSERT` the engine runs, so what a
//! later `SELECT` returns is what the client streamed. A row the table cannot take is
//! refused with PostgreSQL's own DATA error for that case — the load is a data operation,
//! and a syntax error would be the wrong class for a malformed row — and the rows before it
//! are rolled back with it, because `COPY` "stops operation at the first error".

use super::sql_engine::{EngineError, EngineErrorKind, InstanceDatabase};
use super::wire_session::SessionFacts;
use super::EngineWire;
use sqlparser::tokenizer::Token;

/// How many bytes of copy data one `COPY FROM STDIN` may stream, so a client cannot make
/// this endpoint buffer without bound. Well above a bulk load a test or a tool sends, and
/// the same order as the statement limit the session already carries.
pub const MAX_COPY_BYTES: usize = 8 * 1024 * 1024;

/// What `COPY <table> [(<columns>)] FROM STDIN` names.
#[derive(Debug, Clone)]
pub struct CopyTarget {
    /// The table the rows are loaded into.
    pub table: String,
    /// The columns the statement named, in its own order; empty where it named none, which
    /// means every column of the table in its declared order.
    pub columns: Vec<String>,
    /// The format the `WITH (…)` clause named, or `COPY`'s own text format where it named
    /// none.
    pub format: super::pg_aws_s3::CopyFormat,
}

/// The refusal a `COPY` that names a FILE (or a program) gets, or `None` for a statement
/// that names none.
///
/// Amazon RDS gives no client access to the host the engine runs on, so the server-side
/// forms of `COPY` — `COPY … TO '<file>'`, `COPY … FROM '<file>'` and the `PROGRAM` forms —
/// are not served on an RDS instance at all. RDS answers them with its own sentence rather
/// than PostgreSQL's privilege refusal (recorded behavior: `COPY <table> TO '<path>'` on an
/// RDS for PostgreSQL instance answers SQLSTATE `0A000` with "COPY to a file is not
/// supported"), which is the capability refusal it is — and what a client is told instead of
/// this emulator reading or writing a path on its own host. The client-side `COPY … FROM
/// STDIN`, which streams the rows over the connection, is what this endpoint serves
/// ([`copy_in_target`]).
pub fn refused_file_copy(facts: &SessionFacts, tokens: &[Token]) -> Option<EngineError> {
    if facts.wire != EngineWire::Postgres {
        return None;
    }
    let leads_with_copy = matches!(tokens.first(), Some(Token::Word(word)) if word.value.eq_ignore_ascii_case("COPY"));
    if !leads_with_copy {
        return None;
    }
    for (at, token) in tokens.iter().enumerate() {
        let direction = match token {
            Token::Word(word) if word.value.eq_ignore_ascii_case("TO") => "to",
            Token::Word(word) if word.value.eq_ignore_ascii_case("FROM") => "from",
            _ => continue,
        };
        let names_a_path = match tokens.get(at + 1) {
            // A quoted path, and the `PROGRAM` form, which runs a command on the host.
            Some(Token::SingleQuotedString(_)) => true,
            Some(Token::Word(word)) => word.value.eq_ignore_ascii_case("PROGRAM"),
            _ => false,
        };
        if names_a_path {
            return Some(
                EngineError::new(
                    EngineErrorKind::FeatureNotSupported,
                    format!("COPY {direction} a file is not supported"),
                )
                .with_sqlstate("0A000"),
            );
        }
    }
    None
}

/// The copy-in target a statement names, or `None` for a statement that is not a `COPY …
/// FROM STDIN` at all (including `COPY … TO`, which streams the other way).
///
/// Only the PostgreSQL wire has this exchange; the MySQL protocol's bulk load is a
/// different command, which this endpoint does not serve.
pub fn copy_in_target(
    facts: &SessionFacts,
    sql: &str,
    tokens: &[Token],
) -> Option<Result<CopyTarget, EngineError>> {
    if facts.wire != EngineWire::Postgres {
        return None;
    }
    let words: Vec<String> = tokens
        .iter()
        .map(|token| match token {
            Token::Word(word) => word.value.to_ascii_uppercase(),
            other => other.to_string(),
        })
        .collect();
    if words.first().map(String::as_str) != Some("COPY") {
        return None;
    }
    // `FROM STDIN` is what makes it a copy IN; `COPY … TO …` is the other direction.
    let from = words.iter().position(|word| word == "FROM")?;
    if words.get(from + 1).map(String::as_str) != Some("STDIN") {
        return None;
    }
    let table = match tokens.get(1) {
        Some(Token::Word(word)) => word.value.clone(),
        _ => return None,
    };
    let format = match named_format(sql, "STDIN") {
        Ok(format) => format,
        Err(refusal) => return Some(Err(refusal)),
    };
    // The column list, when the statement carries one: the identifiers between the
    // parentheses that follow the table name.
    let mut columns = Vec::new();
    if matches!(tokens.get(2), Some(Token::LParen)) {
        for token in tokens.iter().skip(3) {
            match token {
                Token::RParen => break,
                Token::Word(word) => columns.push(word.value.clone()),
                _ => {}
            }
        }
    }
    Some(Ok(CopyTarget {
        table,
        columns,
        format,
    }))
}

/// The FORMAT the `WITH (…)` clause after `keyword` names, or `COPY`'s own text format where
/// the statement carries no such clause.
///
/// The options are `COPY`'s own, so they are read where that clause is read
/// ([`super::pg_aws_s3::read_options`]) — the same reading for both directions of the copy.
fn named_format(sql: &str, keyword: &str) -> Result<super::pg_aws_s3::CopyFormat, EngineError> {
    let after = sql
        .to_ascii_uppercase()
        .find(keyword)
        .and_then(|at| sql.get(at + keyword.len()..))
        .unwrap_or_default()
        .to_string();
    match parenthesised(&after) {
        Some(options) => super::pg_aws_s3::read_options(&format!("({options})")),
        None => Ok(super::pg_aws_s3::CopyFormat::text()),
    }
}

/// Load the rows `data` carries into `target`, and answer with how many were loaded.
///
/// `data` is the whole of what the client streamed, in the format `COPY` documents as its
/// default: one row per line, columns separated by tabs, `\N` for a NULL, the backslash
/// escapes PostgreSQL lists, and an optional `\.` line marking the end of the data.
///
/// Every row is loaded inside ONE transaction, so a row the table refuses leaves none of
/// the load behind: `COPY` "stops operation at the first error", and the rows it had
/// already read are rolled back with it.
pub fn load(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &CopyTarget,
    data: &[u8],
) -> Result<u64, EngineError> {
    if data.len() > MAX_COPY_BYTES {
        return Err(EngineError::new(
            EngineErrorKind::LimitExceeded,
            format!("the copy data is larger than this endpoint serves ({MAX_COPY_BYTES} bytes)"),
        ));
    }
    let rows = match target.format.binary {
        // The BINARY format: each value carries the bytes its own type's binary form has,
        // which are read into the text the engine binds ([`binary_records`]).
        true => binary_records(database, facts, target, data)?,
        false => text_records(
            target.format.delimiter,
            &target.format.null,
            target.format.header,
            data,
        ),
    };
    load_rows(database, facts, target, &rows)
}

/// The records a BINARY copy-in carries, read into the text each value has in the text format
/// — which is what the load then binds, exactly as a text-format record's field is.
///
/// Source: the `COPY` reference's "Binary Format" section: the 11-byte signature, a 32-bit
/// flags field and a 32-bit header extension whose length it names, then each row as a 16-bit
/// field count followed by each field's 32-bit length (-1 for a NULL) and its bytes, and a
/// field count of -1 ending the stream. Each field's bytes are read under the type the COLUMN
/// it is loaded into was declared with ([`super::pg_binary::parameter_text`]), which is the
/// type the client wrote them in.
fn binary_records(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &CopyTarget,
    data: &[u8],
) -> Result<Records, EngineError> {
    let unreadable = |what: &str| {
        EngineError::new(EngineErrorKind::DataError, what.to_string())
            .with_sqlstate(SQLSTATE_BAD_COPY_FILE_FORMAT)
    };
    if !data.starts_with(BINARY_SIGNATURE) {
        return Err(unreadable("COPY file signature not recognized"));
    }
    let number = |at: usize, width: usize| -> Option<i64> {
        let bytes = data.get(at..at + width)?;
        let mut value: i64 = 0;
        for byte in bytes {
            value = (value << 8) | i64::from(*byte);
        }
        // The widths read here are signed, so the sign bit of the first byte is the value's.
        let bits = 8 * width as u32;
        Some(match bytes.first().is_some_and(|byte| byte & 0x80 != 0) {
            true => value - (1i64 << bits),
            false => value,
        })
    };
    let mut at = BINARY_SIGNATURE.len() + 4;
    let extension = number(at, 4).ok_or_else(|| unreadable("COPY file header is truncated"))?;
    at += 4 + usize::try_from(extension.max(0)).unwrap_or_default();
    // The extension's own length is the client's number: a header that says it carries more
    // than the stream holds is a header this reader cannot get past.
    if at > data.len() {
        return Err(unreadable("COPY file header is truncated"));
    }
    // The columns the fields are loaded into, which say what type each field's bytes are in.
    let oids = column_oids(database, facts, target)?;
    let mut rows: Records = Vec::new();
    loop {
        let count = match number(at, 2) {
            // "The file trailer consists of a 16-bit integer word containing -1."
            Some(-1) => break,
            Some(count) => count,
            // A stream that ends without that trailer ended early, however many rows it
            // carried: the rows it did carry are not a load.
            None => return Err(unreadable("COPY data is truncated")),
        };
        at += 2;
        let mut fields: Vec<Option<String>> = Vec::new();
        for field in 0..count.max(0) {
            let length = number(at, 4).ok_or_else(|| unreadable("COPY data is truncated"))?;
            at += 4;
            if length < 0 {
                fields.push(None);
                continue;
            }
            let length = usize::try_from(length).unwrap_or_default();
            let bytes = data
                .get(at..at + length)
                .ok_or_else(|| unreadable("COPY data is truncated"))?;
            at += length;
            let oid = oids.get(usize::try_from(field).unwrap_or_default()).copied();
            match oid {
                Some(oid) => fields.push(Some(super::pg_binary::parameter_text(oid, bytes)?)),
                // A field the load has no column for is under no type at all: what is wrong
                // is the RECORD's shape, which the load refuses in PostgreSQL's own words for
                // a record with a field too many — the one place that shape is judged
                // ([`row_values`]), as it is for a text-format record.
                None => fields.push(Some(String::new())),
            }
        }
        rows.push(CopyRecord {
            // A binary stream carries no lines: the ROW is what a refusal names, counted the
            // way the text format's lines are.
            line: rows.len() + 1,
            fields,
        });
    }
    Ok(rows)
}

/// The TYPE of each column a load's fields go into, as the object identifier the binary
/// format's values are read under.
fn column_oids(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &CopyTarget,
) -> Result<Vec<i32>, EngineError> {
    Ok(target_columns(database, facts, target)?
        .into_iter()
        .map(|(name, declared)| {
            super::pg_wire::type_oid(
                &super::sql_engine::ColumnInfo {
                    name,
                    declared_type: declared,
                },
                None,
            )
        })
        .collect())
}

/// The records one bulk load carries: one entry per row, one field per column of the row, and
/// `None` for a field the format spelled as a NULL.
pub(super) type Records = Vec<CopyRecord>;

/// One record of a bulk load, with WHERE the format it was read out of carried it.
///
/// The line is kept because a load that stops reports it: PostgreSQL's `CONTEXT` for a row it
/// refused names the line of the copy data, which is not the record's position among the
/// records — a blank line, the end-of-data marker and a header line carry no record
/// ([`text_records`]).
#[derive(Debug, Clone)]
pub(super) struct CopyRecord {
    /// Which line of the copy data this record was read from, counting from 1.
    pub line: usize,
    /// The record's fields, one per column, `None` for a field spelled as a NULL.
    pub fields: Vec<Option<String>>,
}

/// The records `body` carries in `COPY`'s TEXT format: one row per line, the fields separated
/// by `delimiter`, `null` standing for a NULL, the backslash escapes resolved
/// ([`unescaped`]) and the `\.` line ending the data.
///
/// `pub(super)` and parameterised by the delimiter, the NULL string and whether the first line
/// is a header because the Amazon S3 import of the `aws_s3` extension reads that same format
/// out of an object's bytes with the options its call named ([`super::pg_aws_s3`]): one
/// reading of the format, whichever statement asked for it.
pub(super) fn text_records(
    delimiter: u8,
    null: &str,
    header: bool,
    body: &[u8],
) -> Records {
    let text = String::from_utf8_lossy(body);
    let separator = char::from(delimiter);
    let mut rows: Records = Vec::new();
    for (at, line) in text.split('\n').enumerate() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        // The end-of-data marker, and the empty tail a trailing newline leaves.
        if line == "\\." {
            break;
        }
        if line.is_empty() {
            continue;
        }
        // A header line names the columns rather than carrying a row.
        if at == 0 && header {
            continue;
        }
        rows.push(CopyRecord {
            line: at + 1,
            fields: line
                .split(separator)
                .map(|field| match field == null {
                    true => None,
                    false => Some(unescaped(field)),
                })
                .collect(),
        });
    }
    rows
}

/// The COLUMNS a load's records go into, each with the type its declaration gave it: the
/// columns the statement named, or every column of the table in its declared order.
///
/// The declarations are the ENGINE's for the table, so a column the statement names that the
/// table does not have carries no type here and is refused by the engine rather than by a list
/// kept in this module.
fn target_columns(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &CopyTarget,
) -> Result<Vec<(String, Option<String>)>, EngineError> {
    let declared = database.describe(
        facts.connection_id,
        &facts.database,
        &format!("SELECT * FROM {}", quoted(&target.table)),
        &[],
    )?;
    if target.columns.is_empty() {
        return Ok(declared
            .iter()
            .map(|column| (column.name.clone(), column.declared_type.clone()))
            .collect());
    }
    Ok(target
        .columns
        .iter()
        .map(|named| {
            let declared_type = declared
                .iter()
                .find(|column| column.name.eq_ignore_ascii_case(named))
                .and_then(|column| column.declared_type.clone());
            (named.clone(), declared_type)
        })
        .collect())
}

/// Load `rows` — records already read out of whatever format carried them — into `target`,
/// and answer with how many were loaded.
///
/// This is the load itself, shared by every bulk load of this wire: the client's
/// `COPY … FROM STDIN` above, and the Amazon S3 import of the `aws_s3` extension
/// ([`super::pg_aws_s3`]), which differ in the FORMAT they read and not in what loading a
/// record means. The columns are the ENGINE's for the table, so a column the statement names
/// that the table does not have is refused by it rather than by a list kept here.
///
/// Every row is loaded inside ONE transaction, so a row the table refuses leaves none of
/// the load behind: `COPY` "stops operation at the first error", and the rows it had
/// already read are rolled back with it.
pub(super) fn load_rows(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    target: &CopyTarget,
    rows: &Records,
) -> Result<u64, EngineError> {
    // The columns the rows are loaded into, and the type each one is declared with: the
    // ENGINE reports them for the table, so a column the statement names that the table
    // does not have is refused by it rather than by a list kept here.
    let columns = target_columns(database, facts, target)?;
    let statement = format!(
        "INSERT INTO {} ({}) VALUES ({})",
        quoted(&target.table),
        columns
            .iter()
            .map(|(name, _)| quoted(name))
            .collect::<Vec<_>>()
            .join(", "),
        std::iter::repeat_n("?", columns.len())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut loaded = 0u64;
    // A `COPY` the client sent INSIDE a transaction of its own loads into that transaction:
    // the rows are the client's to commit or roll back, so each row is loaded for the
    // SESSION and recorded against the transaction it has open. A `COPY` outside one is its
    // OWN transaction — one of this session's, not the engine connection's, so the rows of
    // another session running at the same time are no part of it — which is what makes "COPY
    // stops operation at the first error" leave no rows behind.
    let inside_the_clients_transaction = database.in_transaction(facts.connection_id);
    if !inside_the_clients_transaction {
        database.begin_transaction(facts.connection_id);
    }
    let outcome = (|| -> Result<u64, EngineError> {
        for row in rows {
            // The values, with where the load would stop already on a refusal: the table, the
            // line of the copy data and — for a field the column could not read — that
            // column and the datum itself.
            let values = row_values(&row.fields, &columns, &target.table, row.line)?;
            database.execute_for(
                facts.connection_id,
                &facts.database,
                &super::wire_session::server_view(facts),
                &statement,
                &values,
            )?;
            loaded += 1;
        }
        Ok(loaded)
    })();
    if inside_the_clients_transaction {
        // Whatever the outcome, the transaction stays the client's: a failed `COPY` leaves
        // it to be rolled back, exactly as any other failed statement inside one does.
        return outcome;
    }
    match outcome {
        Ok(loaded) => {
            // The copy's own transaction, which no client opened: it is the statement's and
            // it is committed here, so there is no isolation level of the client's to hold
            // it to.
            database.commit_transaction(facts.connection_id, false)?;
            Ok(loaded)
        }
        Err(error) => {
            let _ = database.rollback_transaction(facts.connection_id);
            Err(error)
        }
    }
}

/// `bad_copy_file_format`: the SQLSTATE PostgreSQL carries a record whose SHAPE does not fit
/// the columns of the load under, from the condition names in its "Error Codes" appendix.
const SQLSTATE_BAD_COPY_FILE_FORMAT: &str = "22P04";

/// `invalid_text_representation`: the same, for a datum the column's type cannot read.
const SQLSTATE_INVALID_TEXT_REPRESENTATION: &str = "22P02";

/// WHERE a load of `table` that stopped on `line` stopped, as PostgreSQL reports it in the
/// error's own `CONTEXT` field: `COPY <table>, line <n>, column <column>: "<datum>"` for a
/// datum its column could not read, and the table and the line alone where the record's SHAPE
/// is what was wrong.
///
/// Every value in it comes from the load: the table the statement named, the line of the copy
/// data the record was read from, and the column and the datum the load refused.
fn in_the_copy(table: &str, line: usize, field: Option<(&str, &str)>) -> String {
    match field {
        Some((column, datum)) => {
            format!("COPY {table}, line {line}, column {column}: \"{datum}\"")
        }
        None => format!("COPY {table}, line {line}"),
    }
}

/// The values one record carries, refused the way PostgreSQL refuses a row that does not
/// fit the columns it is being loaded into — with the place the load stopped on it
/// ([`in_the_copy`]), which is assembled here because the column and the datum are known
/// here and the table and the line are the caller's.
fn row_values(
    fields: &[Option<String>],
    columns: &[(String, Option<String>)],
    table: &str,
    line: usize,
) -> Result<Vec<Option<String>>, EngineError> {
    let shape = |message: String| {
        EngineError::new(EngineErrorKind::DataError, message)
            .with_sqlstate(SQLSTATE_BAD_COPY_FILE_FORMAT)
            .with_context(in_the_copy(table, line, None))
    };
    if fields.len() > columns.len() {
        return Err(shape("extra data after last expected column".to_string()));
    }
    if fields.len() < columns.len() {
        let missing = columns
            .get(fields.len())
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        return Err(shape(format!("missing data for column \"{missing}\"")));
    }
    let mut values = Vec::with_capacity(fields.len());
    for (field, (column, declared)) in fields.iter().zip(columns) {
        let Some(value) = field else {
            values.push(None);
            continue;
        };
        check_input_syntax(declared.as_deref(), value).map_err(|refusal| {
            refusal.with_context(in_the_copy(table, line, Some((column, value))))
        })?;
        values.push(Some(value.clone()));
    }
    Ok(values)
}

/// Refuse a value the column's declared type does not accept, in PostgreSQL's own words
/// for a value it cannot read as that type.
///
/// The engine behind this endpoint reads a value into whatever class its column has, so a
/// value that is not a number in a numeric column would be stored as given rather than
/// refused; a load has to refuse it, because that is the error a bulk load is expected to
/// stop on. The types checked are the ones whose text form has to parse: the integer and
/// the floating/exact numeric families PostgreSQL's "Numeric Types" section lists.
fn check_input_syntax(declared: Option<&str>, value: &str) -> Result<(), EngineError> {
    let Some(declared) = declared else {
        return Ok(());
    };
    let base = declared
        .to_ascii_lowercase()
        .split('(')
        .next()
        .unwrap_or_default()
        .trim()
        .to_string();
    let refused = |named: &str| {
        Err(EngineError::new(
            EngineErrorKind::DataError,
            format!("invalid input syntax for type {named}: \"{value}\""),
        )
        .with_sqlstate(SQLSTATE_INVALID_TEXT_REPRESENTATION))
    };
    match base.as_str() {
        "smallint" | "int2" => {
            if value.trim().parse::<i16>().is_err() {
                return refused("smallint");
            }
        }
        "int" | "integer" | "int4" => {
            if value.trim().parse::<i32>().is_err() {
                return refused("integer");
            }
        }
        "bigint" | "int8" => {
            if value.trim().parse::<i64>().is_err() {
                return refused("bigint");
            }
        }
        "numeric" | "decimal" => {
            if value.trim().parse::<f64>().is_err() {
                return refused("numeric");
            }
        }
        "real" | "float4" => {
            if value.trim().parse::<f32>().is_err() {
                return refused("real");
            }
        }
        "double" | "float8" | "double precision" => {
            if value.trim().parse::<f64>().is_err() {
                return refused("double precision");
            }
        }
        "bool" | "boolean"
            if !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "t" | "f" | "true" | "false" | "y" | "n" | "yes" | "no" | "1" | "0" | "on" | "off"
            ) =>
        {
            return refused("boolean");
        }
        _ => {}
    }
    Ok(())
}

/// One field with the backslash escapes `COPY`'s text format defines resolved.
///
/// Source: the `COPY` reference's "Text Format" section, which lists the escapes the
/// format recognises (`\b`, `\f`, `\n`, `\r`, `\t`, `\v`, `\\`) and states that "any other
/// backslashed character that is not mentioned above will be taken to represent itself".
///
/// `pub(super)` because the Amazon S3 import of the `aws_s3` extension reads the same text
/// format out of the object's bytes ([`super::pg_aws_s3`]), and one copy of these escapes is
/// what keeps the two readings of that format identical.
pub(super) fn unescaped(field: &str) -> String {
    if !field.contains('\\') {
        return field.to_string();
    }
    let mut out = String::with_capacity(field.len());
    let mut characters = field.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('v') => out.push('\u{b}'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// What `COPY … TO STDOUT` streams: the statement whose rows the server sends to the client,
/// and the FORMAT it sends them in.
#[derive(Debug, Clone)]
pub struct CopyOut {
    /// The query the rows come from, as the engine is asked for them: the table the statement
    /// named, or the query it carried.
    pub query: String,
    /// The format the `WITH (…)` clause named, or `COPY`'s own text format where it named
    /// none.
    pub format: super::pg_aws_s3::CopyFormat,
}

/// The bytes one copy-out streams, as the `CopyData` messages that carry them.
#[derive(Debug, Clone)]
pub struct CopyStream {
    /// Whether the stream is in the BINARY format, which the `CopyOutResponse` announces.
    pub binary: bool,
    /// How many columns each row carries, which that same message announces one format code
    /// per.
    pub columns: usize,
    /// How many rows were streamed, which the command tag names.
    pub rows: usize,
    /// The payloads, in the order they are sent.
    pub payloads: Vec<Vec<u8>>,
}

/// The copy-out source a statement names, or `None` for a statement that is not a
/// `COPY … TO STDOUT` at all.
///
/// `COPY` streams the other way too — "COPY TO copies the contents of a table to a file", and
/// `STDOUT` is "the client application" rather than a file — so the server answers it with the
/// rows of the table (or of the query) the statement names, in the format `COPY` documents.
/// Only the PostgreSQL wire has this exchange.
///
/// The table form becomes the query that reads it, with the columns the statement named or
/// every column of the table; the query form is the client's own statement, taken from the
/// text between the parentheses so it reaches the engine exactly as it was written.
pub fn copy_out_target(
    facts: &SessionFacts,
    sql: &str,
    tokens: &[Token],
) -> Option<Result<CopyOut, EngineError>> {
    if facts.wire != EngineWire::Postgres {
        return None;
    }
    let words: Vec<String> = tokens
        .iter()
        .map(|token| match token {
            Token::Word(word) => word.value.to_ascii_uppercase(),
            other => other.to_string(),
        })
        .collect();
    if words.first().map(String::as_str) != Some("COPY") {
        return None;
    }
    // `TO STDOUT` is what makes it a copy OUT; `COPY … FROM STDIN` is the other direction.
    let to = words.iter().position(|word| word == "TO")?;
    if words.get(to + 1).map(String::as_str) != Some("STDOUT") {
        return None;
    }
    // The `WITH (…)` clause, which follows `STDOUT`: the options are `COPY`'s own, and are
    // read where that clause is read ([`super::pg_aws_s3::read_options`]).
    let after = sql
        .to_ascii_uppercase()
        .find("STDOUT")
        .and_then(|at| sql.get(at + "STDOUT".len()..))
        .unwrap_or_default()
        .to_string();
    let format = match parenthesised(&after) {
        Some(options) => match super::pg_aws_s3::read_options(&format!("({options})")) {
            Ok(format) => format,
            Err(refusal) => return Some(Err(refusal)),
        },
        None => super::pg_aws_s3::CopyFormat::text(),
    };
    // `COPY (<query>) TO STDOUT`: the query the client wrote, between its own parentheses.
    if matches!(tokens.first(), Some(Token::Word(_))) && matches!(tokens.get(1), Some(Token::LParen))
    {
        return parenthesised(sql).map(|query| Ok(CopyOut { query, format }));
    }
    let table = match tokens.get(1) {
        Some(Token::Word(word)) => word.value.clone(),
        _ => return None,
    };
    // The column list, when the statement carries one.
    let mut columns: Vec<String> = Vec::new();
    if matches!(tokens.get(2), Some(Token::LParen)) {
        for token in tokens.iter().skip(3) {
            match token {
                Token::RParen => break,
                Token::Word(word) => columns.push(quoted(&word.value)),
                _ => {}
            }
        }
    }
    let selected = match columns.is_empty() {
        true => "*".to_string(),
        false => columns.join(", "),
    };
    Some(Ok(CopyOut {
        query: format!("SELECT {selected} FROM {}", quoted(&table)),
        format,
    }))
}

/// The text between the FIRST parenthesis of `sql` and the one that closes it, or `None`
/// where they do not close: a parenthesis inside a quoted string is part of that string and
/// closes nothing.
fn parenthesised(sql: &str) -> Option<String> {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut opened = None;
    for (at, character) in sql.char_indices() {
        match character {
            '\'' => quoted = !quoted,
            '(' if !quoted => {
                depth += 1;
                if depth == 1 {
                    opened = Some(at + character.len_utf8());
                }
            }
            ')' if !quoted => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    let inside = sql.get(opened?..at)?.trim();
                    return (!inside.is_empty()).then(|| inside.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// The rows one `COPY … TO STDOUT` streams, in the FORMAT the statement asked for.
///
/// The query is run the way a query of the session is ([`super::wire_session::streamed_rows`])
/// and its rows written by the format's own writing, so what the client reads back is what a
/// `SELECT` of the same query answers: the text and `csv` formats are the ones an export
/// writes ([`super::pg_aws_s3::rendered`]), and the BINARY format carries each value in its
/// own type's binary form, after the signature and the two header words that format opens with
/// and before the trailer it ends with.
pub fn stream_out(
    database: &InstanceDatabase,
    facts: &SessionFacts,
    source: &CopyOut,
) -> Result<CopyStream, EngineError> {
    let result = super::wire_session::streamed_rows(database, facts, &source.query)?;
    let stream = CopyStream {
        binary: source.format.binary,
        columns: result.columns.len(),
        rows: result.rows.len(),
        payloads: Vec::new(),
    };
    if !source.format.binary {
        let body = super::pg_aws_s3::rendered(&source.format, &result)?;
        return Ok(CopyStream {
            payloads: body
                .chunks(MAX_COPY_CHUNK_BYTES)
                .map(<[u8]>::to_vec)
                .collect(),
            ..stream
        });
    }
    Ok(CopyStream {
        payloads: binary_payloads(&result)?,
        ..stream
    })
}

/// How many bytes of the stream one `CopyData` message carries. The protocol puts no meaning
/// on where one message ends — the data is a stream — so this is only what keeps a message
/// from being as large as the whole answer.
const MAX_COPY_CHUNK_BYTES: usize = 64 * 1024;

/// The payloads of a BINARY copy-out: its signature and header, one message per row, and its
/// trailer.
///
/// Source: the `COPY` reference's "Binary Format" section — the file starts with the
/// 11-byte signature, a 32-bit flags field and a 32-bit header-extension length; each row is
/// a 16-bit field count followed by each field's 32-bit length (-1 for a NULL) and its bytes
/// in the type's own binary form ([`super::pg_binary::copy_field`]); and the stream ends with
/// a field count of -1.
fn binary_payloads(
    result: &super::sql_engine::ResultSet,
) -> Result<Vec<Vec<u8>>, EngineError> {
    let mut payloads = Vec::with_capacity(result.rows.len() + 2);
    let mut header = BINARY_SIGNATURE.to_vec();
    header.extend_from_slice(&0i32.to_be_bytes());
    header.extend_from_slice(&0i32.to_be_bytes());
    payloads.push(header);
    for row in &result.rows {
        let mut body = Vec::new();
        body.extend_from_slice(&i16::try_from(row.len()).unwrap_or_default().to_be_bytes());
        for (at, cell) in row.iter().enumerate() {
            let declared = result
                .columns
                .get(at)
                .and_then(|column| column.declared_type.as_deref());
            match super::pg_binary::copy_field(declared, cell)? {
                Some(bytes) => {
                    body.extend_from_slice(
                        &i32::try_from(bytes.len()).unwrap_or_default().to_be_bytes(),
                    );
                    body.extend_from_slice(&bytes);
                }
                // "As a special case, -1 indicates a NULL field value."
                None => body.extend_from_slice(&(-1i32).to_be_bytes()),
            }
        }
        payloads.push(body);
    }
    payloads.push((-1i16).to_be_bytes().to_vec());
    Ok(payloads)
}

/// The signature every binary-format copy stream opens with: "PGCOPY\n\377\r\n\0", which is
/// what a client checks to tell the format apart from the text one.
const BINARY_SIGNATURE: &[u8; 11] = b"PGCOPY\n\xff\r\n\0";

/// One value as the text format WRITES it: the NULL string for a NULL — written as it is,
/// because the marker stands for no value and is not escaped — and the value's own text with
/// the format's escapes put back ([`escaped_field`]).
///
/// `pub(super)` because the Amazon S3 export of the `aws_s3` extension writes the same fields
/// into an object's bytes ([`super::pg_aws_s3`]): one writing of the format, whichever
/// statement asked for it.
pub(super) fn written_field(cell: &super::sql_engine::Cell, null: &str) -> String {
    match cell {
        super::sql_engine::Cell::Null => null.to_string(),
        other => escaped_field(&field_text(other, null)),
    }
}

/// One value as its FIELD in the text format: the NULL string for a NULL, and the value's own
/// text otherwise.
///
/// `pub(super)` because the Amazon S3 export of the `aws_s3` extension writes the same fields
/// into an object's bytes ([`super::pg_aws_s3`]), and one reading of what a field IS keeps the
/// two from writing a value differently.
pub(super) fn field_text(cell: &super::sql_engine::Cell, null: &str) -> String {
    use super::sql_engine::Cell;
    match cell {
        Cell::Null => null.to_string(),
        Cell::Integer(value) => value.to_string(),
        Cell::Real(value) => value.to_string(),
        Cell::Text(value) => value.clone(),
        Cell::Blob(bytes) => String::from_utf8_lossy(bytes).to_string(),
    }
}

/// One field with the backslash escapes `COPY`'s text format writes put back in, which is
/// what makes a value carrying a separator or a newline one field.
///
/// Source: the `COPY` reference's "Text Format" section — the same escapes [`unescaped`]
/// resolves when the format is read.
pub(super) fn escaped_field(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    for character in field.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}

/// An identifier as the engine is handed it, quoted so a name that needs quoting keeps its
/// case and so nothing in it is read as SQL.
///
/// `pub(super)` because every bulk load in this module tree hands the engine the table and
/// column names a client wrote: the other wire's `LOAD DATA FROM S3`
/// ([`super::mysql_s3_load`]) quotes them the same way, and one copy of that is what keeps
/// the two from quoting differently.
pub(super) fn quoted(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}
