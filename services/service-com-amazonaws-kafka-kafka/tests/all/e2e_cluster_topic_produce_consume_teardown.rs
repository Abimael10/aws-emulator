// SPDX-License-Identifier: CC0-1.0

//! The WHOLE sequence a client drives to make an Amazon MSK cluster usable and then take
//! it down: `CreateConfiguration`, `CreateClusterV2`, the SASL/SCRAM secret association,
//! `CreateTopic`, `GetBootstrapBrokers`, the ACL grant and the produce/consume over an
//! advertised endpoint, then `DeleteTopic` and `DeleteCluster`.
//!
//! RULE under test: every leg of that sequence is SERVED, and each leg's own contract
//! holds where the leg before it left the resource:
//!
//! * a transition the clock owes a resource it created reaches its terminal value, so a
//!   client that LOOKS AGAIN sees `ACTIVE` rather than a resource that never settles;
//! * every `host:port` `GetBootstrapBrokers` advertises accepts a connection and completes
//!   the SASL exchange its own access method defines;
//! * a record produced over such an endpoint is handed back to a member of a CONSUMER
//!   GROUP the same brokers coordinate — at the offset the member asked for, which is
//!   what a client's position is read from, and in the order it was produced;
//! * and after the deletes, `DescribeTopic` and `DescribeClusterV2` each answer 404
//!   `NotFoundException` with AWS's fixed sentence for that error class.
//!
//! The data-plane rows span what the rule generalises over: WHICH advertised SASL endpoint
//! the client reached the brokers on (SASL/SCRAM with the credential in the cluster's
//! associated Secrets Manager secret, SASL/IAM with the `OAUTHBEARER` token a non-Java
//! client holds), how many RECORDS the producer sent and whether they carry a key, and
//! which VERSION of each group-protocol API the client negotiated — a client picks those
//! from the broker's own advertisement, so the oldest form a consuming client sends is
//! covered beside a newer one.
//!
//! Every data-plane assertion goes over a real socket the core opened for the cluster,
//! dialled at the address `GetBootstrapBrokers` published; the control-plane legs go over
//! the same `MultiServiceRouter` the emulator serves from.

use std::time::{Duration, Instant};

use ::bytes::Bytes;
use ::kafka_protocol::messages::{
    create_acls_request::AclCreation, join_group_request::JoinGroupRequestProtocol,
    list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic},
    offset_commit_request::{OffsetCommitRequestPartition, OffsetCommitRequestTopic},
    offset_fetch_request::OffsetFetchRequestTopic, sync_group_request::SyncGroupRequestAssignment,
    ApiKey, BrokerId, CreateAclsRequest, CreateAclsResponse, DescribeAclsRequest,
    DescribeAclsResponse, FetchRequest, FetchResponse, FindCoordinatorRequest,
    FindCoordinatorResponse, GroupId, HeartbeatRequest, HeartbeatResponse, JoinGroupRequest,
    JoinGroupResponse, LeaveGroupRequest, LeaveGroupResponse, ListOffsetsRequest,
    ListOffsetsResponse, OffsetCommitRequest, OffsetCommitResponse, OffsetFetchRequest,
    OffsetFetchResponse, SyncGroupRequest, SyncGroupResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::RecordBatchDecoder;
use serde_json::json;

use crate::broker_data_plane::{
    epoch_millis, given_secret_associated_as, Client, OAUTHBEARER, SCRAM_SHA_512,
};
use crate::kafka_harness::{pct, v2_provisioned_body, Emulator};

/// The Apache Kafka version this cluster runs, and the brokers it runs on: the pair a
/// client of a standard provisioned cluster asks for.
const KAFKA_VERSION: &str = "3.9.x";
const INSTANCE_TYPE: &str = "kafka.m5.large";

/// The SASL/SCRAM user and password the cluster's associated secret carries, which is the
/// credential the SASL/SCRAM rows authenticate with.
const SCRAM_USER: &str = "e2e-scram-user";
const SCRAM_PASSWORD: &str = "e2e-scram-Secret-99";

/// The sentence Amazon MSK refuses a reference that resolves to nothing with, in full.
/// The apostrophe is U+2019, as AWS writes it. Written out here rather than imported, so
/// the assertion cannot agree with a constant that is wrong.
const NOT_FOUND_MESSAGE: &str = "The requested resource doesn\u{2019}t exist.";

