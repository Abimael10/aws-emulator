// SPDX-License-Identifier: CC0-1.0

//! What a client that changes a data-delivery channel's S3 or Iceberg buffering
//! can observe straight afterwards, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from.
//!
//! RULE, asserted here over a table of inputs rather than for one request: an
//! accepted `UpdateChannel` is applied by the CALL THAT ACCEPTS IT — the whole
//! change is one stored buffering parameter, so the FIRST independent read after
//! the call reports the submitted `DataFreshnessInSeconds` on a channel still in
//! its terminal `ACTIVE` status, and the cluster operation the call answers
//! reports its work complete on that same first read — while every other stored
//! member of the channel comes back byte-identical, non-ASCII code points
//! included: nothing in the model lets a destination change rewrite the channel's
//! name, its tags, its storage prefix or its topic list.
//!
//! The rows differ along what the rule generalises over: the script mix of the
//! members the update must not touch (Japanese katakana with accented Latin and
//! Chinese, Cyrillic, Greek, combining marks, an astral-plane code point),
//! the number of tags, the destination kind, and the value the update carries,
//! which walks each published bound of the documented range (`300 to 900` on
//! `smithy_query(query="shape",shape="S3DestinationUpdate")`) and a value between
//! them, including a repeat that submits the value the channel already has.
//!
//! Every wait here polls for the CONDITION under a deadline far longer than any
//! span the service owes a transition, never for a fixed time, so nothing below
//! depends on how fast the host runs. The harness — `Emulator`, `Reply`, `pct`,
//! `poll_until_active` — is the shared one in `all/kafka_harness.rs`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_kafka_kafka::state::{
    MAX_DATA_FRESHNESS_SECONDS, MIN_DATA_FRESHNESS_SECONDS,
};

use crate::kafka_harness::{
    pct, poll_until_active, ClusterRequest, Emulator, Reply, ACCOUNT, REGION,
};

/// The role a channel assumes to write to its destination; any role ARN does.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/service-role/msk-channel-delivery";
/// The dead-letter bucket every row below points at.
const DLQ_BUCKET_ARN: &str = "arn:aws:s3:::msk-unicode-dlq";
/// The destination bucket the rows deliver into.
const RECORDS_BUCKET_ARN: &str = "arn:aws:s3:::msk-unicode-records";

/// The broker instance type every cluster below runs, and the Kafka version it
/// runs: a data-delivery channel "streams records from an Amazon MSK Express
/// cluster topic" (`CreateChannel`'s own documentation), so an Express cluster is
/// the host every row stands its channel on.
const EXPRESS_INSTANCE_TYPE: &str = "express.m7g.large";
const EXPRESS_KAFKA_VERSION: &str = "3.9.x";

/// How long a poll below waits for a resource to report the value the clock owes
/// it before the suite calls it a failure: far longer than any span the service
/// owes a transition, so the deadline says nothing about how fast the host runs.
const CONVERGENCE_DEADLINE: Duration = Duration::from_secs(60);

/// The cadence an unsettled resource is re-read at — a probing interval, not a
/// bound on the transition. `std::thread::sleep` between probes because the
/// workspace's tokio carries no timer feature and nothing else has to make
/// progress here, exactly as the harness's own cluster poll waits.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The `OperationState` values a cluster operation of a finished channel change
/// is reported in: the service's own `UPDATE_COMPLETE`, plus the `COMPLETED`
/// spelling a client may be looking for.
const FINISHED_OPERATION_STATES: [&str; 2] = ["UPDATE_COMPLETE", "COMPLETED"];

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

    /// Where `DescribeChannel` reports the caller's own storage label: the S3
    /// output prefix for an S3 channel, the Iceberg destination table name for an
    /// Iceberg one. Both are strings the request submits and the service stores.
    fn label_pointer(self) -> &'static str {
        match self {
            Self::S3 => "/s3DestinationConfiguration/storage/outputPrefix",
            Self::Iceberg => {
                "/icebergDestinationConfiguration/destinationTableList/0/destinationTableName"
            }
        }
    }
}

