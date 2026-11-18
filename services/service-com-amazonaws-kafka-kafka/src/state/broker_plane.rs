// SPDX-License-Identifier: CC0-1.0

//! The cluster's DATA plane: the address a Kafka client reaches its brokers at, and
//! the log those brokers keep.
//!
//! `GetBootstrapBrokers` announces where a client's producer, consumer and admin
//! client connect, and the contract of every string it answers is that the
//! `host:port` in it is an address THIS emulator accepts an Apache Kafka wire
//! connection on. So the address is a socket the core owns
//! ([`::emulator_server::Listeners::tcp_plaintext`], bound on the emulator's own
//! interface — loopback unless the operator says otherwise — with the core owning the
//! port, the connection caps and their lifetime), and this module holds the handle its
//! lifetime hangs off ([`ClusterEndpoint`]) together with the state the protocol
//! answers from.
//!
//! ONE socket per access path a cluster enables. A Kafka
//! request frame begins with its length, so its first byte is never the `0x16` a TLS
//! record opens with: the connection's first byte says whether the client dialled the
//! encrypted half or the plaintext one ([`::service_common::in_band_tls`]), which is
//! how one announced address serves the plaintext, TLS and SASL ports MSK publishes
//! separately. What a connection may do once it is open is the cluster's own
//! client-authentication configuration ([`wire`] and [`sasl`]) and, for a connection
//! authenticated with IAM, the policies of the principal that signed it
//! ([`iam_access`]).
//!
//! The log itself is this state's: a produced record is kept here and handed back to a
//! fetch from the offset it was written at, and a record older than the topic's
//! `retention.ms` is dropped — the clock owes that removal at the instant the record
//! aged out, whether or not anybody fetched ([`PartitionLog::expire`]).

use std::collections::{BTreeMap, VecDeque};

use super::*;

pub mod acls;
pub mod broker_config;
pub mod groups;
pub mod iam_access;
pub mod sasl;
pub mod topics;
pub mod wire;

/// The broker node id a cluster with no broker nodes of its own reports itself under in
/// `Metadata`: an Amazon MSK Serverless cluster publishes no brokers, so the one endpoint
/// its clients reach is one broker to them. It is also the node a group's coordinator is
/// reported as.
pub const BROKER_NODE_ID: i32 = 1;

/// The most bytes one request frame on a broker connection may carry. Apache Kafka's
/// own `socket.request.max.bytes` default (100 MiB) is far above what an emulator
/// needs to buffer per connection, so this is the ceiling the handler enforces on a
/// frame before it reads one: a client cannot make the process hold more than this per
/// connection.
pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

/// The most records one partition's log keeps, and the most BYTES of record key and
/// value it retains. CHOSEN as process-safety bounds, not Apache Kafka settings: the
/// log is in memory, so without a ceiling on both a producer would size it — a count
/// alone leaves the product of the count and the record size unbounded. The oldest
/// record is dropped first until both hold, exactly as a retention sweep drops it, so
/// the log stays the newest window of the partition.
pub const MAX_RECORDS_PER_PARTITION: usize = 4096;
/// See [`MAX_RECORDS_PER_PARTITION`].
pub const MAX_LOG_BYTES_PER_PARTITION: usize = 8 * 1024 * 1024;

/// The most bytes one produced record's key and value may carry together: Apache
/// Kafka's own `max.message.bytes` topic default (1048588), so a record this endpoint
/// refuses is one a broker with the default configuration refuses too.
pub const MAX_RECORD_BYTES: usize = 1_048_588;

/// Apache Kafka's documented topic-level configuration defaults, as
/// `DescribeConfigs` reports them for a topic whose `Configs` does not set them
/// ("Topic-Level Configs", Apache Kafka documentation). A value the topic sets
/// overrides the entry here, and an entry a client did not ask about is not reported.
pub const TOPIC_CONFIG_DEFAULTS: &[(&str, &str)] = &[
    ("cleanup.policy", "delete"),
    ("compression.type", "producer"),
    ("delete.retention.ms", "86400000"),
    ("file.delete.delay.ms", "60000"),
    ("flush.messages", "9223372036854775807"),
    ("max.message.bytes", "1048588"),
    ("message.timestamp.type", "CreateTime"),
    ("min.insync.replicas", "1"),
    ("retention.bytes", "-1"),
    ("retention.ms", "604800000"),
    ("segment.bytes", "1073741824"),
    ("segment.ms", "604800000"),
];

