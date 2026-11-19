// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a RECORDED table export reports about where its data
//! landed, how that destination is encrypted and how much the task moved.
//!
//! The rules pinned here:
//!
//! 1. `ItemCount` is "the number of items exported" — the items the task really wrote,
//!    counted exactly, which is the snapshot the export was taken of rather than the
//!    METERED figure the source table reports for itself;
//! 2. `S3SseAlgorithm` is the encryption IN FORCE on the destination the data landed in,
//!    RESOLVED rather than echoed: the algorithm the request named, the KMS-managed
//!    encryption where it pointed at a KMS key and named no algorithm, and otherwise the
//!    S3-managed default an S3 destination carries;
//! 3. the catalogue's READERS — `DescribeExport` and `ListExports` — report the kind of
//!    destination the record was written to, which is `S3` for every export this API can
//!    start, whatever status the record stands in; the response to the call that STARTS an
//!    export carries no such member;
//! 4. `BilledSizeBytes` is the source table's metered size when the export was taken —
//!    the same figure that table reports as `TableSizeBytes`, which DynamoDB "updates
//!    approximately every six hours" — and not a byte sum of the items the task wrote, so
//!    an export of a table whose metering has not run since its data was written is billed
//!    nothing while the items it wrote are still counted.
//!
//! Every DynamoDB call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same
//! `MultiServiceRouter` the emulator's front doors use, with Amazon S3 mounted beside it
//! on ONE engine, because an export's destination is that service's resource. Each rule is
//! asserted over a SET of rows — the members the request carries, the items the table
//! holds, the point in time the export is taken at, the status the record stands in and
//! whether the table has been metered — rather than one example.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::ops::types::S3_SSE_ALGORITHM_VALUES;
use service_com_amazonaws_dynamodb_dynamodb_20120810::state::{
    self, service_state, COUNTER_REFRESH_INTERVAL,
};

use crate::emulator::{
    create_bucket, describe_when_active, emulator_with_s3_and_engine, export_arn, ok,
    restorable_window, POLL_INTERVAL, REGION,
};

/// The bucket every export here writes to.
const BUCKET: &str = "export-reports";

/// A bucket the object store holds NO bucket for, which is what leaves an export's task
/// `FAILED` — a record whose destination kind is still the one it was pointed at.
const MISSING_BUCKET: &str = "export-reports-that-do-not-exist";

/// A KMS key id in the form the member takes, for the rows about an export that points at
/// one (`ExportTableToPointInTime$S3SseKmsKeyId`).
const KMS_KEY_ID: &str = "1234abcd-12ab-34cd-56ef-1234567890ab";

/// How long a poll for an export's `IN_PROGRESS` -> terminal transition may take before a
/// suite calls it a hang. Generous slack over the emulator's own span: the whole workspace
/// runs its suites in parallel, so a loaded machine is not a defect, and what the bound is
/// for is that the wait ENDS.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(180);

/// The emulator with the destination bucket already created through Amazon S3's own API,
/// together with the engine the store lives on.
async fn emulator_with_destination() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let (router, engine) = emulator_with_s3_and_engine();
    create_bucket(&router, BUCKET).await;
    (router, engine)
}

/// Create one on-demand table, wait out the span the clock owes a new table, put `items`
/// items in it and turn point-in-time recovery on — the state an export can be taken from.
async fn exportable_table(router: &MultiServiceRouter, name: &str, items: usize) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, name, name).await;
    for index in 0..items {
        ok(
            router,
            "PutItem",
            json!({
                "TableName": name,
                "Item": {
                    "Part": {"S": format!("item-{index}")},
                    "Payload": {"S": "x".repeat(24)},
                },
            }),
        )
        .await;
    }
    ok(
        router,
        "UpdateContinuousBackups",
        json!({
            "TableName": name,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
        }),
    )
    .await;
}

/// An `ExportTableToPointInTime` body for `table` carrying the two required members, the
/// idempotency token every export is asked for under — a token of its own per call, so each
/// body asks for a NEW export — and whatever else `extra` names.
fn export_body(table: &str, bucket: &str, extra: &Value) -> Value {
    let mut body = json!({
        "TableArn": state::table_arn(REGION, table),
        "S3Bucket": bucket,
        "ClientToken": fresh_token(),
    });
    for (member, value) in extra.as_object().cloned().unwrap_or_default() {
        body[member] = value;
    }
    body
}

