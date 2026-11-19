// SPDX-License-Identifier: CC0-1.0

//! A replica's OWN change feed: the stream each regional table of a global table carries.
//!
//! The rule these tests pin:
//!
//! > a replica of a table that publishes its changes is a regional table with a stream of
//! > its OWN Region, registered with DynamoDB Streams and capturing what the group's table
//! > captures — the replica reports it under `StreamSpecification` and `LatestStreamArn`,
//! > that service resolves it, and an item change replicated into the replica is a record of
//! > it; a replica of a table with NO stream has none.
//!
//! "A global table ... The table must have DynamoDB Streams enabled, with the stream
//! containing both the new and the old images of the item" (the `CreateGlobalTable` doc) is
//! the condition a replica is added under, and a stream is a resource of ONE Region — so the
//! replica's change feed cannot be the source table's.
//!
//! The rows differ along what the rule generalises over: what the group's stream captures
//! (every `StreamViewType` the model declares), and a table that enables no stream at all.
//!
//! Every test drives both services over the wire, in each Region, through the same
//! `MultiServiceRouter` the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{
    dispatch_as, emulator_with_streams, ok_in, records_of, table_when, ACTIVE_DEADLINE,
    POLL_INTERVAL, REGION,
};

/// The Region the replica of each row is created in.
const PEER_REGION: &str = "us-west-2";

/// Send one DynamoDB STREAMS operation in `region`. Both services sign as `dynamodb`, so
/// the `X-Amz-Target` prefix is the whole discriminator; the Region travels because a
/// replica's stream is a resource of the replica's own Region, which the suites that read
/// one table's stream never need.
async fn streams_in(
    router: &MultiServiceRouter,
    region: &str,
    operation: &str,
    body: Value,
) -> (u16, Value) {
    dispatch_as(
        router,
        region,
        "dynamodb",
        &format!("DynamoDBStreams_20120810.{operation}"),
        body,
    )
    .await
}

/// Every record the stream `stream_arn` holds, read in `region` from each shard's trim
/// horizon — the walk a change-data-capture consumer runs, bounded so a stream that never
/// advances fails rather than hanging.
async fn drain_stream_in(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    stream_arn: &str,
) -> Vec<Value> {
    let (status, described) =
        streams_in(router, region, "DescribeStream", json!({"StreamArn": stream_arn})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeStream in {region} must resolve the replica's stream, got \
         {status} with {described}"
    );
    let shards = described["StreamDescription"]["Shards"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert!(
        !shards.is_empty(),
        "case {case:?}: the replica's stream must serve a shard, got {described}"
    );
    let mut records = Vec::new();
    for shard in &shards {
        let (status, minted) = streams_in(
            router,
            region,
            "GetShardIterator",
            json!({
                "StreamArn": stream_arn,
                "ShardId": shard["ShardId"].as_str().unwrap_or_default(),
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetShardIterator in {region} answered {status} with {minted}"
        );
        let mut iterator = minted["ShardIterator"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        for _ in 0..16 {
            let (status, page) =
                streams_in(router, region, "GetRecords", json!({"ShardIterator": iterator})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetRecords in {region} answered {status} with {page}"
            );
            let batch = page["Records"].as_array().cloned().unwrap_or_default();
            records.extend(batch.iter().cloned());
            match page["NextShardIterator"].as_str() {
                Some(next) if !batch.is_empty() => iterator = next.to_string(),
                _ => break,
            }
        }
    }
    records
}

/// The `StreamViewType` values the model declares, in the order `StreamSpecification`'s own
/// enum states them (`smithy_query(query="shape",shape="StreamViewType")`), plus the table
/// that enables no stream at all.
const CAPTURED: [(&str, Option<&str>); 5] = [
    ("both images", Some("NEW_AND_OLD_IMAGES")),
    ("the new image", Some("NEW_IMAGE")),
    ("the old image", Some("OLD_IMAGE")),
    ("the keys alone", Some("KEYS_ONLY")),
    ("no stream at all", None),
];

#[tokio::test]
async fn a_replica_carries_a_stream_of_its_own_region_capturing_what_the_group_captures() {
    let router = emulator_with_streams();
    for (index, (case, view_type)) in CAPTURED.into_iter().enumerate() {
        let name = format!("Replicated.Stream{index}");
        let mut create = json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "Id", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
        });
        if let Some(view_type) = view_type {
            create["StreamSpecification"] =
                json!({"StreamEnabled": true, "StreamViewType": view_type});
        }
        ok_in(&router, REGION, "CreateTable", create).await;
        let source = table_when(&router, case, REGION, &name, |table| {
            table["TableStatus"] == json!("ACTIVE")
        })
        .await;
        ok_in(
            &router,
            REGION,
            "UpdateTable",
            json!({
                "TableName": name,
                "ReplicaUpdates": [{"Create": {"RegionName": PEER_REGION}}],
            }),
        )
        .await;
        let replica = table_when(&router, case, PEER_REGION, &name, |table| {
            table["TableStatus"] == json!("ACTIVE")
        })
        .await;

        let Some(view_type) = view_type else {
            // A replica of a table that publishes nothing has no change feed: no
            // specification, and no ARN for a consumer to read.
            assert_eq!(
                (
                    replica["StreamSpecification"].clone(),
                    replica["LatestStreamArn"].clone(),
                ),
                (Value::Null, Value::Null),
                "case {case:?}: expected the replica to report no stream, got {replica}"
            );
            continue;
        };

        // The replica reports a stream of its OWN Region, capturing what the group's table
        // captures — a different stream from the source table's.
        let replica_arn = replica["LatestStreamArn"].as_str().unwrap_or_default();
        let source_arn = source["LatestStreamArn"].as_str().unwrap_or_default();
        assert_eq!(
            (
                replica["StreamSpecification"].clone(),
                replica_arn.contains(&format!(":{PEER_REGION}:")),
                replica_arn == source_arn,
            ),
            (
                json!({"StreamEnabled": true, "StreamViewType": view_type}),
                true,
                false,
            ),
            "case {case:?}: expected a stream of {PEER_REGION} capturing {view_type}, got \
             {replica} beside {source}"
        );

        // ...and that stream is one DynamoDB Streams really holds in that Region, carrying
        // the change an item written in the group's other Region replicated in.
        ok_in(
            &router,
            REGION,
            "PutItem",
            json!({
                "TableName": name,
                "Item": {"Id": {"S": "replicated"}, "Status": {"S": "OPEN"}},
            }),
        )
        .await;
        let started = std::time::Instant::now();
        let records = loop {
            let drained = drain_stream_in(&router, case, PEER_REGION, replica_arn).await;
            let matching = records_of(&drained, "Id", "replicated");
            if !matching.is_empty() || started.elapsed() >= ACTIVE_DEADLINE {
                break matching;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: expected the replicated change as one record of the replica's own \
             stream, got {records:?}"
        );
    }
}
