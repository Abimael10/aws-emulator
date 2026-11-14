// SPDX-License-Identifier: CC0-1.0

//! How `GetTable` is addressed, and what it answers to a request that addresses
//! a table by neither whole form.
//!
//! The rule under test: `GetTable` offers two whole forms for naming a table and
//! takes EXACTLY one of them — the table's own ARN alone, or the table bucket
//! ARN, the namespace and the table name together (`smithy_query(query =
//! "operation", operation = "GetTable")` makes all four members optional, which
//! is what lets a request carry any mixture of them). Any other combination —
//! both forms at once, a part of the triple, nothing at all — is refused with
//! `BadRequestException` (400) and the one fixed sentence naming both forms,
//! before the request is resolved: a stale ARN beside a fresh triple names the
//! table twice over, and whichever member the service preferred would hide the
//! disagreement.
//!
//! The rows span every subset of the four members: both whole forms (served),
//! every proper non-empty subset of the triple, the triple with the ARN beside
//! it, the ARN with each single member of the triple, and the empty request.

use serde_json::json;

use crate::harness::{Emulator, Reply};

/// The region every case works in.
const REGION: &str = "us-east-1";

/// The fixed sentence a request addressing neither whole form is refused with,
/// spelled out here rather than read from the crate, so this suite pins the
/// wording itself.
const INVALID_ADDRESSING: &str = "Invalid input: Must provide either table arn alone OR all of \
                                  (table bucket arn, namespace, table name) together";

/// The namespace and table one bucket of this suite holds.
const NAMESPACE: &str = "analytics";
const TABLE: &str = "user_events";

/// The bucket ARN and the table ARN of the one table this suite reads.
struct Addressed {
    bucket_arn: String,
    table_arn: String,
}

/// Create a bucket, a namespace and one table, and answer both ARNs.
async fn addressed(emulator: &Emulator) -> Addressed {
    let created = emulator
        .create_table_bucket(REGION, &json!({ "name": "get-table-addressing" }))
        .await;
    assert_eq!(
        created.status, 200,
        "creating the bucket must succeed, got {status} {body}",
        status = created.status,
        body = created.body
    );
    let bucket_arn = created.text("arn");
    let namespace = emulator
        .create_namespace(REGION, &bucket_arn, &json!({ "namespace": [NAMESPACE] }))
        .await;
    assert_eq!(
        namespace.status, 200,
        "creating the namespace must succeed, got {status} {body}",
        status = namespace.status,
        body = namespace.body
    );
    let table = emulator
        .create_table(
            REGION,
            &bucket_arn,
            NAMESPACE,
            &json!({ "name": TABLE, "format": "ICEBERG" }),
        )
        .await;
    assert_eq!(
        table.status, 200,
        "creating the table must succeed, got {status} {body}",
        status = table.status,
        body = table.body
    );
    Addressed {
        table_arn: table.text("tableARN"),
        bucket_arn,
    }
}

/// Assert `reply` is the one refusal this rule states.
fn assert_refused(reply: &Reply, case: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected status 400, got {status} {body}",
        status = reply.status,
        body = reply.body
    );
    assert_eq!(
        reply.error_code.as_deref(),
        Some("BadRequestException"),
        "case {case:?}: expected BadRequestException, got {actual:?}",
        actual = reply.error_code
    );
    assert_eq!(
        reply.message(),
        INVALID_ADDRESSING,
        "case {case:?}: expected the fixed sentence, got {actual:?}",
        actual = reply.message()
    );
}

#[tokio::test]
async fn each_whole_addressing_form_resolves_the_same_table() {
    let emulator = Emulator::new();
    let table = addressed(&emulator).await;

    let by_arn = emulator.get_table_by_arn(REGION, &table.table_arn).await;
    assert_eq!(
        by_arn.status, 200,
        "the table ARN alone must resolve the table, got {status} {body}",
        status = by_arn.status,
        body = by_arn.body
    );
    let by_triple = emulator
        .get_table(REGION, &table.bucket_arn, NAMESPACE, TABLE)
        .await;
    assert_eq!(
        by_triple.status, 200,
        "the whole triple must resolve the table, got {status} {body}",
        status = by_triple.status,
        body = by_triple.body
    );
    assert_eq!(
        by_arn.body, by_triple.body,
        "both whole forms must resolve the same stored record"
    );
}

#[tokio::test]
async fn a_request_addressing_neither_whole_form_is_refused_before_it_is_resolved() {
    let emulator = Emulator::new();
    let table = addressed(&emulator).await;

    let bucket = ("tableBucketARN", table.bucket_arn.clone());
    let namespace = ("namespace", NAMESPACE.to_string());
    let name = ("name", TABLE.to_string());
    let arn = ("tableArn", table.table_arn.clone());

    // Every combination of the four members that is neither whole form: the two
    // forms together, the ARN beside each single member of the triple, every
    // proper non-empty subset of the triple, and a request that names nothing.
    let cases: Vec<(&str, Vec<(&str, String)>)> = vec![
        (
            "the table ARN and the whole triple together",
            vec![arn.clone(), bucket.clone(), namespace.clone(), name.clone()],
        ),
        (
            "the table ARN beside the bucket ARN",
            vec![arn.clone(), bucket.clone()],
        ),
        (
            "the table ARN beside the namespace",
            vec![arn.clone(), namespace.clone()],
        ),
        (
            "the table ARN beside the table name",
            vec![arn.clone(), name.clone()],
        ),
        ("the bucket ARN alone", vec![bucket.clone()]),
        ("the namespace alone", vec![namespace.clone()]),
        ("the table name alone", vec![name.clone()]),
        (
            "the namespace and the name without the bucket ARN",
            vec![namespace.clone(), name.clone()],
        ),
        (
            "the bucket ARN and the namespace without the name",
            vec![bucket.clone(), namespace.clone()],
        ),
        (
            "the bucket ARN and the name without the namespace",
            vec![bucket.clone(), name.clone()],
        ),
        ("no member at all", Vec::new()),
    ];

    for (case, query) in cases {
        let reply = emulator.get_table_with(REGION, &query).await;
        assert_refused(&reply, case);
    }

    // The refusal is of the REQUEST's shape, not of what it resolves to: the
    // same partial forms naming a table that does not exist are refused the same
    // way, so a client cannot tell the two apart by the error it gets.
    for (case, query) in [
        (
            "a partial triple naming no table",
            vec![
                ("tableBucketARN", table.bucket_arn.clone()),
                ("namespace", "never_created".to_string()),
            ],
        ),
        (
            "an ARN naming no table beside a triple",
            vec![
                (
                    "tableArn",
                    format!("{}/table/00000000-0000-4000-8000-000000000000", table.bucket_arn),
                ),
                ("tableBucketARN", table.bucket_arn.clone()),
                ("namespace", NAMESPACE.to_string()),
                ("name", TABLE.to_string()),
            ],
        ),
    ] {
        let reply = emulator.get_table_with(REGION, &query).await;
        assert_refused(&reply, case);
    }
}
