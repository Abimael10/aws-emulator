// SPDX-License-Identifier: CC0-1.0

//! What happens to a SECOND destination change submitted while the first is still
//! being carried out, driven end to end over the same `MultiServiceRouter` the
//! emulator serves from.
//!
//! RULE, asserted over a table of inputs rather than for one recorded request: an
//! update the service answers with a cluster-operation handle is an ASYNCHRONOUS
//! operation — `UpdateChannel`'s output carries `ClusterOperationArn` and echoes no
//! destination member
//! (`smithy_query(query="operation",operation="UpdateChannel")`) — so an accepted
//! call leaves the channel in the non-terminal `ChannelStatus` that operation holds
//! it in, every reader of the channel reports that status and the operation behind
//! it, and a further update of the same channel is refused while the status is not
//! `ACTIVE`: HTTP 400 `BadRequestException` naming the status the channel is in
//! now. The channel returns to `ACTIVE` when the clock owes that operation its
//! completion, and only the value an ACCEPTED call carried stands — a refused one
//! leaves no trace.
//!
//! The rows differ along what the rule generalises over: the destination kind, the
//! value the accepted update carries (each published bound of the documented range
//! and a value between them, from
//! `smithy_query(query="shape",shape="S3DestinationUpdate")`: "Allowed range: 300
//! to 900"), the value the refused one carries, and how many overlapping calls are
//! made inside the one window.
//!
//! Every wait here polls for the CONDITION under a deadline far longer than any
//! span the service owes a transition, never for a fixed time, so nothing below
//! depends on how fast the host runs. The harness — `Emulator`, `Reply`, `pct`,
//! `poll_until_channel_active`, `poll_until_operation_complete` — is the shared one
//! in `all/kafka_harness.rs`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_kafka_kafka::state::{
    MAX_DATA_FRESHNESS_SECONDS, MIN_DATA_FRESHNESS_SECONDS,
};

use crate::kafka_harness::{
    pct, poll_until_channel_active, poll_until_operation_complete, ClusterRequest, Emulator, Reply,
    REGION,
};

/// The role a channel assumes to write to its destination; any role ARN does.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-channel-races";
/// The dead-letter bucket every row below points at.
const DLQ_BUCKET_ARN: &str = "arn:aws:s3:::msk-race-dlq";
/// The destination bucket the S3 rows deliver into.
const RECORDS_BUCKET_ARN: &str = "arn:aws:s3:::msk-race-records";

/// How long a row keeps looking for the window in which the first update is still
/// in flight before the suite calls it a failure: far longer than any span the
/// service owes a transition, so the deadline says nothing about how fast the host
/// runs.
const WINDOW_DEADLINE: Duration = Duration::from_secs(60);

/// Where a channel delivers: the two values `ChannelDestinationType` names, which
/// is the kind an update's payload has to agree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    S3,
    Iceberg,
}

impl Destination {
    /// The member a `CreateChannel` carries this destination's configuration under.
    fn configuration_member(self) -> &'static str {
        match self {
            Self::S3 => "s3DestinationConfiguration",
            Self::Iceberg => "icebergDestinationConfiguration",
        }
    }

    /// The member an `UpdateChannel` carries this destination's change under.
    fn update_member(self) -> &'static str {
        match self {
            Self::S3 => "s3DestinationUpdate",
            Self::Iceberg => "icebergDestinationUpdate",
        }
    }

    /// Where `DescribeChannel` reports this destination's buffering parameter.
    fn freshness_pointer(self) -> &'static str {
        match self {
            Self::S3 => "/s3DestinationConfiguration/dataFreshnessInSeconds",
            Self::Iceberg => "/icebergDestinationConfiguration/dataFreshnessInSeconds",
        }
    }
}

/// One row: the channel to create, the value the update that WINS the window
/// carries, the value each refused overlapping update carries, and how many of
/// those overlapping calls are made.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    name: &'static str,
    destination: Destination,
    /// The `dataFreshnessInSeconds` the create submits.
    created: i32,
    /// The value the accepted update asks for.
    accepted: i32,
    /// The value every refused overlapping update asks for — different from
    /// `accepted`, so a lost write would be visible as that value.
    refused: i32,
    /// How many overlapping updates are submitted inside the one window.
    overlaps: usize,
}

