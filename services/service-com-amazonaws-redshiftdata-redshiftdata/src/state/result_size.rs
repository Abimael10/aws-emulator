// SPDX-License-Identifier: CC0-1.0

//! The size a statement's result is REPORTED as (`ResultSize`), and the one
//! definition of the text a result's value carries.
//!
//! `DescribeStatementResponse.ResultSize` is "the size in bytes of the returned
//! results", so it is a size of the RESULT — the columns it names and the values
//! it carries, as text — and not of whatever the engine holding those values
//! happens to spend on them in this process. Those are two different numbers, and
//! the second one is [`super::value_bytes`], which bounds what the retained
//! history may hold.
//!
//! The model fixes the member's meaning and not its accounting, so WHICH cells
//! are counted — the line of column names as well as the rows — and the single
//! byte between two adjacent cells are what the service's own answer for a known
//! result pins: a result of two rows over three columns measures exactly the text
//! of its nine cells plus the eight separators between them. An answer that ever
//! says otherwise refines this rule here, in the one place it is stated.
//!
//! The text of a value is also what the CSV form of a result is written from
//! (`GetStatementResultV2`), so it is stated here once: the size a statement
//! reports and the characters a caller reads back cannot disagree.

use super::*;

/// One value as the TEXT it carries: the characters the warehouse prints for it.
///
/// A SQL NULL carries no text at all, which is the empty cell; binary carries its
/// bytes as hexadecimal, the text form Amazon Redshift prints a `VARBYTE` value
/// in. An exact-numeric value already carries the decimal text its column's scale
/// calls for ([`super::field_from`]), so it is reported with exactly the digits
/// that column declares.
pub fn value_text(field: &Field) -> String {
    match field {
        Field::isNull(_) => String::new(),
        Field::booleanValue(value) => value.to_string(),
        Field::longValue(value) => value.to_string(),
        Field::doubleValue(value) => value.to_string(),
        Field::stringValue(value) => value.clone(),
        Field::blobValue(value) => hex::encode(value),
    }
}

/// `ResultSize` for one result set: the bytes of the result as TEXT — the line of
/// its column names and one line per row, each line carrying its values — with
/// one byte of separator between two adjacent cells of a line and one between two
/// lines.
///
/// A value's own bytes are the characters it carries ([`value_text`]), so a
/// result's size is a function of the result alone: the same rows are the same
/// number of bytes whatever storage class the engine happened to keep each value
/// in, and a NULL carries nothing.
pub fn result_size(column_metadata: &[ColumnMetadata], rows: &[Vec<Field>]) -> i64 {
    let labels = column_metadata
        .iter()
        .map(|column| column.name.as_deref().unwrap_or_default().len());
    let mut cells: usize = 0;
    let mut bytes: usize = 0;
    for label in labels {
        cells = cells.saturating_add(1);
        bytes = bytes.saturating_add(label);
    }
    for row in rows {
        for field in row {
            cells = cells.saturating_add(1);
            bytes = bytes.saturating_add(value_text(field).len());
        }
    }
    // Every cell but the last is followed by one separator byte: the one between
    // two values of a line, and the one between two lines.
    let separators = cells.saturating_sub(1);
    i64::try_from(bytes.saturating_add(separators)).unwrap_or(i64::MAX)
}
