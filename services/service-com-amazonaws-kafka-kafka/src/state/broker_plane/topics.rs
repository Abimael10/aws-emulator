// SPDX-License-Identifier: CC0-1.0

//! The TOPIC ADMINISTRATION a client performs over the cluster's own endpoint:
//! `CreateTopics` and `DeleteTopics`, the two APIs an admin client manages topics
//! with.
//!
//! A topic an admin client creates here is registered in the SAME registry
//! `CreateTopic` writes and every control-plane reader reports from, and one it
//! deletes leaves it the same way `DeleteTopic` does — so a client that creates a
//! topic over the wire can produce to it at once, read it back out of `Metadata`
//! and `DescribeConfigs`, and see it in the cluster's topic listing; there is one
//! topic registry per cluster, not one per plane. That matters for a cluster whose
//! topics can ONLY be managed this way: Amazon MSK Serverless offers no
//! control-plane topic management, so its clients create every topic with an admin
//! client over the bootstrap address.
//!
//! The per-topic answers are Apache Kafka's: a name the broker will not take is
//! `INVALID_TOPIC_EXCEPTION`, a partition count or replication factor it cannot
//! place is `INVALID_PARTITIONS`/`INVALID_REPLICATION_FACTOR`, a name the cluster
//! already holds is `TOPIC_ALREADY_EXISTS`, and a name it does not hold is
//! `UNKNOWN_TOPIC_OR_PARTITION` — each with the sentence that error class carries,
//! so a client reports what a broker would have told it.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::kafka_protocol::messages::{
    create_topics_request::CreatableTopic, create_topics_response::CreatableTopicResult,
    delete_topics_response::DeletableTopicResult, CreateTopicsRequest, CreateTopicsResponse,
    DeleteTopicsRequest, DeleteTopicsResponse, TopicName,
};
use ::kafka_protocol::protocol::StrBytes;
use ::kafka_protocol::ResponseError;

use super::MAX_PARTITIONS_PER_TOPIC;
use crate::state::{
    topic_name_violation, Cluster, ComAmazonawsKafkaKafkaState, Topic, TopicNameViolation,
    TopicRejected, MAX_TOPICS_IN_REGISTRY, MAX_TOPICS_PER_CLUSTER, MAX_TOPIC_NAME_LENGTH,
};

/// The `NumPartitions`/`ReplicationFactor` a request sends to ask for the broker's
/// own default instead of naming one (Apache Kafka protocol, `CreateTopics`).
const USE_BROKER_DEFAULT: i32 = -1;

/// The partition count such a request is served with when the broker's configuration
/// states none: Apache Kafka's `num.partitions` default, the value this endpoint reports
/// as its broker configuration ([`super::BROKER_CONFIG_DEFAULTS`]).
const DEFAULT_PARTITION_COUNT: i32 = 1;

/// The replication factor such a request is served with when the broker's configuration
/// states none: Apache Kafka's `default.replication.factor` default ("Broker Configs",
/// Apache Kafka documentation).
const DEFAULT_REPLICATION_FACTOR: i32 = 1;

/// The most topic-level configuration entries one created topic may carry, and the
/// most bytes one entry's name or value may. CHOSEN as process-safety bounds, not
/// Apache Kafka settings: the entries come from the request and are stored, so
/// without a ceiling one client would size the record. Sized well above the
/// topic-level configuration keys Apache Kafka documents.
const MAX_TOPIC_CONFIG_ENTRIES: usize = 64;
/// See [`MAX_TOPIC_CONFIG_ENTRIES`].
const MAX_TOPIC_CONFIG_FIELD_BYTES: usize = 1024;

/// Why one topic of the request could not be created or deleted: the Apache Kafka
/// error code the per-topic result carries, and the sentence that error class is
/// reported with.
struct Refusal {
    code: i16,
    message: String,
}

impl Refusal {
    fn new(error: ResponseError, message: impl Into<String>) -> Self {
        Self {
            code: error.code(),
            message: message.into(),
        }
    }
}

