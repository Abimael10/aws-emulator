// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WORDING the table-version family refuses a miss with:
//! `GetTableVersion`, `GetTableVersions`, `DeleteTableVersion` and
//! `BatchDeleteTableVersion`.
//!
//! The two rules under test:
//!
//! 1. An operation that addresses one table's VERSION HISTORY by name refuses a table the
//!    database does not hold with the catalog's table-miss sentence, which NAMES the table
//!    as the catalog keys it (the Hive-compatible fold the model states for the member:
//!    "The table name. For Hive compatibility, this is folded to lowercase when it is
//!    stored", `TableInput$Name`) and names no database — the same refusal every other
//!    operation that resolves a table by name answers. The table READ's own fixed sentence,
//!    which names nothing, belongs to `GetTable` alone, so a client can tell an absent
//!    table from an absent version of a stored one.
//! 2. A VERSION miss carries no part of the request: the per-item record of the batch
//!    delete reports the very sentence the single-version read and delete answer for that
//!    same id, because `TableVersionError` carries the identity in its own `VersionId` and
//!    `TableName` members (`smithy_query shape=TableVersionError,ErrorDetail`) rather than
//!    in `ErrorDetail$ErrorMessage`.
//!
//! Every call goes over the wire the way a client makes it, through the shared
//! [`crate::wire`] harness, dispatched by the same `MultiServiceRouter` the production
//! front door uses.
//!
//! Each suite is a table whose rows differ along what the rule generalises over: for rule
//! 1 the absent table name's value, length, casing and character set, how it is absent
//! (never created, or created and then deleted) and which operation of the family asks;
//! for rule 2 the addressed id's value and spelling, whether the table ever minted it, and
//! which operation of the family asks. The recorded wire cases — a version read of a table
//! the catalog does not hold, and a batch item naming a version past the table's newest —
//! are one row each. The expected sentences are written out here as literals rather than
//! read from the service's own constants, so this suite pins the wording instead of
//! restating it.

use serde_json::{json, Value};

use crate::error_messages::{assert_refusal, assert_withholds};
use crate::table_version_deletion::listed_versions;
use crate::table_versions::{write_history, TABLE};
use crate::wire::{call, emulator, error_message, ok, ERROR_DETAIL_TAG};

/// The code every miss below is modelled with: "A specified entity does not exist"
/// (`EntityNotFoundException`, HTTP 400, from `smithy_query(query="operation",
/// operation="GetTableVersion,BatchDeleteTableVersion")`).
const ENTITY_NOT_FOUND: &str = "EntityNotFoundException";

/// The whole `Message` the table READ answers for its own miss — the sentence that names
/// nothing, which no operation of the version family answers.
const TABLE_READ_MISS: &str = "Entity Not Found";

/// The whole sentence a miss of a version the table once MINTED earns — one at or below
/// the counter its live definition carries, whose definition the history no longer holds.
const MINTED_VERSION_MISS: &str = "Table version not found.";

/// The whole sentence a miss of an id the table NEVER minted earns — one past that
/// counter, which has never stood for a definition of this table.
const NEVER_MINTED_MISS: &str = "Version not found.";

/// The database every row below is written into.
const DATABASE: &str = "version_miss_db";

/// Table names spanning the axis the table-miss template generalises over: the recorded
/// fixture's own shape, an already-folded name, a mixed-case one, an upper-case one, one
/// carrying the digits and punctuation `NameString`'s pattern admits, and one at the
/// member's documented 255-character ceiling (`NameString`, length 1..255, from
/// `smithy_query(query="operation",operation="GetTableVersion")`).
fn absent_tables() -> Vec<(&'static str, String)> {
    vec![
        (
            "the recorded fixture's shape",
            "sim2real-probe-aorigin-tvv-tbl-47fb6af6-v".to_string(),
        ),
        ("already folded", "absent_table".to_string()),
        ("mixed case", "Absent_Table".to_string()),
        ("upper case", "ABSENT_TABLE".to_string()),
        (
            "mixed case with digits and punctuation",
            "Team.Data_Lake-2026/Raw".to_string(),
        ),
        ("mixed case at the 255 ceiling", "Tt".repeat(127) + "T"),
    ]
}

/// The four-write history every row is written over: versions 0, 1 and 2 are archived and
/// 3 is the live definition.
fn four_writes() -> Vec<Value> {
    vec![
        json!({ "Name": TABLE, "Description": "as created" }),
        json!({ "Name": TABLE, "Description": "as updated once" }),
        json!({ "Name": TABLE, "Description": "as updated twice" }),
        json!({ "Name": TABLE, "Description": "as updated three times" }),
    ]
}