/// One token no other call of this suite uses.
fn fresh_token() -> String {
    static NEXT: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
    format!(
        "reported-figures-{}",
        NEXT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    )
}

/// The `ExportDescription` of either operation's response.
fn description(body: &Value) -> Value {
    body["ExportDescription"].clone()
}

/// Poll `DescribeExport` until the record leaves `IN_PROGRESS`, answering the description
/// it reported then: the loop a client runs after starting an export.
async fn described_when_settled(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < TERMINAL_DEADLINE {
        let reported = description(&ok(router, "DescribeExport", json!({"ExportArn": arn})).await);
        if reported["ExportStatus"] != json!("IN_PROGRESS") {
            return reported;
        }
        last = reported;
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the export never settled within {TERMINAL_DEADLINE:?}, last {last}")
}

/// The summary `ListExports` reports for one `ExportArn`.
async fn summary_for(router: &MultiServiceRouter, table: &str, arn: &str) -> Value {
    let listed = ok(
        router,
        "ListExports",
        json!({"TableArn": state::table_arn(REGION, table)}),
    )
    .await;
    listed["ExportSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .find(|summary| summary["ExportArn"] == json!(arn))
        .unwrap_or(Value::Null)
}

/// What `DescribeTable` reports for one table.
async fn table_description(router: &MultiServiceRouter, name: &str) -> Value {
    ok(router, "DescribeTable", json!({"TableName": name})).await["Table"].clone()
}

#[tokio::test]
async fn an_export_reports_the_encryption_in_force_on_its_destination() {
    let (router, _engine) = emulator_with_destination().await;
    exportable_table(&router, "Encrypted", 2).await;

    // Rows differ in what the request says about encryption. The two named rows are built
    // from the model's own value table (`S3SseAlgorithm`, `smithy_query(query="shape",
    // shape="S3SseAlgorithm")`), so every value the member declares is covered, and the
    // other rows are the requests that name no algorithm at all.
    let mut cases: Vec<(String, Value, String)> = vec![
        (
            "neither an algorithm nor a key".to_string(),
            json!({}),
            "AES256".to_string(),
        ),
        (
            "a KMS key and no algorithm".to_string(),
            json!({"S3SseKmsKeyId": KMS_KEY_ID}),
            "KMS".to_string(),
        ),
    ];
    for algorithm in S3_SSE_ALGORITHM_VALUES {
        let mut asked = json!({"S3SseAlgorithm": algorithm});
        if *algorithm == "KMS" {
            asked["S3SseKmsKeyId"] = json!(KMS_KEY_ID);
        }
        cases.push((
            format!("{algorithm} named by the request"),
            asked,
            (*algorithm).to_string(),
        ));
    }

    for (case, extra, expected) in cases {
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            export_body("Encrypted", BUCKET, &extra),
        )
        .await;
        let reported = description(&started);
        let arn = export_arn(&started);
        assert_eq!(
            reported["S3SseAlgorithm"],
            json!(expected),
            "case {case:?}: expected the encryption in force on the destination \
             ({expected}), got {reported}"
        );
        // The reader answers the same from the record, before and after the task settles.
        let running = description(&ok(&router, "DescribeExport", json!({"ExportArn": &arn})).await);
        assert_eq!(
            running["S3SseAlgorithm"],
            json!(expected),
            "case {case:?}: the reader reports the encryption the record carries \
             ({expected}), got {running}"
        );
        let settled = described_when_settled(&router, &case, &arn).await;
        assert_eq!(
            (
                settled["ExportStatus"].clone(),
                settled["S3SseAlgorithm"].clone(),
            ),
            (json!("COMPLETED"), json!(expected)),
            "case {case:?}: a finished export reports the encryption it landed under \
             ({expected}), got {settled}"
        );
        // A key the request named is reported as asked for, and one it did not name is
        // absent: resolving the algorithm invents no key.
        assert_eq!(
            settled["S3SseKmsKeyId"],
            extra
                .get("S3SseKmsKeyId")
                .cloned()
                .unwrap_or(Value::Null),
            "case {case:?}: the key reported is the one the request named, got {settled}"
        );
    }
}

#[tokio::test]
async fn the_catalogue_readers_name_the_kind_of_destination_a_record_was_written_to() {
    let (router, _engine) = emulator_with_destination().await;
    exportable_table(&router, "Destined", 1).await;

    // Rows differ in what the record stands in and what the request asked for: the kind of
    // destination is a fact about the record, so neither the status nor the other members
    // can change it. The last row's destination holds no bucket, which leaves the task
    // FAILED — still an export whose destination was an S3 bucket.
    let cases: [(&str, &str, Value, bool); 4] = [
        ("only the required members", BUCKET, json!({}), true),
        (
            "a prefix and a format",
            BUCKET,
            json!({"S3Prefix": "snapshots/nightly/", "ExportFormat": "DYNAMODB_JSON"}),
            true,
        ),
        (
            "an export with a client token",
            BUCKET,
            json!({"ClientToken": "a-token-for-a-destined-export"}),
            true,
        ),
        ("a destination nothing holds", MISSING_BUCKET, json!({}), false),
    ];

    for (case, bucket, extra, wrote) in cases {
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            export_body("Destined", bucket, &extra),
        )
        .await;
        let reported = description(&started);
        let arn = export_arn(&started);
        assert!(
            reported["DestinationType"].is_null(),
            "case {case:?}: the call that starts an export reports no DestinationType, \
             got {reported}"
        );

        // While the task runs, and once it has settled, the single-export reader names the
        // destination kind.
        let running = description(&ok(&router, "DescribeExport", json!({"ExportArn": &arn})).await);
        assert_eq!(
            running["DestinationType"],
            json!("S3"),
            "case {case:?}: a running export's reader names the destination kind, \
             got {running}"
        );
        let settled = described_when_settled(&router, case, &arn).await;
        let expected_status = if wrote { "COMPLETED" } else { "FAILED" };
        assert_eq!(
            (
                settled["ExportStatus"].clone(),
                settled["DestinationType"].clone(),
            ),
            (json!(expected_status), json!("S3")),
            "case {case:?}: expected a {expected_status} record naming its destination \
             kind, got {settled}"
        );
        // And the listing says the same thing about the same record.
        let summary = summary_for(&router, "Destined", &arn).await;
        assert_eq!(
            (
                summary["ExportStatus"].clone(),
                summary["DestinationType"].clone(),
            ),
            (json!(expected_status), json!("S3")),
            "case {case:?}: the listing reports the record's status and destination kind, \
             got {summary}"
        );
    }
}