/// The `protocol_type` a CONSUMING client joins a group with, and the assignor it names:
/// the two strings Apache Kafka's consumer protocol is identified by.
const CONSUMER_PROTOCOL: &str = "consumer";
const RANGE_ASSIGNOR: &str = "range";

/// The session and rebalance windows every join below states, in milliseconds: values in
/// the REQUEST, sized well above the time this suite's rebalances take, so no row depends
/// on how fast the host runs.
const SESSION_TIMEOUT_MS: i32 = 30_000;
const REBALANCE_TIMEOUT_MS: i32 = 30_000;

/// How long this suite waits for a value the clock owes a resource before calling it a
/// failure, and the cadence it looks again at: the deadline is far longer than any span
/// this service owes a transition, so it says nothing about how fast the host runs.
const SETTLED_DEADLINE: Duration = Duration::from_secs(60);
const PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// The timestamp a `ListOffsets` request names to ask for the EARLIEST offset a partition
/// still holds, and what a group that has committed nothing is answered (Apache Kafka
/// protocol, `ListOffsets` and `OffsetFetch`).
const OFFSET_EARLIEST: i64 = -2;
const NO_COMMITTED_OFFSET: i64 = -1;

/// The `FindCoordinator` key type a client asks for a CONSUMER GROUP's coordinator with
/// (Apache Kafka protocol, `FindCoordinator.KeyType`: 0 is a group, 1 a transaction).
const COORDINATOR_KEY_TYPE_GROUP: i8 = 0;

/// The Apache Kafka ACL values the grant below carries: the resource types a binding on a
/// topic and on a consumer group name, the pattern type a binding whose name is taken
/// literally carries (`*` as a literal name is Apache Kafka's wildcard), the operation
/// that covers every one, and the permission that allows it.
const ACL_RESOURCE_TYPE_TOPIC: i8 = 2;
const ACL_RESOURCE_TYPE_GROUP: i8 = 3;
const ACL_PATTERN_TYPE_LITERAL: i8 = 3;
const ACL_PATTERN_TYPE_ANY: i8 = 1;
const ACL_RESOURCE_TYPE_ANY: i8 = 1;
const ACL_OPERATION_ALL: i8 = 2;
const ACL_OPERATION_ANY: i8 = 1;
const ACL_PERMISSION_ALLOW: i8 = 3;
const ACL_PERMISSION_ANY: i8 = 1;

/// The resource name a binding over EVERY topic or group of the cluster names, and the
/// host a binding that covers every client carries.
const EVERY_NAME: &str = "*";
const EVERY_HOST: &str = "*";

/// How long a `Fetch` that is waiting for records asks the broker to hold it, and how much
/// it asks for: values in the REQUEST, as a consuming client states them. The wait is
/// generous — the answer comes as soon as the records are there, so the row says nothing
/// about how fast the host runs — and a read that expects the log to hold NOTHING asks to
/// wait for nothing instead, so it is answered out of what the partition holds now.
const LONG_POLL_MS: i32 = 30_000;
const ONE_BYTE: i32 = 1;
const NO_WAIT: i32 = 0;
const NOTHING: i32 = 0;

/// The versions of each group-protocol API one row's client negotiated. A client picks the
/// version it sends from the broker's own `ApiVersions` advertisement, so a row carries a
/// whole consistent set of them.
#[derive(Debug, Clone, Copy)]
struct Versions {
    find_coordinator: i16,
    join: i16,
    sync: i16,
    heartbeat: i16,
    offset_fetch: i16,
    offset_commit: i16,
    list_offsets: i16,
    fetch: i16,
    leave: i16,
}

/// One data-plane row: the advertised endpoint the client reached the brokers on, the
/// credential it authenticated with, the topic and records it produced, the group it
/// consumed them in, and the protocol versions it did all of it at.
#[derive(Debug, Clone, Copy)]
struct DataPlaneRow {
    case: &'static str,
    member: &'static str,
    mechanism: &'static str,
    credential: &'static str,
    topic: &'static str,
    records: &'static [(Option<&'static str>, &'static str)],
    group: &'static str,
    versions: Versions,
}

