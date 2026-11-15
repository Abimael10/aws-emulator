// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `DeletePartitionIndex`: taking ONE index out of the indexes of a
//! table the catalog holds, which is the last step of the partition-index lifecycle the
//! create and the read make up.
//!
//! Every call goes over the wire through the shared [`crate::wire`] harness, so what is
//! asserted is HTTP behaviour and not a direct call into a handler. The table a row deletes
//! from is created by the same helper the other two index suites use
//! ([`crate::partition_indexes::create_indexed_table`]) and read back through the same reader
//! ([`crate::partition_indexes::get_indexes`]), so what a delete did is asserted through the
//! operation a client would see it through. Only the suite that positions a record's DUE
//! instants reaches into the store, for the same reason
//! [`crate::partition_indexes`] does: a wire call is served at whatever instant it reaches
//! the service, so the side of a due instant a call lands on is chosen in the store rather
//! than raced for against the wall clock.
//!
//! The rules under test:
//!
//! * a delete is addressed through a STORED table and then by the index's own `IndexName` (an
//!   absent database, an absent table or a name the table holds no index under is refused and
//!   removes nothing), and an accepted delete takes that index out of the table's indexes:
//!   the read settles to a list without it, never reporting it built again;
//! * the status the index is in decides whether the delete is served at all — "when an index
//!   is in a CREATING state, the index or its table cannot be deleted"
//!   (`PartitionIndexDescriptor$IndexStatus`), and neither can one whose own removal is
//!   already under way, while one whose build has finished, succeeded or failed, can be;
//! * once the removal has settled the table holds no index of that name, so the name and the
//!   place it took against the per-table maximum are free for another registration.
//!
//! The rows differ along what the rules generalise over: how the deleted index got onto the
//! table (with its create, or registered on it afterwards), how many indexes the table holds
//! and where in the name order the deleted one sits, how many keys it indexes, which part of
//! a delete's address is missing, and which status the record is in when the delete arrives.

use std::sync::Arc;
use std::time::SystemTime;

use serde_json::{json, Value};
use service_com_amazonaws_glue_awsglue::ops::types::{
    BackfillError, KeySchemaElement, PartitionValueList,
};
use service_com_amazonaws_glue_awsglue::service::partition_indexes::{
    messages as index_messages, PartitionIndexEntry, PartitionIndexStore, TableIndexKey,
    BACKFILL_INVALID_PARTITION_TYPE_DATA, INDEX_STATUS_ACTIVE, INDEX_STATUS_CREATING,
    INDEX_STATUS_DELETING, INDEX_STATUS_FAILED, MAX_INDEXES_PER_TABLE,
    PARTITION_INDEX_SETTLES_AFTER,
};
use service_com_amazonaws_glue_awsglue::state::{fold_name, messages, ComAmazonawsGlueAwsglueState};
use smithy_server_runtime::MultiServiceRouter;

use crate::job_runs::{already_past, far_future};
use crate::partition_indexes::{create_indexed_table, get_indexes, reported, reported_names};
use crate::wire::{call, emulator, emulator_on, error_code, error_message, ok, ACCOUNT_ID};

/// How many spans a poll waits for one the service owes an index to fall due.
///
/// Deliberately generous — many times the span itself — and nothing is asserted about how
/// long anything took: what the poll waits for is the CONDITION the next read reports, so a
/// loaded or a fast host reaches the same verdict.
const SETTLE_SPANS: u32 = 20;

/// One row of the removal table.
struct RemovalCase {
    /// What this row varies.
    case: &'static str,
    /// The `PartitionKeys` the table declares.
    partition_keys: Value,
    /// The indexes the table's own create registers, as `CreateTable$PartitionIndexes`.
    with_table: Option<Value>,
    /// The indexes registered on the stored table afterwards, as `(IndexName, keys)`.
    registered: Vec<(&'static str, Vec<&'static str>)>,
    /// The `IndexName` the delete addresses.
    deleted: &'static str,
    /// The index names the read must settle to, in report order.
    remaining: Vec<&'static str>,
}

/// One row of the status table: the case, where the build's due instant sits, where the
/// removal's due instant sits (`None` when nothing has deleted the record), whether the
/// build found anything at fault, and the status the record is therefore in.
type StatusCase = (
    &'static str,
    fn() -> SystemTime,
    Option<fn() -> SystemTime>,
    bool,
    &'static str,
);

