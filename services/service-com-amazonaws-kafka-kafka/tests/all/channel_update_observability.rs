// SPDX-License-Identifier: CC0-1.0

//! What a CLIENT of a data-delivery channel sees after it submits a destination
//! change: `UpdateChannel` echoes no destination member at all
//! (`smithy_query(query="operation",operation="UpdateChannel")`: its output is
//! `channelArn` plus `clusterOperationArn`), so the only way a caller can tell an
//! applied change from a dropped one is to read the channel back through an
//! INDEPENDENT reader until it settles.
//!
//! RULE, which every suite below asserts over a table of inputs rather than for
//! one request: an accepted asynchronous channel change becomes observable to
//! every independent reader of that channel once the clock owes the transition it
//! started — the readers report the non-terminal status and the operation carrying
//! the change while it is in flight, and the terminal `ACTIVE` with the submitted
//! value afterwards — without any caller having read the resource in between, so a
//! client that polls a reader converges instead of running to its own deadline.
//!
//! Every wait here is a poll for that CONDITION under a deadline far longer than
//! the span the service owes the transition, never a wait for a fixed time, so
//! nothing below depends on how fast the host runs. The harness — `Emulator`,
//! `Reply`, `pct`, `poll_until_active` — is the shared one in `all/kafka_harness.rs`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::kafka_harness::{pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION};

/// The role a channel assumes to write to its destination; any role ARN does.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-channel-delivery";
/// The dead-letter bucket every row below points at.
const DLQ_BUCKET_ARN: &str = "arn:aws:s3:::msk-observability-dlq";
/// The destination bucket the rows deliver into.
const RECORDS_BUCKET_ARN: &str = "arn:aws:s3:::msk-observability-records";

/// Every value `ChannelStatus` names
/// (`smithy_query(query="shape",shape="ChannelStatus")`), so a probe reporting
/// anything else is a defect whatever else that probe says.
const CHANNEL_STATUSES: [&str; 7] = [
    "CREATING",
    "ACTIVE",
    "UPDATING",
    "DELETING",
    "FAILED",
    "SUSPENDING",
    "SUSPENDED",
];

/// The statuses `ChannelInfo$ClusterOperationArn` is "returned only while the
/// channel is in" — so a listing entry names an in-flight operation exactly while
/// it reports one of these, and names none for any other status the enum carries.
const STATUSES_WITH_AN_OPERATION_IN_FLIGHT: [&str; 3] = ["CREATING", "UPDATING", "DELETING"];

/// How long a poll below waits for a channel to reach the status and the value the
/// clock owes it before the suite calls it a failure: far longer than any span the
/// service owes a channel transition, so the deadline says nothing about how fast
/// the host runs.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(60);

/// The cadence an unsettled channel is re-read at — a probing interval, not a
/// bound on the transition. `std::thread::sleep` between probes because the
/// workspace's tokio carries no timer feature and nothing else has to make
/// progress here, exactly as the harness's own cluster poll waits.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Where a channel delivers: the two values `ChannelDestinationType` names, which
/// is what the member an update carries its change under has to agree with.
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

    /// Where `DescribeChannel` reports this destination's
    /// `dataFreshnessInSeconds`.
    fn freshness_pointer(self) -> &'static str {
        match self {
            Self::S3 => "/s3DestinationConfiguration/dataFreshnessInSeconds",
            Self::Iceberg => "/icebergDestinationConfiguration/dataFreshnessInSeconds",
        }
    }
}

/// A `CreateChannel` body for `name`, streaming `topic_arn` to `destination` with
/// `freshness` seconds of buffering.
fn channel_body(
    name: &str,
    destination: Destination,
    topic_arn: &str,
    freshness: i64,
    converter: &str,
) -> Value {
    let configuration = match destination {
        Destination::S3 => json!({
            "dataFreshnessInSeconds": freshness,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN, "errorOutputPrefix": "dlq/" },
            "serviceExecutionRoleArn": ROLE_ARN,
            "storage": {
                "bucketArn": RECORDS_BUCKET_ARN,
                "compressionType": "NONE",
                "storageClass": "STANDARD",
                "outputPrefix": "data/",
            },
        }),
        Destination::Iceberg => json!({
            "appendOnly": true,
            "dataFreshnessInSeconds": freshness,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN },
            "destinationTableList": [{
                "destinationDatabaseName": "records",
                "destinationTableName": name,
                // `PartitionStrategy` names exactly one value, TIME_HOUR.
                "partitionSpec": {
                    "partitionStrategy": "TIME_HOUR",
                    "sourceList": [{ "sourceName": "event_time" }],
                },
            }],
            "schemaEvolution": { "enableSchemaEvolution": false },
            "serviceExecutionRoleArn": ROLE_ARN,
            "tableCreation": { "enableTableCreation": true },
        }),
    };
    let mut body = json!({
        "channelName": name,
        "topicConfigurationList": [{
            "recordConverter": { "valueConverter": converter },
            "topicArn": topic_arn,
        }],
    });
    body[destination.configuration_member()] = configuration;
    body
}