/// RULE: the whole provision-produce-consume-teardown sequence is served — every clock-owed
/// transition in it reaches its terminal value, so a client that looks again reads it rather
/// than polling a resource that never settles; each advertised SASL endpoint completes its
/// own exchange and serves a produce and a GROUP consume of the records the control plane's
/// topic holds; and the deletes leave the topic and the cluster resolving to 404
/// `NotFoundException` with AWS's fixed sentence.
#[tokio::test]
async fn the_cluster_topic_produce_consume_teardown_sequence_reaches_every_verdict() {
    let emulator = Emulator::with_secrets_manager();
    let case = "a provisioned SCRAM and IAM cluster";

    // The configuration the cluster's brokers run with: an ACL-enforcing one, which is
    // what a client that then grants ACLs over the wire protocol configures.
    let configuration = emulator
        .create_configuration_ok(
            "e2e-acl-enforced",
            None,
            b"allow.everyone.if.no.acl.found=false",
        )
        .await;

    let mut request = v2_provisioned_body("e2e-topic-lifecycle", KAFKA_VERSION, INSTANCE_TYPE);
    request["provisioned"]["clientAuthentication"] = json!({
        "sasl": { "scram": { "enabled": true }, "iam": { "enabled": true } },
    });
    request["provisioned"]["encryptionInfo"] =
        json!({ "encryptionInTransit": { "clientBroker": "TLS" } });
    request["provisioned"]["configurationInfo"] =
        json!({ "arn": configuration, "revision": 1 });
    let created = emulator.create_cluster_v2(request).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateClusterV2 must be accepted, got {} {}",
        created.status, created.body
    );
    let cluster = created
        .at("/clusterArn")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateClusterV2 returned no clusterArn"))
        .to_string();
    // The create's own answer reports the cluster in the non-terminal state the model
    // declares for a cluster being provisioned.
    assert_eq!(
        created.at("/state").as_str(),
        Some("CREATING"),
        "case {case:?}: CreateClusterV2 must answer the state it provisions from, got {}",
        created.body
    );

    // LOOKS AGAIN until the cluster reports the terminal state, which is the transition
    // the clock owes it: one span, not one poll interval of a client's own cadence.
    let state = poll_cluster_state(&emulator, &cluster, "ACTIVE", case).await;
    assert_eq!(
        state, "ACTIVE",
        "case {case:?}: the cluster must reach ACTIVE, it reported {state}"
    );

    // The SASL/SCRAM credential the brokers then authenticate: minted through Secrets
    // Manager's own `CreateSecret` and associated with the cluster, with the association
    // leaving nothing unprocessed.
    given_secret_associated_as(
        &emulator,
        &cluster,
        "AmazonMSK_e2e_topic_lifecycle",
        SCRAM_USER,
        SCRAM_PASSWORD,
        case,
    )
    .await;

    let rows: &[DataPlaneRow] = &[
        DataPlaneRow {
            case: "the SASL/SCRAM endpoint",
            member: "bootstrapBrokerStringSaslScram",
            mechanism: SCRAM_SHA_512,
            credential: SCRAM_PASSWORD,
            topic: "e2e-scram-topic",
            records: &[(None, "the-payload")],
            group: "e2e-scram-readers",
            // The oldest forms a consuming client still sends: the versions a client
            // that negotiated an Apache Kafka 1.x-era broker picks.
            versions: Versions {
                find_coordinator: 1,
                join: 1,
                sync: 1,
                heartbeat: 1,
                offset_fetch: 2,
                offset_commit: 2,
                list_offsets: 1,
                fetch: 4,
                leave: 1,
            },
        },
        DataPlaneRow {
            case: "the SASL/IAM endpoint",
            member: "bootstrapBrokerStringSaslIam",
            mechanism: OAUTHBEARER,
            credential: "a-signature",
            topic: "e2e-iam-topic",
            records: &[
                (Some("a"), "alpha"),
                (Some("b"), "beta"),
                (Some("c"), "gamma"),
            ],
            group: "e2e-iam-readers",
            // Newer forms of the same calls, which a client that negotiated a recent
            // broker picks instead.
            versions: Versions {
                find_coordinator: 2,
                join: 5,
                sync: 3,
                heartbeat: 3,
                offset_fetch: 5,
                offset_commit: 4,
                list_offsets: 5,
                fetch: 11,
                leave: 2,
            },
        },
    ];

    // The ACL grant a client makes over the SASL/IAM endpoint, naming the SASL/SCRAM
    // principal: ALLOW every operation on every topic and every consumer group, which is
    // what an operator grants a SCRAM user of an ACL-enforcing cluster.
    let iam_address = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    grant_every_operation(&iam_address, case).await;

    for row in rows {
        given_topic(&emulator, &cluster, row).await;
        let address = emulator
            .published_bootstrap(&cluster, row.member, row.case)
            .await;
        assert!(
            !address.is_empty(),
            "case {:?}: the cluster must publish {}, got {address:?}",
            row.case,
            row.member
        );
        produce_and_consume(&address, row).await;
        delete_topic(&emulator, &cluster, row).await;
    }

    // And the cluster itself: once deleted it resolves to nothing, through the V2 reader
    // the sequence provisioned it with.
    let deleted = emulator
        .call("DELETE", &format!("/v1/clusters/{}", pct(&cluster)), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeleteCluster must be accepted, got {} {}",
        deleted.status, deleted.body
    );
    let gone = poll_until_gone(
        &emulator,
        &format!("/api/v2/clusters/{}", pct(&cluster)),
        case,
    )
    .await;
    assert_eq!(
        (gone.0, gone.1.as_str()),
        (404, "NotFoundException"),
        "case {case:?}: a deleted cluster must resolve to 404 NotFoundException"
    );
    assert_eq!(
        gone.2, NOT_FOUND_MESSAGE,
        "case {case:?}: expected AWS's fixed sentence for that class, got {:?}",
        gone.2
    );
}