/// `CreateTopics`: register each topic the request names on the cluster this endpoint
/// serves, answering one result per topic.
///
/// `withheld` names the topics an ACL binding this cluster's brokers hold keeps the
/// connection's principal from creating; `validate_only` runs every check and
/// registers nothing, which is what that flag means. At most `max_topics` entries are
/// served — the list comes from the request, and each entry is a write to the
/// registry.
pub fn create(
    store: &mut ComAmazonawsKafkaKafkaState,
    region: &str,
    cluster_arn: &str,
    request: &CreateTopicsRequest,
    withheld: &BTreeSet<String>,
    max_topics: usize,
    now: SystemTime,
) -> CreateTopicsResponse {
    // The cluster's own record, read once for the whole request: it is what bounds a
    // topic's replication factor and what every minted topic's identity is derived
    // from.
    let cluster = store.settled_cluster_in(region, cluster_arn, now).cloned();
    // What the BROKER decides for a topic that leaves a decision to it is its own
    // configuration — the MSK configuration revision this cluster runs with, over Apache
    // Kafka's defaults — read once for the whole request.
    let configuration = cluster
        .as_ref()
        .map(|cluster| super::broker_config::in_force(store, cluster, now))
        .unwrap_or_default();
    // A request that names one topic TWICE says two things about one name, so every
    // entry for that name is refused rather than the first one acted on. Read before
    // anything is created, so a request that only asks to be validated is answered
    // the same way.
    let repeated = repeated_names(request.topics.iter().map(|topic| topic.name.0.to_string()));
    let results: Vec<CreatableTopicResult> = request
        .topics
        .iter()
        .take(max_topics)
        .map(|requested| {
            let name = requested.name.0.to_string();
            let created = match cluster.as_ref() {
                Some(_) if repeated.contains(&name) => Err(named_twice()),
                Some(cluster) => create_one(
                    store,
                    &Broker {
                        cluster,
                        configuration: &configuration,
                    },
                    requested,
                    &name,
                    withheld.contains(&name),
                    request.validate_only,
                    now,
                ),
                // The cluster this socket was opened for has left the registry, so
                // there is no broker left to create a topic on.
                None => Err(broker_gone()),
            };
            match created {
                Ok(topic) => CreatableTopicResult::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_num_partitions(topic.partition_count)
                    .with_replication_factor(replication_factor_reported(topic.replication_factor)),
                Err(refusal) => CreatableTopicResult::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_error_code(refusal.code)
                    .with_error_message(Some(StrBytes::from_string(refusal.message))),
            }
        })
        .collect();
    CreateTopicsResponse::default().with_topics(results)
}

/// The broker a topic is created ON: the cluster whose registry the topic joins and the
/// configuration that cluster's brokers run with.
///
/// The two travel together because every decision below reads both: what the cluster can
/// PLACE (its broker nodes) and what its configuration DECIDES (the defaults a request
/// may leave to the broker).
struct Broker<'a> {
    cluster: &'a Cluster,
    configuration: &'a super::broker_config::BrokerConfiguration,
}

