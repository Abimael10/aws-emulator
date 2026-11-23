// SPDX-License-Identifier: CC0-1.0

//! Which RECORD an accepted `UpdateChannel` moves, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from.
//!
//! RULE 1 (one record): `UpdateChannel` takes a `ChannelArn` and the `ClusterArn`
//! of the cluster that holds it (`smithy_query(query="operation",
//! operation="UpdateChannel")`), so an accepted call writes the submitted
//! `DataFreshnessInSeconds` onto exactly that stored channel — every other channel
//! of the same cluster, and a channel of ANOTHER cluster carrying the same channel
//! name, keeps its own stored value, its own status and its own in-flight cluster
//! operation, because a channel's identity is its ARN under its cluster and not its
//! name.
//!
//! RULE 2 (repeat): the operation is modelled `@idempotent`, so a repeat of an
//! accepted update writes the same value again and is accepted again — and it is
//! nonetheless its own accepted change, so it files its own cluster operation
//! rather than re-reporting an earlier one, and every operation it files resolves
//! through the cluster-operation reader as an `UPDATE_CHANNEL`.
//!
//! Each rule is asserted over a TABLE rather than for one request: the rows differ
//! along the destination kind, the submitted value (each published bound of the
//! documented range — `smithy_query(query="shape",shape="S3DestinationUpdate")`:
//! "Allowed range: 300 to 900" — and a value between them), which channel of the
//! fixture is addressed, whether another cluster holds a channel of the same name,
//! and how many times one value is submitted in a row.
//!
//! Every wait here polls for the CONDITION under a deadline far longer than any
//! span the service owes a transition, never for a fixed time, so nothing below
//! depends on how fast the host runs. The harness — `Emulator`, `Reply`, `pct`,
//! `poll_until_channel_active`, `poll_until_operation_complete` — is the shared one
//! in `all/kafka_harness.rs`.

use serde_json::{json, Value};
use service_com_amazonaws_kafka_kafka::state::{
    MAX_DATA_FRESHNESS_SECONDS, MIN_DATA_FRESHNESS_SECONDS,
};

use crate::kafka_harness::{
    pct, poll_until_channel_active, poll_until_operation_complete, ClusterRequest, Emulator, Reply,
    ACCOUNT, REGION,
};

/// The role a channel assumes to write to its destination; any role ARN does.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-channel-records";
/// The dead-letter bucket every channel below points at.
const DLQ_BUCKET_ARN: &str = "arn:aws:s3:::msk-one-record-dlq";
/// The destination bucket the S3 channels deliver into.
const RECORDS_BUCKET_ARN: &str = "arn:aws:s3:::msk-one-record-records";

/// Where a channel delivers: the two values `ChannelDestinationType` names, which
/// is the kind an update's payload has to agree with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    S3,
    Iceberg,
}

impl Destination {
    /// The member a `CreateChannel` carries this destination's configuration under,
    /// which is also where `DescribeChannel` reports it back.
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
}

/// One channel of the fixture: where it lives, what it is, and the
/// `dataFreshnessInSeconds` every reader of it must report right now.
#[derive(Debug, Clone)]
struct ChannelFixture {
    case: &'static str,
    cluster: String,
    arn: String,
    destination: Destination,
    freshness: i64,
}

/// The `CreateChannel` body for a channel called `name` delivering to
/// `destination` with `freshness` seconds of buffering.
fn channel_body(name: &str, destination: Destination, freshness: i64) -> Value {
    let configuration = match destination {
        Destination::S3 => json!({
            "dataFreshnessInSeconds": freshness,
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
            "dataFreshnessInSeconds": freshness,
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
        "channelName": name,
        "topicConfigurationList": [{
            "recordConverter": { "valueConverter": "JSON" },
            "topicArn": format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/records/orders"),
        }],
    });
    body[destination.configuration_member()] = configuration;
    body
}

/// Create the channel this fixture describes and hand it back with its minted ARN.
/// The channel is NOT waited out of `CREATING` here: every caller creates the whole
/// fixture first and then polls each channel for the terminal status, so one
/// activation span covers them all.
async fn created_channel(
    emulator: &Emulator,
    cluster: &str,
    case: &'static str,
    name: &str,
    destination: Destination,
    freshness: i64,
) -> ChannelFixture {
    let created = emulator
        .create_channel(cluster, channel_body(name, destination, freshness))
        .await;
    assert_eq!(
        created.status, 200,
        "case {case}: expected CreateChannel to be accepted, got {} {}",
        created.status, created.body
    );
    let arn = created
        .at("/channelArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: CreateChannel returned no channelArn"))
        .to_string();
    ChannelFixture {
        case,
        cluster: cluster.to_string(),
        arn,
        destination,
        freshness,
    }
}