#[tokio::test]
async fn a_finished_export_counts_the_items_it_wrote() {
    let (router, _engine) = emulator_with_destination().await;

    // Rows differ in how many items the table holds when the export is taken — including
    // none at all — so the count is read from the data and not from the table's name.
    for items in [0_usize, 1, 2, 5] {
        let case = format!("a table holding {items} items");
        let table = format!("Counted{items}");
        exportable_table(&router, &table, items).await;
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            export_body(&table, BUCKET, &json!({})),
        )
        .await;
        let arn = export_arn(&started);
        assert!(
            description(&started)["ItemCount"].is_null(),
            "case {case:?}: a running export reports no ItemCount, got {started}"
        );
        let settled = described_when_settled(&router, &case, &arn).await;
        assert_eq!(
            (settled["ExportStatus"].clone(), settled["ItemCount"].clone()),
            (json!("COMPLETED"), json!(items)),
            "case {case:?}: expected the finished export to count {items} items, \
             got {settled}"
        );
        // The export counts what it wrote EXACTLY, while the table's own `ItemCount` is
        // the metered figure a write does not move — so the two disagree here, and the
        // export's is the one that is the data's.
        let described = table_description(&router, &table).await;
        assert_eq!(
            described["ItemCount"],
            json!(0),
            "case {case:?}: the table reports its metered count, which these writes did \
             not move, got {described}"
        );
    }
}

