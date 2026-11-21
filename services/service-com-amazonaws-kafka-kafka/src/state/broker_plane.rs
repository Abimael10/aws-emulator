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
//! ONE socket per ACCESS METHOD a cluster enables, shared by every BROKER NODE of the
//! cluster: Amazon MSK publishes one port per access method, and the nodes behind it are
//! told apart by their own hosts ([`addressing`]) — so the bootstrap string of an
//! N-broker cluster carries N `host:port` pairs, one per node, every one of them on that
//! method's single port, and the `Metadata` answer reports each node at its own host. A
//! Kafka request frame begins with its length, so its first byte is never the `0x16` a TLS
//! record opens with: the connection's first byte says whether the client dialled the
//! encrypted half or the plaintext one ([`::service_common::in_band_tls`]), which is how
//! one socket serves a method whichever of them its scheme is. What a connection may do
//! once it is open is the cluster's own client-authentication configuration ([`wire`] and
//! [`sasl`]) and, for a connection authenticated with IAM, the policies of the principal
//! that signed it ([`iam_access`]).
//!
//! That same broker answers at BOTH ends of the announced address: the socket the core
//! accepts a client's connections on, and the connections a client running in THIS process
//! opens to the address the control plane published
//! ([`::service_common::dialable_data_planes`]). "Amazon MSK brokers are only accessible from
//! within the cluster's VPC", so the client that reads a bootstrap string and dials it is
//! often code this emulator itself runs — the code of a Lambda function attached to that VPC,
//! which opens no host socket — and an address the control plane publishes has to be one such
//! a client can reach.
//!
//! The log itself is this state's: a produced record is kept here and handed back to a
//! fetch from the offset it was written at, and a record older than the topic's
//! `retention.ms` is dropped — the clock owes that removal at the instant the record
//! aged out, whether or not anybody fetched ([`PartitionLog::expire`]).

use std::collections::{BTreeMap, VecDeque};

use super::*;

pub mod acls;
pub mod addressing;
pub mod broker_config;
pub mod codecs;
pub mod groups;
pub mod iam_access;
pub mod records;
pub mod sasl;
pub mod topic_config;
pub mod topics;
pub mod wire;

/// The broker node id a cluster with no broker nodes of its own reports itself under in
/// `Metadata`: an Amazon MSK Serverless cluster publishes no brokers, so the one endpoint
/// its clients reach is one broker to them. It is also the node a group's coordinator is
/// reported as.
pub const BROKER_NODE_ID: i32 = 1;

/// The service that opened the endpoint's socket, as the register of the planes this
/// emulator serves in process names it: this service's own SigV4 signing name
/// (`sigv4Name` `kafka`, `smithy_query(query="service_info")`), which is what a diagnosis
/// of a connection to the published address has to say.
const SIGNING_NAME: &str = "kafka";

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

/// The topic configuration key that says WHOSE clock a record's timestamp is, and the
/// value of it that makes the timestamp the broker's own ("Topic-Level Configs", Apache
/// Kafka documentation: `message.timestamp.type`, `CreateTime` or `LogAppendTime`).
pub const MESSAGE_TIMESTAMP_TYPE_CONFIG: &str = "message.timestamp.type";
/// See [`MESSAGE_TIMESTAMP_TYPE_CONFIG`].
pub const LOG_APPEND_TIME: &str = "LogAppendTime";

/// The topic configuration key that says which codec the broker keeps the topic's
/// records in.
pub const COMPRESSION_TYPE_CONFIG: &str = "compression.type";

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
/// The `AclOperation` CHANGING a topic needs — raising its partition count, for one (the
/// same enum).
pub const OPERATION_ALTER: i8 = 7;

/// The most partitions of one topic this endpoint reports and serves.
///
/// CHOSEN as a process-safety bound, not an Apache Kafka or Amazon MSK limit:
/// `CreateTopic`'s `PartitionCount` is modelled `__integerMin1` with no upper bound, and
/// the metadata a client reads renders one entry per partition, so without a ceiling one
/// topic would size every `Metadata` answer about it. Sized far above the partition
/// counts a cluster of this emulator is driven with.
pub const MAX_PARTITIONS_PER_TOPIC: i32 = 1024;