/// `CreateTopic` for one row's topic, and the wait for the status the clock owes it.
///
/// The create's own answer reports the non-terminal state the model declares, and the
/// topic the brokers then serve is the one the control plane stored: its partition count
/// and replication factor are read back from `DescribeTopic`.
async fn given_topic(emulator: &Emulator, cluster: &str, row: &DataPlaneRow) {
    let case = row.case;
    let created = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(json!({
                "topicName": row.topic,
                "partitionCount": 1,
                "replicationFactor": 2,
            })),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: CreateTopic({}) must be accepted, got {} {}",
        row.topic, created.status, created.body
    );
    assert_eq!(
        created.at("/status").as_str(),
        Some("CREATING"),
        "case {case:?}: CreateTopic must answer the state it creates from, got {}",
        created.body
    );
    // LOOKS AGAIN until the topic reports the terminal status rather than waiting out a
    // span: the move out of `CREATING` is a transition the clock owes it.
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let described = emulator
            .call(
                "GET",
                &format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(row.topic)),
                None,
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeTopic({}) must answer, got {} {}",
            row.topic, described.status, described.body
        );
        if described.at("/status").as_str() == Some("ACTIVE") {
            assert_eq!(
                (
                    described.at("/partitionCount").as_i64(),
                    described.at("/replicationFactor").as_i64(),
                ),
                (Some(1), Some(2)),
                "case {case:?}: the topic must report the layout it was created with, got {}",
                described.body
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the topic never reported ACTIVE, last read {}",
            described.body
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// `DeleteTopic` for one row's topic, and the wait for the removal the clock owes it:
/// `DescribeTopic` then answers 404 `NotFoundException` with AWS's fixed sentence.
async fn delete_topic(emulator: &Emulator, cluster: &str, row: &DataPlaneRow) {
    let case = row.case;
    let uri = format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(row.topic));
    let deleted = emulator.call("DELETE", &uri, None).await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeleteTopic({}) must be accepted, got {} {}",
        row.topic, deleted.status, deleted.body
    );
    let gone = poll_until_gone(emulator, &uri, case).await;
    assert_eq!(
        (gone.0, gone.1.as_str()),
        (404, "NotFoundException"),
        "case {case:?}: a deleted topic must resolve to 404 NotFoundException"
    );
    assert_eq!(
        gone.2, NOT_FOUND_MESSAGE,
        "case {case:?}: expected AWS's fixed sentence for that class, got {:?}",
        gone.2
    );
}