/// One topic of a `CreateTopics` request: validated against what the broker can
/// place, then registered unless the request only asked for the validation.
///
/// The record is minted by the SAME constructor an accepted `CreateTopic` mints one
/// with, so a topic created over the wire and one created through the control plane
/// are the same record with the same identity, the same `CREATING` span and the same
/// ceilings.
fn create_one(
    store: &mut ComAmazonawsKafkaKafkaState,
    broker: &Broker<'_>,
    requested: &CreatableTopic,
    name: &str,
    withheld: bool,
    validate_only: bool,
    now: SystemTime,
) -> Result<Topic, Refusal> {
    // An ACL binding that withholds the topic from this principal refuses before
    // anything is validated, the same way it refuses a produce or a fetch.
    if withheld {
        return Err(Refusal::new(
            ResponseError::TopicAuthorizationFailed,
            "Topic authorization failed.",
        ));
    }
    // The ONE reading of what an Apache Kafka topic name may be, shared with the
    // control plane's own create, reported here in the wording Apache Kafka's own
    // brokers answer that error class with.
    if let Some(violation) = topic_name_violation(name) {
        return Err(Refusal::new(
            ResponseError::InvalidTopicException,
            illegal_name_message(name, violation),
        ));
    }
    assignments_place_every_partition(requested, broker.cluster)?;
    let partition_count = partition_count(requested, broker.configuration)?;
    let replication_factor = replication_factor(requested, broker.cluster, broker.configuration)?;
    let configs = topic_configs(requested)?;

    // A name the cluster already reports a topic under — its own, or one a running
    // replicator carries onto it — is taken, whether or not this call would go on to
    // register anything, so the validation and the create agree.
    if store
        .observable_topic_in(&broker.cluster.arn, name, now)
        .is_some()
    {
        return Err(Refusal::new(
            ResponseError::TopicAlreadyExists,
            format!("Topic '{name}' already exists."),
        ));
    }

    let topic = Topic::created(
        broker.cluster,
        name.to_string(),
        partition_count,
        replication_factor,
        configs,
        now,
    );
    if validate_only {
        return Ok(topic);
    }
    let reported = topic.clone();
    store
        .register_topic(topic, now)
        .map(|()| reported)
        .map_err(|rejected| match rejected {
            // Unreachable: the name was resolved above under the lock this call
            // still holds. Reported as the same conflict that lookup reports.
            TopicRejected::DuplicateName => Refusal::new(
                ResponseError::TopicAlreadyExists,
                format!("Topic '{name}' already exists."),
            ),
            TopicRejected::ClusterCeiling => Refusal::new(
                ResponseError::PolicyViolation,
                format!("This cluster holds its maximum of {MAX_TOPICS_PER_CLUSTER} topics."),
            ),
            TopicRejected::RegistryCeiling => Refusal::new(
                ResponseError::PolicyViolation,
                format!("This broker holds its maximum of {MAX_TOPICS_IN_REGISTRY} topics."),
            ),
            TopicRejected::NoSuchCluster => broker_gone(),
        })
}

/// How Apache Kafka's own brokers word the refusal of a topic name that breaks each
/// of the rules `Topic.validate` enforces — the sentences a Kafka client surfaces to
/// its user, which is why this path renders these rather than the control plane's
/// own ([`crate::state::topic_name_error`]).
fn illegal_name_message(name: &str, violation: TopicNameViolation) -> String {
    match violation {
        TopicNameViolation::Empty => "Topic name is illegal, it can't be empty".to_string(),
        TopicNameViolation::Reserved => "Topic name cannot be \".\" or \"..\"".to_string(),
        TopicNameViolation::TooLong(_) => format!(
            "Topic name is illegal, it can't be longer than {MAX_TOPIC_NAME_LENGTH} characters, \
             topic name: {name}"
        ),
        TopicNameViolation::IllegalCharacter(_) => format!(
            "Topic name \"{name}\" is illegal, it contains a character other than ASCII \
             alphanumerics, '.', '_' and '-'"
        ),
    }
}