/// One record's headers, in the order the producer wrote them: a sequence of
/// `(key, value)` pairs where the key may REPEAT and the value may be absent, which is
/// what a Kafka record header is (KIP-82). Order and bytes are part of the record, so
/// they are kept as submitted rather than folded into a map.
pub type RecordHeaders = Vec<(String, Option<Vec<u8>>)>;

/// One record a `Produce` submitted, as the log appends it.
#[derive(Debug, Clone, Default)]
pub struct SubmittedRecord {
    /// The producer's timestamp in epoch milliseconds, negative when it set none — in
    /// which case the broker stamps the record itself.
    pub timestamp_ms: i64,
    /// The record key, absent for a record produced without one.
    pub key: Option<Vec<u8>>,
    /// The record value, absent for a tombstone.
    pub value: Option<Vec<u8>>,
    /// The record's headers, as the producer wrote them.
    pub headers: RecordHeaders,
    /// The codec the batch this record arrived in was compressed with.
    pub codec: RecordCodec,
}

/// One record in a partition's log, as a fetch hands it back.
#[derive(Debug, Clone, Default)]
pub struct StoredRecord {
    /// The offset this record was written at; a fetch reads from an offset.
    pub offset: i64,
    /// The record's timestamp in epoch milliseconds — the producer's when it sent one,
    /// otherwise the instant the broker appended it. What retention ages out.
    pub timestamp_ms: i64,
    /// Whether that timestamp is the instant the BROKER appended the record rather than
    /// the one the producer stated: the topic's `message.timestamp.type`, which a fetch
    /// reports as the batch's timestamp type so a consumer reads back the kind of
    /// timestamp the topic keeps.
    pub log_append_time: bool,
    /// The record key, absent for a record produced without one.
    pub key: Option<Vec<u8>>,
    /// The record value, absent for a tombstone.
    pub value: Option<Vec<u8>>,
    /// The record's headers, byte for byte as the producer wrote them.
    pub headers: RecordHeaders,
    /// The codec the batch this record arrived in was compressed with, which is the
    /// codec a fetch answers with for a topic whose `compression.type` is `producer`.
    pub codec: RecordCodec,
}

/// The compression codec a record batch travels in: the codecs Apache Kafka's record
/// batch attributes name, which are also the values a topic's `compression.type` may
/// take beside `producer` and `uncompressed` ("Topic-Level Configs", Apache Kafka
/// documentation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordCodec {
    /// The batch carries its records uncompressed.
    #[default]
    None,
    /// gzip (`compression.type=gzip`).
    Gzip,
    /// snappy (`compression.type=snappy`).
    Snappy,
    /// LZ4 (`compression.type=lz4`).
    Lz4,
    /// Zstandard (`compression.type=zstd`).
    Zstd,
}