/// Register one index on `table`, answering the HTTP status and body.
async fn register(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    index_name: &str,
    keys: &[&str],
) -> (u16, Value) {
    call(
        router,
        "CreatePartitionIndex",
        &json!({
            "CatalogId": ACCOUNT_ID,
            "DatabaseName": database,
            "TableName": table,
            "PartitionIndex": { "IndexName": index_name, "Keys": keys }
        }),
    )
    .await
}

/// Delete one index of `table`, answering the HTTP status and body.
async fn delete_index(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    index_name: &str,
) -> (u16, Value) {
    call(
        router,
        "DeletePartitionIndex",
        &json!({
            "CatalogId": ACCOUNT_ID,
            "DatabaseName": database,
            "TableName": table,
            "IndexName": index_name
        }),
    )
    .await
}

/// The key of the records belonging to the STORED table `database`.`table`, read off the
/// table itself exactly as the handlers read it.
fn indexed_table(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    database: &str,
    table: &str,
) -> TableIndexKey {
    let state = engine.state::<ComAmazonawsGlueAwsglueState>();
    let store = state.lock();
    let stored = store
        .database(ACCOUNT_ID, database)
        .and_then(|database| database.tables.get(&fold_name(table)))
        .unwrap_or_else(|| panic!("case {case}: the table was created"));
    TableIndexKey::of(stored)
}

/// Poll the indexes of `table` until `settled` accepts what the read reports, answering the
/// last report. Each report is handed to `witness` as it is read, so a row can assert what
/// every intermediate read said as well as where the reads came to rest.
async fn poll_indexes(
    router: &MultiServiceRouter,
    database: &str,
    table: &str,
    settled: impl Fn(&Value) -> bool,
    mut witness: impl FnMut(&Value),
) -> Value {
    let deadline = SystemTime::now() + SETTLE_SPANS * PARTITION_INDEX_SETTLES_AFTER;
    loop {
        let (_, read) = get_indexes(router, database, table, None).await;
        witness(&read);
        if settled(&read) || SystemTime::now() >= deadline {
            return read;
        }
        // A fraction of the span, so the condition is looked at many times inside it.
        tokio::time::sleep(PARTITION_INDEX_SETTLES_AFTER / 10).await;
    }
}

/// Whether no index the read reports is still being built.
fn nothing_building(read: &Value) -> bool {
    reported(read)
        .iter()
        .all(|(_, _, status)| status != INDEX_STATUS_CREATING)
}

// ---------------------------------------------------------------------------
// An accepted delete takes the index out of its table's indexes
// ---------------------------------------------------------------------------