/// `UpdateChannel`, as a client submits a destination change: the channel is
/// addressed under the cluster that holds it, and the body carries exactly one
/// destination update.
async fn update_channel(
    emulator: &Emulator,
    cluster_arn: &str,
    channel_arn: &str,
    destination: Destination,
    freshness: i64,
) -> Reply {
    emulator
        .call(
            "PUT",
            &format!(
                "/v1/clusters/{}/channels/{}",
                pct(cluster_arn),
                pct(channel_arn)
            ),
            Some(json!({
                destination.update_member(): { "dataFreshnessInSeconds": freshness },
            })),
        )
        .await
}

/// Poll `DescribeChannel` until the channel reports the terminal `ACTIVE`, and
/// hand that converged read back.
///
/// Waits for the CONDITION, not for a span: the transition is one the clock owes
/// the channel, so this reads again until it is reported or
/// [`CONVERGENCE_DEADLINE`] passes. Every probe is checked for the agreement the
/// rule is about — a status the model names, and, once that status is terminal,
/// the value the last accepted change submitted — so a reader that settles while
/// still projecting the old destination fails here rather than passing quietly.
async fn poll_until_channel_active(
    emulator: &Emulator,
    cluster_arn: &str,
    channel_arn: &str,
    destination: Destination,
    expected_freshness: i64,
    case: &str,
) -> Reply {
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        let described = emulator.describe_channel(cluster_arn, channel_arn).await;
        assert_eq!(
            described.status, 200,
            "case {case}: expected DescribeChannel to answer 200 while polling, got {} {}",
            described.status, described.body
        );
        let status = described
            .at("/status")
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            CHANNEL_STATUSES.contains(&status.as_str()),
            "case {case}: expected one of the modeled channel statuses, got {status:?}"
        );
        if status == "ACTIVE" {
            assert_eq!(
                described.at(destination.freshness_pointer()).as_i64(),
                Some(expected_freshness),
                "case {case}: a channel reported ACTIVE must project the freshness its last \
                 accepted change wrote; expected {expected_freshness}, got {}",
                described.body
            );
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the channel never converged to ACTIVE with {expected_freshness}, \
             last read {}",
            described.body
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The one `ListChannels` entry for `channel_arn`, which the listing must hold
/// exactly once.
fn listed_entry(listed: &Reply, channel_arn: &str, case: &str) -> Value {
    assert_eq!(
        listed.status, 200,
        "case {case}: expected ListChannels to answer 200, got {} {}",
        listed.status, listed.body
    );
    let mut matching: Vec<Value> = listed
        .at("/channels")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.get("channelArn").and_then(Value::as_str) == Some(channel_arn))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case}: expected the listing to hold the channel exactly once, got {}",
        listed.body
    );
    matching.remove(0)
}

