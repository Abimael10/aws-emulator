// SPDX-License-Identifier: CC0-1.0

//! The TOPIC ADMINISTRATION an admin client performs over the published bootstrap
//! address: what it may configure a topic with, and how a topic GROWS.
//!
//! RULE under test: a topic-level configuration entry is not free text. Each key Apache
//! Kafka defines declares a type, and some declare the exact set of strings they admit,
//! so `CreateTopics` reads every entry of a topic before it creates it and refuses THAT
//! topic — and only that topic — with `INVALID_CONFIG` (error 40) and the broker's own
//! `Invalid value <value> for configuration <key>: <why>` sentence, leaving the topic
//! uncreated and absent from a later listing, while the other topics of the same request
//! are created and carry a NULL error message rather than an empty one. And a topic only
//! GROWS: `CreatePartitions` raises its partition count to the total it states, refusing
//! a total that is not above the count the topic holds.
//!
//! The rows span what the rule generalises over: which KIND of value the key declares (a
//! fixed set of strings, a list of them, a 64-bit and a 32-bit integer, a floating-point
//! number, a boolean), whether the value is admitted, where a refused entry sits in a
//! request beside a good one, and how the stated partition total compares with the count
//! the topic holds (above, equal, below, and a topic that does not exist). The keys, the
//! value sets and the type names are Apache Kafka's published topic-level configuration
//! ("Topic-Level Configs"), not this suite's invention.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published.

use ::kafka_protocol::messages::{
    create_partitions_request::CreatePartitionsTopic, create_topics_request::CreatableTopicConfig,
    ApiKey, CreatePartitionsRequest, CreatePartitionsResponse, CreateTopicsRequest,
    CreateTopicsResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;

use crate::broker_data_plane::Client;
use crate::kafka_harness::{ClusterRequest, Emulator};

/// What one `CreateTopics` result states about one topic: its error code and its error
/// message, which is `None` where the topic carries no error at all.
type TopicResult = (i16, Option<String>);

/// One row of the partition-growth table: what it is called, the total it states, the
/// count the topic then holds, and the refusal it is answered with.
type GrowthRow<'a> = (&'a str, i32, i32, Option<(i16, &'a str)>);

/// RULE: a configuration value outside what its key admits refuses that topic with
/// `INVALID_CONFIG` and the broker's own sentence; a value the key admits creates it with
/// no error message at all.
#[tokio::test]
async fn a_topic_configuration_value_is_read_against_what_its_key_admits() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "topic-configs").await;
    let authority = plaintext_address(&emulator, &cluster, "configs").await;
    let mut client = Client::plaintext("configs", &authority).await;
    client.api_versions("configs").await;

    // (the entry, whether the broker admits it, the sentence it is refused with).
    let rows: &[(&str, &str, Option<&str>)] = &[
        (
            "compression.type",
            "brotli",
            Some(
                "Invalid value brotli for configuration compression.type: String must be one \
                 of: uncompressed, zstd, lz4, snappy, gzip, producer",
            ),
        ),
        ("compression.type", "lz4", None),
        ("compression.type", "producer", None),
        // A key the issue's own case does not name, read from the same catalog.
        (
            "message.timestamp.type",
            "WallClock",
            Some(
                "Invalid value WallClock for configuration message.timestamp.type: String \
                 must be one of: CreateTime, LogAppendTime",
            ),
        ),
        ("message.timestamp.type", "LogAppendTime", None),
        (
            "cleanup.policy",
            "archive",
            Some(
                "Invalid value archive for configuration cleanup.policy: String must be one \
                 of: compact, delete",
            ),
        ),
        // A list of admitted values is admitted; one element outside the set is not.
        ("cleanup.policy", "compact,delete", None),
        (
            "retention.ms",
            "a week",
            Some("Invalid value a week for configuration retention.ms: Not a number of type LONG"),
        ),
        ("retention.ms", "-1", None),
        (
            "max.message.bytes",
            "many",
            Some(
                "Invalid value many for configuration max.message.bytes: Not a number of type \
                 INT",
            ),
        ),
        ("max.message.bytes", "1048588", None),
        (
            "min.cleanable.dirty.ratio",
            "half",
            Some(
                "Invalid value half for configuration min.cleanable.dirty.ratio: Not a number \
                 of type DOUBLE",
            ),
        ),
        ("min.cleanable.dirty.ratio", "0.5", None),
        (
            "preallocate",
            "yes",
            Some(
                "Invalid value yes for configuration preallocate: Expected value to be either \
                 true or false",
            ),
        ),
        ("preallocate", "false", None),
        // A key this broker's catalog does not name says nothing about its value, so the
        // topic is created with it.
        ("retention.local.target.bytes", "anything", None),
    ];
    for (index, (key, value, refusal)) in rows.iter().enumerate() {
        let case = format!("{key}={value}");
        let topic = format!("configured-{index}");
        let answered = create_topics(
            &mut client,
            &case,
            &[(topic.as_str(), &[(*key, *value)])],
            false,
        )
        .await;
        let expected: TopicResult = match refusal {
            Some(message) => (ResponseError::InvalidConfig.code(), Some((*message).to_string())),
            None => (0, None),
        };
        assert_eq!(
            answered.get(&topic).cloned(),
            Some(expected.clone()),
            "case {case:?}: expected {expected:?}, got {:?}",
            answered.get(&topic)
        );
        // A refused topic was not created, so the cluster does not hold it; an admitted
        // one was, so it does.
        let held = topics_held(&emulator, &cluster).await;
        assert_eq!(
            held.contains(&topic),
            refusal.is_none(),
            "case {case:?}: the cluster holds {topic}: {}, which the answer contradicts",
            held.contains(&topic)
        );
    }
}