/// Read `uri` again until it answers a status other than 200, and hand back that status,
/// its wire error code and its message.
///
/// A removal is a transition the clock owes the resource, so the loop LOOKS AGAIN under a
/// deadline far longer than the span the service owes it rather than waiting a fixed time.
async fn poll_until_gone(emulator: &Emulator, uri: &str, case: &str) -> (u16, String, String) {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let reply = emulator.call("GET", uri, None).await;
        if reply.status != 200 {
            return (
                reply.status,
                reply.error_code().to_string(),
                reply.error_message().to_string(),
            );
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {uri} never stopped resolving, last read {}",
            reply.body
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Read `DescribeClusterV2` again until the cluster reports `wanted`, and hand that state
/// back — the CONDITION a client waits for, under a generous deadline.
async fn poll_cluster_state(
    emulator: &Emulator,
    cluster: &str,
    wanted: &str,
    case: &str,
) -> String {
    let deadline = Instant::now() + SETTLED_DEADLINE;
    loop {
        let described = emulator.describe_v2(cluster).await;
        assert_eq!(
            described.status, 200,
            "case {case:?}: DescribeClusterV2 must answer, got {} {}",
            described.status, described.body
        );
        let state = described
            .at("/clusterInfo/state")
            .as_str()
            .unwrap_or_default()
            .to_string();
        if state == wanted {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the cluster never reported {wanted}, last read {}",
            described.body
        );
        tokio::time::sleep(PROBE_INTERVAL).await;
    }
}

/// Grant the SASL/SCRAM principal every operation on every topic and every consumer group,
/// over the SASL/IAM endpoint — the path an admin client of an ACL-enforcing cluster
/// administers bindings on — and read the bindings back, so the grant is one the cluster
/// HOLDS rather than one that was merely accepted.
async fn grant_every_operation(address: &str, case: &str) {
    let mut admin = Client::tls(case, address).await;
    admin.api_versions(case).await;
    let code = admin.authenticate(case, OAUTHBEARER, "a-signature").await;
    assert_eq!(
        code, 0,
        "case {case:?}: the SASL/IAM exchange must authenticate the admin client, got error \
         {code}"
    );
    let principal = format!("User:{SCRAM_USER}");
    let creations: Vec<AclCreation> = [ACL_RESOURCE_TYPE_TOPIC, ACL_RESOURCE_TYPE_GROUP]
        .iter()
        .map(|resource_type| {
            AclCreation::default()
                .with_resource_type(*resource_type)
                .with_resource_name(StrBytes::from_static_str(EVERY_NAME))
                .with_resource_pattern_type(ACL_PATTERN_TYPE_LITERAL)
                .with_principal(StrBytes::from_string(principal.clone()))
                .with_host(StrBytes::from_static_str(EVERY_HOST))
                .with_operation(ACL_OPERATION_ALL)
                .with_permission_type(ACL_PERMISSION_ALLOW)
        })
        .collect();
    let granted: CreateAclsResponse = admin
        .call(
            case,
            ApiKey::CreateAcls,
            1,
            &CreateAclsRequest::default().with_creations(creations.clone()),
        )
        .await;
    assert_eq!(
        granted.results.len(),
        creations.len(),
        "case {case:?}: CreateAcls must report every binding it was sent, got {:?}",
        granted.results
    );
    for result in &granted.results {
        assert_eq!(
            result.error_code, 0,
            "case {case:?}: every binding must be created, got error {} ({:?})",
            result.error_code, result.error_message
        );
    }
    let described: DescribeAclsResponse = admin
        .call(
            case,
            ApiKey::DescribeAcls,
            1,
            &DescribeAclsRequest::default()
                .with_resource_type_filter(ACL_RESOURCE_TYPE_ANY)
                // A NULL filter is the protocol's "any": one naming the empty string
                // would select a binding about the empty name.
                .with_resource_name_filter(None)
                .with_pattern_type_filter(ACL_PATTERN_TYPE_ANY)
                .with_principal_filter(None)
                .with_host_filter(None)
                .with_operation(ACL_OPERATION_ANY)
                .with_permission_type(ACL_PERMISSION_ANY),
        )
        .await;
    let held: Vec<i8> = described
        .resources
        .iter()
        .map(|resource| resource.resource_type)
        .collect();
    assert_eq!(
        held,
        vec![ACL_RESOURCE_TYPE_TOPIC, ACL_RESOURCE_TYPE_GROUP],
        "case {case:?}: the cluster must hold a binding for each resource granted"
    );
}

/// The data-plane leg of one row: authenticate as that endpoint's exchange requires,
/// produce the row's records, then read them back as a member of a CONSUMER GROUP the same
/// brokers coordinate.
async fn produce_and_consume(address: &str, row: &DataPlaneRow) {
    let case = row.case;
    let mut client = Client::tls(case, address).await;
    client.api_versions(case).await;
    let code = client
        .authenticate_as(case, row.mechanism, SCRAM_USER, row.credential)
        .await;
    assert_eq!(
        code, 0,
        "case {case:?}: the advertised endpoint must complete its own SASL exchange, got \
         error {code}"
    );

    let produced_at = epoch_millis();
    let batch: Vec<(i64, Option<&str>, &str)> = row
        .records
        .iter()
        .map(|(key, value)| (produced_at, *key, *value))
        .collect();
    let base = client.produce(case, row.topic, 0, &batch).await;
    assert_eq!(
        base, 0,
        "case {case:?}: the first batch of an empty partition starts at offset 0, got {base}"
    );

    // The coordinator of the group, which is where its membership and its committed
    // offsets live: a client asks for it and then speaks the group protocol to the address
    // it was given.
    let coordinator: FindCoordinatorResponse = client
        .call(
            case,
            ApiKey::FindCoordinator,
            row.versions.find_coordinator,
            &FindCoordinatorRequest::default()
                .with_key(StrBytes::from_static_str(row.group))
                .with_key_type(COORDINATOR_KEY_TYPE_GROUP),
        )
        .await;
    assert_eq!(
        coordinator.error_code, 0,
        "case {case:?}: the group's coordinator must be named, got error {}",
        coordinator.error_code
    );
    let coordinator_address = format!("{}:{}", coordinator.host.as_str(), coordinator.port);
    let (brokers, _) = reported_metadata(&mut client, row).await;
    assert!(
        brokers
            .iter()
            .any(|(_, authority)| *authority == coordinator_address),
        "case {case:?}: the coordinator must be named at an address this cluster's brokers \
         are reached at, got {coordinator_address} among {brokers:?}"
    );

    // The join and the assignment: the one member of a group leads it, so it is told the
    // whole membership and states the assignment every member then holds.
    let joined: JoinGroupResponse = client
        .call(
            case,
            ApiKey::JoinGroup,
            row.versions.join,
            &JoinGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_session_timeout_ms(SESSION_TIMEOUT_MS)
                .with_rebalance_timeout_ms(REBALANCE_TIMEOUT_MS)
                .with_protocol_type(StrBytes::from_static_str(CONSUMER_PROTOCOL))
                .with_protocols(vec![JoinGroupRequestProtocol::default()
                    .with_name(StrBytes::from_static_str(RANGE_ASSIGNOR))
                    .with_metadata(Bytes::from_static(b"subscription"))]),
        )
        .await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the join must be admitted, got error {}",
        joined.error_code
    );
    assert_eq!(
        joined.leader.as_str(),
        joined.member_id.as_str(),
        "case {case:?}: the only member of a group leads it, got leader {:?} for member {:?}",
        joined.leader,
        joined.member_id
    );
    let assignment = Bytes::from_static(b"partition-0");
    let synced: SyncGroupResponse = client
        .call(
            case,
            ApiKey::SyncGroup,
            row.versions.sync,
            &SyncGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_generation_id(joined.generation_id)
                .with_member_id(joined.member_id.clone())
                .with_assignments(vec![SyncGroupRequestAssignment::default()
                    .with_member_id(joined.member_id.clone())
                    .with_assignment(assignment.clone())]),
        )
        .await;
    assert_eq!(
        (synced.error_code, synced.assignment.clone()),
        (0, assignment),
        "case {case:?}: the member must be handed the assignment its leader stated"
    );
    let beat: HeartbeatResponse = client
        .call(
            case,
            ApiKey::Heartbeat,
            row.versions.heartbeat,
            &HeartbeatRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_generation_id(joined.generation_id)
                .with_member_id(joined.member_id.clone()),
        )
        .await;
    assert_eq!(
        beat.error_code, 0,
        "case {case:?}: a member of the generation in force must be kept, got error {}",
        beat.error_code
    );

    // Where the member starts reading: the group has committed nothing, so a client
    // configured to start at the earliest record asks the log for its own first offset.
    let committed = fetched_offset(&mut client, row, &joined.member_id.to_string()).await;
    assert_eq!(
        committed, NO_COMMITTED_OFFSET,
        "case {case:?}: a group nobody has committed in must report no committed offset, got \
         {committed}"
    );
    let earliest: ListOffsetsResponse = client
        .call(
            case,
            ApiKey::ListOffsets,
            row.versions.list_offsets,
            &ListOffsetsRequest::default()
                .with_replica_id(BrokerId(-1))
                .with_topics(vec![ListOffsetsTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(row.topic)))
                    .with_partitions(vec![ListOffsetsPartition::default()
                        .with_partition_index(0)
                        .with_timestamp(OFFSET_EARLIEST)])]),
        )
        .await;
    let position = earliest
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: ListOffsets reported no partition"))
        .clone();
    assert_eq!(
        (position.error_code, position.offset),
        (0, 0),
        "case {case:?}: the earliest offset of a partition nothing aged out is 0"
    );

    // And the read itself: every record produced, in order, starting AT the offset the
    // member asked for — a client advances its position by that offset, so a batch that
    // does not carry it is a batch the client cannot consume.
    let read = fetch_from(&mut client, row, position.offset, ONE_BYTE, LONG_POLL_MS).await;
    let expected: Vec<(i64, Option<String>, String)> = row
        .records
        .iter()
        .enumerate()
        .map(|(index, (key, value))| {
            (
                position.offset + index as i64,
                key.map(ToString::to_string),
                (*value).to_string(),
            )
        })
        .collect();
    assert_eq!(
        read, expected,
        "case {case:?}: the group member must read back what was produced, in order"
    );

    // How far it has read, stored in the group: the position it reports back is the one it
    // committed, and a member that resumes from it reads no record twice.
    let read_through = row.records.len() as i64;
    let commit: OffsetCommitResponse = client
        .call(
            case,
            ApiKey::OffsetCommit,
            row.versions.offset_commit,
            &OffsetCommitRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_generation_id_or_member_epoch(joined.generation_id)
                .with_member_id(joined.member_id.clone())
                .with_topics(vec![OffsetCommitRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(row.topic)))
                    .with_partitions(vec![OffsetCommitRequestPartition::default()
                        .with_partition_index(0)
                        .with_committed_offset(read_through)])]),
        )
        .await;
    let outcome = commit
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: OffsetCommit reported no partition"));
    assert_eq!(
        outcome, 0,
        "case {case:?}: the commit must be stored, got error {outcome}"
    );
    let resumed = fetched_offset(&mut client, row, &joined.member_id.to_string()).await;
    assert_eq!(
        resumed, read_through,
        "case {case:?}: the group must report the offset its member committed"
    );
    assert!(
        fetch_from(&mut client, row, resumed, NOTHING, NO_WAIT)
            .await
            .is_empty(),
        "case {case:?}: a member resuming from the offset it committed must read no record \
         again"
    );

    // And every broker the cluster's own `Metadata` names is an address the client then
    // opens a connection of its OWN to — one per node it reads from, beside the one it
    // bootstrapped on — so each of them completes the same exchange and serves the same
    // log, and the partition's leader is one of them.
    served_by_every_reported_broker(&mut client, row, &expected).await;

    let left: LeaveGroupResponse = client
        .call(
            case,
            ApiKey::LeaveGroup,
            row.versions.leave,
            &LeaveGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_member_id(joined.member_id.clone()),
        )
        .await;
    assert_eq!(
        left.error_code, 0,
        "case {case:?}: the member must be let out of the group, got error {}",
        left.error_code
    );
}

