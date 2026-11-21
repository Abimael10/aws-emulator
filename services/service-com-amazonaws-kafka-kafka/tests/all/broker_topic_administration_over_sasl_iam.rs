// SPDX-License-Identifier: CC0-1.0

//! TOPIC ADMINISTRATION over the SASL/IAM path an admin client reaches a cluster's
//! brokers on.
//!
//! RULE under test: the `ApiVersions` table a broker advertises is the same on every
//! access path the cluster publishes, and a pair in it is answered on a connection of
//! that path — the topic-administration APIs included. An admin client picks what it may
//! send from that advertisement alone and refuses to send anything else, blaming the
//! broker's VERSION rather than the request; so an endpoint that omits a
//! topic-administration api key from its table leaves a client unable to create a topic at
//! all, which for an MSK Serverless cluster — whose clients reach their brokers only over
//! SASL/IAM, and whose topics have no control-plane management — means no topic can be
//! created by any means. What is served is nonetheless DECIDED: on a connection the IAM
//! exchange authenticated, each request is judged as the `kafka-cluster:` action it asks
//! for on the resource it addresses.
//!
//! The rows span what the rule generalises over: which ACCESS PATH the client reached the
//! brokers on (a provisioned cluster's plaintext, TLS, SASL/SCRAM and SASL/IAM endpoints,
//! and the one SASL/IAM endpoint a serverless cluster publishes), which topic-ADMIN API
//! the client sends (create, grow, delete), which VERSION of it the advertisement allows
//! (every version from the advertised minimum to the maximum, so the oldest form a client
//! may pick is covered beside the newest), how many PARTITIONS the create asks for
//! (including `-1`, which asks for the broker's own default), and which actions and
//! resources the POLICY in force on the signing principal grants. The whole advertised
//! table is answered over the plaintext path by the `broker_advertised_api_versions`
//! suite, which this one leaves to it.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published and authenticated as that path requires.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use ::kafka_protocol::messages::{
    create_partitions_request::CreatePartitionsTopic, create_topics_request::CreatableTopic, ApiKey,
    CreatePartitionsRequest, CreatePartitionsResponse, CreateTopicsRequest, CreateTopicsResponse,
    DeleteTopicsRequest, DeleteTopicsResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use serde_json::{json, Value};

use crate::broker_data_plane::{Client, AWS_MSK_IAM};
use crate::kafka_harness::{
    given_assumed_role_session, given_role_inline_policy, ClusterRequest, Emulator, ACCOUNT,
    DEFAULT_ACCESS_KEY_ID, REGION,
};

/// The api key the Apache Kafka protocol assigns each topic-administration request, as
/// its protocol guide numbers them: `CreateTopics` is 19, `DeleteTopics` 20 and
/// `CreatePartitions` 37. The numbers are written out here so the advertisement is
/// checked against the protocol's own numbering rather than against whatever the
/// implementation behind [`ApiKey`] happens to name.
const TOPIC_ADMIN_KEYS: &[(i16, ApiKey)] = &[
    (19, ApiKey::CreateTopics),
    (20, ApiKey::DeleteTopics),
    (37, ApiKey::CreatePartitions),
];

/// How long a request below gives the broker to place the topics it names: a value in the
/// request, not a bound on the test.
const REQUEST_TIMEOUT_MS: i32 = 1_000;

/// The `num_partitions` a client sends to ask for the broker's own default rather than a
/// count of its own (Apache Kafka protocol, `CreateTopics`).
const BROKER_DEFAULT_PARTITIONS: i32 = -1;

/// How long this suite waits for a condition the clock owes the cluster — a deleted
/// topic's removal — before calling it a failure: far longer than the span the service
/// owes it, so the deadline says nothing about how fast the host runs.
const REMOVAL_DEADLINE: Duration = Duration::from_secs(60);

/// How long such a probe waits between two looks, a probing cadence rather than a bound
/// on the transition.
const REMOVAL_PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// The `kafka-cluster:` actions the policies below grant: connecting at all, reading a
/// topic's metadata, and creating one (Amazon MSK IAM access control actions).
const CONNECT: &str = "kafka-cluster:Connect";
const DESCRIBE_TOPIC: &str = "kafka-cluster:DescribeTopic";
const CREATE_TOPIC: &str = "kafka-cluster:CreateTopic";

