// SPDX-License-Identifier: CC0-1.0

//! What `DescribeStream` reports as the shard its page stopped at.
//!
//! The rule pinned here: `LastEvaluatedShardId` is the RESUME POINT a page offers — the
//! last shard the page carried — reported from the page that was served and never from
//! peeking past it for a further shard, because that is not what the member claims:
//! "If LastEvaluatedShardId is not empty, it does not necessarily mean that there is
//! more data in the result set. The only way to know when you have reached the end of
//! the result set is when LastEvaluatedShardId is empty"
//! (`StreamDescription$LastEvaluatedShardId`). So the one page that offers no cursor is
//! the one that evaluated no shard at all, which is what ends a consumer's walk.
//!
//! The rows span what the rule generalises over: how many shards the stream serves,
//! how wide the page asked for is (absent, narrower than the shard list, exactly as
//! wide, wider, and the published ceiling), where the page resumes from, whether a
//! `ShardFilter` narrowed it, and the two ways a page can evaluate nothing — a resume
//! point past the last shard, and a stream whose asynchronous enable is not due so it
//! serves no shard yet.
//!
//! A consumer's whole walk is then driven by the cursor, which is what the member is
//! for: it must visit every shard once and terminate.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams;
use smithy_server_runtime::MultiServiceRouter;
use streams::ops::types::KeySchemaElement;
use streams::state::{
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, StreamRegistration,
    StreamViewType, MAX_PAGE_LIMIT,
};

/// A SigV4 credential scope naming `dynamodb`, which is how a DynamoDB Streams
/// request is signed. Request authentication is off here, so only its routing signal
/// matters.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/dynamodb/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=deadbeef";

const REGION: &str = "us-east-1";

fn build() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(streams::EmulatedService::new(Arc::clone(&engine)));
    (engine, router)
}

async fn call(
    router: &MultiServiceRouter,
    operation: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header("authorization", AUTHORIZATION)
        .header(
            "x-amz-target",
            format!("DynamoDBStreams_20120810.{operation}"),
        )
        .header("content-type", "application/x-amz-json-1.0")
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(serde_json::Value::Null);
    (status, parsed)
}

/// An instant `seconds` before now, so a registered stream's enable is long due.
fn seconds_ago(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch");
    UNIX_EPOCH + now - Duration::from_secs(seconds)
}

/// An instant `seconds` AFTER now: a stream registered then has an enable that cannot
/// fall due while this suite runs, so it deterministically serves no shard.
fn seconds_ahead(seconds: u64) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the Unix epoch");
    UNIX_EPOCH + now + Duration::from_secs(seconds)
}

fn register(engine: &engine::InMemoryEngine, table_name: &str, at: SystemTime) -> String {
    engine
        .state::<StreamsState>()
        .lock()
        .register_stream(
            &StreamRegistration {
                table_name: table_name.to_string(),
                stream_view_type: StreamViewType::KeysOnly,
                key_schema: vec![KeySchemaElement {
                    attribute_name: "Id".to_string(),
                    key_type: "HASH".to_string(),
                }],
                region: REGION.to_string(),
            },
            at,
        )
        .unwrap_or_else(|rejected| panic!("the registry must accept {table_name}: {rejected}"))
}

/// Close the stream's open shard and open a child on it, so the stream serves one
/// more shard to page over.
fn roll_shard(engine: &engine::InMemoryEngine, stream_arn: &str, at: SystemTime) {
    engine
        .state::<StreamsState>()
        .lock()
        .roll_shard(stream_arn, at)
        .expect("the stream is registered and has an open shard");
}

/// One `DescribeStream` page: its shard ids in order, and the cursor it reported.
async fn page(
    router: &MultiServiceRouter,
    case: &str,
    body: serde_json::Value,
) -> (Vec<String>, Option<String>) {
    let (status, described) = call(router, "DescribeStream", body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStream must answer 200, got {status} with {described}"
    );
    let description = &described["StreamDescription"];
    let shard_ids: Vec<String> = description["Shards"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Shards must be a list, got {described}"))
        .iter()
        .map(|shard| {
            shard["ShardId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a shard has no ShardId: {shard}"))
                .to_string()
        })
        .collect();
    let cursor = description["LastEvaluatedShardId"]
        .as_str()
        .map(str::to_string);
    (shard_ids, cursor)
}