/// Poll `ListChannels` until its entry for `channel_arn` reports `ACTIVE`, and
/// hand that entry back — the second independent reader of the same record, so a
/// change observable through one reader and not the other fails here.
async fn poll_until_listed_active(
    emulator: &Emulator,
    cluster_arn: &str,
    channel_arn: &str,
    case: &str,
) -> Value {
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        let listed = emulator.list_channels(cluster_arn, "").await;
        let entry = listed_entry(&listed, channel_arn, case);
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            CHANNEL_STATUSES.contains(&status.as_str()),
            "case {case}: expected one of the modeled channel statuses in the listing, got \
             {status:?}"
        );
        // `ChannelInfo$ClusterOperationArn` is "returned only while the channel is
        // in CREATING, UPDATING, or DELETING", so the listing names an operation
        // exactly while the status it reports is one of those three — compared
        // against that list rather than against "not ACTIVE", since the enum also
        // names statuses no operation is in flight for.
        let operation = entry.get("clusterOperationArn").and_then(Value::as_str);
        assert_eq!(
            operation.is_some(),
            STATUSES_WITH_AN_OPERATION_IN_FLIGHT.contains(&status.as_str()),
            "case {case}: the in-flight operation must be reported exactly while the channel is \
             CREATING, UPDATING or DELETING (status {status:?}), got {entry}"
        );
        if status == "ACTIVE" {
            return entry;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the listing never reported the channel ACTIVE, last entry {entry}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One row of the convergence table: where the channel delivers, how its records
/// are converted, the buffering it is created with, and the values it is then
/// updated through in turn.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    destination: Destination,
    /// `ValueConverter`, varied across the rows: how records are converted does
    /// not change what a reader must report.
    converter: &'static str,
    /// The `dataFreshnessInSeconds` the create submits.
    created_freshness: i64,
    /// The values the channel is then updated through, in turn — each published
    /// bound of the documented range ("Allowed range: 300 to 900",
    /// `S3DestinationUpdate$DataFreshnessInSeconds`), the documented default and
    /// one value strictly between the bounds.
    targets: Vec<i64>,
}

/// The broker instance type every cluster below runs, and the Kafka version it
/// runs it on: a data-delivery channel "streams records from an Amazon MSK Express
/// cluster topic" (`CreateChannel`'s own documentation), so an Express cluster is
/// the host every row stands a channel on.
const EXPRESS_INSTANCE_TYPE: &str = "express.m7g.large";
const EXPRESS_KAFKA_VERSION: &str = "3.9.x";