/// The topics the policy rows address.
const TOPIC: &str = "orders";
const OTHER_TOPIC: &str = "payments";

/// The sentence Apache Kafka's `TOPIC_AUTHORIZATION_FAILED` carries, which is what a
/// request no policy allows is refused with.
const TOPIC_AUTHORIZATION_FAILED_MESSAGE: &str = "Topic authorization failed.";

/// The advertised table, as `api key -> (min version, max version)`.
type Advertised = BTreeMap<i16, (i16, i16)>;

/// What one `CreateTopics` result says about one topic: its error code and its error
/// message, which is `None` where the answer carries none at all.
type TopicOutcome = (i16, Option<String>);

/// RULE: every access path a cluster publishes advertises the SAME APIs and versions, and
/// that table names the topic-administration api keys — so a client that bootstraps
/// against any published address negotiates the same surface, and an admin client finds
/// the topic APIs on the path it reached.
#[tokio::test]
async fn every_published_access_path_advertises_the_same_apis() {
    let emulator = Emulator::new();
    let provisioned = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS_PLAINTEXT"),
            unauthenticated: Some(true),
            sasl_scram: Some(true),
            sasl_iam: Some(true),
            brokers: 2,
            subnets: vec!["subnet-a", "subnet-b"],
            ..ClusterRequest::named("admin-advertised-paths")
        })
        .await;
    let serverless = emulator
        .create_serverless_ok(
            "admin-advertised-serverless",
            &[vec!["subnet-0a1b2c3d"]],
            Some(true),
        )
        .await;

    // (case, the cluster, the published member, whether that path's wire is encrypted)
    let paths: &[(&str, &str, &str, bool)] = &[
        (
            "a provisioned cluster's plaintext path",
            &provisioned,
            "bootstrapBrokerString",
            false,
        ),
        (
            "a provisioned cluster's TLS path",
            &provisioned,
            "bootstrapBrokerStringTls",
            true,
        ),
        (
            "a provisioned cluster's SASL/SCRAM path",
            &provisioned,
            "bootstrapBrokerStringSaslScram",
            true,
        ),
        (
            "a provisioned cluster's SASL/IAM path",
            &provisioned,
            "bootstrapBrokerStringSaslIam",
            true,
        ),
        (
            "a serverless cluster's SASL/IAM path",
            &serverless,
            "bootstrapBrokerStringSaslIam",
            true,
        ),
    ];

    let mut first: Option<(&str, Advertised)> = None;
    for (case, cluster, member, encrypted) in paths {
        // LOOKS AGAIN until the cluster publishes the member rather than waiting out a
        // span: the move out of `CREATING` is a transition the clock owes it.
        let authority = emulator.published_bootstrap(cluster, member, case).await;
        let advertised = advertised_table(case, &authority, *encrypted).await;

        for (number, key) in TOPIC_ADMIN_KEYS {
            assert_eq!(
                *key as i16, *number,
                "case {case:?}: {key:?} is api key {number} in the Kafka protocol, got {}",
                *key as i16
            );
            let served = advertised.get(number).copied();
            assert!(
                served.is_some_and(|(min, max)| min >= 0 && min <= max),
                "case {case:?}: the broker must advertise api key {number} ({key:?}), which is \
                 what an admin client negotiates topic administration from; it advertised \
                 {served:?} out of {advertised:?}"
            );
        }

        match &first {
            None => first = Some((case, advertised)),
            Some((reference, table)) => assert_eq!(
                &advertised, table,
                "case {case:?}: every published path must advertise the same table as \
                 {reference:?}, got {advertised:?} beside {table:?}"
            ),
        }
    }
}