/// The requests of the version family that address the table `table`, each naming the
/// version `version_id` where it takes one.
fn family_requests(table: &str, version_id: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "GetTableVersion",
            json!({ "DatabaseName": DATABASE, "TableName": table, "VersionId": version_id }),
        ),
        (
            "GetTableVersion",
            json!({ "DatabaseName": DATABASE, "TableName": table }),
        ),
        (
            "GetTableVersions",
            json!({ "DatabaseName": DATABASE, "TableName": table }),
        ),
        (
            "DeleteTableVersion",
            json!({ "DatabaseName": DATABASE, "TableName": table, "VersionId": version_id }),
        ),
        (
            "BatchDeleteTableVersion",
            json!({
                "DatabaseName": DATABASE,
                "TableName": table,
                "VersionIds": [version_id],
            }),
        ),
    ]
}

// ---------------------------------------------------------------------------
// Rule 1: the table miss of the version family
// ---------------------------------------------------------------------------

/// Every operation over one table's version history — the single-version read, the
/// listing, the single-version delete and the batch delete, whose table is named once for
/// the whole batch — refuses a table the database does not hold by NAMING that table,
/// folded, and nothing else. The table read's own fixed sentence is answered by `GetTable`
/// alone, so the two misses stay distinguishable.
#[tokio::test]
async fn the_version_family_names_the_table_it_cannot_resolve() {
    let router = emulator();
    write_history(&router, DATABASE, &four_writes()).await;
    // A table created and then deleted is absent the same way a name nothing created is.
    ok(
        &router,
        "CreateTable",
        &json!({ "DatabaseName": DATABASE, "TableInput": { "Name": "Dropped_Table" } }),
    )
    .await;
    ok(
        &router,
        "DeleteTable",
        &json!({ "DatabaseName": DATABASE, "Name": "Dropped_Table" }),
    )
    .await;

    let mut rows = absent_tables();
    rows.push(("created then deleted", "Dropped_Table".to_string()));

    for (case, table) in &rows {
        let folded = table.to_ascii_lowercase();
        let expected = format!("Table {folded} not found.");
        for (operation, request) in family_requests(table, "0") {
            let label = format!("{case} via {operation} {request}");
            let body = assert_refusal(
                &router,
                &label,
                operation,
                &request,
                ENTITY_NOT_FOUND,
                &expected,
            )
            .await;
            let message = error_message(&body);
            // The refusal names the table the catalog keys, never the casing the request
            // sent, and never the database the caller also named.
            if table.as_str() != folded {
                assert!(
                    !message.contains(table.as_str()),
                    "case {label}: expected the folded table {folded:?}, got {message:?}"
                );
            }
            assert_withholds(&label, &message, DATABASE);
        }

        // The table READ words the same lookup its own way, so the two are distinguishable.
        let read = assert_refusal(
            &router,
            &format!("{case} via GetTable"),
            "GetTable",
            &json!({ "DatabaseName": DATABASE, "Name": table }),
            ENTITY_NOT_FOUND,
            TABLE_READ_MISS,
        )
        .await;
        assert_ne!(
            error_message(&read),
            expected,
            "case {case}: the table read and the version family must be distinguishable by \
             message"
        );
    }

    // Every refusal above wrote nothing: the stored table's whole history still stands.
    let listed = listed_versions(&router, DATABASE).await;
    let whole: Vec<String> = (0..four_writes().len()).map(|c| c.to_string()).collect();
    assert_eq!(
        listed, whole,
        "case after every table miss: the stored history is whole; expected {whole:?}, got \
         {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 2: the version miss carries no part of the request
// ---------------------------------------------------------------------------

/// One row of the version-miss table: an id of the stored table, and the sentence the
/// service words that miss with.
struct VersionMissCase {
    case: &'static str,
    /// The `VersionId` every operation of the family is asked for.
    version_id: &'static str,
    /// The whole message each of them must answer.
    expected: &'static str,
}

/// A version miss answers the service's sentence for that miss with NO part of the request
/// in it, and the batch's per-item `ErrorDetail$ErrorMessage` is that same sentence: the
/// id and the table travel in `TableVersionError`'s own members instead.
///
/// The rows differ along the addressed id's value and spelling and along whether the table
/// ever minted it — the two conditions the service words differently.
#[tokio::test]
async fn a_version_miss_is_worded_without_the_requested_id() {
    let router = emulator();
    let history = four_writes();
    write_history(&router, DATABASE, &history).await;
    // Version 1 is minted and then dropped, so addressing it again is the miss of a
    // version this table DID mint.
    ok(
        &router,
        "DeleteTableVersion",
        &json!({ "DatabaseName": DATABASE, "TableName": TABLE, "VersionId": "1" }),
    )
    .await;

    let cases = [
        VersionMissCase {
            case: "the counter after the newest version",
            version_id: "4",
            expected: NEVER_MINTED_MISS,
        },
        VersionMissCase {
            // The recorded batch item addressed an id far past the table's newest.
            case: "a counter far past the newest version",
            version_id: "99",
            expected: NEVER_MINTED_MISS,
        },
        VersionMissCase {
            case: "a never-minted counter, zero-padded",
            version_id: "0004",
            expected: NEVER_MINTED_MISS,
        },
        VersionMissCase {
            case: "a large never-minted counter",
            version_id: "12345",
            expected: NEVER_MINTED_MISS,
        },
        VersionMissCase {
            case: "a version the table minted and gave up",
            version_id: "1",
            expected: MINTED_VERSION_MISS,
        },
        VersionMissCase {
            case: "the same version, zero-padded",
            version_id: "01",
            expected: MINTED_VERSION_MISS,
        },
    ];

    assert_ne!(
        MINTED_VERSION_MISS, NEVER_MINTED_MISS,
        "the two version-miss sentences must stay distinguishable"
    );

    for row in &cases {
        let case = row.case;
        // The single-version read and the single-version delete both answer the sentence,
        // with no part of the id in it.
        for operation in ["GetTableVersion", "DeleteTableVersion"] {
            let label = format!("{case} via {operation}");
            let body = assert_refusal(
                &router,
                &label,
                operation,
                &json!({
                    "DatabaseName": DATABASE,
                    "TableName": TABLE,
                    "VersionId": row.version_id,
                }),
                ENTITY_NOT_FOUND,
                row.expected,
            )
            .await;
            assert_withholds(&label, &error_message(&body), row.version_id);
        }

        // The batch reports the same miss IN BAND at 200, in the same words, with the id
        // and the table in their own members.
        let label = format!("{case} via BatchDeleteTableVersion");
        let batch = ok(
            &router,
            "BatchDeleteTableVersion",
            &json!({
                "DatabaseName": DATABASE,
                "TableName": TABLE,
                "VersionIds": [row.version_id],
            }),
        )
        .await;
        let errors = batch["Errors"]
            .as_array()
            .unwrap_or_else(|| panic!("case {label}: Errors must be a list, got {batch}"));
        assert_eq!(
            errors.len(),
            1,
            "case {label}: the one addressed version missed, so exactly one item is \
             reported, got {batch}"
        );
        let entry = &errors[0];
        assert_eq!(
            entry["VersionId"].as_str(),
            Some(row.version_id),
            "case {label}: the record must name the id its item sent, got {entry}"
        );
        assert_eq!(
            entry["TableName"].as_str(),
            Some(TABLE),
            "case {label}: the record must name the table its item addressed, got {entry}"
        );
        let detail = &entry["ErrorDetail"];
        assert_eq!(
            detail["ErrorCode"].as_str(),
            Some(ENTITY_NOT_FOUND),
            "case {label}: expected ErrorCode {ENTITY_NOT_FOUND}, got {entry}"
        );
        assert_eq!(
            detail["ErrorMessage"].as_str(),
            Some(row.expected),
            "case {label}: expected ErrorMessage {:?}, got {entry}",
            row.expected
        );
        assert_eq!(
            detail["__type"].as_str(),
            Some(ERROR_DETAIL_TAG),
            "case {label}: expected the ErrorDetail type tag, got {entry}"
        );
        let reported = detail["ErrorMessage"].as_str().unwrap_or_default();
        assert_withholds(&label, reported, row.version_id);
    }

    // None of the misses above changed the history: versions 1 is the only one gone.
    let listed = listed_versions(&router, DATABASE).await;
    let expected: Vec<String> = vec!["0".to_string(), "2".to_string(), "3".to_string()];
    assert_eq!(
        listed, expected,
        "case after every version miss: only the version deleted up front is gone; expected \
         {expected:?}, got {listed:?}"
    );

    // A version the table still holds is served, so every refusal above is a miss and not
    // a broken lookup.
    let (status, served) = call(
        &router,
        "GetTableVersion",
        &json!({ "DatabaseName": DATABASE, "TableName": TABLE, "VersionId": "2" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case a version the table holds: expected 200, got {status} with {served}"
    );
    assert_eq!(
        served["TableVersion"]["VersionId"].as_str(),
        Some("2"),
        "case a version the table holds: expected version 2, got {served}"
    );
}