/// Every broker the cluster's `Metadata` reports, dialled as a client dials them: one
/// connection per node, each completing the access method's own exchange and reading the
/// same partition back — and the partition's leader is one of the nodes reported.
///
/// This is the shape a client library takes after it bootstraps: it keeps a connection per
/// broker it reads from (and one more for the group's coordinator), so an address the
/// metadata names that serves nothing leaves the client retrying with no error to report.
async fn served_by_every_reported_broker(
    client: &mut Client,
    row: &DataPlaneRow,
    expected: &[(i64, Option<String>, String)],
) {
    let case = row.case;
    let (brokers, leader) = reported_metadata(client, row).await;
    assert!(
        brokers.iter().any(|(node, _)| *node == leader),
        "case {case:?}: the partition's leader {leader} must be one of the brokers reported, \
         got {brokers:?}"
    );
    for (node, authority) in &brokers {
        let mut per_node = Client::tls(case, authority).await;
        per_node.api_versions(case).await;
        let code = per_node
            .authenticate_as(case, row.mechanism, SCRAM_USER, row.credential)
            .await;
        assert_eq!(
            code, 0,
            "case {case:?}: the connection a client opens to broker {node} must complete the \
             same exchange, got error {code}"
        );
        let read = fetch_from(&mut per_node, row, 0, ONE_BYTE, LONG_POLL_MS).await;
        assert_eq!(
            read, expected,
            "case {case:?}: broker {node} at {authority} must serve the same log"
        );
    }
}