/// The partitions one requested topic is created with: the count it named, the
/// number of partitions its manual replica assignment lays out, or the broker's own
/// default where it named neither.
///
/// The broker's own default is the `num.partitions` its configuration puts in force —
/// the cluster's MSK configuration revision over Apache Kafka's default — so a topic
/// created without a count gets the count the cluster was configured for.
///
/// A count past [`MAX_PARTITIONS_PER_TOPIC`] — the most partitions of one topic this
/// endpoint reports and serves — is refused rather than stored, so a topic never
/// reports a placement that has no partitions behind it.
fn partition_count(
    requested: &CreatableTopic,
    broker: &super::broker_config::BrokerConfiguration,
) -> Result<i32, Refusal> {
    let count = match requested.assignments.len() {
        // A request that lays its replicas out by hand creates one partition per
        // assignment, which is what the assignment list IS.
        0 => match requested.num_partitions {
            USE_BROKER_DEFAULT => broker
                .number(super::broker_config::NUM_PARTITIONS_CONFIG)
                .unwrap_or(DEFAULT_PARTITION_COUNT),
            named => named,
        },
        assignments => i32::try_from(assignments).unwrap_or(i32::MAX),
    };
    match count {
        count if (1..=MAX_PARTITIONS_PER_TOPIC).contains(&count) => Ok(count),
        // Apache Kafka's own sentence for a count that is no count of partitions at
        // all.
        count if count < 1 => Err(Refusal::new(
            ResponseError::InvalidPartitions,
            "Number of partitions was set to an invalid non-positive value.",
        )),
        // Past what this broker serves: refused with the limit, rather than stored
        // as a placement nothing is behind.
        _ => Err(Refusal::new(
            ResponseError::InvalidPartitions,
            format!(
                "This broker places at most {MAX_PARTITIONS_PER_TOPIC} partitions per topic."
            ),
        )),
    }
}

/// The replication factor one requested topic is created with: the one it named, the
/// number of brokers its first manual assignment places a partition on, or the
/// broker's own default where it named neither.
///
/// Bounded by the cluster's OWN broker count, because Apache Kafka cannot place more
/// replicas of a partition than there are brokers to place them on — the same bound
/// the control plane's create enforces. A cluster whose brokers are not the
/// customer's (an Amazon MSK Serverless cluster reports no broker nodes at all) has
/// no such count, so nothing there bounds what the service places.
fn replication_factor(
    requested: &CreatableTopic,
    cluster: &Cluster,
    broker: &super::broker_config::BrokerConfiguration,
) -> Result<i32, Refusal> {
    let factor = match requested.assignments.first() {
        Some(assignment) => i32::try_from(assignment.broker_ids.len()).unwrap_or(i32::MAX),
        None => match i32::from(requested.replication_factor) {
            USE_BROKER_DEFAULT => broker
                .number(super::broker_config::DEFAULT_REPLICATION_FACTOR_CONFIG)
                .unwrap_or(DEFAULT_REPLICATION_FACTOR),
            named => named,
        },
    };
    if factor < 1 {
        return Err(Refusal::new(
            ResponseError::InvalidReplicationFactor,
            "Replication factor must be larger than 0, or -1 to use the default value.",
        ));
    }
    let brokers = cluster.number_of_broker_nodes;
    if brokers > 0 && factor > brokers {
        return Err(Refusal::new(
            ResponseError::InvalidReplicationFactor,
            format!("Replication factor: {factor} larger than available brokers: {brokers}."),
        ));
    }
    // A cluster whose brokers are the SERVICE's publishes no count to bound the
    // factor by, but a factor this protocol cannot carry is still one no reader of
    // the topic could report: `CreateTopicsResponse` and every later reader state it
    // as an `int16`, so a value past that is refused rather than stored as a number
    // the answer would have to misreport.
    if factor > i32::from(i16::MAX) {
        return Err(Refusal::new(
            ResponseError::InvalidReplicationFactor,
            format!(
                "Replication factor: {factor} larger than the maximum of {} this broker \
                 places.",
                i16::MAX
            ),
        ));
    }
    Ok(factor)
}