/// RULE: the refusal is PER TOPIC — a request naming one topic the broker will not
/// configure and one it will refuses the first and creates the second, in one answer.
#[tokio::test]
async fn one_request_refuses_the_topic_it_cannot_configure_and_creates_its_neighbour() {
    let case = "one request, one bad codec and one good";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "topic-neighbour").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let answered = create_topics(
        &mut client,
        case,
        &[
            ("bad-codec", &[("retention.ms", "-1"), ("compression.type", "brotli")]),
            ("good-codec", &[("retention.ms", "-1"), ("compression.type", "lz4")]),
        ],
        false,
    )
    .await;
    assert_eq!(
        answered.get("bad-codec").cloned(),
        Some((
            ResponseError::InvalidConfig.code(),
            Some(
                "Invalid value brotli for configuration compression.type: String must be one \
                 of: uncompressed, zstd, lz4, snappy, gzip, producer"
                    .to_string()
            ),
        )),
        "case {case:?}: the topic with the impossible codec must be refused, got {:?}",
        answered.get("bad-codec")
    );
    assert_eq!(
        answered.get("good-codec").cloned(),
        Some((0, None)),
        "case {case:?}: its neighbour must be created with no error message, got {:?}",
        answered.get("good-codec")
    );
    let held = topics_held(&emulator, &cluster).await;
    assert!(
        !held.contains(&"bad-codec".to_string()) && held.contains(&"good-codec".to_string()),
        "case {case:?}: the cluster must hold only the created topic, it holds {held:?}"
    );
}

/// RULE: `CreatePartitions` raises a topic's partition count to the total it states, and
/// refuses a total that is not above the count the topic holds.
#[tokio::test]
async fn create_partitions_only_grows_a_topic() {
    let case = "partition growth";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "topic-growth").await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;
    let answered = create_topics(&mut client, case, &[("grown", &[])], false).await;
    assert_eq!(
        answered.get("grown").cloned(),
        Some((0, None)),
        "case {case:?}: the topic must be created, got {:?}",
        answered.get("grown")
    );

    // (the total stated, the count the topic then holds, the refusal).
    let rows: &[GrowthRow<'_>] = &[
        ("a growth", 4, 4, None),
        ("another growth", 7, 7, None),
        (
            "the count it already holds",
            7,
            7,
            Some((
                ResponseError::InvalidPartitions.code(),
                "Topic already has 7 partitions.",
            )),
        ),
        (
            "fewer than it holds",
            2,
            7,
            Some((
                ResponseError::InvalidPartitions.code(),
                "Topic currently has 7 partitions, which is higher than the requested 2.",
            )),
        ),
    ];
    for (row, stated, held, refusal) in rows {
        let answered = create_partitions(&mut client, row, "grown", *stated).await;
        let expected: TopicResult = match refusal {
            Some((code, message)) => (*code, Some((*message).to_string())),
            None => (0, None),
        };
        assert_eq!(
            answered, expected,
            "case {row:?}: expected {expected:?}, got {answered:?}"
        );
        assert_eq!(
            partitions_of(&mut client, row, "grown").await,
            *held,
            "case {row:?}: the topic must hold {held} partitions"
        );
    }

    // A topic the cluster does not hold is refused by name, which is how this API words
    // a topic it cannot find.
    let absent = create_partitions(&mut client, "a topic that does not exist", "absent", 4).await;
    assert_eq!(
        absent,
        (
            ResponseError::UnknownTopicOrPartition.code(),
            Some("The topic 'absent' does not exist.".to_string())
        ),
        "case {case:?}: a missing topic must be refused by name, got {absent:?}"
    );
}