/// `UpdateChannel` on this channel, asking its own destination kind for `seconds`.
async fn update_channel(emulator: &Emulator, channel: &ChannelFixture, seconds: i32) -> Reply {
    emulator
        .call(
            "PUT",
            &format!(
                "/v1/clusters/{}/channels/{}",
                pct(&channel.cluster),
                pct(&channel.arn)
            ),
            Some(json!({
                channel.destination.update_member(): { "dataFreshnessInSeconds": seconds }
            })),
        )
        .await
}

/// The `dataFreshnessInSeconds` a read reports for this channel's own destination.
fn reported_freshness(reply: &Reply, destination: Destination) -> Option<i64> {
    reply
        .at(&format!(
            "/{}/dataFreshnessInSeconds",
            destination.configuration_member()
        ))
        .as_i64()
}

/// The `ClusterOperationArn` the update answered, asserted to be a cluster-operation
/// ARN of this endpoint and one no earlier accepted call in this suite was handed.
fn accepted_operation(reply: &Reply, case: &str, seen: &mut Vec<String>) -> String {
    let operation = reply
        .at("/clusterOperationArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: UpdateChannel returned no clusterOperationArn"))
        .to_string();
    assert!(
        operation.starts_with(&format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster-operation/")),
        "case {case}: expected a cluster-operation ARN of this endpoint, got {operation:?}"
    );
    assert!(
        !seen.contains(&operation),
        "case {case}: every accepted update files its own operation, got {operation:?} again"
    );
    seen.push(operation.clone());
    operation
}

/// The entry `ListChannels` reports for this channel under its own cluster, which
/// must be exactly one — the independent reader a bystander is checked through.
async fn listed_entry(emulator: &Emulator, channel: &ChannelFixture, case: &str) -> Value {
    let listed = emulator.list_channels(&channel.cluster, "").await;
    assert_eq!(
        listed.status, 200,
        "case {case}: expected ListChannels to answer, got {} {}",
        listed.status, listed.body
    );
    let entries: Vec<Value> = listed
        .at("/channels")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.get("channelArn").and_then(Value::as_str) == Some(channel.arn.as_str()))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "case {case}: expected the listing to hold {} exactly once, got {}",
        channel.case,
        listed.body
    );
    entries[0].clone()
}

/// Assert that `channel` is a bystander of whatever is in flight elsewhere: both of
/// its independent readers report its OWN stored value, the terminal status, and —
/// because `ChannelInfo$ClusterOperationArn` is "returned only while the channel is
/// in CREATING, UPDATING, or DELETING" — no operation in flight on it.
async fn assert_bystander_untouched(emulator: &Emulator, channel: &ChannelFixture, case: &str) {
    let case = format!("{case}, bystander {}", channel.case);
    let described = emulator
        .describe_channel(&channel.cluster, &channel.arn)
        .await;
    assert_eq!(
        described.status, 200,
        "case {case}: expected DescribeChannel to answer, got {} {}",
        described.status, described.body
    );
    assert_eq!(
        (
            described.at("/status").as_str(),
            reported_freshness(&described, channel.destination),
        ),
        (Some("ACTIVE"), Some(channel.freshness)),
        "case {case}: an update of another channel must leave this one settled at its own \
         value {}, got {}",
        channel.freshness,
        described.body
    );
    let listed = listed_entry(emulator, channel, &case).await;
    assert_eq!(
        (
            listed.get("status").and_then(Value::as_str),
            listed.get("clusterOperationArn").and_then(Value::as_str),
        ),
        (Some("ACTIVE"), None),
        "case {case}: a settled bystander must report no operation in flight, got {listed}"
    );
}