/// What the cluster's own `Metadata` names for the row's topic: the brokers a client then
/// connects to, as `(node id, host:port)`, and the node leading its partition.
///
/// Version 1 names the topics it asks about, which is the form a consuming client sends
/// once it knows which topic it subscribed to.
async fn reported_metadata(client: &mut Client, row: &DataPlaneRow) -> (Vec<(i32, String)>, i32) {
    use ::kafka_protocol::messages::metadata_request::MetadataRequestTopic;
    use ::kafka_protocol::messages::{MetadataRequest, MetadataResponse};

    let case = row.case;
    let reported: MetadataResponse = client
        .call(
            case,
            ApiKey::Metadata,
            1,
            &MetadataRequest::default().with_topics(Some(vec![MetadataRequestTopic::default()
                .with_name(Some(TopicName(StrBytes::from_static_str(row.topic))))])),
        )
        .await;
    let brokers: Vec<(i32, String)> = reported
        .brokers
        .iter()
        .map(|broker| {
            (
                broker.node_id.0,
                format!("{}:{}", broker.host.as_str(), broker.port),
            )
        })
        .collect();
    assert!(
        !brokers.is_empty(),
        "case {case:?}: the cluster's metadata must name the brokers a client connects to"
    );
    let leader = reported
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.leader_id.0)
        .unwrap_or_else(|| panic!("case {case:?}: the metadata reported no partition"));
    (brokers, leader)
}