/// Whether the manual replica assignment one requested topic carries is one this
/// cluster can place, refused as Apache Kafka refuses each way it cannot be.
///
/// An assignment states, per partition, the brokers that partition's replicas sit on,
/// so a request carrying one states the partition count and the replication factor by
/// stating the placement — which is why naming `NumPartitions` or `ReplicationFactor`
/// beside it is a request that says the same thing twice and may disagree with itself
/// (`INVALID_REQUEST`). Within the assignment, every partition is replicated the same
/// number of times, no partition places two replicas on one broker, and a replica sits
/// on a broker the cluster HAS — the three ways a placement can name something the
/// cluster cannot do (`INVALID_REPLICA_ASSIGNMENT`). A cluster whose brokers are not
/// the customer's (an Amazon MSK Serverless cluster reports no broker nodes at all)
/// publishes no broker ids to place replicas on, so only the first two are read for
/// it.
fn assignments_place_every_partition(
    requested: &CreatableTopic,
    cluster: &Cluster,
) -> Result<(), Refusal> {
    if requested.assignments.is_empty() {
        return Ok(());
    }
    if requested.num_partitions != USE_BROKER_DEFAULT
        || i32::from(requested.replication_factor) != USE_BROKER_DEFAULT
    {
        return Err(Refusal::new(
            ResponseError::InvalidRequest,
            "Both numPartitions or replicationFactor and replicasAssignments were set. Both \
             cannot be used at the same time.",
        ));
    }
    let held: BTreeSet<i32> = cluster.brokers.iter().map(|broker| broker.id).collect();
    let replicas = requested
        .assignments
        .first()
        .map_or(0, |assignment| assignment.broker_ids.len());
    for (index, assignment) in requested.assignments.iter().enumerate() {
        let placed: BTreeSet<i32> = assignment.broker_ids.iter().map(|broker| broker.0).collect();
        let count = assignment.broker_ids.len();
        if count != replicas || count == 0 {
            return Err(Refusal::new(
                ResponseError::InvalidReplicaAssignment,
                format!(
                    "Inconsistent replication factor between partitions, partition 0 has \
                     {replicas} while partition {index} has {count} replicas."
                ),
            ));
        }
        if placed.len() != count {
            return Err(Refusal::new(
                ResponseError::InvalidReplicaAssignment,
                format!(
                    "Duplicate brokers not allowed in replica assignment for partition {index}."
                ),
            ));
        }
        if !held.is_empty() && placed.iter().any(|broker| !held.contains(broker)) {
            return Err(Refusal::new(
                ResponseError::InvalidReplicaAssignment,
                format!("Unknown broker(s) in replica assignment for partition {index}."),
            ));
        }
    }
    Ok(())
}

/// The topic-level configuration one requested topic is stored with, in the form the
/// control plane stores a topic's `Configs` in — so `DescribeConfigs` reports the
/// entries a client set here exactly as it reports the ones a control-plane create
/// set ([`super::submitted_topic_configuration`] reads this form back).
fn topic_configs(requested: &CreatableTopic) -> Result<Option<String>, Refusal> {
    if requested.configs.len() > MAX_TOPIC_CONFIG_ENTRIES {
        return Err(Refusal::new(
            ResponseError::InvalidConfig,
            format!(
                "A topic carries at most {MAX_TOPIC_CONFIG_ENTRIES} configuration entries, but \
                 this one carries {}.",
                requested.configs.len()
            ),
        ));
    }
    let mut entries: BTreeMap<String, String> = BTreeMap::new();
    for config in requested.configs.iter() {
        let name = config.name.to_string();
        let value = config
            .value
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        if name.len() > MAX_TOPIC_CONFIG_FIELD_BYTES || value.len() > MAX_TOPIC_CONFIG_FIELD_BYTES {
            return Err(Refusal::new(
                ResponseError::InvalidConfig,
                format!(
                    "A topic configuration entry's name and value are at most \
                     {MAX_TOPIC_CONFIG_FIELD_BYTES} bytes each."
                ),
            ));
        }
        entries.insert(name, value);
    }
    if entries.is_empty() {
        return Ok(None);
    }
    let object: ::serde_json::Map<String, ::serde_json::Value> = entries
        .into_iter()
        .map(|(name, value)| (name, ::serde_json::Value::String(value)))
        .collect();
    Ok(Some(::aws_smithy_types::base64::encode(
        ::serde_json::Value::Object(object).to_string(),
    )))
}

