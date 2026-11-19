// SPDX-License-Identifier: CC0-1.0

//! Rules of the export surface's REQUEST side, each pinned over a set of inputs rather
//! than one example:
//!
//! * an export must STATE the idempotency token it is asked for under: a request that
//!   leaves the member out, or states it as an explicit null, is refused — HTTP 400
//!   `ValidationException`, naming the member that must carry a value — and records
//!   nothing;
//! * an `S3Prefix` an export is asked to be written under must have at least
//!   [`state::MIN_EXPORT_S3_PREFIX_LENGTH`] characters: a shorter one is refused as a
//!   length violation of that member — stating the member, the value the request sent and
//!   the bound it broke — and records no export, while a prefix at or above the floor is
//!   served and read back by the export readers;
//! * a `TableArn` that addresses no table of this endpoint is refused as the miss this
//!   operation models, over the ARN the request sent;
//! * an export-addressed read of an ARN the catalogue holds no export for answers the
//!   modelled `ExportNotFoundException` with the sentence DynamoDB words that refusal in,
//!   over the ARN the request named and nothing else.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use, with Amazon S3 mounted beside this service on ONE engine
//! so the exports have a destination that really exists.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_dynamodb_dynamodb_20120810::state;

use crate::emulator::{
    call, create_bucket, describe_when_active, emulator_with_s3, error_code, export_arn, message,
    ok, REGION,
};

/// The bucket every export of this suite names as its destination.
const BUCKET: &str = "export-prefix-floor";

/// The table every export of this suite is taken from.
const TABLE: &str = "PrefixFloor";

/// One on-demand table with an item in it and a point in time to export from.
async fn exportable_table(router: &MultiServiceRouter) {
    ok(
        router,
        "CreateTable",
        json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "Part", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Part", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;
    describe_when_active(router, TABLE, TABLE).await;
    ok(
        router,
        "PutItem",
        json!({"TableName": TABLE, "Item": {"Part": {"S": "only"}}}),
    )
    .await;
    ok(
        router,
        "UpdateContinuousBackups",
        json!({
            "TableName": TABLE,
            "PointInTimeRecoverySpecification": {"PointInTimeRecoveryEnabled": true},
        }),
    )
    .await;
}

/// How many exports this endpoint holds for the suite's table — what shows that a refused
/// request recorded nothing.
async fn recorded_exports(router: &MultiServiceRouter) -> usize {
    let listed = ok(
        router,
        "ListExports",
        json!({"TableArn": state::table_arn(REGION, TABLE)}),
    )
    .await;
    listed["ExportSummaries"].as_array().map_or(0, Vec::len)
}

/// One `ExportTableToPointInTime` request asking for `prefix`, where the member is named at
/// all, under a client token of its own: the token is the member this surface requires
/// every export to be asked under, and one per call is what makes each row a NEW export.
fn export_body(prefix: Option<&str>) -> Value {
    let mut body = json!({
        "TableArn": state::table_arn(REGION, TABLE),
        "S3Bucket": BUCKET,
        "ClientToken": fresh_token(),
    });
    if let Some(prefix) = prefix {
        body["S3Prefix"] = json!(prefix);
    }
    body
}

/// One token no other call of this suite uses.
fn fresh_token() -> String {
    static NEXT: ::std::sync::atomic::AtomicUsize = ::std::sync::atomic::AtomicUsize::new(0);
    format!(
        "prefix-floor-{}",
        NEXT.fetch_add(1, ::std::sync::atomic::Ordering::Relaxed)
    )
}

#[tokio::test]
async fn an_s3_prefix_below_the_floor_is_refused_and_records_nothing() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    exportable_table(&router).await;

    // Rows differ in the LENGTH of the prefix the request names and in how its characters
    // are spelled — an empty value, one and two ASCII characters, and two characters that
    // are each several bytes — because a length bound counts characters rather than bytes
    // and holds whatever they are.
    let below: [(&str, &str); 5] = [
        ("an empty prefix", ""),
        ("one character", "a"),
        ("two characters", "ab"),
        ("two characters spelled with a separator", "a/"),
        ("two multi-byte characters", "€é"),
    ];

    for (case, prefix) in below {
        let (status, answered) = call(&router, "ExportTableToPointInTime", export_body(Some(prefix)))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a prefix below the floor is refused, got {status} with {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected the refusal a bad member earns, got {answered:?}"
        );
        assert_eq!(
            message(&answered),
            format!(
                "Invalid Request: 1 validation error detected: Value '{prefix}' at 's3Prefix' \
                 failed to satisfy constraint: Member must have length greater than or equal to 3"
            ),
            "case {case:?}: the refusal states the member, the value and the bound, got \
             {answered:?}"
        );
        assert_eq!(
            recorded_exports(&router).await,
            0,
            "case {case:?}: a refused export records nothing"
        );
    }
}

