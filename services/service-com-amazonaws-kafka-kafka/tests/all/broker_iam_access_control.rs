// SPDX-License-Identifier: CC0-1.0

//! Amazon MSK's IAM ACCESS CONTROL on the data plane: what a client that authenticated
//! with IAM may then do over the connection it authenticated.
//!
//! RULE under test: on a connection whose SASL/IAM exchange was signed by a principal
//! this account holds policies for, every request is decided against those policies as
//! the `kafka-cluster:` action it asks for on the resource it addresses — a produce needs
//! `WriteData` on the topic, a fetch `ReadData`, joining or committing in a consumer
//! group `AlterGroup` on the group — and a request no policy allows is refused with the
//! protocol's `TOPIC_AUTHORIZATION_FAILED` / `GROUP_AUTHORIZATION_FAILED` rather than
//! served. A credential nothing readable bounds is not refused for a missing grant.
//!
//! The rows span what the rule generalises over: which ACTION the request asks for
//! (produce, fetch, group join), which RESOURCE the policy is scoped to (one topic, one
//! group, everything), and whether the principal's permission set is readable at all.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the SASL/IAM address `GetBootstrapBrokers` published and authenticated with the
//! credential the row names.

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    join_group_request::JoinGroupRequestProtocol,
    produce_request::{PartitionProduceData, TopicProduceData},
    fetch_request::{FetchPartition, FetchTopic},
    ApiKey, FetchRequest, FetchResponse, GroupId, JoinGroupRequest, JoinGroupResponse,
    ProduceRequest, ProduceResponse, SaslAuthenticateRequest, SaslAuthenticateResponse,
    SaslHandshakeRequest, SaslHandshakeResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use ::kafka_protocol::ResponseError;
use serde_json::{json, Value};

use crate::broker_data_plane::{Client, AWS_MSK_IAM, CLIENT_ID, LOOPBACK};
use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, pct, ClusterRequest, Emulator, ACCOUNT,
    DEFAULT_ACCESS_KEY_ID, REGION,
};

/// The protocol type a consuming client joins a group with, and the assignor it names.
const CONSUMER_PROTOCOL: &str = "consumer";
const RANGE_ASSIGNOR: &str = "range";

/// The topic and group every row below addresses, and the ones no policy covers.
const TOPIC: &str = "orders";
const OTHER_TOPIC: &str = "payments";
const GROUP: &str = "readers";
const OTHER_GROUP: &str = "auditors";

/// The `kafka-cluster:` actions a policy below grants.
const CONNECT: &str = "kafka-cluster:Connect";
const WRITE_DATA: &str = "kafka-cluster:WriteData";
const READ_DATA: &str = "kafka-cluster:ReadData";
const DESCRIBE_TOPIC: &str = "kafka-cluster:DescribeTopic";
const ALTER_GROUP: &str = "kafka-cluster:AlterGroup";

/// RULE: a produce is served only where the principal's policies allow `WriteData` on
/// that topic, and a fetch only where they allow `ReadData` — on the same connection, so
/// the decision is per request and per resource rather than per connection.
#[tokio::test]
async fn a_produce_and_a_fetch_are_each_decided_against_the_principals_policies() {
    // (case, the policy in force, the topic the request addresses, the produce answer,
    // the fetch answer)
    let rows: &[(&str, Option<Value>, &str, i16, i16)] = &[
        (
            "a policy that allows the read but not the write",
            Some(policy_allowing(
                &[CONNECT, READ_DATA, DESCRIBE_TOPIC],
                &[cluster_wide_resource("topic")],
            )),
            TOPIC,
            ResponseError::TopicAuthorizationFailed.code(),
            0,
        ),
        (
            "a policy that allows both",
            Some(policy_allowing(
                &[CONNECT, READ_DATA, WRITE_DATA, DESCRIBE_TOPIC],
                &[cluster_wide_resource("topic")],
            )),
            TOPIC,
            0,
            0,
        ),
        (
            "a policy scoped to another topic",
            Some(policy_allowing(
                &[CONNECT, READ_DATA, WRITE_DATA, DESCRIBE_TOPIC],
                &[topic_resource(OTHER_TOPIC)],
            )),
            TOPIC,
            ResponseError::TopicAuthorizationFailed.code(),
            ResponseError::TopicAuthorizationFailed.code(),
        ),
        (
            "a principal whose permission set is empty",
            None,
            TOPIC,
            ResponseError::TopicAuthorizationFailed.code(),
            ResponseError::TopicAuthorizationFailed.code(),
        ),
    ];
    for (index, (case, policy, topic, produce_code, fetch_code)) in rows.iter().enumerate() {
        let emulator = Emulator::with_iam();
        let cluster = given_iam_cluster(&emulator, &format!("iam-dataplane-{index}")).await;
        given_topic(&emulator, &cluster, TOPIC).await;
        let access_key_id = format!("ASIAKAFKADATAPLANE{index}");
        given_principal(&emulator, &format!("kafka-client-{index}"), &access_key_id, policy.as_ref());

        let authority = sasl_iam_address(&emulator, &cluster, case).await;
        let mut client = iam_client(case, &authority, &access_key_id).await;

        let produced = produce(&mut client, case, topic).await;
        assert_eq!(
            produced, *produce_code,
            "case {case:?}: expected a produce to {topic} to answer {produce_code}, got {produced}"
        );
        let fetched = fetch(&mut client, case, topic).await;
        assert_eq!(
            fetched, *fetch_code,
            "case {case:?}: expected a fetch of {topic} to answer {fetch_code}, got {fetched}"
        );
    }
}