/// RULE 1: an accepted `UpdateChannel` moves only the channel its `ChannelArn`
/// names under the `ClusterArn` it was addressed with — the value it submitted, the
/// `UPDATING` status and the cluster operation carrying the change belong to that
/// one record, while every other channel of that cluster and the same-named channel
/// of another cluster still report their own stored value, the terminal status and
/// no operation in flight.
#[tokio::test]
async fn an_accepted_update_moves_only_the_channel_it_names() {
    let emulator = Emulator::new();
    // Two clusters, so a row can address a channel whose NAME another cluster also
    // holds: a channel's identity is its ARN under its cluster.
    let targeted = emulator.create_ok(&ClusterRequest::named("targeted")).await;
    let neighbouring = emulator
        .create_ok(&ClusterRequest::named("neighbouring"))
        .await;

    // Distinct stored values per channel, so a write that reached the wrong record
    // is visible as another row's value rather than as a coincidence. The shared
    // name "shipments" is held by both clusters.
    let mut channels = vec![
        created_channel(
            &emulator,
            &targeted,
            "the first channel of the targeted cluster",
            "arrivals",
            Destination::S3,
            MIN_DATA_FRESHNESS_SECONDS.into(),
        )
        .await,
        created_channel(
            &emulator,
            &targeted,
            "a channel whose name the other cluster also holds",
            "shipments",
            Destination::Iceberg,
            600,
        )
        .await,
        created_channel(
            &emulator,
            &targeted,
            "the last channel of the targeted cluster",
            "returns",
            Destination::S3,
            MAX_DATA_FRESHNESS_SECONDS.into(),
        )
        .await,
        created_channel(
            &emulator,
            &neighbouring,
            "the same name under the other cluster",
            "shipments",
            Destination::Iceberg,
            450,
        )
        .await,
    ];
    for channel in &channels {
        let settled =
            poll_until_channel_active(&emulator, &channel.cluster, &channel.arn, channel.case)
                .await;
        assert_eq!(
            reported_freshness(&settled, channel.destination),
            Some(channel.freshness),
            "case {}: the fixture did not store the value it was created with, got {}",
            channel.case,
            settled.body
        );
    }

    // The rows: which channel is addressed, and the value it asks for — each
    // published bound of the documented range and a value between them, every one of
    // them different from what any other channel of the fixture holds.
    let rows: Vec<(usize, i32)> = vec![
        (0, MAX_DATA_FRESHNESS_SECONDS),
        (1, MIN_DATA_FRESHNESS_SECONDS),
        (2, 450),
        (3, 601),
    ];
    let mut operations: Vec<String> = Vec::new();

    for (index, submitted) in rows {
        let target = channels[index].clone();
        let case = format!("{} -> {submitted}", target.case);
        let accepted = update_channel(&emulator, &target, submitted).await;
        assert_eq!(
            accepted.status, 200,
            "case {case}: expected UpdateChannel to be accepted, got {} {}",
            accepted.status, accepted.body
        );
        assert_eq!(
            accepted.at("/channelArn").as_str(),
            Some(target.arn.as_str()),
            "case {case}: UpdateChannel must answer about the channel it was addressed \
             with, got {}",
            accepted.at("/channelArn")
        );
        let operation = accepted_operation(&accepted, &case, &mut operations);

        // The addressed record carries the submitted value from the accepting call
        // on, under the operation that call handed back.
        let described = emulator
            .describe_channel(&target.cluster, &target.arn)
            .await;
        assert_eq!(
            reported_freshness(&described, target.destination),
            Some(i64::from(submitted)),
            "case {case}: expected the submitted value stored by the accepting call, got {}",
            described.body
        );
        assert_eq!(
            described.at("/clusterOperationArn").as_str(),
            Some(operation.as_str()),
            "case {case}: the reader disagrees with the update about the operation carrying \
             it, got {}",
            described.at("/clusterOperationArn")
        );

        // And nothing else moved: every other channel of either cluster is still
        // settled at its own value, read while this change may still be in flight.
        for bystander in channels.iter().filter(|other| other.arn != target.arn) {
            assert_bystander_untouched(&emulator, bystander, &case).await;
        }

        let settled = poll_until_channel_active(&emulator, &target.cluster, &target.arn, &case)
            .await;
        assert_eq!(
            reported_freshness(&settled, target.destination),
            Some(i64::from(submitted)),
            "case {case}: expected the settled channel to report the submitted value, got {}",
            settled.body
        );
        poll_until_operation_complete(&emulator, &operation, &case).await;
        channels[index].freshness = i64::from(submitted);

        // Read once more from the far side of the transition: the bystanders are
        // untouched by the settling too.
        for bystander in channels.iter().filter(|other| other.arn != target.arn) {
            assert_bystander_untouched(&emulator, bystander, &case).await;
        }
    }
}