/// RULE: a topic-administration request is served on the SASL/IAM path at every version
/// the broker advertises for it, and acts on the cluster's own topics — a create places
/// the partitions it asked for, a growth raises that count, a delete removes the topic,
/// and the cluster's brokers report each outcome at once.
#[tokio::test]
async fn topic_administration_is_served_on_the_sasl_iam_path_at_every_advertised_version() {
    let case = "an MSK Serverless cluster's SASL/IAM path";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_serverless_ok("admin-iam-versions", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    let authority = emulator
        .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
        .await;
    let advertised = advertised_table(case, &authority, true).await;

    // (the partitions the create asks for, the partitions the brokers must then report)
    let counts: &[(i32, usize)] = &[(1, 1), (3, 3), (BROKER_DEFAULT_PARTITIONS, 1)];

    for version in versions_of(case, &advertised, ApiKey::CreateTopics) {
        for (asked, placed) in counts {
            let row = format!("{case} / CreateTopics v{version} / {asked} partitions");
            let topic = format!("iam-created-v{version}-p{asked}");
            let mut client = iam_client(&row, &authority).await;
            let created = create_topic(&mut client, &row, version, &topic, *asked).await;
            assert_eq!(
                created,
                (0, None),
                "case {row:?}: the broker must create {topic} on the path it advertised \
                 CreateTopics v{version} on, got {created:?}"
            );
            assert_eq!(
                partitions_reported(&mut client, &row, &topic).await,
                Some(*placed),
                "case {row:?}: the cluster's brokers must report {topic} with {placed} \
                 partitions"
            );
        }
    }

    for version in versions_of(case, &advertised, ApiKey::CreatePartitions) {
        let row = format!("{case} / CreatePartitions v{version}");
        let topic = format!("iam-grown-v{version}");
        let mut client = iam_client(&row, &authority).await;
        let created = create_topic(
            &mut client,
            &row,
            newest(&advertised, ApiKey::CreateTopics),
            &topic,
            1,
        )
        .await;
        assert_eq!(
            created,
            (0, None),
            "case {row:?}: the topic to grow must be created first, got {created:?}"
        );
        let code = grow_topic(&mut client, &row, version, &topic, 2).await;
        assert_eq!(
            code, 0,
            "case {row:?}: the broker must grow {topic} at the version it advertised, got \
             error {code}"
        );
        assert_eq!(
            partitions_reported(&mut client, &row, &topic).await,
            Some(2),
            "case {row:?}: the cluster's brokers must report the raised partition count"
        );
    }

    for version in versions_of(case, &advertised, ApiKey::DeleteTopics) {
        let row = format!("{case} / DeleteTopics v{version}");
        let topic = format!("iam-deleted-v{version}");
        let mut client = iam_client(&row, &authority).await;
        let created = create_topic(
            &mut client,
            &row,
            newest(&advertised, ApiKey::CreateTopics),
            &topic,
            1,
        )
        .await;
        assert_eq!(
            created,
            (0, None),
            "case {row:?}: the topic to delete must be created first, got {created:?}"
        );
        let code = delete_topic(&mut client, &row, version, &topic).await;
        assert_eq!(
            code, 0,
            "case {row:?}: the broker must delete {topic} at the version it advertised, got \
             error {code}"
        );
        // The REMOVAL is a transition the clock owes the topic a delete put into
        // `DELETING`, so the brokers are read AGAIN until they no longer report the name
        // rather than after a wait: the deadline is far longer than the span the service
        // owes it, so it says nothing about how fast the host runs.
        let deadline = Instant::now() + REMOVAL_DEADLINE;
        loop {
            let reported = partitions_reported(&mut client, &row, &topic).await;
            if reported.is_none() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {row:?}: a deleted topic must stop being reported by the cluster's \
                 brokers, it still reports {reported:?}"
            );
            tokio::time::sleep(REMOVAL_PROBE_INTERVAL).await;
        }
    }
}

