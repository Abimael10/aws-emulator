// SPDX-License-Identifier: CC0-1.0

//! A table's STREAM is a resource the tagging surface addresses, and its tags are its
//! own — driven end to end over this service's own wire protocol.
//!
//! The rule under test: `TagResource`, `UntagResource` and `ListTagsOfResource` take a
//! `ResourceArn` ("the Amazon DynamoDB resource with tags to be listed" —
//! `smithy_query(query="operation",operation="ListTagsOfResource")`, whose `ResourceArn`
//! is a `ResourceArnString` of length 1..1283 and is not constrained to a table), and a
//! table's stream is addressed by the `arn:…:table/<name>/stream/<label>` the table
//! reports as its `LatestStreamArn`. So:
//!
//! * a tag write addressed at the stream ARN lands on the STREAM and leaves the table's
//!   own set exactly as it was, and the other way round — the two resources carry
//!   independent sets, as their resource-based policies do;
//! * a removal addressed at one of them removes from that one only;
//! * a stream ARN of the right form naming a stream this endpoint does not hold is a
//!   MISSING RESOURCE (`ResourceNotFoundException`) rather than a malformed value: the
//!   value was read, and what is wrong is that no such stream is there;
//! * a sub-resource no tag set is held for — an index of the table — is still the input
//!   refusal it was.
//!
//! The rows below span what the rule generalises over: both resources of the table, each
//! tagging operation, several keys, and the ways a stream ARN can name no stream (a label
//! the table never published, a table that publishes no stream at all, and a table that
//! is not there).
//!
//! The tag READ is eventually consistent ("ListTagsOfResource API will only reflect the
//! changes after a few seconds" — the `TagResource` doc), so every read here POLLS for
//! the set it expects instead of waiting a span of its own choosing.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    call, emulator_with_streams, error_code, message, ok, streamed_table, ACTIVE_DEADLINE,
    POLL_INTERVAL, REGION,
};

/// The account id every generated emulator presents (`engine::mock_account_id`).
const ACCOUNT: &str = "000000000000";

/// The table both resources of this suite belong to.
const TABLE: &str = "stream-tagged";

/// `(key, value)` pairs as the modelled `TagList` carries them.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