/// One row of the table: a channel whose client-supplied strings the update must
/// leave untouched, and the values it is then updated through in turn.
#[derive(Debug, Clone)]
struct Row {
    case: &'static str,
    destination: Destination,
    /// `ChannelName`, which the update may not rewrite.
    name: &'static str,
    /// The storage label (S3 output prefix or Iceberg table name), likewise.
    label: &'static str,
    /// The tag map the create submits, likewise — keys and values both.
    tags: Vec<(&'static str, &'static str)>,
    /// The `dataFreshnessInSeconds` the create submits.
    created_freshness: i64,
    /// The values the channel is then updated through, in turn.
    targets: Vec<i64>,
}

/// The `CreateChannel` body for `row`, streaming `topic_arn`.
fn channel_body(row: &Row, topic_arn: &str) -> Value {
    let destination = match row.destination {
        Destination::S3 => json!({
            "dataFreshnessInSeconds": row.created_freshness,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN, "errorOutputPrefix": "dlq/" },
            "serviceExecutionRoleArn": ROLE_ARN,
            "storage": {
                "bucketArn": RECORDS_BUCKET_ARN,
                "compressionType": "NONE",
                "storageClass": "STANDARD",
                "outputPrefix": row.label,
            },
        }),
        Destination::Iceberg => json!({
            "dataFreshnessInSeconds": row.created_freshness,
            "appendOnly": true,
            "deadLetterQueueS3": { "bucketArn": DLQ_BUCKET_ARN, "errorOutputPrefix": "dlq/" },
            "destinationTableList": [{
                "destinationDatabaseName": "records",
                "destinationTableName": row.label,
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
            "recordConverter": { "valueConverter": "STRING" },
            "topicArn": topic_arn,
        }],
        "tags": Value::Object(
            row.tags
                .iter()
                .map(|(key, value)| ((*key).to_string(), json!(value)))
                .collect(),
        ),
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
    seconds: i64,
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

/// The members an accepted update may NOT move, as `DescribeChannel` reports
/// them: the channel's name, its storage label, its tag map and its topic list.
///
/// Compared as one value, so a row fails on whichever of them a change rewrote.
fn preserved_members(reply: &Reply, destination: Destination) -> Value {
    json!({
        "channelName": reply.at("/channelName").clone(),
        "label": reply.at(destination.label_pointer()).clone(),
        "tags": reply.at("/tags").clone(),
        "topicConfigurationList": reply.at("/topicConfigurationList").clone(),
    })
}

/// Poll `DescribeChannel` until the channel reports `ACTIVE`, and hand that read
/// back. Waits for the CONDITION, so it says nothing about how long the create's
/// own span is.
async fn poll_until_channel_active(
    emulator: &Emulator,
    cluster_arn: &str,
    channel_arn: &str,
    case: &str,
) -> Reply {
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        let described = emulator.describe_channel(cluster_arn, channel_arn).await;
        assert_eq!(
            described.status, 200,
            "case {case}: expected DescribeChannel to answer, got {} {}",
            described.status, described.body
        );
        if described.at("/status").as_str() == Some("ACTIVE") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case}: the channel never reported ACTIVE, last read {}",
            described.body
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Mint a topic on `cluster_arn` through `CreateTopic` and wait it out of
/// `CREATING`, so the channels below stream a topic the cluster really holds.
async fn settled_topic(emulator: &Emulator, cluster_arn: &str, name: &str) -> String {
    let created = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster_arn)),
            Some(json!({
                "topicName": name,
                "partitionCount": 1,
                "replicationFactor": 2,
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected CreateTopic to be accepted, got {} {}",
        created.status, created.body
    );
    let arn = created
        .at("/topicArn")
        .as_str()
        .unwrap_or_else(|| panic!("CreateTopic returned no topicArn: {}", created.body))
        .to_string();
    let deadline = Instant::now() + CONVERGENCE_DEADLINE;
    loop {
        let described = emulator
            .call(
                "GET",
                &format!("/v1/clusters/{}/topics/{}", pct(cluster_arn), pct(name)),
                None,
            )
            .await;
        if described.at("/status").as_str() == Some("ACTIVE") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "the topic never reported ACTIVE, last read {}",
            described.body
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The table every suite below walks: the same channels, differing in the script
/// mix of the members an update must not touch, in tag count, in destination kind
/// and in the values each one is updated through.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "japanese-accented-latin-and-chinese",
            destination: Destination::S3,
            // Katakana, accented Latin and Chinese in one name.
            name: "\u{30c7}\u{30fc}\u{30bf}-donn\u{e9}es-\u{4e2d}\u{6587}",
            label: "logger/donn\u{e9}es-zhongwen/",
            tags: vec![(
                "sim2real-unicode",
                "\u{30ab}\u{30bf}\u{30ab}\u{30ca}/donn\u{e9}es/\u{4e2d}\u{6587}",
            )],
            created_freshness: MIN_DATA_FRESHNESS_SECONDS.into(),
            targets: vec![600],
        },
        Row {
            case: "cyrillic-and-greek-with-two-tags",
            destination: Destination::S3,
            name: "\u{43a}\u{430}\u{43d}\u{430}\u{43b}-\u{3b4}\u{3b9}\u{3b1}\u{3bd}",
            label: "\u{43b}\u{43e}\u{433}\u{438}/\u{3b1}\u{3c1}\u{3c7}\u{3b5}\u{3af}\u{3b1}/",
            tags: vec![
                ("\u{432}\u{43b}\u{430}\u{434}\u{435}\u{43b}\u{435}\u{446}", "\u{43e}\u{43f}\u{435}\u{440}\u{430}\u{446}\u{438}\u{438}"),
                ("env", "prod"),
            ],
            // Both published bounds and one value between them, applied in turn:
            // the rule is about the value each request carries, not about one step
            // away from the create.
            created_freshness: 600,
            targets: vec![
                MIN_DATA_FRESHNESS_SECONDS.into(),
                MAX_DATA_FRESHNESS_SECONDS.into(),
                601,
            ],
        },
        Row {
            case: "combining-marks-and-an-astral-code-point",
            destination: Destination::Iceberg,
            // A decomposed e-acute (e + U+0301) and a code point outside the BMP,
            // so a lossy normalise or a UTF-16 round trip shows up here.
            name: "e\u{301}ve\u{301}nements-\u{1f6f8}",
            label: "table_e\u{301}ve\u{301}nements",
            tags: vec![("\u{30bf}\u{30b0}", "\u{5024}\u{1f6f8}")],
            created_freshness: MAX_DATA_FRESHNESS_SECONDS.into(),
            // A repeat of the value the channel already has is still an accepted
            // change with its own operation.
            targets: vec![450, 450],
        },
        Row {
            case: "ascii-control-row",
            destination: Destination::S3,
            name: "plain-ascii",
            label: "logger/records/",
            tags: Vec::new(),
            created_freshness: 600,
            targets: vec![MAX_DATA_FRESHNESS_SECONDS.into()],
        },
    ]
}

/// RULE: an accepted `UpdateChannel` is applied by the call that accepts it, so
/// the FIRST independent `DescribeChannel` after it reports the submitted
/// `DataFreshnessInSeconds` on an `ACTIVE` channel, the cluster operation it
/// answered reports its work complete on the first read of it, and every
/// client-supplied string the channel holds comes back byte-identical.
#[tokio::test]
async fn an_accepted_update_is_observable_on_the_next_read_and_moves_nothing_else() {
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            kafka_version: EXPRESS_KAFKA_VERSION,
            instance_type: EXPRESS_INSTANCE_TYPE,
            brokers: 3,
            subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
            ..ClusterRequest::named("unicode-round-trip")
        })
        .await;
    poll_until_active(&emulator, &cluster, "unicode-round-trip").await;
    let topic_arn = settled_topic(&emulator, &cluster, "deliveries").await;

    for row in rows() {
        let created = emulator
            .create_channel(&cluster, channel_body(&row, &topic_arn))
            .await;
        assert_eq!(
            created.status, 200,
            "case {}: expected CreateChannel to be accepted, got {} {}",
            row.case, created.status, created.body
        );
        let channel = created
            .at("/channelArn")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {}: CreateChannel returned no channelArn", row.case)
            })
            .to_string();

        // What the channel holds once its own create has settled: the baseline the
        // updates below may not move.
        let before = poll_until_channel_active(&emulator, &cluster, &channel, row.case).await;
        let preserved = preserved_members(&before, row.destination);
        assert_eq!(
            before.at("/channelName").as_str(),
            Some(row.name),
            "case {}: expected the submitted channel name back unchanged, got {}",
            row.case,
            before.body
        );
        assert_eq!(
            before.at(row.destination.label_pointer()).as_str(),
            Some(row.label),
            "case {}: expected the submitted storage label back unchanged, got {}",
            row.case,
            before.body
        );
        assert_eq!(
            before.at("/tags"),
            &expected_tags(&row),
            "case {}: expected the submitted tag map back unchanged, got {}",
            row.case,
            before.body
        );
        assert_eq!(
            before.at(row.destination.freshness_pointer()).as_i64(),
            Some(row.created_freshness),
            "case {}: expected the created buffering back, got {}",
            row.case,
            before.body
        );

        let mut operations: Vec<String> = Vec::new();
        for target in &row.targets {
            let case = format!("{} -> {target}", row.case);
            let updated =
                update_channel(&emulator, &cluster, &channel, row.destination, *target).await;
            assert_eq!(
                updated.status, 200,
                "case {case}: expected UpdateChannel to return 200, got {} {}",
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
            assert!(
                !operations.contains(&operation),
                "case {case}: every accepted update is its own operation, got {operation:?} again"
            );
            operations.push(operation.clone());

            // The FIRST read of the operation the caller was handed: the change is
            // applied, so the work it carried is already finished.
            for (reader, uri) in [
                ("v1", format!("/v1/operations/{}", pct(&operation))),
                ("v2", format!("/api/v2/operations/{}", pct(&operation))),
            ] {
                let reply = emulator.call("GET", &uri, None).await;
                assert_eq!(
                    reply.status, 200,
                    "case {case}: expected {reader} to resolve the operation, got {} {}",
                    reply.status, reply.body
                );
                let state = reply
                    .at("/clusterOperationInfo/operationState")
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    FINISHED_OPERATION_STATES.contains(&state.as_str()),
                    "case {case}: expected {reader} to report the update finished on the first \
                     read, got {state:?} in {}",
                    reply.body
                );
                assert_eq!(
                    reply.at("/clusterOperationInfo/clusterArn").as_str(),
                    Some(cluster.as_str()),
                    "case {case}: expected the operation to name its own cluster, got {}",
                    reply.body
                );
            }

            // The FIRST independent read of the channel: the new value, a terminal
            // status, and nothing else moved.
            let after = emulator.describe_channel(&cluster, &channel).await;
            assert_eq!(
                (
                    after.at("/status").as_str(),
                    after.at(row.destination.freshness_pointer()).as_i64(),
                ),
                (Some("ACTIVE"), Some(*target)),
                "case {case}: expected an ACTIVE channel reporting {target} on the first read \
                 after the update, got {}",
                after.body
            );
            assert_eq!(
                preserved_members(&after, row.destination),
                preserved,
                "case {case}: expected every client-supplied member to survive the update as \
                 {preserved}, got {}",
                after.body
            );
            assert_eq!(
                after.at("/channelName").as_str(),
                Some(row.name),
                "case {case}: expected the submitted channel name byte-identical, got {}",
                after.body
            );
            assert_eq!(
                after.at(row.destination.label_pointer()).as_str(),
                Some(row.label),
                "case {case}: expected the submitted storage label byte-identical, got {}",
                after.body
            );
            assert_eq!(
                after.at("/tags"),
                &expected_tags(&row),
                "case {case}: expected the submitted tag map byte-identical, got {}",
                after.body
            );

            // The second independent reader of the same record agrees, and holds
            // the channel exactly once.
            let listed = emulator.list_channels(&cluster, "").await;
            let entries: Vec<Value> = listed
                .at("/channels")
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| {
                    entry.get("channelArn").and_then(Value::as_str) == Some(channel.as_str())
                })
                .collect();
            assert_eq!(
                entries.len(),
                1,
                "case {case}: expected the listing to hold the channel exactly once, got {}",
                listed.body
            );
            assert_eq!(
                (
                    entries[0].get("channelName").and_then(Value::as_str),
                    entries[0].get("status").and_then(Value::as_str),
                ),
                (Some(row.name), Some("ACTIVE")),
                "case {case}: expected the listing to report the same settled channel, got {}",
                entries[0]
            );
        }
    }
}