/// RULE: joining a consumer group is served only where the principal's policies allow
/// `AlterGroup` on THAT group, while a read that joins no group is untouched by the group
/// scope.
#[tokio::test]
async fn joining_a_group_is_decided_against_the_groups_own_scope() {
    let case = "a policy scoped to one consumer group";
    let emulator = Emulator::with_iam();
    let cluster = given_iam_cluster(&emulator, "iam-group-scope").await;
    given_topic(&emulator, &cluster, TOPIC).await;
    let access_key_id = "ASIAKAFKAGROUPSCOPE";
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [CONNECT, READ_DATA, DESCRIBE_TOPIC],
                "Resource": [cluster_wide_resource("topic"), cluster_resource()],
            },
            {
                "Effect": "Allow",
                "Action": [ALTER_GROUP],
                "Resource": [group_resource(GROUP)],
            },
        ],
    });
    given_principal(&emulator, "kafka-group-client", access_key_id, Some(&policy));

    let authority = sasl_iam_address(&emulator, &cluster, case).await;
    let mut client = iam_client(case, &authority, access_key_id).await;

    // (the group the join names, the answer)
    let rows: &[(&str, i16)] = &[
        (GROUP, 0),
        (OTHER_GROUP, ResponseError::GroupAuthorizationFailed.code()),
    ];
    for (group, expected) in rows {
        let joined = join(&mut client, case, group).await;
        assert_eq!(
            joined.error_code, *expected,
            "case {case:?}: expected a join of {group} to answer {expected}, got {}",
            joined.error_code
        );
    }

    // The same connection's read, which joins no group, is served: the group scope
    // decides group calls alone.
    let fetched = fetch(&mut client, case, TOPIC).await;
    assert_eq!(
        fetched, 0,
        "case {case:?}: a read that joins no group must be served, got {fetched}"
    );
}

/// RULE: a credential nothing readable bounds — one this account holds no policies for —
/// is not refused for a missing grant, because nothing that can be read says what it may
/// do.
#[tokio::test]
async fn a_credential_no_policy_bounds_is_not_refused_for_a_missing_grant() {
    let case = "a credential this account holds no policies for";
    let emulator = Emulator::with_iam();
    let cluster = given_iam_cluster(&emulator, "iam-unbounded").await;
    given_topic(&emulator, &cluster, TOPIC).await;
    let authority = sasl_iam_address(&emulator, &cluster, case).await;
    let mut client = iam_client(case, &authority, DEFAULT_ACCESS_KEY_ID).await;

    let produced = produce(&mut client, case, TOPIC).await;
    assert_eq!(
        produced, 0,
        "case {case:?}: the produce must be served, got {produced}"
    );
    let joined = join(&mut client, case, GROUP).await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the join must be served, got {}",
        joined.error_code
    );
}

/// A policy document allowing `actions` on `resources`, in the shape
/// `iam:PutRolePolicy` takes one.
fn policy_allowing(actions: &[&str], resources: &[String]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": actions,
            "Resource": resources,
        }],
    })
}

/// The ARN an IAM policy addresses every resource of one KIND of this suite's cluster by:
/// Amazon MSK scopes a `kafka-cluster:` action to `<kind>/<cluster name>/<cluster uuid>/`
/// plus the resource's own name, and this suite's cluster name is a prefix no other
/// cluster of the account shares.
fn cluster_wide_resource(kind: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:{kind}/*")
}

/// The ARN of the cluster itself, which is what the `Connect` action is scoped to.
fn cluster_resource() -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*")
}

/// The ARN one topic of this suite's cluster is addressed by.
fn topic_resource(topic: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*/{topic}")
}