/// The broker-level configuration this endpoint reports for its own node, as
/// `DescribeConfigs` on a `BROKER` resource answers it: Apache Kafka's documented
/// default for the retention sweep's period ("log.retention.check.interval.ms",
/// Apache Kafka broker configs) and the ones a client reads beside it. Every value is
/// either that documented default or a fact about this endpoint.
pub const BROKER_CONFIG_DEFAULTS: &[(&str, &str)] = &[
    ("log.retention.check.interval.ms", "300000"),
    ("log.retention.hours", "168"),
    ("log.segment.bytes", "1073741824"),
    ("message.max.bytes", "1048588"),
    ("min.insync.replicas", "1"),
    ("num.partitions", "1"),
];

/// The topic configuration key that says how long a record is kept.
pub const RETENTION_MS_CONFIG: &str = "retention.ms";

/// The `AclOperation` a produce needs on a topic, and the one a fetch needs (Apache
/// Kafka's `AclOperation` enum).
pub const OPERATION_WRITE: i8 = 4;
/// See [`OPERATION_WRITE`].
pub const OPERATION_READ: i8 = 3;
/// The `AclOperation` creating a topic needs, and the one deleting it needs (the same
/// enum).
pub const OPERATION_CREATE: i8 = 5;
/// See [`OPERATION_CREATE`].
pub const OPERATION_DELETE: i8 = 6;

/// The most partitions of one topic this endpoint reports and serves.
///
/// CHOSEN as a process-safety bound, not an Apache Kafka or Amazon MSK limit:
/// `CreateTopic`'s `PartitionCount` is modelled `__integerMin1` with no upper bound, and
/// the metadata a client reads renders one entry per partition, so without a ceiling one
/// topic would size every `Metadata` answer about it. Sized far above the partition
/// counts a cluster of this emulator is driven with.
pub const MAX_PARTITIONS_PER_TOPIC: i32 = 1024;

/// One record a `Produce` submitted, as the log appends it: the producer's timestamp
/// (negative when it set none, so the broker stamps the record itself), the record key
/// and the record value.
pub type SubmittedRecord = (i64, Option<Vec<u8>>, Option<Vec<u8>>);

/// One record in a partition's log, as a fetch hands it back.
#[derive(Debug, Clone)]
pub struct StoredRecord {
    /// The offset this record was written at; a fetch reads from an offset.
    pub offset: i64,
    /// The record's timestamp in epoch milliseconds — the producer's when it sent one,
    /// otherwise the instant the broker appended it. What retention ages out.
    pub timestamp_ms: i64,
    /// The record key, absent for a record produced without one.
    pub key: Option<Vec<u8>>,
    /// The record value, absent for a tombstone.
    pub value: Option<Vec<u8>>,
}

/// One partition's log: the records it holds, where the log starts and the offset the
/// next produced record takes.
#[derive(Debug, Default)]
pub struct PartitionLog {
    /// The records held, in offset order.
    pub records: VecDeque<StoredRecord>,
    /// The offset the next appended record takes — the partition's high watermark.
    pub next_offset: i64,
    /// The first offset a fetch can still read: the offset of the oldest record the
    /// log holds, which rises as records age out.
    pub log_start_offset: i64,
    /// The bytes of record key and value the log currently retains, kept with the
    /// records so the byte ceiling costs no walk of the log.
    retained_bytes: usize,
}