impl RecordCodec {
    /// The codec a topic's `compression.type` puts in force, or `None` for the two
    /// values that name no codec of their own: `producer` keeps the codec the producer
    /// chose, and `uncompressed` is the absence of one — which is [`Self::None`], not
    /// the absence of an answer.
    #[must_use]
    pub fn of_topic_config(value: &str) -> Option<Self> {
        match value.trim() {
            "gzip" => Some(Self::Gzip),
            "snappy" => Some(Self::Snappy),
            "lz4" => Some(Self::Lz4),
            "zstd" => Some(Self::Zstd),
            "uncompressed" => Some(Self::None),
            _ => None,
        }
    }
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
    pub fn append(&mut self, record: StoredRecord) -> i64 {
        let offset = self.next_offset;
        self.retained_bytes += record_bytes(&record);
        self.records.push_back(StoredRecord { offset, ..record });
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
            self.retained_bytes = self.retained_bytes.saturating_sub(record_bytes(&dropped));
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

/// The bytes one record's key, value and headers retain.
fn record_bytes(record: &StoredRecord) -> usize {
    record.key.as_ref().map_or(0, Vec::len)
        + record.value.as_ref().map_or(0, Vec::len)
        + header_bytes(&record.headers)
}

/// The bytes one record's headers retain: every key and every value of them, which are
/// part of the record a producer sent and of the one a fetch hands back.
pub fn header_bytes(headers: &RecordHeaders) -> usize {
    headers
        .iter()
        .map(|(key, value)| key.len() + value.as_ref().map_or(0, Vec::len))
        .sum()
}

/// One ACCESS METHOD a cluster's brokers are reached over: the connection scheme a
/// client picks, over the network it arrives on — the cluster's own brokers, their
/// public (service-provided EIP) siblings, or their multi-VPC ones.
///
/// Amazon MSK publishes ONE PORT per access method, shared by every broker node of the
/// cluster ([`super::PORT_PLAINTEXT`], [`super::PORT_TLS`], [`super::PORT_SASL_SCRAM`],
/// [`super::PORT_SASL_IAM`] and their public and multi-VPC siblings), so each method is
/// served here on a socket of its OWN and every node of the cluster is published on
/// that socket's port: a bootstrap string's port says which method a client is
/// reaching, and its host says which broker ([`addressing`]). What a method requires of
/// a client is its scheme's: the plaintext one encrypts nothing, the TLS one encrypts
/// and authenticates no client, and each SASL one requires the exchange its mechanism
/// defines before anything else is served. The PORT a client dials is the core's, not
/// MSK's: the core owns the socket and assigns its port, and the control plane
/// publishes the one it assigned.
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
    /// The TLS scheme over the cluster's PUBLIC connectivity.
    PublicTls,
    /// The SASL/SCRAM scheme over the cluster's public connectivity.
    PublicSaslScram,
    /// The SASL/IAM scheme over the cluster's public connectivity.
    PublicSaslIam,
    /// The TLS scheme over the cluster's MULTI-VPC connectivity.
    VpcTls,
    /// The SASL/SCRAM scheme over the cluster's multi-VPC connectivity.
    VpcSaslScram,
    /// The SASL/IAM scheme over the cluster's multi-VPC connectivity.
    VpcSaslIam,
}

impl AccessPath {
    /// The SASL mechanisms a client of this method authenticates with — none for a
    /// scheme that authenticates no client, and for an IAM one both the exchange a Java
    /// client speaks and the `OAUTHBEARER` one every other client does.
    ///
    /// A method's mechanisms are its SCHEME's, whichever network it is reached over: the
    /// public and multi-VPC siblings of a scheme are the same brokers requiring the same
    /// exchange of a client.
    #[must_use]
    pub fn mechanisms(self) -> &'static [sasl::Mechanism] {
        match self {
            Self::Plaintext | Self::Tls | Self::PublicTls | Self::VpcTls => &[],
            Self::SaslScram | Self::PublicSaslScram | Self::VpcSaslScram => {
                &[sasl::Mechanism::ScramSha512]
            }
            Self::SaslIam | Self::PublicSaslIam | Self::VpcSaslIam => {
                &[sasl::Mechanism::AwsMskIam, sasl::Mechanism::OAuthBearer]
            }
        }
    }
}

/// The ONE socket a cluster's brokers answer one ACCESS METHOD's connections on, and
/// what every broker node of that cluster is published at on it.
///
/// One socket per method, shared by every node, is Amazon MSK's own listener model: a
/// method has one port, and the nodes behind it are told apart by their HOSTS
/// ([`addressing`]). So the record holds the port the core assigned and the zone the
/// nodes' names are published in, and a node's own pair is composed from the node's
/// host and that port.
///
/// The store OWNS the listener: dropping it closes the port and severs every
/// connection on it, so it must outlive every bootstrap string handed out — which is
/// as long as the cluster is in the registry.
pub struct ClusterEndpoint {
    /// The port every broker node of the cluster answers this method on — the one the
    /// core assigned this socket.
    port: u16,
    /// The DNS zone the broker hosts published on this socket sit in, read off the
    /// address the core bound it at ([`addressing::published_zone`]). Kept with the
    /// socket, so a node added to the cluster later is published in the same zone as the
    /// nodes already there.
    zone: String,
    /// Held only to keep the port open; the core owns the socket and its lifetime.
    _listener: ::emulator_server::ResourceListener,
    /// Per BROKER NODE, the record that makes that node reachable at the address it is
    /// published at by a client running in THIS process
    /// (`service_common::dialable_data_planes`) — the code of a Lambda function attached
    /// to the cluster's VPC, which is the one vantage point a VPC-only bootstrap address
    /// is reachable from. One per node, so a node the cluster gains later is recorded
    /// without disturbing the nodes already published; all of them are dropped with the
    /// listener, so nothing resolves to a port that is closed.
    published: BTreeMap<i32, ::service_common::dialable_data_planes::OpenedPlanes>,
}