/// RULE: a topic-administration request on a connection the SASL/IAM exchange
/// authenticated is decided against the signing principal's policies, as the
/// `kafka-cluster:` action it asks for on the resource it addresses — so a create no
/// policy allows is answered with the protocol's `TOPIC_AUTHORIZATION_FAILED` and that
/// error class's own sentence, and registers nothing; an advertised API is not a blanket
/// success.
///
/// The rows span what the rule generalises over: which ACTIONS the policy in force grants
/// and which RESOURCE it scopes them to (every topic of the cluster, one topic of it), and
/// which topic the request then addresses.
#[tokio::test]
async fn a_create_on_the_sasl_iam_path_is_decided_against_the_principals_policies() {
    // (case, the policy in force, the topic the create names, whether it must be created)
    let rows: Vec<(&str, Value, &str, bool)> = vec![
        (
            "a policy that admits the connection but not the create",
            policy_allowing(&[CONNECT, DESCRIBE_TOPIC], &[every_topic(), every_cluster()]),
            TOPIC,
            false,
        ),
        (
            "a policy that allows the create on every topic",
            policy_allowing(
                &[CONNECT, DESCRIBE_TOPIC, CREATE_TOPIC],
                &[every_topic(), every_cluster()],
            ),
            TOPIC,
            true,
        ),
        (
            "a policy whose create is scoped to another topic",
            policy_allowing(
                &[CONNECT, DESCRIBE_TOPIC, CREATE_TOPIC],
                &[one_topic(OTHER_TOPIC), every_cluster()],
            ),
            TOPIC,
            false,
        ),
        (
            "the same policy, for the topic it is scoped to",
            policy_allowing(
                &[CONNECT, DESCRIBE_TOPIC, CREATE_TOPIC],
                &[one_topic(OTHER_TOPIC), every_cluster()],
            ),
            OTHER_TOPIC,
            true,
        ),
    ];

    for (index, (case, policy, topic, allowed)) in rows.iter().enumerate() {
        // A policy source of its own per row: the permissions in force are the ones this
        // row's principal holds.
        let emulator = Emulator::with_iam();
        let cluster = emulator
            .create_serverless_ok(
                &format!("admin-iam-policy-{index}"),
                &[vec!["subnet-0a1b2c3d"]],
                Some(true),
            )
            .await;
        let role = format!("kafka-admin-client-{index}");
        let authority = emulator
            .published_bootstrap(&cluster, "bootstrapBrokerStringSaslIam", case)
            .await;
        let advertised = advertised_table(case, &authority, true).await;
        // The credential is bound to this row's principal only once the endpoint has been
        // read, so what the policy in force decides is the DATA-plane connection below
        // rather than the control-plane reads that set the row up.
        given_assumed_role_session(&emulator.engine, &role, "session-1", DEFAULT_ACCESS_KEY_ID);
        given_role_inline_policy(&emulator.engine, &role, Some(policy));

        let mut client = iam_client(case, &authority).await;
        let outcome = create_topic(
            &mut client,
            case,
            newest(&advertised, ApiKey::CreateTopics),
            topic,
            1,
        )
        .await;

        let expected: TopicOutcome = match allowed {
            true => (0, None),
            false => (
                ::kafka_protocol::ResponseError::TopicAuthorizationFailed.code(),
                Some(TOPIC_AUTHORIZATION_FAILED_MESSAGE.to_string()),
            ),
        };
        assert_eq!(
            outcome, expected,
            "case {case:?}: expected the create of {topic} to be answered {expected:?}, got \
             {outcome:?}"
        );
        // And the refusal registered nothing, while the create that was allowed did.
        let reported = partitions_reported(&mut client, case, topic).await;
        assert_eq!(
            reported.is_some(),
            *allowed,
            "case {case:?}: the cluster's brokers report {topic} as {reported:?}, which the \
             answer contradicts"
        );
    }
}

/// A policy document allowing `actions` on `resources`, in the shape `iam:PutRolePolicy`
/// takes one.
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

/// The ARN a policy statement addresses EVERY topic of this account's clusters by: Amazon
/// MSK scopes a `kafka-cluster:` action to `<kind>/<cluster name>/<cluster uuid>/` plus
/// the resource's own name, so a statement over every topic of the cluster under test is
/// written with a wildcard.
fn every_topic() -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*")
}

/// See [`every_topic`]: the cluster itself, which the `Connect` action is scoped to.
fn every_cluster() -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/*")
}

/// See [`every_topic`]: ONE topic of the account's clusters, by name.
fn one_topic(topic: &str) -> String {
    format!("arn:aws:kafka:{REGION}:{ACCOUNT}:topic/*/{topic}")
}

/// The table the endpoint at `authority` advertises, read the way a client reads it: the
/// first call on a fresh connection, before any authentication the path requires.
async fn advertised_table(case: &str, authority: &str, encrypted: bool) -> Advertised {
    let mut client = match encrypted {
        true => Client::tls(case, authority).await,
        false => Client::plaintext(case, authority).await,
    };
    let advertised = client.api_versions(case).await;
    assert_eq!(
        advertised.error_code, 0,
        "case {case:?}: ApiVersions must be answered, got error {}",
        advertised.error_code
    );
    let table: Advertised = advertised
        .api_keys
        .iter()
        .map(|api| (api.api_key, (api.min_version, api.max_version)))
        .collect();
    assert!(
        !table.is_empty(),
        "case {case:?}: the broker must advertise the APIs it serves"
    );
    table
}