impl PartitionLog {
    /// Append one record, handing back the offset it took.
    ///
    /// Bounded by [`MAX_RECORDS_PER_PARTITION`] and
    /// [`MAX_LOG_BYTES_PER_PARTITION`]: the oldest records go until both hold, which
    /// raises the log's start offset exactly as a retention sweep does.
    pub fn append(&mut self, timestamp_ms: i64, key: Option<Vec<u8>>, value: Option<Vec<u8>>) -> i64 {
        let offset = self.next_offset;
        self.retained_bytes += record_bytes(key.as_ref(), value.as_ref());
        self.records.push_back(StoredRecord {
            offset,
            timestamp_ms,
            key,
            value,
        });
        self.next_offset += 1;
        while self.records.len() > MAX_RECORDS_PER_PARTITION
            || (self.retained_bytes > MAX_LOG_BYTES_PER_PARTITION && self.records.len() > 1)
        {
            self.drop_oldest();
        }
        self.restate_log_start();
        offset
    }

    /// Drop the oldest record the log holds, keeping the retained byte count with it.
    fn drop_oldest(&mut self) {
        if let Some(dropped) = self.records.pop_front() {
            self.retained_bytes = self
                .retained_bytes
                .saturating_sub(record_bytes(dropped.key.as_ref(), dropped.value.as_ref()));
        }
    }

    /// The first offset a fetch can still read, after records have left the log.
    fn restate_log_start(&mut self) {
        self.log_start_offset = self
            .records
            .front()
            .map(|record| record.offset)
            .unwrap_or(self.next_offset);
    }

    /// Drop every record the topic's retention has aged out at `now_ms`: one older
    /// than `retention_ms` milliseconds. `None` — or a negative value, which Apache
    /// Kafka documents as "no limit" — keeps every record.
    ///
    /// The removal was owed at the instant each record aged out, whether or not
    /// anybody fetched, so materialising it on a read reports the log the clock
    /// already left behind rather than changing what a later caller sees.
    pub fn expire(&mut self, now_ms: i64, retention_ms: Option<i64>) {
        let Some(retention_ms) = retention_ms.filter(|retention| *retention >= 0) else {
            return;
        };
        let cutoff = now_ms.saturating_sub(retention_ms);
        while self
            .records
            .front()
            .is_some_and(|record| record.timestamp_ms < cutoff)
        {
            self.drop_oldest();
        }
        self.restate_log_start();
    }
}

/// The bytes one record's key and value retain.
fn record_bytes(key: Option<&Vec<u8>>, value: Option<&Vec<u8>>) -> usize {
    key.map_or(0, Vec::len) + value.map_or(0, Vec::len)
}

/// One ACCESS PATH a cluster's brokers are reached over: the four a client picks
/// between, which Amazon MSK publishes on four ports
/// ([`super::PORT_PLAINTEXT`], [`super::PORT_TLS`], [`super::PORT_SASL_SCRAM`] and
/// [`super::PORT_SASL_IAM`], with the public and multi-VPC siblings beside them).
///
/// Each path is served on a socket of its own, because what a path requires of a
/// client differs: the plaintext path encrypts nothing, the TLS path encrypts and
/// authenticates no client, and each SASL path requires the exchange its mechanism
/// defines before anything else is served. A path's public and multi-VPC variants are
/// the same brokers reached over a different network, so they share the path's socket.
/// The PORT a client dials is the core's, not MSK's: the core owns the socket and
/// assigns its port, and the control plane publishes the one it assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AccessPath {
    /// Unencrypted, unauthenticated access.
    Plaintext,
    /// TLS, with no SASL exchange.
    Tls,
    /// TLS plus SASL/SCRAM.
    SaslScram,
    /// TLS plus Amazon MSK's IAM access control exchange.
    SaslIam,
}

impl AccessPath {
    /// The SASL mechanisms a client of this path authenticates with — none for a path
    /// that authenticates no client, and for the IAM path both the exchange a Java
    /// client speaks and the `OAUTHBEARER` one every other client does.
    #[must_use]
    pub fn mechanisms(self) -> &'static [sasl::Mechanism] {
        match self {
            Self::Plaintext | Self::Tls => &[],
            Self::SaslScram => &[sasl::Mechanism::ScramSha512],
            Self::SaslIam => &[sasl::Mechanism::AwsMskIam, sasl::Mechanism::OAuthBearer],
        }
    }
}