/// Written by hand rather than derived: a [`::emulator_server::ResourceListener`] is a
/// live socket and carries no `Debug`. What is diagnosable about the endpoint is the
/// address it announced.
impl std::fmt::Debug for ClusterEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ClusterEndpoint")
            .field("port", &self.port)
            .field("zone", &self.zone)
            .field("published", &self.published.len())
            .finish()
    }
}

impl ClusterEndpoint {
    /// The port every broker node of the cluster answers this access method on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The zone the broker hosts published on this socket sit in.
    #[must_use]
    pub fn zone(&self) -> &str {
        &self.zone
    }

    /// The `host:port` the broker node whose published host is `host` answers on.
    #[must_use]
    pub fn authority_of(&self, host: &str) -> String {
        ::service_common::data_plane_addressing::host_port_authority(host, self.port)
    }
}

/// The `host:port` pairs the cluster `cluster_arn` names answers `path` connections on:
/// ONE per broker node the cluster holds, in broker-id order, every one of them on the
/// SAME port — the one this access method is served on — and each under that node's own
/// host.
///
/// A bootstrap string "contains one or more hostname:port pairs"
/// (`GetBootstrapBrokers`), one per broker node — so a client that splits it to size its
/// connection pool, to pin a replication factor to the broker count or to fail over to
/// another broker sees the cluster the control plane reports, and a client that reads the
/// port as the access method reads the same port from every pair. A cluster that publishes
/// no broker nodes of its own stands behind one ([`BROKER_NODE_ID`]), so its string
/// carries exactly one pair.
///
/// `None` when the socket could not be opened — the caller then answers with a modelled
/// error rather than announcing a broker list with an address nothing serves in it.
pub async fn ensure_cluster_endpoints<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_arn: &str,
    path: AccessPath,
) -> Option<Vec<String>> {
    let state = super::service_state(engine)?;
    let key = (cluster_arn.to_string(), path);
    // Everything read off the store is read under ONE guard, taken and dropped before
    // the await below: a lock held across an await would make this future non-`Send`.
    let (open, admitted) = {
        let store = state.lock();
        (
            store.cluster_endpoints.contains_key(&key),
            // A socket is bound for a cluster this endpoint HOLDS, and for no other ARN:
            // the key comes from the request, so binding on a name nothing resolves would
            // let one caller grow the process's port and descriptor use without end.
            store.cluster_in(region, cluster_arn).is_some()
                && store.cluster_endpoints.len() < MAX_CLUSTER_ENDPOINTS,
        )
    };
    if !open {
        if !admitted {
            return None;
        }
        open_access_method(engine, ctx, region, cluster_arn, path, &state).await?;
    }
    // The nodes are read ONCE, with the socket already open, and the addresses answered are
    // the ones just published — so a node a growing cluster gained is never announced ahead
    // of the record that makes it reachable.
    let announced = publish_broker_nodes(engine, ctx, region, cluster_arn, path, &state);
    (!announced.is_empty()).then_some(announced)
}