/// RULE 2: `UpdateChannel` is modelled `@idempotent`, so submitting the SAME
/// destination change again on the settled channel is accepted again and writes the
/// same value again — and because each accepted call is its own change, each files
/// its own cluster operation, which the cluster-operation reader resolves as an
/// `UPDATE_CHANNEL` of that channel's cluster.
#[tokio::test]
async fn repeating_an_accepted_update_writes_the_same_value_under_its_own_operation() {
    let emulator = Emulator::new();
    let cluster = emulator.create_ok(&ClusterRequest::named("repeated")).await;

    // The rows differ along the destination kind, the value repeated (a published
    // bound and a value between them) and how many times it is submitted in a row.
    let rows: Vec<(ChannelFixture, i32, usize)> = vec![
        (
            created_channel(
                &emulator,
                &cluster,
                "an s3 channel repeating the documented floor",
                "repeat-s3",
                Destination::S3,
                600,
            )
            .await,
            MIN_DATA_FRESHNESS_SECONDS,
            3,
        ),
        (
            created_channel(
                &emulator,
                &cluster,
                "an iceberg channel repeating a value between the bounds",
                "repeat-iceberg",
                Destination::Iceberg,
                MAX_DATA_FRESHNESS_SECONDS.into(),
            )
            .await,
            450,
            2,
        ),
    ];
    let mut operations: Vec<String> = Vec::new();

    for (channel, submitted, repeats) in &rows {
        poll_until_channel_active(&emulator, &channel.cluster, &channel.arn, channel.case).await;
        for attempt in 1..=*repeats {
            let case = format!("{} (submission {attempt} of {repeats})", channel.case);
            let accepted = update_channel(&emulator, channel, *submitted).await;
            assert_eq!(
                accepted.status, 200,
                "case {case}: a repeat of an accepted update must be accepted too, got {} {}",
                accepted.status, accepted.body
            );
            let operation = accepted_operation(&accepted, &case, &mut operations);

            // Its own operation, resolved through the cluster-operation reader as
            // the kind of work it carries out, for the cluster that holds the
            // channel.
            let reported = emulator
                .call("GET", &format!("/v1/operations/{}", pct(&operation)), None)
                .await;
            assert_eq!(
                reported.status, 200,
                "case {case}: expected the operation the repeat filed to resolve, got {} {}",
                reported.status, reported.body
            );
            assert_eq!(
                (
                    reported.at("/clusterOperationInfo/operationType").as_str(),
                    reported.at("/clusterOperationInfo/clusterArn").as_str(),
                ),
                (Some("UPDATE_CHANNEL"), Some(channel.cluster.as_str())),
                "case {case}: expected an UPDATE_CHANNEL of this channel's own cluster, got {}",
                reported.body
            );

            let settled =
                poll_until_channel_active(&emulator, &channel.cluster, &channel.arn, &case).await;
            assert_eq!(
                reported_freshness(&settled, channel.destination),
                Some(i64::from(*submitted)),
                "case {case}: expected the repeated value to stand, got {}",
                settled.body
            );
            poll_until_operation_complete(&emulator, &operation, &case).await;
        }
    }
}