#[tokio::test]
async fn an_s3_prefix_at_or_above_the_floor_is_served_and_read_back() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    exportable_table(&router).await;

    // Rows differ in the prefix the export is written under, each at or above the floor,
    // plus the request that names the member at all: the floor holds only the values below
    // it, so everything here is served and echoed back by the single-export reader.
    let accepted: [(&str, Option<&str>); 6] = [
        ("no prefix at all", None),
        ("exactly the floor", Some("abc")),
        ("three multi-byte characters", Some("€éü")),
        ("a path", Some("snapshots/nightly")),
        ("a path spelled with slashes around it", Some("/nightly/")),
        (
            "a long prefix",
            Some("snapshots/nightly/2026/exports/table/part-of-a-much-longer-path"),
        ),
    ];

    let mut served = 0usize;
    for (case, prefix) in accepted {
        let (status, answered) =
            call(&router, "ExportTableToPointInTime", export_body(prefix)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a prefix at or above the floor is served, got {status} with \
             {answered:?}"
        );
        served += 1;
        let arn = export_arn(&answered);
        let described = ok(&router, "DescribeExport", json!({"ExportArn": arn.clone()})).await;
        let reported = &described["ExportDescription"];
        assert_eq!(
            reported["ExportArn"],
            json!(arn),
            "case {case:?}: the reader reports the export the call recorded, got {described:?}"
        );
        assert_eq!(
            reported["S3Prefix"],
            prefix.map_or(Value::Null, |prefix| json!(prefix)),
            "case {case:?}: the record carries the prefix the request named, got {described:?}"
        );
        assert_eq!(
            recorded_exports(&router).await,
            served,
            "case {case:?}: a served export is one more record in the catalogue"
        );
    }
}

