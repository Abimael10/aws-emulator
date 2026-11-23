// SPDX-License-Identifier: CC0-1.0

//! The ROWS a relational-database answer holds: what `toJsonObject` reads out of the result
//! a step's data source produced.
//!
//! The answer a `RELATIONAL_DATABASE` data source produces is the Data API's own result, one
//! entry per statement the request carried ([`super::RESULTS_MEMBER`]), and each entry
//! carries its rows as the API returns them: a list of records, each a list of values in
//! column order, beside the `columnMetadata` describing the columns those positions belong
//! to. A resolver reads a FIELD off a row by name, so this is the one reading that turns a
//! positional record into the object a row is: each value under the label its column carries.
//!
//! One reading for both runtimes, as the statement building beside it is
//! ([`super::utilities`]): the APPSYNC_JS `toJsonObject` and the `$util.rds.toJsonObject` a
//! Velocity template calls answer the same rows for one result.

use ::serde_json::{Map as JsonMap, Value as Json};

/// The rows `result` holds: one list per statement of the request, each holding one object
/// per row of that statement's result.
///
/// `result` is the answer as the step's data source produced it, or the JSON text of it —
/// which is what a definition that carried the answer through a string hands back.
pub fn rows(result: &Json) -> Result<Json, String> {
    let parsed = match result {
        Json::String(text) => ::serde_json::from_str::<Json>(text)
            .map_err(|error| format!("{NOT_A_RESULT} {error}."))?,
        other => other.clone(),
    };
    let results = match parsed.get(super::RESULTS_MEMBER).and_then(Json::as_array) {
        Some(results) => results.clone(),
        // A single statement's result, as the Data API answers one, is read as the one
        // result it is: a definition that carried just that through is reading the same
        // answer.
        None if parsed.get(RECORDS_MEMBER).is_some() => vec![parsed.clone()],
        None => return Err(format!("{NOT_A_RESULT} {parsed}.")),
    };
    Ok(Json::Array(results.iter().map(statement_rows).collect()))
}

/// The rows ONE statement's result holds, in the order it returned them.
fn statement_rows(result: &Json) -> Json {
    let labels: Vec<String> = result
        .get(COLUMN_METADATA_MEMBER)
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(at, column)| label(column, at))
        .collect();
    let records = result
        .get(RECORDS_MEMBER)
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    Json::Array(
        records
            .iter()
            .map(|record| row(record, &labels))
            .collect(),
    )
}

/// ONE row: each value of the record under the label of the column that position belongs to.
///
/// A position no column describes is reported under the position itself, so a result read
/// without its column metadata still carries its values rather than dropping them.
fn row(record: &Json, labels: &[String]) -> Json {
    let values = record.as_array().cloned().unwrap_or_default();
    let mut members = JsonMap::with_capacity(values.len());
    for (at, value) in values.iter().enumerate() {
        let name = labels
            .get(at)
            .cloned()
            .unwrap_or_else(|| format!("{UNNAMED_COLUMN_PREFIX}{at}"));
        members.insert(name, held(value));
    }
    Json::Object(members)
}

/// The label one column's values are reported under: "the label for the column", else the
/// column's own name, else the position it holds.
fn label(column: &Json, at: usize) -> String {
    [LABEL_MEMBER, NAME_MEMBER]
        .iter()
        .filter_map(|member| column.get(*member).and_then(Json::as_str))
        .map(str::to_string)
        .find(|named| !named.is_empty())
        .unwrap_or_else(|| format!("{UNNAMED_COLUMN_PREFIX}{at}"))
}

/// The value ONE field holds: a Data API field states its type by the member it carries, so
/// the value is that member's own — and nothing at all for the field that states a NULL.
///
/// An ARRAY field carries its elements under the member naming their type, so the value is
/// that list. A value that is no field at all is the value as it stands, which is what a
/// result assembled by something other than that API carries.
fn held(field: &Json) -> Json {
    let Some(members) = field.as_object() else {
        return field.clone();
    };
    if members
        .get(NULL_MEMBER)
        .and_then(Json::as_bool)
        .unwrap_or(false)
    {
        return Json::Null;
    }
    for member in VALUE_MEMBERS {
        if let Some(value) = members.get(*member) {
            return value.clone();
        }
    }
    if let Some(array) = members.get(ARRAY_MEMBER).and_then(Json::as_object) {
        for member in ARRAY_MEMBERS {
            if let Some(values) = array.get(*member) {
                return values.clone();
            }
        }
    }
    field.clone()
}

/// The members one statement's result carries its rows and its columns under
/// (`ExecuteStatement`'s own output members).
const RECORDS_MEMBER: &str = "records";
const COLUMN_METADATA_MEMBER: &str = "columnMetadata";