/// The rows: both destination kinds, the published bounds of the documented range
/// and a value between them as the accepted value, a different value as the
/// refused one, and one row that submits more than one overlapping call.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "s3 at the documented floor, one overlap",
            name: "race-s3-floor",
            destination: Destination::S3,
            created: 600,
            accepted: MIN_DATA_FRESHNESS_SECONDS,
            refused: MAX_DATA_FRESHNESS_SECONDS,
            overlaps: 1,
        },
        Row {
            case: "s3 between the bounds, two overlaps",
            name: "race-s3-middle",
            destination: Destination::S3,
            created: MIN_DATA_FRESHNESS_SECONDS,
            accepted: 450,
            refused: 601,
            overlaps: 2,
        },
        Row {
            case: "iceberg at the documented ceiling, one overlap",
            name: "race-iceberg-ceiling",
            destination: Destination::Iceberg,
            created: 450,
            accepted: MAX_DATA_FRESHNESS_SECONDS,
            refused: MIN_DATA_FRESHNESS_SECONDS,
            overlaps: 1,
        },
    ]
}

/// The `CreateChannel` body this row submits.
fn channel_body(row: &Row) -> Value {
    let destination = match row.destination {
        Destination::S3 => json!({
            "dataFreshnessInSeconds": row.created,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN },
            "serviceExecutionRoleArn": ROLE_ARN,
            "storage": {
                "bucketArn": RECORDS_BUCKET_ARN,
                "compressionType": "NONE",
                "storageClass": "STANDARD",
                "outputPrefix": "records/",
            },
        }),
        Destination::Iceberg => json!({
            "dataFreshnessInSeconds": row.created,
            "appendOnly": true,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN },
            "destinationTableList": [{
                "destinationDatabaseName": "records",
                "destinationTableName": "events",
                "partitionSpec": {
                    "partitionStrategy": "TIME_HOUR",
                    "sourceList": [{ "sourceName": "event_time" }],
                },
            }],
            "schemaEvolution": { "enableSchemaEvolution": true },
            "serviceExecutionRoleArn": ROLE_ARN,
            "tableCreation": { "enableTableCreation": true },
        }),
    };
    let mut body = json!({
        "channelName": row.name,
        "topicConfigurationList": [{
            "recordConverter": { "valueConverter": "JSON" },
            "topicArn": format!("arn:aws:kafka:{REGION}:000000000000:topic/races/orders"),
        }],
    });
    body[row.destination.configuration_member()] = destination;
    body
}

/// `UpdateChannel` on `channel_arn`, asking `destination` for `seconds`.
async fn update_channel(
    emulator: &Emulator,
    cluster_arn: &str,
    channel_arn: &str,
    destination: Destination,
    seconds: i32,
) -> Reply {
    emulator
        .call(
            "PUT",
            &format!(
                "/v1/clusters/{}/channels/{}",
                pct(cluster_arn),
                pct(channel_arn)
            ),
            Some(json!({ destination.update_member(): { "dataFreshnessInSeconds": seconds } })),
        )
        .await
}

/// What the service answers an `UpdateChannel` on a channel whose status is
/// `status`: fixed text, with the status the channel's own record is in between its
/// two halves.
fn not_updatable_message(status: &str) -> String {
    format!("Channel cannot be updated in state {status}. Channel must be in ACTIVE state to be updated.")
}

/// The channel this row describes, created under `cluster_arn` and settled, with
/// its ARN handed back.
async fn settled_channel(emulator: &Emulator, cluster_arn: &str, row: &Row) -> String {
    let created = emulator.create_channel(cluster_arn, channel_body(row)).await;
    assert_eq!(
        created.status, 200,
        "case {}: expected CreateChannel to be accepted, got {} {}",
        row.case, created.status, created.body
    );
    let arn = created
        .at("/channelArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {}: CreateChannel returned no channelArn", row.case))
        .to_string();
    poll_until_channel_active(emulator, cluster_arn, &arn, row.case).await;
    arn
}