/// RULE: an accepted `UpdateChannel` becomes observable through BOTH independent
/// readers — `DescribeChannel` and `ListChannels` — once the clock owes the
/// channel `ACTIVE` again, with no caller having read it in between: a client that
/// polls either reader converges on the submitted value, and the channel it
/// converges on is the one the update named, listed exactly once.
#[tokio::test]
async fn an_accepted_update_converges_in_every_independent_reader() {
    let rows = vec![
        Row {
            case: "s3-to-the-upper-bound",
            destination: Destination::S3,
            converter: "BYTE_ARRAY",
            created_freshness: 600,
            targets: vec![900],
        },
        Row {
            case: "s3-walks-the-range",
            destination: Destination::S3,
            converter: "JSON",
            created_freshness: 300,
            // Both published bounds, the documented default and one value
            // strictly between them, applied in turn: the rule is about the
            // value each request carries, not about one step away from the create.
            targets: vec![900, 300, 600, 601],
        },
        Row {
            case: "iceberg-down-to-the-lower-bound",
            destination: Destination::Iceberg,
            converter: "STRING",
            created_freshness: 900,
            targets: vec![300],
        },
    ];

    let emulator = Emulator::new();
    for row in &rows {
        let cluster = emulator
            .create_ok(&ClusterRequest {
                kafka_version: EXPRESS_KAFKA_VERSION,
                instance_type: EXPRESS_INSTANCE_TYPE,
                ..ClusterRequest::named(row.case)
            })
            .await;
        poll_until_active(&emulator, &cluster, row.case).await;

        // A topic ARN of this cluster is all a channel's topic list needs to
        // name; the sequence that mints one through `CreateTopic` is driven in
        // its own suite below.
        let topic_arn = format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/{}/orders", row.case);
        let created = emulator
            .create_channel(
                &cluster,
                channel_body(
                    row.case,
                    row.destination,
                    &topic_arn,
                    row.created_freshness,
                    row.converter,
                ),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateChannel to be accepted, got {} {}",
            row.case, created.status, created.body
        );
        let channel = created
            .at("/channelArn")
            .as_str()
            .unwrap_or_else(|| panic!("case {}: CreateChannel returned no channelArn", row.case))
            .to_string();

        // The create's own transition converges first, and on the value the
        // create submitted: a channel that never settles is the defect this rule
        // is about, whether the change came from the create or from an update.
        poll_until_channel_active(
            &emulator,
            &cluster,
            &channel,
            row.destination,
            row.created_freshness,
            row.case,
        )
        .await;

        for target in &row.targets {
            let case = format!("{} -> {target}", row.case);
            let updated =
                update_channel(&emulator, &cluster, &channel, row.destination, *target).await;
            assert_eq!(
                updated.status, 200,
                "case {case}: expected UpdateChannel to be accepted, got {} {}",
                updated.status, updated.body
            );
            assert_eq!(
                updated.at("/channelArn").as_str(),
                Some(channel.as_str()),
                "case {case}: UpdateChannel must answer the channel it was asked about, got {}",
                updated.body
            );
            let operation = updated
                .at("/clusterOperationArn")
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: UpdateChannel returned no operation ARN"))
                .to_string();
            assert!(
                operation
                    .starts_with(&format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster-operation/")),
                "case {case}: expected a cluster-operation ARN of this region and account, got \
                 {operation:?}"
            );

            // Nothing reads the channel between the update and the poll below, so
            // the transition converges because the clock owed it — not because a
            // caller looked.
            let converged = poll_until_channel_active(
                &emulator,
                &cluster,
                &channel,
                row.destination,
                *target,
                &case,
            )
            .await;
            assert_eq!(
                converged.at("/channelArn").as_str(),
                Some(channel.as_str()),
                "case {case}: the reader answered about another channel, got {}",
                converged.body
            );

            // The second reader of the same record converges on the same thing,
            // and holds the channel exactly once.
            let entry = poll_until_listed_active(&emulator, &cluster, &channel, &case).await;
            assert_eq!(
                entry.get("destinationType").and_then(Value::as_str),
                converged.at("/destinationType").as_str(),
                "case {case}: the listing and the reader must agree about the destination, got \
                 {entry}"
            );

            // A read changes nothing: the converged value is still there, and
            // still terminal, for the next caller.
            let again = emulator.describe_channel(&cluster, &channel).await;
            assert_eq!(
                (
                    again.at("/status").as_str(),
                    again.at(row.destination.freshness_pointer()).as_i64(),
                ),
                (Some("ACTIVE"), Some(*target)),
                "case {case}: a converged channel must read back the same way again, got {}",
                again.body
            );
        }
    }
}

/// RULE, for the listing specifically: `ListChannels` projects the SAME record
/// `DescribeChannel` reports all the way through an update window — the entry
/// carries the non-terminal status together with the operation carrying the change
/// while it is in flight, and the terminal status with no operation once the
/// change is done — so a client may poll either reader and reach the same verdict.
#[tokio::test]
async fn the_listing_follows_a_channel_through_its_update_window() {
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            kafka_version: EXPRESS_KAFKA_VERSION,
            instance_type: EXPRESS_INSTANCE_TYPE,
            ..ClusterRequest::named("listing-window")
        })
        .await;
    poll_until_active(&emulator, &cluster, "listing-window").await;

    let topic_arn = format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/listing-window/orders");
    let created = emulator
        .create_channel(
            &cluster,
            channel_body("window", Destination::S3, &topic_arn, 300, "JSON"),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected CreateChannel to be accepted, got {} {}",
        created.status, created.body
    );
    let channel = created
        .at("/channelArn")
        .as_str()
        .expect("CreateChannel returned no channelArn")
        .to_string();
    // The create's window is the same rule: the listing follows it to ACTIVE too.
    poll_until_listed_active(&emulator, &cluster, &channel, "after-create").await;

    for target in [900_i64, 450] {
        let case = format!("listing-window -> {target}");
        let updated = update_channel(&emulator, &cluster, &channel, Destination::S3, target).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected UpdateChannel to be accepted, got {} {}",
            updated.status, updated.body
        );
        let operation = updated
            .at("/clusterOperationArn")
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: UpdateChannel returned no operation ARN"))
            .to_string();

        // The FIRST reader the client reaches for after the update is the
        // listing. Whichever side of the span it lands on, the entry is
        // self-consistent: a non-terminal status comes with the operation this
        // update answered, a terminal one with none.
        let listed = emulator.list_channels(&cluster, "").await;
        let entry = listed_entry(&listed, &channel, &case);
        let status = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        match status.as_str() {
            "ACTIVE" => assert_eq!(
                entry.get("clusterOperationArn"),
                None,
                "case {case}: a settled channel has no operation in flight, got {entry}"
            ),
            "UPDATING" => assert_eq!(
                entry.get("clusterOperationArn").and_then(Value::as_str),
                Some(operation.as_str()),
                "case {case}: the listing must name the operation this update answered, got \
                 {entry}"
            ),
            other => panic!(
                "case {case}: an accepted update leaves the channel UPDATING or ACTIVE, got \
                 {other:?} in {entry}"
            ),
        }

        let settled = poll_until_listed_active(&emulator, &cluster, &channel, &case).await;
        let described = emulator.describe_channel(&cluster, &channel).await;
        assert_eq!(
            (
                settled.get("channelName").and_then(Value::as_str),
                settled.get("status").and_then(Value::as_str),
            ),
            (
                described.at("/channelName").as_str(),
                described.at("/status").as_str(),
            ),
            "case {case}: the listing and the reader must agree, got {settled} and {}",
            described.body
        );
        assert_eq!(
            described
                .at(Destination::S3.freshness_pointer())
                .as_i64(),
            Some(target),
            "case {case}: expected the converged channel to project {target}, got {}",
            described.body
        );
    }
}