/// The socket one access path of one cluster is served on, and the address it landed
/// on.
///
/// The store OWNS the listener: dropping it closes the port and severs every
/// connection on it, so it must outlive every bootstrap string handed out — which is
/// as long as the cluster is in the registry.
pub struct ClusterEndpoint {
    /// `host:port`, the authority the bootstrap strings of this path carry.
    authority: String,
    /// Held only to keep the port open; the core owns the socket and its lifetime.
    _listener: ::emulator_server::ResourceListener,
}

/// Written by hand rather than derived: a [`::emulator_server::ResourceListener`] is a
/// live socket and carries no `Debug`. What is diagnosable about the endpoint is the
/// address it announced.
impl std::fmt::Debug for ClusterEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterEndpoint")
            .field("authority", &self.authority)
            .finish()
    }
}

impl ClusterEndpoint {
    /// The `host:port` a client dials this path at.
    #[must_use]
    pub fn authority(&self) -> &str {
        &self.authority
    }
}

/// The `host:port` the cluster `cluster_arn` names answers `path` connections on,
/// opening the socket on first use.
///
/// `None` when this service was mounted on an engine with no listener surface or no
/// store, when the store holds no cluster under that ARN in this Region, when this
/// emulator already holds [`MAX_CLUSTER_ENDPOINTS`] endpoints, or when the core could not
/// bind — the caller then answers with a modelled error rather than announcing an address
/// nothing serves. A socket is opened only for a cluster the store HOLDS, so no request
/// can make the process bind one for an ARN it invented.
///
/// Idempotent: the first socket opened for a cluster's path is kept, so every later
/// `GetBootstrapBrokers` answers the same address and a client may key a connection
/// on it. It is released when the cluster leaves the registry
/// ([`ComAmazonawsKafkaKafkaState::unregister_cluster`]), which closes the port.
pub async fn ensure_cluster_endpoint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_arn: &str,
    path: AccessPath,
) -> Option<String> {
    let state = super::service_state(engine)?;
    let key = (cluster_arn.to_string(), path);
    // Everything read off the store is read under ONE guard, taken and dropped before
    // the awaits below: a lock held across an await would make this future non-`Send`.
    let held = {
        let store = state.lock();
        if let Some(endpoint) = store.cluster_endpoints.get(&key) {
            return Some(endpoint.authority().to_string());
        }
        // A socket is bound for a cluster this endpoint HOLDS, and for no other ARN: the
        // key comes from the request, so binding on a name nothing resolves would let one
        // caller grow the process's port and descriptor use without end.
        store.cluster_in(region, cluster_arn).is_some()
            && store.cluster_endpoints.len() < MAX_CLUSTER_ENDPOINTS
    };
    if !held {
        return None;
    }

    let listeners = (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()?
        .listeners();
    let listener = listeners
        .tcp_plaintext(wire::ClusterBroker::new(
            cluster_arn.to_string(),
            region.to_string(),
            path,
            state.clone(),
            // The core's own TLS server, for a client that dialled one of the
            // encrypted paths. A core that cannot prepare one leaves this `None`, and
            // such a client is then dropped at its first byte rather than answered in
            // the clear.
            listeners.tls_upgrader().ok(),
            ctx.callback().clone(),
            // Where a connection's IAM permissions are read from: the stores
            // themselves, so a policy written after this socket was opened is in force
            // on the next connection ([`iam_access`]).
            iam_access::PolicySource::of_engine(engine),
        ))
        .await
        .ok()?;
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);

    let mut store = state.lock();
    // Another call may have opened one while this was binding; keep the first, so an
    // announced address never moves under a client. Dropping the loser closes its port.
    if let Some(held) = store.cluster_endpoints.get(&key) {
        return Some(held.authority().to_string());
    }
    // The ceiling is re-read under the write lock, so concurrent calls cannot carry the
    // registry past it between the check above and this insert.
    if store.cluster_endpoints.len() >= MAX_CLUSTER_ENDPOINTS {
        return None;
    }
    let endpoint = ClusterEndpoint {
        authority: authority.clone(),
        _listener: listener,
    };
    store.cluster_endpoints.insert(key, endpoint);
    ::tracing::info!(
        %cluster_arn,
        %authority,
        ?path,
        "opened a cluster's Apache Kafka endpoint"
    );
    Some(authority)
}