/// The entry `ListChannels` reports for `channel_arn`, which must be exactly one.
async fn listed_channel(emulator: &Emulator, cluster_arn: &str, channel_arn: &str, case: &str) -> Value {
    let listed = emulator.list_channels(cluster_arn, "").await;
    let entries: Vec<Value> = listed
        .at("/channels")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.get("channelArn").and_then(Value::as_str) == Some(channel_arn))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "case {case}: expected the listing to hold the channel exactly once, got {}",
        listed.body
    );
    entries[0].clone()
}

/// RULE: while the cluster operation an accepted `UpdateChannel` handed back is
/// still in flight, the channel reports the non-terminal `UPDATING` status to every
/// reader, together with that operation, and a further `UpdateChannel` is refused
/// with HTTP 400 `BadRequestException` naming that status — the refused call
/// changing neither the status, nor the stored buffering parameter, nor the
/// operation in flight.
#[tokio::test]
async fn an_update_overlapping_one_in_flight_is_refused_naming_the_status_the_channel_is_in() {
    let emulator = Emulator::new();
    let cluster = emulator.create_ok(&ClusterRequest::named("racing")).await;

    for row in rows() {
        let channel = settled_channel(&emulator, &cluster, &row).await;

        // Look for the window the first update holds open. The service may have
        // finished carrying the change out before the second call is made — which
        // is the other behaviour AWS is permitted here — so a round that misses it
        // settles the channel and tries again, under a deadline rather than a
        // fixed wait.
        let deadline = Instant::now() + WINDOW_DEADLINE;
        let mut rounds = 0_u32;
        let operation = loop {
            rounds += 1;
            let accepted =
                update_channel(&emulator, &cluster, &channel, row.destination, row.accepted).await;
            assert_eq!(
                accepted.status, 200,
                "case {}: expected UpdateChannel to be accepted, got {} {}",
                row.case, accepted.status, accepted.body
            );
            let operation = accepted
                .at("/clusterOperationArn")
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {}: UpdateChannel returned no operation ARN", row.case)
                })
                .to_string();
            let described = emulator.describe_channel(&cluster, &channel).await;
            if described.at("/status").as_str() == Some("UPDATING") {
                assert_eq!(
                    described.at("/clusterOperationArn").as_str(),
                    Some(operation.as_str()),
                    "case {}: expected the channel to report the operation carrying its \
                     change, got {}",
                    row.case,
                    described.body
                );
                assert_eq!(
                    described.at(row.destination.freshness_pointer()).as_i64(),
                    Some(i64::from(row.accepted)),
                    "case {}: expected the accepted value stored by the accepting call, \
                     got {}",
                    row.case,
                    described.body
                );
                break operation;
            }
            assert!(
                Instant::now() < deadline,
                "case {}: the channel was never observed under its in-flight update after \
                 {rounds} rounds, last read {}",
                row.case,
                described.body
            );
            poll_until_channel_active(&emulator, &cluster, &channel, row.case).await;
        };

        // The listing agrees with the read of the one channel: the same status, and
        // the same in-flight operation, which `ChannelInfo$ClusterOperationArn` is
        // reported for exactly while the channel is not settled.
        let listed = listed_channel(&emulator, &cluster, &channel, row.case).await;
        assert_eq!(
            (
                listed.get("status").and_then(Value::as_str),
                listed.get("clusterOperationArn").and_then(Value::as_str),
            ),
            (Some("UPDATING"), Some(operation.as_str())),
            "case {}: expected the listing to report the channel under its in-flight \
             operation, got {listed}",
            row.case
        );

        // Every overlapping update inside that window is refused, and refusing it
        // moves nothing.
        for attempt in 1..=row.overlaps {
            let case = format!("{} (overlap {attempt})", row.case);
            let refused =
                update_channel(&emulator, &cluster, &channel, row.destination, row.refused).await;
            assert_eq!(
                (
                    refused.status,
                    refused.error_code(),
                    refused.error_message()
                ),
                (
                    400,
                    "BadRequestException",
                    not_updatable_message("UPDATING").as_str()
                ),
                "case {case}: expected the overlapping update to be refused for the status \
                 the channel is in, got {} {}",
                refused.status,
                refused.body
            );
            let described = emulator.describe_channel(&cluster, &channel).await;
            assert_eq!(
                (
                    described.at("/status").as_str(),
                    described.at("/clusterOperationArn").as_str(),
                    described.at(row.destination.freshness_pointer()).as_i64(),
                ),
                (
                    Some("UPDATING"),
                    Some(operation.as_str()),
                    Some(i64::from(row.accepted))
                ),
                "case {case}: a refused update must move neither the status, nor the \
                 operation in flight, nor the stored value, got {}",
                described.body
            );
        }

        // Once the operation is done, the channel is `ACTIVE` again and only the
        // accepted value stands: the refused one left no trace.
        let settled = poll_until_channel_active(&emulator, &cluster, &channel, row.case).await;
        assert_eq!(
            settled.at(row.destination.freshness_pointer()).as_i64(),
            Some(i64::from(row.accepted)),
            "case {}: expected the settled channel to report the accepted value only, got {}",
            row.case,
            settled.body
        );
        poll_until_operation_complete(&emulator, &operation, row.case).await;

        // And a settled channel takes the next update, so the refusal was about the
        // in-flight operation rather than about the channel.
        let again =
            update_channel(&emulator, &cluster, &channel, row.destination, row.refused).await;
        assert_eq!(
            again.status, 200,
            "case {}: expected the settled channel to accept a further update, got {} {}",
            row.case, again.status, again.body
        );
        let later = again
            .at("/clusterOperationArn")
            .as_str()
            .unwrap_or_else(|| panic!("case {}: UpdateChannel returned no operation ARN", row.case))
            .to_string();
        assert_ne!(
            later, operation,
            "case {}: every accepted update is its own operation, got {later:?} again",
            row.case
        );
        let settled = poll_until_channel_active(&emulator, &cluster, &channel, row.case).await;
        assert_eq!(
            settled.at(row.destination.freshness_pointer()).as_i64(),
            Some(i64::from(row.refused)),
            "case {}: expected the later accepted value to stand, got {}",
            row.case,
            settled.body
        );
    }
}