/// RULE: the whole delivery sequence a client runs — a provisioned cluster, a
/// topic on it, a channel streaming that topic, and a destination change — reaches
/// a terminal, observable state at every step, each step converging on the step
/// before it rather than on a wait.
#[tokio::test]
async fn the_delivery_sequence_from_cluster_to_updated_channel_converges() {
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            kafka_version: EXPRESS_KAFKA_VERSION,
            instance_type: EXPRESS_INSTANCE_TYPE,
            brokers: 3,
            subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
            ..ClusterRequest::named("delivery-sequence")
        })
        .await;
    poll_until_active(&emulator, &cluster, "delivery-sequence").await;

    // The topic the channel streams, minted through `CreateTopic` so the channel's
    // topic list names a topic this cluster really holds.
    let topic = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(&cluster)),
            Some(json!({
                "topicName": "deliveries",
                "partitionCount": 1,
                "replicationFactor": 2,
            })),
        )
        .await;
    assert_eq!(
        topic.status, 200,
        "expected CreateTopic to be accepted, got {} {}",
        topic.status, topic.body
    );
    let topic_arn = topic
        .at("/topicArn")
        .as_str()
        .expect("CreateTopic returned no topicArn")
        .to_string();
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        let described = emulator
            .call(
                "GET",
                &format!(
                    "/v1/clusters/{}/topics/{}",
                    pct(&cluster),
                    pct("deliveries")
                ),
                None,
            )
            .await;
        if described.at("/status").as_str() == Some("ACTIVE") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the topic never reported ACTIVE, last read {}",
            described.body
        );
        std::thread::sleep(POLL_INTERVAL);
    }

    let created = emulator
        .create_channel(
            &cluster,
            channel_body(
                "deliveries",
                Destination::S3,
                &topic_arn,
                600,
                "BYTE_ARRAY",
            ),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected CreateChannel to be accepted, got {} {}",
        created.status, created.body
    );
    let channel = created
        .at("/channelArn")
        .as_str()
        .expect("CreateChannel returned no channelArn")
        .to_string();
    let settled = poll_until_channel_active(
        &emulator,
        &cluster,
        &channel,
        Destination::S3,
        600,
        "created-channel",
    )
    .await;
    // `DescribeChannel`'s required members
    // (`smithy_query(query="operation",operation="DescribeChannel")`): a converged
    // read carries every one of them.
    for member in [
        "/channelArn",
        "/channelName",
        "/status",
        "/destinationType",
        "/creationTime",
        "/topicConfigurationList/0/topicArn",
    ] {
        assert!(
            !settled.at(member).is_null(),
            "expected a converged DescribeChannel to carry {member}, got {}",
            settled.body
        );
    }
    assert_eq!(
        settled.at("/topicConfigurationList/0/topicArn").as_str(),
        Some(topic_arn.as_str()),
        "expected the channel to stream the topic it was created for, got {}",
        settled.body
    );

    let updated = update_channel(&emulator, &cluster, &channel, Destination::S3, 900).await;
    assert_eq!(
        updated.status, 200,
        "expected UpdateChannel to be accepted, got {} {}",
        updated.status, updated.body
    );
    poll_until_channel_active(
        &emulator,
        &cluster,
        &channel,
        Destination::S3,
        900,
        "updated-channel",
    )
    .await;
    let entry = poll_until_listed_active(&emulator, &cluster, &channel, "updated-channel").await;
    assert_eq!(
        entry.get("channelName").and_then(Value::as_str),
        Some("deliveries"),
        "expected the listing to name the channel the sequence created, got {entry}"
    );
}