/// `DeleteTopics`: delete each topic the request names from the cluster this endpoint
/// serves, answering one result per topic.
///
/// A deleted topic leaves exactly as the control plane's own `DeleteTopic` leaves it
/// — `DELETING`, with the instant the clock owes its removal set — so one topic
/// deletion means one thing however it was asked for. At most `max_topics` entries are
/// served, for the reason [`create`] bounds its own list.
pub fn delete(
    store: &mut ComAmazonawsKafkaKafkaState,
    cluster_arn: &str,
    request: &DeleteTopicsRequest,
    withheld: &BTreeSet<String>,
    max_topics: usize,
    now: SystemTime,
) -> DeleteTopicsResponse {
    let named = requested_names(request);
    // The same rule the create follows: a request that names one topic twice is
    // refused for that name rather than acted on once.
    let repeated = repeated_names(named.iter().flatten().cloned());
    let responses: Vec<DeletableTopicResult> = named
        .into_iter()
        .take(max_topics)
        .map(|name| {
            let Some(name) = name else {
                // A topic named by id alone: this endpoint reports no topic ids, so
                // no id it is sent resolves to a topic of this cluster.
                return DeletableTopicResult::default()
                    .with_error_code(ResponseError::UnknownTopicId.code())
                    .with_error_message(Some(StrBytes::from_static_str(
                        "This server does not host this topic ID.",
                    )));
            };
            let refusal = match repeated.contains(&name) {
                true => Some(named_twice()),
                false => match withheld.contains(&name) {
                    true => Some(Refusal::new(
                        ResponseError::TopicAuthorizationFailed,
                        "Topic authorization failed.",
                    )),
                    false => match store.begin_topic_deletion(cluster_arn, &name, now) {
                        Some(_) => None,
                        None => Some(Refusal::new(
                            ResponseError::UnknownTopicOrPartition,
                            "This server does not host this topic-partition.",
                        )),
                    },
                },
            };
            let result = DeletableTopicResult::default()
                .with_name(Some(TopicName(StrBytes::from_string(name))));
            match refusal {
                None => result,
                Some(refusal) => result
                    .with_error_code(refusal.code)
                    .with_error_message(Some(StrBytes::from_string(refusal.message))),
            }
        })
        .collect();
    DeleteTopicsResponse::default().with_responses(responses)
}

/// The topics one `DeleteTopics` request names, in both forms this API states them:
/// the topic-name list the older versions carry, and the per-topic entries the later
/// ones do — where a topic may be named by id instead, which this endpoint resolves
/// nothing from (`None`).
fn requested_names(request: &DeleteTopicsRequest) -> Vec<Option<String>> {
    if !request.topics.is_empty() {
        return request
            .topics
            .iter()
            .map(|topic| topic.name.as_ref().map(|name| name.0.to_string()))
            .collect();
    }
    request
        .topic_names
        .iter()
        .map(|name| Some(name.0.to_string()))
        .collect()
}

/// The names of `named` that appear more than once — the entries a request states
/// twice, which Apache Kafka refuses rather than reading one of them.
fn repeated_names(named: impl Iterator<Item = String>) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut repeated: BTreeSet<String> = BTreeSet::new();
    for name in named {
        if !seen.insert(name.clone()) {
            repeated.insert(name);
        }
    }
    repeated
}

/// What an entry whose name the same request names again is answered: Apache Kafka's
/// own refusal of a request that states one topic twice.
fn named_twice() -> Refusal {
    Refusal::new(
        ResponseError::InvalidRequest,
        "Found multiple entries for this topic.",
    )
}

/// What a request is answered when the cluster this socket was opened for is no
/// longer in the registry: the broker it names is not there to serve it.
fn broker_gone() -> Refusal {
    Refusal::new(
        ResponseError::BrokerNotAvailable,
        "The broker is not available.",
    )
}

/// The replication factor a result reports, in the `int16` this API carries it in.
fn replication_factor_reported(factor: i32) -> i16 {
    i16::try_from(factor).unwrap_or(i16::MAX)
}