/// Every version of `key` the advertisement lets a client pick, or a failure naming the
/// API the broker did not advertise at all.
fn versions_of(case: &str, advertised: &Advertised, key: ApiKey) -> Vec<i16> {
    let (min, max) = advertised
        .get(&(key as i16))
        .copied()
        .unwrap_or_else(|| panic!("case {case:?}: the broker advertises no {key:?}"));
    (min..=max).collect()
}

/// The newest version of `key` the advertisement names, which is what a client sends when
/// the request itself is only setup for the row under test.
fn newest(advertised: &Advertised, key: ApiKey) -> i16 {
    advertised
        .get(&(key as i16))
        .map(|(_, max)| *max)
        .unwrap_or_default()
}

/// A client on the SASL/IAM path, authenticated by the exchange Amazon MSK's IAM access
/// control defines — the only way a client of a serverless cluster reaches its brokers.
async fn iam_client(case: &str, authority: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    let code = client.authenticate(case, AWS_MSK_IAM, "a-signature").await;
    assert_eq!(
        code, 0,
        "case {case:?}: the SASL/IAM exchange must authenticate the connection, got error {code}"
    );
    client
}

/// `CreateTopics` for one topic at `version`, answering what that topic was told.
///
/// Only the members every advertised version carries are set, since a response field a
/// version does not define is not one the answer can be read for — the oldest version
/// carries no error MESSAGE at all, which reads as the `None` a create that went well
/// carries at every version: what the outcome itself is read from is the cluster's own
/// `Metadata`.
async fn create_topic(
    client: &mut Client,
    case: &str,
    version: i16,
    topic: &str,
    partitions: i32,
) -> TopicOutcome {
    let answered: CreateTopicsResponse = client
        .call(
            case,
            ApiKey::CreateTopics,
            version,
            &CreateTopicsRequest::default()
                .with_timeout_ms(REQUEST_TIMEOUT_MS)
                .with_topics(vec![CreatableTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_num_partitions(partitions)
                    .with_replication_factor(1)]),
        )
        .await;
    answered
        .topics
        .iter()
        .find(|result| result.name.0.as_str() == topic)
        .map(|result| {
            (
                result.error_code,
                result.error_message.as_ref().map(ToString::to_string),
            )
        })
        .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported nothing about {topic}"))
}

/// `CreatePartitions` raising `topic` to `count` partitions at `version`, answering the
/// error code it was given.
async fn grow_topic(
    client: &mut Client,
    case: &str,
    version: i16,
    topic: &str,
    count: i32,
) -> i16 {
    let answered: CreatePartitionsResponse = client
        .call(
            case,
            ApiKey::CreatePartitions,
            version,
            &CreatePartitionsRequest::default()
                .with_timeout_ms(REQUEST_TIMEOUT_MS)
                .with_topics(vec![CreatePartitionsTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_count(count)
                    .with_assignments(None)]),
        )
        .await;
    answered
        .results
        .iter()
        .find(|result| result.name.0.as_str() == topic)
        .map(|result| result.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: CreatePartitions reported nothing about {topic}"))
}

/// `DeleteTopics` for one topic at `version`, answering the error code it was given.
async fn delete_topic(client: &mut Client, case: &str, version: i16, topic: &str) -> i16 {
    let answered: DeleteTopicsResponse = client
        .call(
            case,
            ApiKey::DeleteTopics,
            version,
            &DeleteTopicsRequest::default()
                .with_timeout_ms(REQUEST_TIMEOUT_MS)
                .with_topic_names(vec![TopicName(StrBytes::from_string(topic.to_string()))]),
        )
        .await;
    answered
        .responses
        .iter()
        .find(|result| {
            result
                .name
                .as_ref()
                .is_some_and(|name| name.0.as_str() == topic)
        })
        .map(|result| result.error_code)
        .unwrap_or_else(|| panic!("case {case:?}: DeleteTopics reported nothing about {topic}"))
}

/// How many partitions the cluster's brokers report for `topic`, and `None` for a topic
/// they hold no record of — the reading a client lays its producer out by.
async fn partitions_reported(client: &mut Client, case: &str, topic: &str) -> Option<usize> {
    client
        .metadata(case)
        .await
        .topics
        .iter()
        .find(|held| held.name.as_ref().is_some_and(|name| name.0.as_str() == topic))
        .map(|held| held.partitions.len())
}