/// A delete of an index its table holds takes THAT index out: the read settles to the
/// table's other indexes, with the deleted one reported only as being deleted on the way
/// there and never as built again, and every other index left exactly as it was.
///
/// The rows differ along how the deleted index got onto the table, how many indexes the table
/// holds and where in the name order the deleted one sits, and how many keys it indexes.
#[tokio::test]
async fn a_delete_takes_the_named_index_out_of_its_tables_indexes() {
    let cases: Vec<RemovalCase> = vec![
        RemovalCase {
            case: "the only index, registered with its table",
            partition_keys: json!([ { "Name": "dt", "Type": "string" } ]),
            with_table: Some(json!([ { "IndexName": "dt_idx", "Keys": [ "dt" ] } ])),
            registered: Vec::new(),
            deleted: "dt_idx",
            remaining: Vec::new(),
        },
        RemovalCase {
            case: "the only index, registered on the stored table",
            partition_keys: json!([ { "Name": "hour", "Type": "int" } ]),
            with_table: None,
            registered: vec![("hour_idx", vec!["hour"])],
            deleted: "hour_idx",
            remaining: Vec::new(),
        },
        RemovalCase {
            case: "the first of three in name order",
            partition_keys: json!([
                { "Name": "region", "Type": "string" },
                { "Name": "dt", "Type": "date" }
            ]),
            with_table: Some(json!([
                { "IndexName": "a_idx", "Keys": [ "region" ] },
                { "IndexName": "b_idx", "Keys": [ "dt" ] },
                { "IndexName": "c_idx", "Keys": [ "dt", "region" ] }
            ])),
            registered: Vec::new(),
            deleted: "a_idx",
            remaining: vec!["b_idx", "c_idx"],
        },
        RemovalCase {
            case: "the middle of three in name order",
            partition_keys: json!([
                { "Name": "region", "Type": "string" },
                { "Name": "dt", "Type": "date" }
            ]),
            with_table: Some(json!([
                { "IndexName": "a_idx", "Keys": [ "region" ] },
                { "IndexName": "b_idx", "Keys": [ "dt" ] },
                { "IndexName": "c_idx", "Keys": [ "dt", "region" ] }
            ])),
            registered: Vec::new(),
            deleted: "b_idx",
            remaining: vec!["a_idx", "c_idx"],
        },
        RemovalCase {
            case: "the last of three in name order",
            partition_keys: json!([
                { "Name": "region", "Type": "string" },
                { "Name": "dt", "Type": "date" }
            ]),
            with_table: Some(json!([
                { "IndexName": "a_idx", "Keys": [ "region" ] },
                { "IndexName": "b_idx", "Keys": [ "dt" ] },
                { "IndexName": "c_idx", "Keys": [ "dt", "region" ] }
            ])),
            registered: Vec::new(),
            deleted: "c_idx",
            remaining: vec!["a_idx", "b_idx"],
        },
        RemovalCase {
            case: "one of two, the other registered on the stored table",
            partition_keys: json!([ { "Name": "year", "Type": "string" } ]),
            with_table: Some(json!([ { "IndexName": "a_idx", "Keys": [ "year" ] } ])),
            registered: vec![("z_idx", vec!["year"])],
            deleted: "a_idx",
            remaining: vec!["z_idx"],
        },
        RemovalCase {
            case: "a multi-key index, leaving one on a single key",
            partition_keys: json!([
                { "Name": "year", "Type": "int" },
                { "Name": "month", "Type": "int" }
            ]),
            with_table: None,
            registered: vec![
                ("ym_idx", vec!["year", "month"]),
                ("y_idx", vec!["year"]),
            ],
            deleted: "ym_idx",
            remaining: vec!["y_idx"],
        },
    ];

    let router = emulator();

    // Every row's table is created and registered first, so the spans the service owes the
    // builds run together rather than one row at a time.
    for (row, case) in cases.iter().enumerate() {
        let database = format!("delete_db_{row}");
        let table = format!("delete_table_{row}");
        let (status, body) = create_indexed_table(
            &router,
            &database,
            &table,
            &case.partition_keys,
            case.with_table.as_ref(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {}: the create must be served, got {status} with {body}",
            case.case
        );
        for (index_name, keys) in &case.registered {
            let (status, body) = register(&router, &database, &table, index_name, keys).await;
            assert_eq!(
                status, 200,
                "case {}: registering {index_name} must be served, got {status} with {body}",
                case.case
            );
        }
    }

    // An index is deleted only once its build has finished, which is what the status rule
    // says: the poll waits for that condition on each row's table.
    for (row, case) in cases.iter().enumerate() {
        let database = format!("delete_db_{row}");
        let table = format!("delete_table_{row}");
        let read = poll_indexes(&router, &database, &table, nothing_building, |_| {}).await;
        assert!(
            nothing_building(&read),
            "case {}: the builds must finish before the delete, got {read}",
            case.case
        );
    }

    for (row, case) in cases.iter().enumerate() {
        let database = format!("delete_db_{row}");
        let table = format!("delete_table_{row}");
        let (status, body) = delete_index(&router, &database, &table, case.deleted).await;
        assert_eq!(
            status, 200,
            "case {}: the delete must be served, got {status} with {body}",
            case.case
        );
        assert_eq!(
            body,
            json!({}),
            "case {}: the answer carries no member at all, got {body}",
            case.case
        );
    }

    for (row, case) in cases.iter().enumerate() {
        let database = format!("delete_db_{row}");
        let table = format!("delete_table_{row}");
        let expected: Vec<String> = case.remaining.iter().map(|name| name.to_string()).collect();
        let settled = {
            let expected = expected.clone();
            move |read: &Value| reported_names(read) == expected
        };
        // Every read on the way to the settled list is asserted too: the deleted index is
        // reported only as being deleted, and the table's other indexes never move.
        let read = poll_indexes(&router, &database, &table, settled, |read| {
            for (name, _, status) in reported(read) {
                if name == case.deleted {
                    assert_eq!(
                        status, INDEX_STATUS_DELETING,
                        "case {}: the deleted index may only be reported as being deleted, got \
                         {status} in {read}",
                        case.case
                    );
                } else {
                    assert!(
                        case.remaining.contains(&name.as_str()),
                        "case {}: the read reported {name}, which the table was not left with: \
                         {read}",
                        case.case
                    );
                }
            }
        })
        .await;
        assert_eq!(
            reported_names(&read),
            expected,
            "case {}: expected the table to be left with {expected:?}, got {:?} in {read}",
            case.case,
            reported_names(&read)
        );
    }
}

// ---------------------------------------------------------------------------
// The status the index is in decides whether the delete is served
// ---------------------------------------------------------------------------

/// Whether a delete is served is decided by the status the record is in: an index still being
/// built, and one whose own removal is already under way, are refused with the operation's
/// modelled `ConflictException` and left exactly as they were; an index whose build has
/// finished — whether it succeeded or failed — is deleted.
///
/// The rows POSITION the record's due instants rather than leaving a margin against the wall
/// clock, which is what makes the status a delete meets the row's own choice and not the
/// scheduler's.
#[tokio::test]
async fn the_status_an_index_is_in_decides_whether_a_delete_is_served() {
    // (case, where the build's due instant sits, where the removal's sits, whether the
    //  build found a partition at fault, the status the record is therefore in)
    let cases: [StatusCase; 4] = [
        (
            "an index whose build is still ahead of the clock",
            far_future,
            None,
            false,
            INDEX_STATUS_CREATING,
        ),
        (
            "an index the clock owes its built status, with nothing at fault",
            already_past,
            None,
            false,
            INDEX_STATUS_ACTIVE,
        ),
        (
            "an index whose build ended with a partition at fault",
            already_past,
            None,
            true,
            INDEX_STATUS_FAILED,
        ),
        (
            "an index whose own removal is still ahead of the clock",
            already_past,
            Some(far_future),
            false,
            INDEX_STATUS_DELETING,
        ),
    ];

    for (case, build_due, removal_due, at_fault, status) in &cases {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = emulator_on(Arc::clone(&engine));
        create_indexed_table(
            &router,
            "status_db",
            "events",
            &json!([ { "Name": "dt", "Type": "string" } ]),
            None,
        )
        .await;

        let table = indexed_table(&engine, case, "status_db", "events");
        engine.state::<PartitionIndexStore>().lock().record(
            table.clone(),
            vec![PartitionIndexEntry {
                index_name: "dt_idx".to_string(),
                keys: vec![KeySchemaElement {
                    name: "dt".to_string(),
                    r#type: "string".to_string(),
                }],
                active_at: build_due(),
                backfill_errors: if *at_fault {
                    vec![BackfillError {
                        code: Some(BACKFILL_INVALID_PARTITION_TYPE_DATA.to_string()),
                        partitions: Some(vec![PartitionValueList {
                            values: vec!["not-a-date".to_string()],
                        }]),
                    }]
                } else {
                    Vec::new()
                },
                removed_at: removal_due.map(|due| due()),
            }],
        );

        // An instant the delete cannot have reached, so what the record reports at it is the
        // status the delete left it in and not a race against the span.
        let before = SystemTime::now();
        let (http, body) = delete_index(&router, "status_db", "events", "dt_idx").await;

        let deletable = *status != INDEX_STATUS_CREATING && *status != INDEX_STATUS_DELETING;
        if deletable {
            assert_eq!(
                http, 200,
                "case {case}: a delete of an index in {status} must be served, got {http} with \
                 {body}"
            );
            let reported_now = engine
                .state::<PartitionIndexStore>()
                .lock()
                .descriptors(&table, before)
                .iter()
                .map(|descriptor| descriptor.index_status.clone())
                .collect::<Vec<String>>();
            assert_eq!(
                reported_now,
                vec![INDEX_STATUS_DELETING.to_string()],
                "case {case}: the accepted delete must leave the index being deleted, got \
                 {reported_now:?}"
            );
            let left = engine
                .state::<PartitionIndexStore>()
                .lock()
                .descriptors(&table, far_future());
            assert!(
                left.is_empty(),
                "case {case}: from the instant the removal is due the table holds no such \
                 index, got {left:?}"
            );
        } else {
            assert_eq!(
                http, 400,
                "case {case}: a delete of an index in {status} must be refused, got {http} with \
                 {body}"
            );
            assert_eq!(
                error_code(&body),
                "ConflictException",
                "case {case}: expected ConflictException, got {body}"
            );
            assert_eq!(
                error_message(&body),
                index_messages::index_not_deletable("dt_idx", status),
                "case {case}: expected the refusal to name the status, got {body}"
            );
            let (_, read) = get_indexes(&router, "status_db", "events", None).await;
            assert_eq!(
                reported(&read)
                    .iter()
                    .map(|(name, _, status)| (name.clone(), status.clone()))
                    .collect::<Vec<(String, String)>>(),
                vec![("dt_idx".to_string(), (*status).to_string())],
                "case {case}: the refused delete must leave the index as it was, got {read}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// A delete the catalog cannot resolve removes nothing
// ---------------------------------------------------------------------------

/// A delete is addressed through a stored table and then by the index's own `IndexName`: a
/// database the catalog does not hold, a table the database does not hold and a name the
/// table holds no index under are each refused with `EntityNotFoundException`, and the index
/// the table does hold is still reported afterwards.
///
/// The rows differ along which part of the address resolves to nothing, including an
/// `IndexName` that differs from the held one only in case — a partition index is addressed
/// by the name it was registered under, where a database and a table are addressed in the
/// Hive-compatible fold the catalog keys them under — and the shape of the name the refusal
/// has to echo back.
#[tokio::test]
async fn a_delete_the_catalog_cannot_resolve_removes_nothing() {
    let router = emulator();
    let (status, body) = create_indexed_table(
        &router,
        "miss_db",
        "events",
        &json!([ { "Name": "dt", "Type": "string" } ]),
        Some(&json!([ { "IndexName": "dt_idx", "Keys": [ "dt" ] } ])),
    )
    .await;
    assert_eq!(
        status, 200,
        "the table under test must be created, got {status} with {body}"
    );
    // A second table, holding no index at all, so a miss on a table that was never registered
    // with one is a row of its own.
    ok(
        &router,
        "CreateTable",
        &json!({
            "DatabaseName": "miss_db",
            "TableInput": {
                "Name": "bare",
                "StorageDescriptor": {
                    "Location": "s3://lake/bare/",
                    "Columns": [ { "Name": "payload", "Type": "string" } ]
                },
                "PartitionKeys": [ { "Name": "dt", "Type": "string" } ]
            }
        }),
    )
    .await;

    // (case, the database addressed, the table addressed, the IndexName addressed, the
    //  message the refusal must carry)
    let cases: Vec<(&str, &str, &str, &str, String)> = vec![
        (
            "a database the catalog does not hold",
            "absent_db",
            "events",
            "dt_idx",
            messages::database_not_found("absent_db"),
        ),
        (
            "a table the database does not hold",
            "miss_db",
            "absent_table",
            "dt_idx",
            messages::table_not_found("absent_table"),
        ),
        (
            "an IndexName the table holds no index under",
            "miss_db",
            "events",
            "absent_idx",
            index_messages::index_does_not_exist("absent_idx"),
        ),
        (
            "an IndexName of hyphens and digits nothing registered",
            "miss_db",
            "events",
            "events-idx-01",
            index_messages::index_does_not_exist("events-idx-01"),
        ),
        (
            "the held index's name in another case",
            "miss_db",
            "events",
            "DT_IDX",
            index_messages::index_does_not_exist("DT_IDX"),
        ),
        (
            "a table that holds no index at all",
            "miss_db",
            "bare",
            "dt_idx",
            index_messages::index_does_not_exist("dt_idx"),
        ),
    ];

    for (case, database, table, index_name, expected) in &cases {
        let (status, body) = delete_index(&router, database, table, index_name).await;
        assert_eq!(
            status, 400,
            "case {case}: the delete must be refused, got {status} with {body}"
        );
        assert_eq!(
            error_code(&body),
            "EntityNotFoundException",
            "case {case}: expected EntityNotFoundException, got {body}"
        );
        assert_eq!(
            &error_message(&body),
            expected,
            "case {case}: expected {expected:?}, got {:?} in {body}",
            error_message(&body)
        );

        let (status, read) = get_indexes(&router, "miss_db", "events", None).await;
        assert_eq!(
            status, 200,
            "case {case}: the read must be served, got {status} with {read}"
        );
        assert_eq!(
            reported_names(&read),
            vec!["dt_idx".to_string()],
            "case {case}: the refused delete must leave the table's index where it was, got \
             {read}"
        );
    }
}

// ---------------------------------------------------------------------------
// A settled removal frees the name and the place it took
// ---------------------------------------------------------------------------

/// Once a removal has settled the table holds no index of that name: the name is free for
/// another registration, and so is the place the deleted index took against the per-table
/// maximum — which a re-registration under the same `IndexName`, read back through
/// `GetPartitionIndexes`, is what proves.
#[tokio::test]
async fn a_settled_removal_frees_the_name_and_its_place_on_the_table() {
    assert_eq!(
        MAX_INDEXES_PER_TABLE, 3,
        "this test fills the table to the per-table maximum, which is three indexes"
    );

    let router = emulator();
    let (status, body) = create_indexed_table(
        &router,
        "reuse_db",
        "events",
        &json!([
            { "Name": "year", "Type": "int" },
            { "Name": "month", "Type": "int" }
        ]),
        Some(&json!([
            { "IndexName": "a_idx", "Keys": [ "year" ] },
            { "IndexName": "b_idx", "Keys": [ "month" ] },
            { "IndexName": "c_idx", "Keys": [ "year", "month" ] }
        ])),
    )
    .await;
    assert_eq!(
        status, 200,
        "the table must be created with its three indexes, got {status} with {body}"
    );

    // The table holds as many indexes as it can, so one more is refused while all three
    // stand — the place the delete below frees.
    let (status, body) = register(&router, "reuse_db", "events", "d_idx", &["year"]).await;
    assert_eq!(
        status, 400,
        "a fourth index must be refused while three stand, got {status} with {body}"
    );
    assert_eq!(
        error_code(&body),
        "ResourceNumberLimitExceededException",
        "expected the per-table limit refusal, got {body}"
    );

    let read = poll_indexes(&router, "reuse_db", "events", nothing_building, |_| {}).await;
    assert!(
        nothing_building(&read),
        "the builds must finish before the delete, got {read}"
    );

    let (status, body) = delete_index(&router, "reuse_db", "events", "b_idx").await;
    assert_eq!(
        status, 200,
        "the delete must be served, got {status} with {body}"
    );

    let settled = |read: &Value| reported_names(read) == vec!["a_idx".to_string(), "c_idx".to_string()];
    let read = poll_indexes(&router, "reuse_db", "events", settled, |_| {}).await;
    assert_eq!(
        reported_names(&read),
        vec!["a_idx".to_string(), "c_idx".to_string()],
        "the table must be left with the two indexes it still holds, got {read}"
    );

    // A second delete of the same name now addresses nothing: the table holds no index under
    // it any more.
    let (status, body) = delete_index(&router, "reuse_db", "events", "b_idx").await;
    assert_eq!(
        status, 400,
        "a second delete of the settled name must be refused, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        index_messages::index_does_not_exist("b_idx"),
        "expected the index miss, got {body}"
    );

    // The freed name takes a NEW index, on other keys, and the read reports that index's own
    // keys rather than the deleted one's.
    let (status, body) = register(&router, "reuse_db", "events", "b_idx", &["month", "year"]).await;
    assert_eq!(
        status, 200,
        "the freed name must take another index, got {status} with {body}"
    );
    let (status, read) = get_indexes(&router, "reuse_db", "events", None).await;
    assert_eq!(
        status, 200,
        "the read must be served, got {status} with {read}"
    );
    let registered = reported(&read)
        .into_iter()
        .find(|(name, _, _)| name == "b_idx")
        .unwrap_or_else(|| panic!("the re-registered index must be reported, got {read}"));
    assert_eq!(
        registered.1,
        vec![
            ("month".to_string(), "int".to_string()),
            ("year".to_string(), "int".to_string())
        ],
        "the re-registered index must carry its own keys, got {read}"
    );
    assert_eq!(
        reported_names(&read),
        vec!["a_idx".to_string(), "b_idx".to_string(), "c_idx".to_string()],
        "the table must hold three indexes again, got {read}"
    );
}