#[tokio::test]
async fn an_export_taken_in_the_past_counts_the_snapshot_it_wrote() {
    let (router, _engine) = emulator_with_destination().await;
    exportable_table(&router, "Historic", 2).await;
    // The newest RESTORABLE point, which trails the present: the two items above were
    // written before recovery was turned on, so they are in that snapshot, and the ones
    // written after it are not.
    let (_, latest) = restorable_window(&router, "Historic").await;
    for index in 0..3 {
        ok(
            &router,
            "PutItem",
            json!({
                "TableName": "Historic",
                "Item": {"Part": {"S": format!("later-{index}")}, "Payload": {"S": "y"}},
            }),
        )
        .await;
    }

    // Rows differ in the point in time the export is taken at: the snapshot of the past
    // holds the items the table held then, and one taken as the table stands holds every
    // item it holds now.
    let cases: [(&str, Value, usize); 2] = [
        ("the newest point the window reaches", json!({"ExportTime": latest}), 2),
        ("the table as it stands", json!({}), 5),
    ];

    for (case, extra, expected) in cases {
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            export_body("Historic", BUCKET, &extra),
        )
        .await;
        let arn = export_arn(&started);
        let settled = described_when_settled(&router, case, &arn).await;
        assert_eq!(
            (settled["ExportStatus"].clone(), settled["ItemCount"].clone()),
            (json!("COMPLETED"), json!(expected)),
            "case {case:?}: expected the export to count the {expected} items of the \
             point in time it was taken at, got {settled}"
        );
    }
}

#[tokio::test]
async fn an_export_is_billed_the_source_tables_metered_size() {
    let (router, engine) = emulator_with_destination().await;
    exportable_table(&router, "Billed", 3).await;
    exportable_table(&router, "BilledEmpty", 0).await;
    exportable_table(&router, "BilledMetered", 3).await;

    // The metering refresh is owed every six hours, which no test can wait for: it is
    // driven here the way a later request drives it, through the stored table, at an
    // instant one interval on — so one of the rows below has a metered table under it.
    {
        let state = service_state(&*engine).expect("the shared engine holds this service's store");
        let mut store = state.lock();
        store
            .table_mut(REGION, "BilledMetered")
            .expect("the table that was just created")
            .refresh_due_counters(SystemTime::now() + COUNTER_REFRESH_INTERVAL);
    }

    // Rows differ in what the SOURCE TABLE is metered at when the export is taken: a table
    // whose writes have not been metered yet, a table holding no items, and a table whose
    // metering has run over the items it holds. The expected value is read from the table
    // itself in each row, so what is asserted is the rule and not a number.
    let cases: [(&str, &str); 3] = [
        ("a table whose writes have not been metered yet", "Billed"),
        ("a table holding no items", "BilledEmpty"),
        ("a table whose metering has run", "BilledMetered"),
    ];

    let mut sizes: Vec<i64> = Vec::new();
    for (case, table) in cases {
        let metered = table_description(&router, table).await["TableSizeBytes"]
            .as_i64()
            .unwrap_or_default();
        let started = ok(
            &router,
            "ExportTableToPointInTime",
            export_body(table, BUCKET, &json!({})),
        )
        .await;
        let arn = export_arn(&started);
        assert!(
            description(&started)["BilledSizeBytes"].is_null(),
            "case {case:?}: a running export reports no BilledSizeBytes, got {started}"
        );
        let settled = described_when_settled(&router, case, &arn).await;
        assert_eq!(
            (
                settled["ExportStatus"].clone(),
                settled["BilledSizeBytes"].clone(),
            ),
            (json!("COMPLETED"), json!(metered)),
            "case {case:?}: expected the export to be billed the table's metered size \
             ({metered}), got {settled}"
        );
        sizes.push(metered);
    }

    // The rows above are not all the same figure: an unmetered table and an empty one
    // carry nothing billable, and a metered table holding items carries its measured
    // bytes — so the member moves with the state it is read from.
    assert_eq!(
        (sizes.first().copied(), sizes.get(1).copied()),
        (Some(0), Some(0)),
        "a table nothing has metered, and one holding no items, are billed nothing, got \
         {sizes:?}"
    );
    assert!(
        sizes.get(2).copied().unwrap_or_default() > 0,
        "a metered table holding items has a billable size, got {sizes:?}"
    );
}