/// Open the ONE socket the cluster `cluster_arn` answers `path` connections on, and
/// record it.
///
/// `None` when this service was mounted on an engine with no listener surface, when this
/// emulator already holds [`MAX_CLUSTER_ENDPOINTS`] endpoints, or when the core could not
/// bind — the caller then answers with a modelled error rather than announcing an address
/// nothing serves.
///
/// Idempotent: the first socket opened for an access method of a cluster is kept, so every
/// later `GetBootstrapBrokers` answers the same port and a client may key a connection on
/// it. It is released when the cluster leaves the registry
/// ([`ComAmazonawsKafkaKafkaState::unregister_cluster`]), which closes the port.
async fn open_access_method<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_arn: &str,
    path: AccessPath,
    state: &::engine::ServiceState<ComAmazonawsKafkaKafkaState>,
) -> Option<()> {
    let key = (cluster_arn.to_string(), path);
    let listeners = (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()?
        .listeners();
    // The node a client that dialled the METHOD's port lands on: the first broker node of
    // the cluster, which is the node a bootstrap connection reaches. Every node of the
    // cluster reads and writes the same log, so which one a connection is answered as
    // decides only the identity it is answered with — and a client that dialled a
    // particular node's published host in process is answered as THAT node
    // ([`publish_broker_nodes`]).
    let bootstrap_node = state
        .lock()
        .broker_node_ids(region, cluster_arn)
        .first()
        .copied()?;
    let served = broker_of_node(engine, ctx, region, cluster_arn, path, bootstrap_node, state);
    let listener = listeners
        .tcp_plaintext(::service_common::dialable_data_planes::SharedHandler(
            ::std::sync::Arc::clone(&served),
        ))
        .await
        .ok()?;
    let authority = ::service_common::data_plane_addressing::listener_authority(&listener);
    let port = listener.port();
    // The zone the method's broker hosts are published in is the one the address the CORE
    // bound names, which is the interface a client that resolves such a name then dials.
    let zone = addressing::published_zone(&authority);

    let mut store = state.lock();
    // Another call may have opened one while this was binding; keep the first, so an
    // announced address never moves under a client. Dropping the loser closes its port.
    if store.cluster_endpoints.contains_key(&key) {
        return Some(());
    }
    // The ceiling is re-read under the write lock, so concurrent calls cannot carry the
    // registry past it between the check above and this insert.
    if store.cluster_endpoints.len() >= MAX_CLUSTER_ENDPOINTS {
        return None;
    }
    store.cluster_endpoints.insert(
        key,
        ClusterEndpoint {
            port,
            zone,
            _listener: listener,
            published: BTreeMap::new(),
        },
    );
    ::tracing::info!(
        %cluster_arn,
        %authority,
        ?path,
        port,
        "opened an access method's Apache Kafka endpoint"
    );
    Some(())
}

/// Record every broker node of the cluster as reachable at the address it is PUBLISHED
/// at by a client running in this process, on the socket its access method is served on,
/// and answer those addresses in node-id order.
///
/// One record per node, each served by the broker of THAT node: a connection opened to a
/// node's own published host is answered as that node, exactly as a connection to an
/// Amazon MSK broker's own host is. A node already published keeps the record it has, so
/// an announced address never moves under a client; a node the cluster gained since the
/// socket was opened gets one here.
///
/// The node ids are read ONCE here, which is why this answers the addresses too: the list a
/// bootstrap string is rendered from is the list just published, so no node is announced
/// before the record that makes it reachable in process exists. Empty for a cluster this
/// Region holds no record of, or an access method whose socket is not open.
fn publish_broker_nodes<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_arn: &str,
    path: AccessPath,
    state: &::engine::ServiceState<ComAmazonawsKafkaKafkaState>,
) -> Vec<String> {
    let key = (cluster_arn.to_string(), path);
    // The nodes and the addresses they are published at are read off the store under ONE
    // guard, so the list answered and the list recorded below are the same list.
    let announced: Vec<(i32, String)> = {
        let store = state.lock();
        if !store.cluster_endpoints.contains_key(&key) {
            return Vec::new();
        }
        store
            .broker_node_ids(region, cluster_arn)
            .into_iter()
            .filter_map(|node_id| {
                store
                    .broker_authority(region, cluster_arn, path, node_id)
                    .map(|authority| (node_id, authority))
            })
            .collect()
    };
    let Some(planes) = ::service_common::dialable_data_planes::DialablePlanes::of(engine) else {
        return announced
            .into_iter()
            .map(|(_node_id, authority)| authority)
            .collect();
    };
    // A node already published keeps the record it has; only a node the cluster gained
    // since the socket was opened needs one.
    let unpublished: Vec<(i32, String)> = {
        let store = state.lock();
        match store.cluster_endpoints.get(&key) {
            Some(endpoint) => announced
                .iter()
                .filter(|(node_id, _)| !endpoint.published.contains_key(node_id))
                .cloned()
                .collect(),
            None => Vec::new(),
        }
    };
    for (node_id, authority) in unpublished {
        let served = broker_of_node(engine, ctx, region, cluster_arn, path, node_id, state);
        // The plane negotiates its encryption in band itself ([`wire::ClusterBroker`]),
        // reading byte 0 the way it does on the socket, so the record carries no upgrader
        // of its own.
        let plane = ::service_common::dialable_data_planes::served_in_process(served, None);
        let opened = planes.opening([authority], SIGNING_NAME, &plane);
        let mut store = state.lock();
        if let Some(endpoint) = store.cluster_endpoints.get_mut(&key) {
            endpoint.published.entry(node_id).or_insert(opened);
        }
    }
    announced
        .into_iter()
        .map(|(_node_id, authority)| authority)
        .collect()
}