/// The tag map `row` submitted, as `DescribeChannel` must report it back: the
/// SAME map, key for key and byte for byte.
fn expected_tags(row: &Row) -> Value {
    Value::Object(
        row.tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect(),
    )
}

/// RULE, for the sequence a client actually runs: every step of
/// cluster -> topic -> channel -> destination change reaches its terminal,
/// observable value, each step converging on the step before it, so a client that
/// probes once per step reaches a verdict instead of running to its own deadline.
#[tokio::test]
async fn the_sequence_from_cluster_to_updated_channel_needs_one_probe_per_step() {
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            kafka_version: EXPRESS_KAFKA_VERSION,
            instance_type: EXPRESS_INSTANCE_TYPE,
            brokers: 3,
            subnets: vec!["subnet-a", "subnet-b", "subnet-c"],
            ..ClusterRequest::named("one-probe-per-step")
        })
        .await;
    poll_until_active(&emulator, &cluster, "one-probe-per-step").await;
    let topic_arn = settled_topic(&emulator, &cluster, "orders").await;

    let row = Row {
        case: "one-probe-per-step",
        destination: Destination::S3,
        name: "\u{30c7}\u{30ea}\u{30d0}\u{30ea}-donn\u{e9}es",
        label: "logger/donn\u{e9}es-zhongwen/",
        tags: vec![("sim2real-unicode", "\u{30ab}\u{30bf}/donn\u{e9}es/\u{4e2d}\u{6587}")],
        created_freshness: MIN_DATA_FRESHNESS_SECONDS.into(),
        targets: vec![600],
    };
    let created = emulator
        .create_channel(&cluster, channel_body(&row, &topic_arn))
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

    let settled = poll_until_channel_active(&emulator, &cluster, &channel, row.case).await;
    // `DescribeChannel`'s required members
    // (`smithy_query(query="operation",operation="DescribeChannel")`): a settled
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
            "expected a settled DescribeChannel to carry {member}, got {}",
            settled.body
        );
    }
    assert_eq!(
        settled.at("/topicConfigurationList/0/topicArn").as_str(),
        Some(topic_arn.as_str()),
        "expected the channel to stream the topic the sequence created, got {}",
        settled.body
    );

    let updated = update_channel(&emulator, &cluster, &channel, row.destination, 600).await;
    assert_eq!(
        updated.status, 200,
        "expected UpdateChannel to be accepted, got {} {}",
        updated.status, updated.body
    );
    let operation = updated
        .at("/clusterOperationArn")
        .as_str()
        .expect("UpdateChannel returned no operation ARN")
        .to_string();

    // One probe of the operation and one of the channel is all this step costs:
    // both answer the terminal value straight away.
    let reported = emulator
        .call("GET", &format!("/api/v2/operations/{}", pct(&operation)), None)
        .await;
    let state = reported
        .at("/clusterOperationInfo/operationState")
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        FINISHED_OPERATION_STATES.contains(&state.as_str()),
        "expected the update's operation to be finished on the first probe, got {state:?} in {}",
        reported.body
    );
    let after = emulator.describe_channel(&cluster, &channel).await;
    assert_eq!(
        (
            after.at("/status").as_str(),
            after.at(row.destination.freshness_pointer()).as_i64(),
            after.at("/channelName").as_str(),
            after.at(row.destination.label_pointer()).as_str(),
        ),
        (Some("ACTIVE"), Some(600), Some(row.name), Some(row.label)),
        "expected the first probe after the update to report the applied change on an unchanged \
         channel, got {}",
        after.body
    );
    assert_eq!(
        after.at("/tags"),
        &expected_tags(&row),
        "expected the submitted tag map byte-identical after the update, got {}",
        after.body
    );
}
