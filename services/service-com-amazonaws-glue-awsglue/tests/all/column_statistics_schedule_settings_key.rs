// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the KEY the Glue column-statistics schedule pair names when the
//! settings record it looks up is not there: `StartColumnStatisticsTaskRunSchedule` and
//! `StopColumnStatisticsTaskRunSchedule`.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler.
//!
//! The rule under test: the schedule these two operations act on is a member of the
//! addressed table's `ColumnStatisticsTaskSettings`, so the entity they look up is that
//! SETTINGS record and an address holding none is refused with `EntityNotFoundException`
//! naming that record by its composite key — the catalog the request addressed, the account
//! whose estate that catalog is in, then the database and the table AS THE REQUEST SPELLED
//! THEM — whatever the catalog holds under those names on the way to it, and identically for
//! the two operations.
//!
//! Where [`crate::not_found_names_the_absent_entity`] tables WHICH part of the address is
//! absent, this suite tables the VALUES the key is built from and the record's lifecycle:
//! the rows differ along the casing the address is sent in against the casing the catalog
//! stored (the key follows the request, not the store), the characters the names carry
//! (a space and a dot, a tab, characters beyond ASCII), and the length of a name at the
//! published upper bound of `NameString`; and the lifecycle test drives the same address
//! through create, delete and create again, so the refusal is read off the stored record
//! rather than fixed to one answer.

use serde_json::{json, Value};

use crate::column_statistics_task_settings::{create_body, store_table};
// The two operations, the address they are asked with and the sentence their miss carries are
// the schedule suite's: this suite varies the VALUES the key is built from, so it drives the
// same helpers rather than keeping a second copy of them.
use crate::column_statistics_task_run_schedule::{
    schedule_body, settings_not_found, SCHEDULE_OPERATIONS,
};
use crate::wire::{call, emulator, error_code, error_message, ok, ACCOUNT_ID};

/// The error class both operations model for an entity that is not there
/// (`smithy_query operation=StartColumnStatisticsTaskRunSchedule,
/// StopColumnStatisticsTaskRunSchedule`).
const NOT_FOUND: &str = "EntityNotFoundException";

/// The upper bound of `NameString`, the shape `DatabaseName` and `TableName` target
/// (`length 1..255`, `smithy_query shape=NameString`).
const NAME_MAX_LENGTH: usize = 255;

/// A cron expression of the documented form, "to run something every day at 12:15 UTC"
/// (`Schedule$ScheduleExpression`).
const DAILY_SCHEDULE: &str = "cron(15 12 * * ? *)";

/// Assert that both operations refuse `body` with the modelled miss carrying `expected` —
/// which is also what makes the two agree, since one sentence is asserted of both.
async fn assert_both_report(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    body: &Value,
    expected: &str,
) {
    for operation in SCHEDULE_OPERATIONS {
        let (status, parsed) = call(router, operation, body).await;
        assert_eq!(
            status, 400,
            "case {case:?} / {operation}: an address holding no settings is refused, got \
             {status} with {parsed}"
        );
        assert!(
            error_code(&parsed).ends_with(NOT_FOUND),
            "case {case:?} / {operation}: expected {NOT_FOUND}, got {:?} with {:?}",
            error_code(&parsed),
            error_message(&parsed)
        );
        assert_eq!(
            error_message(&parsed),
            expected,
            "case {case:?} / {operation}: expected the refusal {expected:?}, got {:?}",
            error_message(&parsed)
        );
    }
}

/// Assert that both operations are served for `body`, each answering a document of its own
/// with no member in it.
async fn assert_both_served(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    body: &Value,
) {
    for operation in SCHEDULE_OPERATIONS {
        let answer = ok(router, operation, body).await;
        assert!(
            answer.as_object().is_some_and(|members| members.is_empty()),
            "case {case:?} / {operation}: expected an empty document, got {answer}"
        );
    }
}

/// One row of the key table below: the catalog state the row seeds, if any, and the address
/// the start and the stop are sent with.
struct KeyCase<'a> {
    /// What this row varies, named in each of its assertions.
    case: &'a str,
    /// The database and table the catalog is seeded with before the call, when the row seeds
    /// any — stored WITHOUT settings, so the record the two operations look up is absent
    /// while the names on the way to it are held.
    stored: Option<(&'a str, &'a str)>,
    /// The `DatabaseName` the request sends.
    database: String,
    /// The `TableName` the request sends.
    table: String,
    /// The members the request carries beyond the address.
    extra: Value,
    /// The catalog component of the key, which is the catalog the request addressed.
    catalog_id: &'a str,
}