/// `CreateTopics` for each `(name, configs)` pair, answering what each topic was told.
async fn create_topics(
    client: &mut Client,
    case: &str,
    topics: &[(&str, &[(&str, &str)])],
    validate_only: bool,
) -> std::collections::BTreeMap<String, TopicResult> {
    let answered: CreateTopicsResponse = client
        .call(
            case,
            ApiKey::CreateTopics,
            5,
            &CreateTopicsRequest::default()
                .with_timeout_ms(1_000)
                .with_validate_only(validate_only)
                .with_topics(
                    topics
                        .iter()
                        .map(|(name, configs)| {
                            ::kafka_protocol::messages::create_topics_request::CreatableTopic::default()
                                .with_name(TopicName(StrBytes::from_string((*name).to_string())))
                                .with_num_partitions(1)
                                .with_replication_factor(1)
                                .with_configs(
                                    configs
                                        .iter()
                                        .map(|(key, value)| {
                                            CreatableTopicConfig::default()
                                                .with_name(StrBytes::from_string((*key).to_string()))
                                                .with_value(Some(StrBytes::from_string(
                                                    (*value).to_string(),
                                                )))
                                        })
                                        .collect(),
                                )
                        })
                        .collect(),
                ),
        )
        .await;
    answered
        .topics
        .iter()
        .map(|result| {
            (
                result.name.0.to_string(),
                (
                    result.error_code,
                    result.error_message.as_ref().map(ToString::to_string),
                ),
            )
        })
        .collect()
}

/// `CreatePartitions` for one topic, answering what it was told.
async fn create_partitions(
    client: &mut Client,
    case: &str,
    topic: &str,
    count: i32,
) -> TopicResult {
    let answered: CreatePartitionsResponse = client
        .call(
            case,
            ApiKey::CreatePartitions,
            3,
            &CreatePartitionsRequest::default()
                .with_timeout_ms(1_000)
                .with_validate_only(false)
                .with_topics(vec![CreatePartitionsTopic::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_count(count)
                    .with_assignments(None)]),
        )
        .await;
    let result = answered
        .results
        .first()
        .unwrap_or_else(|| panic!("case {case:?}: CreatePartitions reported no result"));
    (
        result.error_code,
        result.error_message.as_ref().map(ToString::to_string),
    )
}

/// How many partitions the brokers report for `topic`, read out of `Metadata` — the count
/// a client lays its producer out by.
async fn partitions_of(client: &mut Client, case: &str, topic: &str) -> i32 {
    let metadata = client.metadata(case).await;
    let reported = metadata
        .topics
        .iter()
        .find(|held| held.name.as_ref().is_some_and(|name| name.0.as_str() == topic))
        .unwrap_or_else(|| panic!("case {case:?}: Metadata reported no topic {topic}"));
    i32::try_from(reported.partitions.len()).expect("a partition count fits in an i32")
}

/// The topics the cluster holds, as the control plane lists them: the same registry the
/// wire's own administration writes.
async fn topics_held(emulator: &Emulator, cluster: &str) -> Vec<String> {
    let reply = emulator
        .call(
            "GET",
            &format!(
                "/v1/clusters/{}/topics",
                crate::kafka_harness::pct(cluster)
            ),
            None,
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "ListTopics must answer, got {} {}",
        reply.status, reply.body
    );
    reply
        .at("/topics")
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|topic| {
            topic
                .get("topicName")
                .and_then(|name| name.as_str())
                .map(ToString::to_string)
        })
        .collect()
}

/// A cluster whose brokers are reached in the clear, so this suite dials one socket
/// without a credential.
async fn given_plaintext_cluster(emulator: &Emulator, name: &str) -> String {
    emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await
}

/// The address a cluster publishes for its plaintext path, once it publishes one.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    emulator
        .published_bootstrap(cluster, "bootstrapBrokerString", case)
        .await
}