/// One row of the page table: what the case is, the `Limit` it sends (`None` for an
/// absent member), the index of the shard it resumes after (`None` for the start of
/// the list), and the span of the shard list the page must carry.
type PageCase = (
    &'static str,
    Option<i64>,
    Option<usize>,
    std::ops::Range<usize>,
);

/// A stream serving `shards` shards: the one its registration opened, plus a rolled
/// child for each shard past the first.
fn stream_with_shards(engine: &engine::InMemoryEngine, table_name: &str, shards: u64) -> String {
    let base = seconds_ago(7_200);
    let stream_arn = register(engine, table_name, base);
    for index in 1..shards {
        roll_shard(engine, &stream_arn, base + Duration::from_secs(index));
    }
    stream_arn
}

/// Whatever page was asked for, the cursor is the last shard the page carries — and a
/// page that carried no shard at all reports none.
#[tokio::test]
async fn the_cursor_is_the_last_shard_of_the_page() {
    let (engine, router) = build();
    let stream_arn = stream_with_shards(&engine, "ledger", 4);
    let (all_shards, cursor) = page(
        &router,
        "the whole shard list",
        serde_json::json!({ "StreamArn": stream_arn }),
    )
    .await;
    assert_eq!(
        all_shards.len(),
        4,
        "the fixture must serve four shards, got {all_shards:?}"
    );
    assert_eq!(
        cursor.as_deref(),
        all_shards.last().map(String::as_str),
        "case the whole shard list: the page stopped at its last shard, so that is the \
         cursor it offers, got {cursor:?}"
    );

    // Each row: the page width asked for (None for an absent member), where to resume
    // from, and the shards of `all_shards` the page must carry.
    let cases: [PageCase; 8] = [
        ("no Limit at all", None, None, 0..4),
        ("Limit 1, the narrowest page", Some(1), None, 0..1),
        ("Limit 3, narrower than the shard list", Some(3), None, 0..3),
        ("Limit 4, exactly the shard list", Some(4), None, 0..4),
        ("Limit 5, wider than the shard list", Some(5), None, 0..4),
        (
            "Limit 100, the published ceiling",
            Some(i64::from(MAX_PAGE_LIMIT)),
            None,
            0..4,
        ),
        ("resuming after the first shard", Some(2), Some(0), 1..3),
        ("resuming after the second-to-last shard", None, Some(2), 3..4),
    ];

    for (case, limit, resume_after, expected) in cases {
        let mut body = serde_json::json!({ "StreamArn": stream_arn });
        if let Some(target) = body.as_object_mut() {
            if let Some(limit) = limit {
                target.insert("Limit".to_string(), serde_json::json!(limit));
            }
            if let Some(index) = resume_after {
                target.insert(
                    "ExclusiveStartShardId".to_string(),
                    serde_json::json!(all_shards[index]),
                );
            }
        }
        let (shard_ids, cursor) = page(&router, case, body).await;
        assert_eq!(
            shard_ids,
            all_shards[expected.clone()].to_vec(),
            "case {case:?}: unexpected page, got {shard_ids:?}"
        );
        // The page offers the shard it stopped at, whether or not shards remain.
        let expected_cursor = shard_ids.last().cloned();
        assert_eq!(
            cursor, expected_cursor,
            "case {case:?}: expected the cursor {expected_cursor:?}, got {cursor:?}"
        );
    }

    // A resume point PAST the last shard evaluates nothing, which is the one thing
    // that ends the walk.
    let (shard_ids, cursor) = page(
        &router,
        "resuming after the last shard",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ExclusiveStartShardId": all_shards[3],
        }),
    )
    .await;
    assert!(
        shard_ids.is_empty(),
        "case resuming after the last shard: expected an empty page, got {shard_ids:?}"
    );
    assert_eq!(
        cursor, None,
        "case resuming after the last shard: a page that evaluated no shard reports no \
         cursor, got {cursor:?}"
    );

    // A filter that matches nothing evaluates nothing either: the last shard has no
    // children, so asking for them is an empty page and no cursor.
    let (shard_ids, cursor) = page(
        &router,
        "a filter that matches no child shard",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ShardFilter": { "Type": "CHILD_SHARDS", "ShardId": all_shards[3] },
        }),
    )
    .await;
    assert!(
        shard_ids.is_empty(),
        "case a filter that matches no child shard: expected an empty page, got {shard_ids:?}"
    );
    assert_eq!(
        cursor, None,
        "case a filter that matches no child shard: expected no cursor, got {cursor:?}"
    );

    // And a filter that DOES match reports the one shard it carried, which the next
    // call resumes after — one child, whose own child list is empty.
    let (shard_ids, cursor) = page(
        &router,
        "a filter that matches one child shard",
        serde_json::json!({
            "StreamArn": stream_arn,
            "ShardFilter": { "Type": "CHILD_SHARDS", "ShardId": all_shards[0] },
        }),
    )
    .await;
    assert_eq!(
        shard_ids,
        vec![all_shards[1].clone()],
        "case a filter that matches one child shard: expected that one child, got {shard_ids:?}"
    );
    assert_eq!(
        cursor.as_deref(),
        Some(all_shards[1].as_str()),
        "case a filter that matches one child shard: the page stopped at that child, got \
         {cursor:?}"
    );
}