/// RULE, from the other side: the status the refusal names is the channel's OWN,
/// read from its record at the instant the call is served — so the same gate that
/// refuses an update overlapping one in flight refuses a channel on its way out
/// naming `DELETING` instead, and nothing in the sentence comes from the request.
#[tokio::test]
async fn the_refusal_names_whichever_non_terminal_status_the_channel_holds() {
    let emulator = Emulator::new();
    let cluster = emulator.create_ok(&ClusterRequest::named("stated")).await;

    let row = Row {
        case: "a channel on its way out",
        name: "state-deleting",
        destination: Destination::S3,
        created: 600,
        accepted: 450,
        refused: 900,
        overlaps: 1,
    };
    let channel = settled_channel(&emulator, &cluster, &row).await;
    let stored = emulator.describe_channel(&cluster, &channel).await;
    let deleted = emulator
        .call(
            "DELETE",
            &format!("/v1/clusters/{}/channels/{}", pct(&cluster), pct(&channel)),
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {}: expected DeleteChannel to be accepted, got {} {}",
        row.case, deleted.status, deleted.body
    );
    let refused =
        update_channel(&emulator, &cluster, &channel, row.destination, row.accepted).await;
    assert_eq!(
        (
            refused.status,
            refused.error_code(),
            refused.error_message()
        ),
        (
            400,
            "BadRequestException",
            not_updatable_message("DELETING").as_str()
        ),
        "case {}: expected the refusal to name the status the channel is in, got {} {}",
        row.case,
        refused.status,
        refused.body
    );
    let after = emulator.describe_channel(&cluster, &channel).await;
    assert_eq!(
        after.at(row.destination.freshness_pointer()).as_i64(),
        stored.at(row.destination.freshness_pointer()).as_i64(),
        "case {}: a refused update must leave the stored value alone, got {}",
        row.case,
        after.body
    );
}