/// The most Apache Kafka endpoints this emulator holds open at once, across every
/// cluster and access path.
///
/// CHOSEN as a process-safety bound, not an Amazon MSK limit: each endpoint owns a
/// listening socket and its file descriptor for as long as its cluster is in the
/// registry, and how many clusters a caller creates is the caller's choice. Sized well
/// above the four paths of the clusters an emulator is driven with
/// ([`super::MAX_CLUSTERS_PER_REGION`] clusters times four paths would be the ceiling of
/// a run that bootstrapped every path of every cluster).
pub const MAX_CLUSTER_ENDPOINTS: usize = 512;

impl ComAmazonawsKafkaKafkaState {
    /// The broker node ids the cluster `cluster_arn` reports to a client, in id order:
    /// its own broker nodes, and the one node a cluster that publishes none stands
    /// behind ([`BROKER_NODE_ID`]).
    pub fn broker_node_ids(&self, region: &str, cluster_arn: &str) -> Vec<i32> {
        let mut held: Vec<i32> = self
            .cluster_in(region, cluster_arn)
            .map(|cluster| cluster.brokers.iter().map(|broker| broker.id).collect())
            .unwrap_or_default();
        held.sort_unstable();
        match held.is_empty() {
            true => vec![BROKER_NODE_ID],
            false => held,
        }
    }
}

/// A topic's configuration as the data plane reads it: the entries the topic SETS, and
/// the effective values those entries make over Apache Kafka's documented defaults.
///
/// Both halves, because a client that asks a broker for a topic's configuration is
/// told which values the topic carries and which are the broker's defaults.
#[derive(Debug, Clone, Default)]
pub struct DataPlaneTopicConfiguration {
    /// Every entry in force, whether the topic set it or it is a default.
    pub effective: BTreeMap<String, String>,
    /// Only the entries the topic's own `Configs` set.
    pub set: BTreeMap<String, String>,
}

/// The topic-level configuration a topic runs with: the entries its `Configs` sets,
/// over Apache Kafka's documented defaults ([`TOPIC_CONFIG_DEFAULTS`]).
///
/// `Configs` is "a Base64 topic configuration string" (`DescribeTopic$Configs`), and
/// what that string carries is a topic configuration: a JSON object of entries, or the
/// `name=value` lines a Kafka configuration file is written in. Both are read, so a
/// client's configuration is reported back whichever form it sent.
pub fn topic_configuration(configs: Option<&str>) -> DataPlaneTopicConfiguration {
    let set = submitted_topic_configuration(configs);
    let mut effective: BTreeMap<String, String> = TOPIC_CONFIG_DEFAULTS
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    effective.extend(set.clone());
    DataPlaneTopicConfiguration { effective, set }
}

/// Only the entries a topic's `Configs` SET, with no default under them — what a
/// reader that has to tell a configured value from a default reads.
pub fn submitted_topic_configuration(configs: Option<&str>) -> BTreeMap<String, String> {
    let Some(encoded) = configs else {
        return BTreeMap::new();
    };
    let Ok(decoded) = ::aws_smithy_types::base64::decode(encoded) else {
        return BTreeMap::new();
    };
    // A JSON object of entries, the form a client that sets topic configuration
    // through this API sends.
    if let Ok(::serde_json::Value::Object(object)) =
        ::serde_json::from_slice::<::serde_json::Value>(&decoded)
    {
        return object
            .into_iter()
            .map(|(name, value)| {
                let rendered = match value {
                    ::serde_json::Value::String(text) => text,
                    other => other.to_string(),
                };
                (name, rendered)
            })
            .collect();
    }
    // Otherwise the `name=value` lines of a Kafka configuration file, read by the
    // same reader this service reads a configuration's `server.properties` with.
    ::java_properties::read(decoded.as_slice())
        .map(|read| read.into_iter().collect())
        .unwrap_or_default()
}

/// How long the topic keeps a record, from its configuration: `retention.ms` as a
/// number of milliseconds. `None` when the configuration does not set a readable
/// one, and a negative value is Apache Kafka's "no limit".
pub fn retention_ms(configuration: &BTreeMap<String, String>) -> Option<i64> {
    configuration
        .get(RETENTION_MS_CONFIG)
        .and_then(|value| value.trim().parse::<i64>().ok())
}