/// A stream that serves no shard yet — its asynchronous enable is not due — reports
/// no cursor, because there was nothing to evaluate.
#[tokio::test]
async fn a_stream_serving_no_shard_reports_no_cursor() {
    let (engine, router) = build();
    // Registered an hour ahead of the clock, so the enable cannot fall due while this
    // test runs and the transient is deterministic rather than raced.
    let stream_arn = register(&engine, "pending", seconds_ahead(3_600));
    let (status, described) = call(
        &router,
        "DescribeStream",
        serde_json::json!({ "StreamArn": stream_arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "an enabling stream must still be described, got {status} with {described}"
    );
    let description = &described["StreamDescription"];
    assert_eq!(
        description["StreamStatus"].as_str(),
        Some("ENABLING"),
        "case an enabling stream: expected ENABLING, got {described}"
    );
    assert_eq!(
        description["Shards"].as_array().map(Vec::len),
        Some(0),
        "case an enabling stream: it serves no shard yet, got {described}"
    );
    assert!(
        description["LastEvaluatedShardId"].is_null(),
        "case an enabling stream: a page that evaluated no shard reports no cursor, got \
         {described}"
    );
}

/// The cursor is what a consumer's walk is driven by: following it visits every shard
/// exactly once, in order, and terminates — which is what an absent cursor means.
#[tokio::test]
async fn following_the_cursor_visits_every_shard_once_and_terminates() {
    let (engine, router) = build();
    let stream_arn = stream_with_shards(&engine, "ledger", 5);
    let (all_shards, _) = page(
        &router,
        "the whole shard list",
        serde_json::json!({ "StreamArn": stream_arn }),
    )
    .await;
    assert_eq!(
        all_shards.len(),
        5,
        "the fixture must serve five shards, got {all_shards:?}"
    );

    // Each row is a page width to walk the same stream with, so the walk is shown to
    // terminate whether a page carries one shard, some of them or all of them.
    for limit in [1_i64, 2, 5, 100] {
        let case = format!("walking with Limit {limit}");
        let mut seen: Vec<String> = Vec::new();
        let mut resume: Option<String> = None;
        // Generous: one call per shard plus the terminating one, and a few spare, so a
        // walk that fails to terminate is caught instead of looping for ever.
        for _ in 0..all_shards.len() + 4 {
            let mut body = serde_json::json!({ "StreamArn": stream_arn, "Limit": limit });
            if let (Some(target), Some(resume)) = (body.as_object_mut(), resume.as_ref()) {
                target.insert(
                    "ExclusiveStartShardId".to_string(),
                    serde_json::Value::String(resume.clone()),
                );
            }
            let (shard_ids, cursor) = page(&router, &case, body).await;
            seen.extend(shard_ids);
            match cursor {
                Some(cursor) => resume = Some(cursor),
                None => break,
            }
        }
        assert_eq!(
            seen, all_shards,
            "case {case:?}: the walk must visit every shard once, in order"
        );
    }
}