/// The offset the group has committed for the row's partition, as `OffsetFetch` reports it.
async fn fetched_offset(client: &mut Client, row: &DataPlaneRow, member: &str) -> i64 {
    let case = row.case;
    let answered: OffsetFetchResponse = client
        .call(
            case,
            ApiKey::OffsetFetch,
            row.versions.offset_fetch,
            &OffsetFetchRequest::default()
                .with_group_id(GroupId(StrBytes::from_static_str(row.group)))
                .with_topics(Some(vec![OffsetFetchRequestTopic::default()
                    .with_name(TopicName(StrBytes::from_static_str(row.topic)))
                    .with_partition_indexes(vec![0])])),
        )
        .await;
    assert_eq!(
        answered.error_code, 0,
        "case {case:?}: OffsetFetch for member {member} must be answered, got error {}",
        answered.error_code
    );
    answered
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.committed_offset)
        .unwrap_or_else(|| panic!("case {case:?}: OffsetFetch reported no partition"))
}

/// One `Fetch` of the row's partition from `offset`, at the version the row's client
/// negotiated, asking for `min_bytes` and waiting at most `max_wait_ms` for them;
/// answering the records it read as `(offset, key, value)`.
async fn fetch_from(
    client: &mut Client,
    row: &DataPlaneRow,
    offset: i64,
    min_bytes: i32,
    max_wait_ms: i32,
) -> Vec<(i64, Option<String>, String)> {
    use ::kafka_protocol::messages::fetch_request::{FetchPartition, FetchTopic};

    let case = row.case;
    let answered: FetchResponse = client
        .call(
            case,
            ApiKey::Fetch,
            row.versions.fetch,
            &FetchRequest::default()
                .with_max_wait_ms(max_wait_ms)
                .with_min_bytes(min_bytes)
                .with_max_bytes(1_048_576)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(TopicName(StrBytes::from_static_str(row.topic)))
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(0)
                        .with_fetch_offset(offset)
                        .with_partition_max_bytes(1_048_576)])]),
        )
        .await;
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
        .clone();
    assert_eq!(
        partition.error_code, 0,
        "case {case:?}: Fetch must be answered, got error {}",
        partition.error_code
    );
    let Some(mut records) = partition.records.clone() else {
        return Vec::new();
    };
    // A fetch that read nothing answers an EMPTY record set, which is no batch at all
    // rather than a batch of no records.
    if records.is_empty() {
        return Vec::new();
    }
    RecordBatchDecoder::decode(&mut records)
        .unwrap_or_else(|error| panic!("case {case:?}: the batch must decode: {error}"))
        .records
        .iter()
        .map(|record| {
            (
                record.offset,
                record
                    .key
                    .as_ref()
                    .map(|key| String::from_utf8_lossy(key).to_string()),
                record
                    .value
                    .as_ref()
                    .map(|value| String::from_utf8_lossy(value).to_string())
                    .unwrap_or_default(),
            )
        })
        .collect()
}