/// The key the refusal names is built from the values the REQUEST sent: the names appear in
/// it exactly as they were spelled, whatever casing the catalog stored them under and
/// whichever characters `NameString` admits they carry.
#[tokio::test]
async fn the_settings_key_carries_the_names_the_request_sent() {
    let router = emulator();

    // A name at the published upper bound of `NameString`: one leading letter and the rest
    // filler, so the row is exactly as long as the model admits.
    let longest_name = format!("t{}", "a".repeat(NAME_MAX_LENGTH - 1));

    let cases: Vec<KeyCase<'_>> = vec![
        KeyCase {
            // The catalog resolves a database and a table without regard to case, so this
            // address names a table the catalog DOES hold — and the key still carries the
            // spelling the request sent, not the stored one.
            case: "an address cased differently from the stored table",
            stored: Some(("Echo_DB", "Echo_Table")),
            database: "echo_db".to_string(),
            table: "ECHO_TABLE".to_string(),
            extra: json!({}),
            catalog_id: ACCOUNT_ID,
        },
        KeyCase {
            // `NameString`'s pattern admits every character from U+0020 up, a space and a
            // dot included.
            case: "names carrying a space and a dot",
            stored: None,
            database: "sales db".to_string(),
            table: "daily.events".to_string(),
            extra: json!({}),
            catalog_id: ACCOUNT_ID,
        },
        KeyCase {
            // The pattern admits the tab as well, as its one control character.
            case: "names carrying a tab",
            stored: None,
            database: "tab\tdb".to_string(),
            table: "tab\ttable".to_string(),
            extra: json!({}),
            catalog_id: ACCOUNT_ID,
        },
        KeyCase {
            // And every character of the BMP beyond ASCII.
            case: "names carrying characters beyond ASCII",
            stored: None,
            database: "ventas_años".to_string(),
            table: "таблица".to_string(),
            extra: json!({}),
            catalog_id: ACCOUNT_ID,
        },
        KeyCase {
            case: "a table name at the published upper length bound",
            stored: None,
            database: "bound_db".to_string(),
            table: longest_name.clone(),
            extra: json!({}),
            catalog_id: ACCOUNT_ID,
        },
        KeyCase {
            // The catalog component comes from the request where it states one, which for
            // this account's own Data Catalog is its account id.
            case: "a database name at the published upper length bound, catalog stated",
            stored: None,
            database: longest_name.clone(),
            table: "bound_table".to_string(),
            extra: json!({ "CatalogID": ACCOUNT_ID }),
            catalog_id: ACCOUNT_ID,
        },
    ];

    for row in &cases {
        if let Some((database, table)) = row.stored {
            store_table(&router, database, table, &["id"]).await;
        }
        let body = schedule_body(&row.database, &row.table, &row.extra);
        let expected = settings_not_found(row.catalog_id, &row.database, &row.table);
        assert_both_report(row.case, &router, &body, &expected).await;
    }
}

/// The refusal is read off the stored record: the SAME address is refused with the settings
/// key before a create has written settings, served once they carry a schedule, refused with
/// that key again once a delete has removed them, and served again once a second create has
/// written them back.
#[tokio::test]
async fn the_refusal_follows_the_stored_settings_through_their_lifecycle() {
    let router = emulator();
    store_table(&router, "life_db", "orders", &["id"]).await;
    let body = schedule_body("life_db", "orders", &json!({}));
    let expected = settings_not_found(ACCOUNT_ID, "life_db", "orders");

    // A stored table no create has written settings for: the record the two operations look
    // up is the one that is absent.
    assert_both_report("before any create", &router, &body, &expected).await;

    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body("life_db", "orders", json!({ "Schedule": DAILY_SCHEDULE })),
    )
    .await;
    assert_both_served("settings carrying a schedule", &router, &body).await;

    ok(
        &router,
        "DeleteColumnStatisticsTaskSettings",
        &schedule_body("life_db", "orders", &json!({})),
    )
    .await;
    // The table is still stored; the settings are not — and it is the settings the refusal
    // names, by the same key as before the first create.
    assert_both_report("after the delete", &router, &body, &expected).await;

    ok(
        &router,
        "CreateColumnStatisticsTaskSettings",
        &create_body("life_db", "orders", json!({ "Schedule": DAILY_SCHEDULE })),
    )
    .await;
    assert_both_served("settings written back", &router, &body).await;
}