/// The ARN one consumer group of this suite's cluster is addressed by.
fn group_resource(group: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:group/*/{group}")
}

/// Record the principal `access_key_id` speaks for and the policies in force on it, as
/// `sts:AssumeRole` and `iam:PutRolePolicy` record them. `None` leaves the principal with
/// a readable permission set that is empty.
fn given_principal(emulator: &Emulator, role: &str, access_key_id: &str, policy: Option<&Value>) {
    given_assumed_role_session(&emulator.engine, role, "session-1", access_key_id);
    given_role_inline_policy(&emulator.engine, role, policy);
}

/// A cluster whose only access path is SASL/IAM over TLS, which is the path MSK's IAM
/// access control governs.
async fn given_iam_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_iam: Some(true),
            ..ClusterRequest::named(name)
        })
        .await
}

/// Create a topic on `cluster` through the control plane, which is where a provisioned
/// cluster's topics come from.
async fn given_topic(emulator: &Emulator, cluster: &str, name: &str) {
    let reply = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(json!({
                "topicName": name,
                "partitionCount": 1,
                "replicationFactor": 1,
            })),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "setup: CreateTopic({name}) must be accepted, got {} {}",
        reply.status, reply.body
    );
}

/// The address a cluster publishes for its SASL/IAM path, once it publishes one.
async fn sasl_iam_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerStringSaslIam", case)
        .await
}

/// A client on the SASL/IAM path, authenticated with the credential `access_key_id`
/// names: the exchange Amazon MSK's IAM access control defines, whose payload says which
/// key signed it.
async fn iam_client(case: &str, authority: &str, access_key_id: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    let handshake: SaslHandshakeResponse = client
        .call(
            case,
            ApiKey::SaslHandshake,
            1,
            &SaslHandshakeRequest::default()
                .with_mechanism(StrBytes::from_static_str(AWS_MSK_IAM)),
        )
        .await;
    assert_eq!(
        handshake.error_code, 0,
        "case {case:?}: the cluster must offer the IAM mechanism, got error {}",
        handshake.error_code
    );
    let payload = json!({
        "version": "2020_10_22",
        "host": LOOPBACK,
        "user-agent": CLIENT_ID,
        "action": "kafka-cluster:Connect",
        "x-amz-algorithm": "AWS4-HMAC-SHA256",
        "x-amz-credential":
            format!("{access_key_id}/20261101/{REGION}/kafka-cluster/aws4_request"),
        "x-amz-date": "20261101T000000Z",
        "x-amz-signedheaders": "host",
        "x-amz-signature": "a-signature",
    });
    let authenticated: SaslAuthenticateResponse = client
        .call(
            case,
            ApiKey::SaslAuthenticate,
            2,
            &SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from(payload.to_string().into_bytes())),
        )
        .await;
    assert_eq!(
        authenticated.error_code, 0,
        "case {case:?}: the IAM exchange must authenticate the connection, got error {}",
        authenticated.error_code
    );
    client
}

/// Produce one record to partition 0 of `topic`, answering the error code the partition
/// was given.
async fn produce(client: &mut Client, case: &str, topic: &str) -> i16 {
    let record = Record {
        transactional: false,
        control: false,
        partition_leader_epoch: 0,
        producer_id: -1,
        producer_epoch: -1,
        timestamp_type: TimestampType::Creation,
        offset: 0,
        sequence: 0,
        timestamp: 0,
        key: None,
        value: Some(Bytes::from_static(b"a-record")),
        headers: Default::default(),
        delete_horizon: false,
    };
    let mut encoded = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut encoded,
        [record].iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    let answered: ProduceResponse = client
        .call(
            case,
            ApiKey::Produce,
            9,
            &ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(0)
                        .with_records(Some(encoded.freeze()))])]),
        )
        .await;
    answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .map(|partition| partition.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"))
}

/// Fetch partition 0 of `topic` from its first offset, answering the error code the
/// partition was given.
async fn fetch(client: &mut Client, case: &str, topic: &str) -> i16 {
    let answered: FetchResponse = client
        .call(
            case,
            ApiKey::Fetch,
            11,
            &FetchRequest::default()
                .with_max_wait_ms(0)
                .with_min_bytes(0)
                .with_max_bytes(1_048_576)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(0)
                        .with_fetch_offset(0)
                        .with_partition_max_bytes(1_048_576)])]),
        )
        .await;
    answered
        .responses
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
}

/// `JoinGroup` for `group`, as a consuming client sends it.
async fn join(client: &mut Client, case: &str, group: &str) -> JoinGroupResponse {
    client
        .call(
            case,
            ApiKey::JoinGroup,
            5,
            &JoinGroupRequest::default()
                .with_group_id(GroupId(StrBytes::from_string(group.to_string())))
                .with_session_timeout_ms(30_000)
                .with_rebalance_timeout_ms(30_000)
                .with_member_id(StrBytes::from_static_str(""))
                .with_protocol_type(StrBytes::from_static_str(CONSUMER_PROTOCOL))
                .with_protocols(vec![JoinGroupRequestProtocol::default()
                    .with_name(StrBytes::from_static_str(RANGE_ASSIGNOR))
                    .with_metadata(Bytes::from_static(b"subscription"))]),
        )
        .await
}