#[tokio::test]
async fn an_export_must_state_the_idempotency_token_it_is_asked_under() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    exportable_table(&router).await;
    let table_arn = state::table_arn(REGION, TABLE);

    // Rows differ in HOW the request states the token, and in what else it carries: the
    // member left out, the member stated as an explicit null (which states the same
    // absence), and each of those beside another member of the request — because the
    // refusal is about the member the request does not state, whatever else it does.
    let without: [(&str, Value); 4] = [
        (
            "the member left out",
            json!({"TableArn": table_arn, "S3Bucket": BUCKET}),
        ),
        (
            "the member stated as null",
            json!({"TableArn": table_arn, "S3Bucket": BUCKET, "ClientToken": Value::Null}),
        ),
        (
            "left out beside a prefix",
            json!({"TableArn": table_arn, "S3Bucket": BUCKET, "S3Prefix": "snapshots/nightly"}),
        ),
        (
            "left out beside the export format",
            json!({"TableArn": table_arn, "S3Bucket": BUCKET, "ExportFormat": "DYNAMODB_JSON"}),
        ),
    ];

    for (case, body) in without {
        let (status, answered) = call(&router, "ExportTableToPointInTime", body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: an export asked for under no token is refused, got {status} with \
             {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected the refusal a bad member earns, got {answered:?}"
        );
        assert_eq!(
            message(&answered),
            "Invalid Request: 1 validation error detected: Value null at 'clientToken' failed \
             to satisfy constraint: Member must not be null",
            "case {case:?}: the refusal names the member that must carry a value, got \
             {answered:?}"
        );
        assert_eq!(
            recorded_exports(&router).await,
            0,
            "case {case:?}: a refused export records nothing"
        );
    }

    // Both members wrong at once is ONE refusal over both, counted and stated in the order
    // the input declares them (`ClientToken` before `S3Prefix`).
    let (status, answered) = call(
        &router,
        "ExportTableToPointInTime",
        json!({"TableArn": table_arn, "S3Bucket": BUCKET, "S3Prefix": "ab"}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case both members wrong: expected HTTP 400, got {status} with {answered:?}"
    );
    assert_eq!(
        message(&answered),
        "Invalid Request: 2 validation errors detected: Value null at 'clientToken' failed to \
         satisfy constraint: Member must not be null; Value 'ab' at 's3Prefix' failed to \
         satisfy constraint: Member must have length greater than or equal to 3",
        "case both members wrong: one refusal states both clauses, got {answered:?}"
    );

    // The control: the same request WITH a token is served, so the refusals above are
    // about the member they name.
    let served = ok(
        &router,
        "ExportTableToPointInTime",
        json!({
            "TableArn": table_arn,
            "S3Bucket": BUCKET,
            "ClientToken": "prefix-floor-control",
        }),
    )
    .await;
    assert_eq!(
        served["ExportDescription"]["ClientToken"],
        json!("prefix-floor-control"),
        "the record echoes the token the export was asked under, got {served:?}"
    );
    assert_eq!(
        recorded_exports(&router).await,
        1,
        "the served export is the one record the catalogue holds"
    );
}

#[tokio::test]
async fn a_table_arn_that_addresses_no_table_is_refused_over_the_arn_the_request_sent() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    exportable_table(&router).await;

    // Rows differ in the ARN the request addresses: a table of this endpoint that was
    // never created, one whose name is spelled like the suite's table with a suffix, and
    // one addressing a table of another account's name space — each a well-formed ARN of
    // this endpoint that resolves to no table.
    let absent: [(&str, String); 3] = [
        ("a table that was never created", "NeverCreated".to_string()),
        ("a name close to the suite's table", format!("{TABLE}Gone")),
        ("a name of its own", "exports.unknown-table".to_string()),
    ];

    for (case, name) in absent {
        let addressed = state::table_arn(REGION, &name);
        let (status, answered) = call(
            &router,
            "ExportTableToPointInTime",
            json!({
                "TableArn": addressed.clone(),
                "S3Bucket": BUCKET,
                "ClientToken": format!("absent-{name}"),
            }),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "TableNotFoundException",
            "case {case:?}: expected the miss this operation models, got {answered:?}"
        );
        assert_eq!(
            message(&answered),
            format!("Table not found: {addressed}"),
            "case {case:?}: the refusal echoes the table as the request named it, got \
             {answered:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_the_catalogue_holds_no_export_for_is_refused_as_aws_words_it() {
    let router = emulator_with_s3();
    create_bucket(&router, BUCKET).await;
    exportable_table(&router).await;

    // One export really exists, so the refusals below are about the ARNs they were given
    // rather than about an empty catalogue.
    let started = ok(
        &router,
        "ExportTableToPointInTime",
        export_body(Some("snapshots/nightly")),
    )
    .await;
    let recorded = export_arn(&started);

    // Rows differ in HOW the ARN misses: an export id this endpoint never minted, the same
    // id against another table of the same endpoint, an id spelled unlike any this service
    // mints, and the recorded ARN with one character of its id changed.
    let table_arn = state::table_arn(REGION, TABLE);
    let other_table_arn = state::table_arn(REGION, "NeverExported");
    let misses: [(&str, String); 4] = [
        (
            "an export that was never started",
            format!("{table_arn}/export/01700000000000-deadbeef"),
        ),
        (
            "a table of this endpoint that was never exported",
            format!("{other_table_arn}/export/01700000000000-deadbeef"),
        ),
        (
            "an id unlike any this service mints",
            format!("{table_arn}/export/no-such-export"),
        ),
        (
            "the recorded ARN with its id changed",
            format!("{recorded}0"),
        ),
    ];

    for (case, addressed) in misses {
        let (status, answered) = call(
            &router,
            "DescribeExport",
            json!({"ExportArn": addressed.clone()}),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ExportNotFoundException",
            "case {case:?}: expected the modelled miss of an export, got {answered:?}"
        );
        assert_eq!(
            message(&answered),
            format!("Export not found: Export not found: {addressed}"),
            "case {case:?}: the refusal is worded as DynamoDB words it over the ARN the \
             request named, got {answered:?}"
        );
    }

    // The ARN that WAS minted still reads back, so the refusals above are about the values
    // they were given and not about the reader.
    let read = ok(
        &router,
        "DescribeExport",
        json!({"ExportArn": recorded.clone()}),
    )
    .await;
    assert_eq!(
        read["ExportDescription"]["ExportArn"],
        json!(recorded),
        "the recorded export still reads back, got {read:?}"
    );
}