/// Epoch milliseconds at `now`, the unit a Kafka record's timestamp is in.
pub fn epoch_millis(now: SystemTime) -> i64 {
    now.duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// What a partition read or write could not be served for: the Apache Kafka error
/// code the response carries for it.
///
/// The codes are the protocol's own
/// ([`::kafka_protocol::ResponseError`]): a client branches on them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionRefusal {
    /// The cluster holds no such topic, or the topic has no such partition:
    /// `UNKNOWN_TOPIC_OR_PARTITION`.
    UnknownTopicOrPartition,
    /// The record is larger than the topic's `max.message.bytes`:
    /// `MESSAGE_TOO_LARGE`.
    MessageTooLarge,
    /// The fetch asked for an offset outside the log: `OFFSET_OUT_OF_RANGE`.
    OffsetOutOfRange,
}

impl PartitionRefusal {
    /// The protocol error code a response reports this refusal under.
    #[must_use]
    pub fn code(self) -> i16 {
        match self {
            Self::UnknownTopicOrPartition => {
                ::kafka_protocol::ResponseError::UnknownTopicOrPartition.code()
            }
            Self::MessageTooLarge => ::kafka_protocol::ResponseError::MessageTooLarge.code(),
            Self::OffsetOutOfRange => ::kafka_protocol::ResponseError::OffsetOutOfRange.code(),
        }
    }
}

/// One topic of a cluster as the data plane reports it: its name, the partitions this
/// endpoint serves for it, and how many brokers each of those partitions is placed on.
#[derive(Debug, Clone)]
pub struct DataPlaneTopic {
    /// The topic's name.
    pub name: String,
    /// How many partitions it has.
    pub partitions: i32,
    /// How many replicas each partition is placed on: the topic's replication factor,
    /// which is what a client reads back as the partition's replica set.
    pub replication_factor: i32,
}

/// What a fetch of one partition answers: the records it read and where the log
/// begins and ends.
#[derive(Debug, Clone)]
pub struct FetchedPartition {
    /// The records from the requested offset on, oldest first.
    pub records: Vec<StoredRecord>,
    /// The offset the next produced record will take, which is the partition's high
    /// watermark.
    pub high_watermark: i64,
    /// The first offset still readable.
    pub log_start_offset: i64,
}

impl ComAmazonawsKafkaKafkaState {
    /// The topic `topic` of the cluster `cluster_arn`, as a reader at `now` sees it —
    /// the one derivation every topic reader of this service answers from, so the data
    /// plane and `DescribeTopic` cannot disagree about which topics a cluster has.
    fn data_plane_topic(&mut self, cluster_arn: &str, topic: &str, now: SystemTime) -> Option<Topic> {
        self.observable_topic_in(cluster_arn, topic, now)
    }

    /// The topics of the cluster `cluster_arn` a client's `Metadata` reports, each with
    /// the number of partitions this endpoint serves for it — the topic's own count,
    /// bounded by [`MAX_PARTITIONS_PER_TOPIC`] so one topic cannot size the answer — and
    /// the number of replicas each of those partitions is placed on, which is the
    /// topic's own replication factor.
    pub fn data_plane_topics(
        &mut self,
        cluster_arn: &str,
        now: SystemTime,
    ) -> Vec<DataPlaneTopic> {
        self.observable_topics_in(cluster_arn, now)
            .into_iter()
            .map(|(name, topic)| DataPlaneTopic {
                name,
                partitions: topic.partition_count.clamp(1, MAX_PARTITIONS_PER_TOPIC),
                replication_factor: topic.replication_factor.max(1),
            })
            .collect()
    }

    /// The configuration the topic `topic` runs with, over Apache Kafka's defaults, or
    /// `None` when the cluster holds no such topic.
    pub fn data_plane_topic_configuration(
        &mut self,
        cluster_arn: &str,
        topic: &str,
        now: SystemTime,
    ) -> Option<DataPlaneTopicConfiguration> {
        let topic = self.data_plane_topic(cluster_arn, topic, now)?;
        Some(topic_configuration(topic.configs.as_deref()))
    }