/// The broker ONE node of a cluster's access method answers as, over this service's own
/// store.
///
/// The same broker answers at BOTH ends of a published address: the socket the core
/// accepts a client's connections on, and the connections an IN-PROCESS client of this
/// emulator opens to the address `GetBootstrapBrokers` publishes
/// (`service_common::dialable_data_planes`). A bootstrap address of a cluster in a VPC is
/// reachable only from inside that VPC, so the client that reads it out of the control
/// plane and dials it is code this emulator itself runs — a function attached to that VPC
/// — and both ends are the same broker over the same log.
fn broker_of_node<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    cluster_arn: &str,
    path: AccessPath,
    node_id: i32,
    state: &::engine::ServiceState<ComAmazonawsKafkaKafkaState>,
) -> ::std::sync::Arc<wire::ClusterBroker> {
    let upgrader = (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .and_then(|engine| engine.listeners().tls_upgrader().ok());
    ::std::sync::Arc::new(wire::ClusterBroker::new(
        wire::ServedNode {
            cluster_arn: cluster_arn.to_string(),
            region: region.to_string(),
            path,
            // WHICH broker node of the cluster answers here, which is the node a client
            // reaching this address is talking to: the id it reports itself under in
            // `Metadata` and as the coordinator of a group it holds.
            node_id,
        },
        state.clone(),
        // The core's own TLS server, for a client that dialled one of the
        // encrypted paths. A core that cannot prepare one leaves this `None`, and
        // such a client is then dropped at its first byte rather than answered in
        // the clear.
        upgrader,
        ctx.callback().clone(),
        // Where a connection's IAM permissions are read from: the stores
        // themselves, so a policy written after this socket was opened is in force
        // on the next connection ([`iam_access`]).
        iam_access::PolicySource::of_engine(engine),
    ))
}

/// The most Apache Kafka endpoints this emulator holds open at once, across every
/// cluster and access method.
///
/// CHOSEN as a process-safety bound, not an Amazon MSK limit: each endpoint owns a
/// listening socket and its file descriptor for as long as its cluster is in the
/// registry, and how many clusters a caller creates is the caller's choice. One is opened
/// per ACCESS METHOD a cluster enables — ten at the very most, however many broker nodes
/// the cluster holds, since the nodes of one method share its socket — so it is sized well
/// above the clusters an emulator is driven with.
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

    /// The `host:port` the broker node `node_id` of the cluster `cluster_arn` is
    /// published at for `path`: that node's own published host
    /// ([`addressing::published_broker_host`]) on the port the access method is served
    /// on.
    ///
    /// `None` for a cluster this Region holds no record of, or an access method whose
    /// socket has not been opened — there is no address to report for either.
    #[must_use]
    pub fn broker_authority(
        &self,
        region: &str,
        cluster_arn: &str,
        path: AccessPath,
        node_id: i32,
    ) -> Option<String> {
        let endpoint = self
            .cluster_endpoints
            .get(&(cluster_arn.to_string(), path))?;
        let cluster = self.cluster_in(region, cluster_arn)?;
        // The node's own Amazon MSK name, as the record carries it; a cluster that
        // publishes no broker nodes of its own stands behind one, whose name is composed
        // the same way.
        let endpoint_host = cluster
            .brokers
            .iter()
            .find(|broker| broker.id == node_id)
            .map(|broker| broker.endpoint_host.clone())
            .unwrap_or_else(|| {
                addressing::broker_endpoint_host(
                    &cluster.region,
                    &cluster.name,
                    &cluster.dns_suffix,
                    node_id,
                )
            });
        Some(endpoint.authority_of(&addressing::published_broker_host(
            &endpoint_host,
            endpoint.zone(),
        )))
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

/// What an accepted produce of one partition answers: where the records landed, and the
/// instant the BROKER stamped them at for a topic that stamps its records itself.
#[derive(Debug, Clone, Copy)]
pub struct Appended {
    /// The offset the first appended record took.
    pub base_offset: i64,
    /// The instant the broker stamped the batch at, for a topic whose
    /// `message.timestamp.type` is [`LOG_APPEND_TIME`] — and `None` for a topic that
    /// keeps the producer's own timestamps, which is what the acknowledgement of a
    /// produce to such a topic carries no append time for.
    pub log_append_time_ms: Option<i64>,
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
    /// The codec the topic keeps its records in, from its own `compression.type`:
    /// `None` for a topic that keeps the codec the PRODUCER chose, which is that
    /// setting's default value (`producer`).
    pub codec: Option<RecordCodec>,
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
    /// first of them took and the instant the broker stamped them at, for a topic that
    /// stamps its records itself ([`LOG_APPEND_TIME`]) — which is what a producer is
    /// acknowledged with, since on such a topic the timestamp it sent is not the one the
    /// record carries.
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
    ) -> Result<Appended, PartitionRefusal> {
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
        for record in &records {
            let size = record.key.as_ref().map_or(0, Vec::len)
                + record.value.as_ref().map_or(0, Vec::len)
                + header_bytes(&record.headers);
            if size > MAX_RECORD_BYTES {
                return Err(PartitionRefusal::MessageTooLarge);
            }
        }
        // Whose clock stamps the record: the topic's `message.timestamp.type`. A topic
        // that keeps `LogAppendTime` replaces every producer timestamp with the instant
        // the broker appended the record, and reports the record as carrying that kind
        // of timestamp ("Topic-Level Configs", Apache Kafka documentation).
        let log_append_time = configuration
            .get(MESSAGE_TIMESTAMP_TYPE_CONFIG)
            .is_some_and(|value| value.trim() == LOG_APPEND_TIME);
        let now_ms = epoch_millis(now);
        let log = self
            .partition_logs
            .entry((cluster_arn.to_string(), topic.to_string(), partition))
            .or_default();
        let base = log.next_offset;
        for record in records {
            let stamped = match log_append_time || record.timestamp_ms < 0 {
                // A producer that sends no timestamp has the broker stamp the record
                // with the instant it was appended, which is also what a topic keeping
                // `LogAppendTime` does to every record.
                true => now_ms,
                false => record.timestamp_ms,
            };
            log.append(StoredRecord {
                offset: 0,
                timestamp_ms: stamped,
                log_append_time,
                key: record.key,
                value: record.value,
                headers: record.headers,
                codec: record.codec,
            });
        }
        Ok(Appended {
            base_offset: base,
            log_append_time_ms: log_append_time.then_some(now_ms),
        })
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
        let codec = configuration
            .get(COMPRESSION_TYPE_CONFIG)
            .and_then(|value| RecordCodec::of_topic_config(value));
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
                codec,
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
            codec,
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