/// The members a column's metadata states its naming in (`ColumnMetadata$label`, `$name`).
const LABEL_MEMBER: &str = "label";
const NAME_MEMBER: &str = "name";

/// The member a field states a NULL with, and the members it states a value with — the
/// `Field` union's own members, in no significant order because a field carries exactly one.
const NULL_MEMBER: &str = "isNull";
const VALUE_MEMBERS: &[&str] = &[
    "stringValue",
    "longValue",
    "doubleValue",
    "booleanValue",
    "blobValue",
];

/// The member a field carries an ARRAY under, and the members such an array states its
/// elements under (`ArrayValue`'s own members).
const ARRAY_MEMBER: &str = "arrayValue";
const ARRAY_MEMBERS: &[&str] = &[
    "stringValues",
    "longValues",
    "doubleValues",
    "booleanValues",
    "arrayValues",
];

/// What a column with no naming of its own is reported under: the position it holds.
const UNNAMED_COLUMN_PREFIX: &str = "column_";

/// What reading a value that is no relational-database result states.
const NOT_A_RESULT: &str =
    "A relational database result carries the rows of every statement of the request, and \
     this value carries none:";

#[cfg(test)]
mod tests {
    use ::serde_json::json;

    use super::*;

    /// RULE: every value of a record is reported under the label of the column that position
    /// belongs to, and the value a field holds is the one its own member states.
    ///
    /// The rows span the field types the Data API returns a value in, a NULL, an array, a
    /// column named only by its name, and a result of two statements.
    #[test]
    fn a_record_is_read_as_the_row_its_columns_name() {
        let column = |label: &str| json!({"name": label, "label": label});
        let rows: &[(&str, Json, Json)] = &[
            (
                "one row of one statement",
                json!({"sqlStatementResults": [{
                    "columnMetadata": [column("id"), column("name")],
                    "records": [[{"longValue": 1}, {"stringValue": "widget-alpha"}]],
                }]}),
                json!([[{"id": 1, "name": "widget-alpha"}]]),
            ),
            (
                "the field types a value is returned in",
                json!({"sqlStatementResults": [{
                    "columnMetadata": [
                        column("flag"),
                        column("score"),
                        column("note"),
                        column("tags"),
                    ],
                    "records": [[
                        {"booleanValue": true},
                        {"doubleValue": 1.5},
                        {"isNull": true},
                        {"arrayValue": {"stringValues": ["a", "b"]}},
                    ]],
                }]}),
                json!([[
                    {"flag": true, "score": 1.5, "note": null, "tags": ["a", "b"]}
                ]]),
            ),
            (
                "a column named only by its name",
                json!({"sqlStatementResults": [{
                    "columnMetadata": [{"name": "total"}],
                    "records": [[{"longValue": 7}]],
                }]}),
                json!([[{"total": 7}]]),
            ),
            (
                "a statement that matched no row, beside one that did",
                json!({"sqlStatementResults": [
                    {"columnMetadata": [column("id")], "records": []},
                    {"columnMetadata": [column("id")], "records": [[{"longValue": 2}]]},
                ]}),
                json!([[], [{"id": 2}]]),
            ),
            (
                "one statement's result, as that API answers one",
                json!({
                    "columnMetadata": [column("id")],
                    "records": [[{"longValue": 3}]],
                }),
                json!([[{"id": 3}]]),
            ),
            (
                "the same result carried through as text",
                json!(
                    "{\"sqlStatementResults\":[{\"columnMetadata\":[{\"label\":\"id\"}],\
                     \"records\":[[{\"longValue\":4}]]}]}"
                ),
                json!([[{"id": 4}]]),
            ),
        ];
        for (case, result, expected) in rows {
            let read = rows_of(case, result);
            assert_eq!(
                &read, expected,
                "case {case:?}: expected {expected}, got {read}"
            );
        }
    }

    /// RULE: a value that is no relational-database result at all is refused rather than
    /// read as an empty one.
    #[test]
    fn a_value_that_is_no_result_is_refused() {
        for (case, value) in [
            ("a sentence", json!("no rows here")),
            ("a list", json!([1, 2])),
            ("nothing at all", Json::Null),
        ] {
            let read = rows(&value);
            assert!(
                read.is_err(),
                "case {case:?}: expected the value to be refused, got {read:?}"
            );
        }
    }

    /// The rows of `result`, which must be readable.
    fn rows_of(case: &str, result: &Json) -> Json {
        rows(result).unwrap_or_else(|error| panic!("case {case:?}: expected rows, got {error}"))
    }
}