    /// Append `records` to one partition of one topic, handing back the offset the
    /// first of them took.
    ///
    /// Refused for a topic the cluster does not hold, a partition the topic does not
    /// have, and a record past [`MAX_RECORD_BYTES`] — the same conditions a broker
    /// refuses a produce for.
    pub fn data_plane_produce(
        &mut self,
        cluster_arn: &str,
        topic: &str,
        partition: i32,
        records: Vec<SubmittedRecord>,
        now: SystemTime,
    ) -> Result<i64, PartitionRefusal> {
        let held = self
            .data_plane_topic(cluster_arn, topic, now)
            .ok_or(PartitionRefusal::UnknownTopicOrPartition)?;
        if partition < 0 || partition >= held.partition_count.clamp(1, MAX_PARTITIONS_PER_TOPIC) {
            return Err(PartitionRefusal::UnknownTopicOrPartition);
        }
        for (_, key, value) in &records {
            let size = key.as_ref().map_or(0, Vec::len) + value.as_ref().map_or(0, Vec::len);
            if size > MAX_RECORD_BYTES {
                return Err(PartitionRefusal::MessageTooLarge);
            }
        }
        let now_ms = epoch_millis(now);
        let log = self
            .partition_logs
            .entry((cluster_arn.to_string(), topic.to_string(), partition))
            .or_default();
        let base = log.next_offset;
        for (timestamp_ms, key, value) in records {
            let stamped = match timestamp_ms >= 0 {
                true => timestamp_ms,
                // A producer that sends no timestamp has the broker stamp the record
                // with the instant it was appended, which is what `CreateTime`'s
                // fallback (`LogAppendTime`) is.
                false => now_ms,
            };
            log.append(stamped, key, value);
        }
        Ok(base)
    }

    /// Read one partition from `fetch_offset` on, with the records the topic's
    /// retention has aged out already gone.
    pub fn data_plane_fetch(
        &mut self,
        cluster_arn: &str,
        topic: &str,
        partition: i32,
        fetch_offset: i64,
        max_records: usize,
        now: SystemTime,
    ) -> Result<FetchedPartition, PartitionRefusal> {
        let configuration = self
            .data_plane_topic_configuration(cluster_arn, topic, now)
            .ok_or(PartitionRefusal::UnknownTopicOrPartition)?
            .effective;
        let held = self
            .data_plane_topic(cluster_arn, topic, now)
            .ok_or(PartitionRefusal::UnknownTopicOrPartition)?;
        if partition < 0 || partition >= held.partition_count.clamp(1, MAX_PARTITIONS_PER_TOPIC) {
            return Err(PartitionRefusal::UnknownTopicOrPartition);
        }
        let now_ms = epoch_millis(now);
        let retention = retention_ms(&configuration);
        // A partition nothing has been produced to has no log to read or to sweep, and
        // reading it writes none: it is empty, which is what a fetch at its first
        // offset is answered.
        let Some(log) = self
            .partition_logs
            .get_mut(&(cluster_arn.to_string(), topic.to_string(), partition))
        else {
            return Ok(FetchedPartition {
                records: Vec::new(),
                high_watermark: 0,
                log_start_offset: 0,
            });
        };
        log.expire(now_ms, retention);
        if fetch_offset > log.next_offset {
            return Err(PartitionRefusal::OffsetOutOfRange);
        }
        let records = log
            .records
            .iter()
            .filter(|record| record.offset >= fetch_offset)
            .take(max_records)
            .cloned()
            .collect();
        Ok(FetchedPartition {
            records,
            high_watermark: log.next_offset,
            log_start_offset: log.log_start_offset,
        })
    }

    /// Where one partition's log begins and ends at `now`, with aged-out records
    /// already gone: `(log start offset, high watermark)`.
    pub fn data_plane_offsets(
        &mut self,
        cluster_arn: &str,
        topic: &str,
        partition: i32,
        now: SystemTime,
    ) -> Result<(i64, i64), PartitionRefusal> {
        let fetched = self.data_plane_fetch(cluster_arn, topic, partition, 0, 0, now)?;
        Ok((fetched.log_start_offset, fetched.high_watermark))
    }
}