/// The tag set `ListTagsOfResource` reports for `arn`, as a key -> value map.
async fn listed(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let answered = ok(router, "ListTagsOfResource", json!({"ResourceArn": arn})).await;
    answered["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| {
            (
                tag["Key"].as_str().unwrap_or_default().to_string(),
                tag["Value"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The set `expected` as this suite compares one.
fn expected_set(tags: &[(&str, &str)]) -> BTreeMap<String, String> {
    tags.iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// Poll `ListTagsOfResource` until `arn` reports `expected`, which is how a client reads a
/// set a tag write is still propagating to: the write landed on the resource at once and
/// the reader sees it a span later, so the test waits for the CONDITION.
async fn tags_when(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    expected: &[(&str, &str)],
) {
    let want = expected_set(expected);
    let started = std::time::Instant::now();
    let mut last = BTreeMap::new();
    while started.elapsed() < ACTIVE_DEADLINE {
        last = listed(router, arn).await;
        if last == want {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: expected {arn} to report {want:?} within {ACTIVE_DEADLINE:?}, last {last:?}");
}

#[tokio::test]
async fn a_streams_tags_are_its_own_and_not_its_tables() {
    let router = emulator_with_streams();
    let stream_arn = streamed_table(&router, "the tagged table", TABLE, "pk").await;
    // The table's own ARN as the service minted it, read back rather than assembled, so
    // the two ARNs under test are both the ones a client holds.
    let described = ok(&router, "DescribeTable", json!({"TableName": TABLE})).await;
    let table_arn = described["Table"]["TableArn"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        stream_arn.starts_with(&format!("{table_arn}/stream/")),
        "case the tagged table: the stream ARN extends the table's own, got {table_arn} and \
         {stream_arn}"
    );

    // Each resource is tagged through its own ARN, with sets that overlap in a key, so a
    // set leaking from one to the other would be visible in both the key list and the
    // value of the shared key.
    ok(
        &router,
        "TagResource",
        json!({"ResourceArn": &table_arn, "Tags": tag_list(&[("owner", "table"), ("env", "prod")])}),
    )
    .await;
    ok(
        &router,
        "TagResource",
        json!({"ResourceArn": &stream_arn, "Tags": tag_list(&[("owner", "stream"), ("feed", "cdc")])}),
    )
    .await;

    tags_when(
        &router,
        "the table after both writes",
        &table_arn,
        &[("owner", "table"), ("env", "prod")],
    )
    .await;
    tags_when(
        &router,
        "the stream after both writes",
        &stream_arn,
        &[("owner", "stream"), ("feed", "cdc")],
    )
    .await;

    // A removal addressed at the STREAM removes from the stream's set only, and one
    // addressed at the TABLE from the table's.
    ok(
        &router,
        "UntagResource",
        json!({"ResourceArn": &stream_arn, "TagKeys": ["owner"]}),
    )
    .await;
    tags_when(
        &router,
        "the stream after its own removal",
        &stream_arn,
        &[("feed", "cdc")],
    )
    .await;
    tags_when(
        &router,
        "the table after the stream's removal",
        &table_arn,
        &[("owner", "table"), ("env", "prod")],
    )
    .await;

    ok(
        &router,
        "UntagResource",
        json!({"ResourceArn": &table_arn, "TagKeys": ["env"]}),
    )
    .await;
    tags_when(
        &router,
        "the table after its own removal",
        &table_arn,
        &[("owner", "table")],
    )
    .await;
    tags_when(
        &router,
        "the stream after the table's removal",
        &stream_arn,
        &[("feed", "cdc")],
    )
    .await;
}

#[tokio::test]
async fn a_stream_arn_naming_no_stream_of_this_endpoint_is_a_missing_resource() {
    let router = emulator_with_streams();
    let held = streamed_table(&router, "the table with a stream", TABLE, "pk").await;
    // A second table that publishes NO stream, so one of the rows addresses a stream of a
    // table that is there.
    ok(
        &router,
        "CreateTable",
        json!({
            "TableName": "no-stream",
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        }),
    )
    .await;

    let stream_of = |table: &str, label: &str| {
        format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{table}/stream/{label}")
    };
    let rows = [
        (
            "a label the table never published",
            stream_of(TABLE, "2020-12-31T23:59:59.999"),
        ),
        (
            "a stream of a table that publishes none",
            stream_of("no-stream", "2026-01-01T00:00:00.000"),
        ),
        (
            "a stream of a table that is not there",
            stream_of("absent-table", "2026-01-01T00:00:00.000"),
        ),
    ];
    for (case, arn) in &rows {
        for (operation, body) in tagging_calls(arn) {
            let (status, answered) = call(&router, operation, body).await;
            assert_eq!(
                (status, error_code(&answered).as_str(), message(&answered)),
                (
                    400,
                    "ResourceNotFoundException",
                    format!("Requested resource not found: ResourceArn: {arn} not found")
                ),
                "case {case:?} on {operation}: expected the miss reported against the member, \
                 got {status} with {answered}"
            );
        }
    }

    // A sub-resource no tag set is held for is still the INPUT refusal, so the rows above
    // are about the stream being addressable rather than about any sub-resource ARN being
    // taken.
    let index_arn = format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/{TABLE}/index/by-gk");
    for (operation, body) in tagging_calls(&index_arn) {
        let (status, answered) = call(&router, operation, body).await;
        assert_eq!(
            (status, error_code(&answered).as_str(), message(&answered)),
            (
                400,
                "ValidationException",
                format!("Invalid TableArn: Invalid ResourceArn provided as input {index_arn}")
            ),
            "case an index sub-resource ARN on {operation}: expected the input refusal, got \
             {status} with {answered}"
        );
    }

    // And the stream the table DOES publish is served on every one of them.
    for (operation, body) in tagging_calls(&held) {
        let answered = ok(&router, operation, body).await;
        assert!(
            answered.is_null() || answered.is_object(),
            "case the table's own stream on {operation}: expected it to be served, got {answered}"
        );
    }
}

/// Every tagging operation over one `ResourceArn`, with the rest of a request that reaches
/// the resolution of that ARN.
fn tagging_calls(arn: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "TagResource",
            json!({"ResourceArn": arn, "Tags": tag_list(&[("probe", "1")])}),
        ),
        (
            "UntagResource",
            json!({"ResourceArn": arn, "TagKeys": ["probe"]}),
        ),
        ("ListTagsOfResource", json!({"ResourceArn": arn})),
    ]
}
