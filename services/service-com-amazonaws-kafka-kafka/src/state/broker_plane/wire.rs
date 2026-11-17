// SPDX-License-Identifier: CC0-1.0

//! The Apache Kafka wire protocol one cluster's endpoint SPEAKS.
//!
//! The core hands this handler an accepted connection and owns everything under it —
//! the socket, the TLS, the bind interface, the connection caps and the idle timeout.
//! What travels on it is the protocol: length-prefixed request frames, each carrying
//! an API key and an API version, answered with the response that key and version
//! declare. The frames are read and written with the protocol's own implementation
//! ([`::kafka_protocol`]), so nothing here hand-rolls a message layout.
//!
//! The answers are COMPUTED from this service's state: `Metadata` reports the address
//! the core bound and the topics the cluster holds, `DescribeConfigs` reports the
//! topic configuration `CreateTopic`/`UpdateTopic` stored over Apache Kafka's
//! documented defaults, `Produce`/`Fetch`/`ListOffsets` write and read the
//! partition log in [`super`], and `CreateTopics`/`DeleteTopics` register and remove
//! the cluster's own topics ([`topics`]) — the same records the control plane's topic
//! operations write. An API this endpoint does not serve is answered
//! `UNSUPPORTED_VERSION` rather than with an invented success, and the versions it
//! does serve are the ones it advertises in `ApiVersions`.
//!
//! A client picks what it sends from that advertisement, so the advertisement is what
//! the endpoint can really carry: every pair in [`SERVED_APIS`] is a version whose
//! request this endpoint can read and whose response it can write, and each answer is
//! built for the version the request came in at, not for the newest one — a response
//! carrying a member a version does not have is a frame that cannot be written, and a
//! frame a broker cannot write or read is a connection Apache Kafka closes, which leaves
//! a client reconnecting with no error it can report.
//!
//! Authentication is the ACCESS PATH's: a connection on the plaintext or TLS path
//! authenticates no client, and one on a SASL path must complete that mechanism's
//! exchange ([`super::sasl`]) before any other API is served. The TOKENS of that
//! exchange travel in the framing the client's `SaslHandshake` version selects — bare,
//! length-prefixed byte strings at version 0, and `SaslAuthenticate` requests and
//! responses from version 1 on ([`Session::unwrapped_sasl`]) — so a client that
//! handshakes at either version is authenticated by the same exchange.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::LazyLock;
use std::time::SystemTime;

use ::bytes::{Bytes, BytesMut};
use ::emulator_server::{ListenerConn, NativeHandler, TlsUpgrader};
use ::kafka_protocol::messages::{
    api_versions_response::ApiVersion,
    describe_configs_response::{DescribeConfigsResourceResult, DescribeConfigsResult},
    fetch_response::{FetchableTopicResponse, PartitionData},
    list_offsets_response::{ListOffsetsPartitionResponse, ListOffsetsTopicResponse},
    metadata_response::{MetadataResponseBroker, MetadataResponsePartition, MetadataResponseTopic},
    produce_response::{PartitionProduceResponse, TopicProduceResponse},
    ApiKey, ApiVersionsRequest, ApiVersionsResponse, BrokerId, CreateAclsRequest,
    CreateTopicsRequest, DeleteAclsRequest, DeleteTopicsRequest, DescribeAclsRequest,
    DescribeConfigsRequest, DescribeConfigsResponse,
    FetchRequest, FetchResponse, FindCoordinatorRequest, FindCoordinatorResponse,
    HeartbeatRequest, InitProducerIdRequest, InitProducerIdResponse, JoinGroupRequest,
    LeaveGroupRequest, ListOffsetsRequest, ListOffsetsResponse, MetadataRequest, MetadataResponse,
    OffsetCommitRequest, OffsetFetchRequest, ProduceRequest, ProduceResponse, RequestHeader,
    ResponseHeader, SaslAuthenticateRequest, SaslAuthenticateResponse, SaslHandshakeRequest,
    SaslHandshakeResponse, SyncGroupRequest, TopicName,
};
use ::kafka_protocol::protocol::{Decodable, Encodable, Message, StrBytes};
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use ::kafka_protocol::ResponseError;
use ::smithy_server_runtime::EmbeddedEmulator;
use ::tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::sasl::{self, Authenticated, Mechanism};
use super::{acls, groups, topics, AccessPath, BROKER_NODE_ID, MAX_FRAME_BYTES};

/// The resource type a `DescribeConfigs` request names a TOPIC by, and the one it
/// names a BROKER by (`ConfigResource.Type`, Apache Kafka protocol).
const RESOURCE_TYPE_TOPIC: i8 = 2;
/// See [`RESOURCE_TYPE_TOPIC`].
const RESOURCE_TYPE_BROKER: i8 = 4;

/// The `config_source` a value the resource itself sets is reported under, and the one
/// a value that comes from the broker's defaults is
/// (`DescribeConfigsResourceResult.ConfigSource`): a client tells a configured value
/// from a default by this field.
const CONFIG_SOURCE_SET_BY_RESOURCE: i8 = 1;
/// See [`CONFIG_SOURCE_SET_BY_RESOURCE`].
const CONFIG_SOURCE_DEFAULT: i8 = 5;

/// The timestamp a `ListOffsets` request names to ask for the EARLIEST offset still
/// readable, and the one it names to ask for the offset the next produced record will
/// take (Apache Kafka protocol, `ListOffsets`).
const OFFSET_EARLIEST: i64 = -2;
/// See [`OFFSET_EARLIEST`].
const OFFSET_LATEST: i64 = -1;

/// The most records one fetch answers with FOR ONE PARTITION, and the most across the
/// whole request: one request names as many (topic, partition) entries as it likes, so
/// what bounds the response is the total, not the per-entry cap. Once the total is
/// reached the remaining entries are answered with an empty record set, which is the
/// partial answer the protocol already lets a broker give.
const MAX_FETCHED_RECORDS: usize = 512;
/// See [`MAX_FETCHED_RECORDS`].
const MAX_FETCHED_RECORDS_PER_REQUEST: usize = 4096;

/// The most (topic, partition) entries one request is served for, across every API that
/// takes a list of them (`Fetch`, `ListOffsets`, `OffsetCommit`), and the most filters
/// one `DescribeAcls`/`DeleteAcls` is served for. CHOSEN as a process-safety bound, not
/// a protocol limit: the list comes from the request, so without it one frame would
/// size both the work and the response. Entries past it are not answered.
const MAX_REQUEST_ENTRIES: usize = 1024;

/// The record batch version this endpoint writes: the format Apache Kafka has used
/// since 0.11, which is what every client this endpoint can negotiate with reads.
const RECORD_BATCH_VERSION: i8 = 2;

/// The APIs this endpoint serves, and the versions of each: what `ApiVersions`
/// advertises and what every other frame is admitted against, so a client negotiates
/// exactly what is implemented here.
///
/// Each entry is the range Amazon MSK's brokers offer for that API, NARROWED to the
/// versions the protocol implementation behind this endpoint can actually carry
/// ([`carried`]). A client picks the version it sends from the advertisement, and Apache
/// Kafka answers a frame it cannot parse by closing the connection — so a version
/// advertised but not readable (or a response not writable in it) would leave a client
/// reconnecting against a socket that drops it, with no error it could report.
static SERVED_APIS: LazyLock<Vec<(ApiKey, i16, i16)>> = LazyLock::new(served_apis);

/// See [`SERVED_APIS`]: the table, built once.
fn served_apis() -> Vec<(ApiKey, i16, i16)> {
    let mut apis: Vec<(ApiKey, i16, i16)> = Vec::new();
    let mut serve = |key: ApiKey, versions: Option<(i16, i16)>| {
        if let Some((min, max)) = versions {
            apis.push((key, min, max));
        }
    };
    serve(
        ApiKey::Produce,
        carried::<ProduceRequest, ProduceResponse>(0, 9),
    );
    // `Fetch` and the topic administration below stop at the last version that names a
    // topic by NAME alone: this endpoint reports no topic ids, so the versions that read
    // and write topics by id are not ones it can serve.
    serve(ApiKey::Fetch, carried::<FetchRequest, FetchResponse>(0, 12));
    serve(
        ApiKey::ListOffsets,
        carried::<ListOffsetsRequest, ListOffsetsResponse>(0, 7),
    );
    serve(
        ApiKey::Metadata,
        carried::<MetadataRequest, MetadataResponse>(0, 12),
    );
    serve(
        ApiKey::FindCoordinator,
        carried::<FindCoordinatorRequest, FindCoordinatorResponse>(0, 4),
    );
    serve(
        ApiKey::SaslHandshake,
        carried::<SaslHandshakeRequest, SaslHandshakeResponse>(0, 1),
    );
    serve(
        ApiKey::ApiVersions,
        carried::<ApiVersionsRequest, ApiVersionsResponse>(0, 3),
    );
    serve(
        ApiKey::DescribeConfigs,
        carried::<DescribeConfigsRequest, DescribeConfigsResponse>(0, 4),
    );
    serve(
        ApiKey::SaslAuthenticate,
        carried::<SaslAuthenticateRequest, SaslAuthenticateResponse>(0, 2),
    );
    serve(
        ApiKey::DescribeAcls,
        carried::<DescribeAclsRequest, ::kafka_protocol::messages::DescribeAclsResponse>(0, 3),
    );
    serve(
        ApiKey::CreateAcls,
        carried::<CreateAclsRequest, ::kafka_protocol::messages::CreateAclsResponse>(0, 3),
    );
    serve(
        ApiKey::DeleteAcls,
        carried::<DeleteAclsRequest, ::kafka_protocol::messages::DeleteAclsResponse>(0, 3),
    );
    serve(
        ApiKey::InitProducerId,
        carried::<InitProducerIdRequest, InitProducerIdResponse>(0, 4),
    );
    serve(
        ApiKey::JoinGroup,
        carried::<JoinGroupRequest, ::kafka_protocol::messages::JoinGroupResponse>(0, 7),
    );
    serve(
        ApiKey::SyncGroup,
        carried::<SyncGroupRequest, ::kafka_protocol::messages::SyncGroupResponse>(0, 5),
    );
    serve(
        ApiKey::Heartbeat,
        carried::<HeartbeatRequest, ::kafka_protocol::messages::HeartbeatResponse>(0, 4),
    );
    serve(
        ApiKey::LeaveGroup,
        carried::<LeaveGroupRequest, ::kafka_protocol::messages::LeaveGroupResponse>(0, 4),
    );
    serve(
        ApiKey::OffsetCommit,
        carried::<OffsetCommitRequest, ::kafka_protocol::messages::OffsetCommitResponse>(0, 7),
    );
    serve(
        ApiKey::OffsetFetch,
        carried::<OffsetFetchRequest, ::kafka_protocol::messages::OffsetFetchResponse>(0, 7),
    );
    serve(
        ApiKey::CreateTopics,
        carried::<CreateTopicsRequest, ::kafka_protocol::messages::CreateTopicsResponse>(0, 5),
    );
    serve(
        ApiKey::DeleteTopics,
        carried::<DeleteTopicsRequest, ::kafka_protocol::messages::DeleteTopicsResponse>(0, 5),
    );
    apis
}

/// The versions of one API this endpoint serves: `min..=max` as Amazon MSK's brokers
/// offer them, narrowed to the versions BOTH its request and its response can be carried
/// in ([`Message::VERSIONS`]).
///
/// `None` when nothing is left of the range, which is an API this endpoint then does not
/// advertise at all rather than one it announces and cannot read.
fn carried<Request: Message, Response: Message>(min: i16, max: i16) -> Option<(i16, i16)> {
    let min = min.max(Request::VERSIONS.min).max(Response::VERSIONS.min);
    let max = max.min(Request::VERSIONS.max).min(Response::VERSIONS.max);
    (min <= max).then_some((min, max))
}

/// One access path of one cluster's Apache Kafka endpoint.
pub struct ClusterBroker {
    /// The cluster whose brokers this endpoint is: every answer is computed from THIS
    /// cluster's record, its topics and its logs.
    cluster_arn: String,
    /// The Region the cluster lives in, which a SASL/IAM credential is scoped to.
    region: String,
    /// The access path this socket serves, which is what it requires of a client.
    path: AccessPath,
    /// This service's store — the same one the control plane writes.
    state: ::engine::ServiceState<crate::state::ComAmazonawsKafkaKafkaState>,
    /// The core's TLS server, for a client that dialled one of the encrypted paths.
    upgrader: Option<TlsUpgrader>,
    /// The emulator the control call that opened this endpoint was served by: an
    /// IN-PROCESS handle, used to read a SASL/SCRAM credential out of the Secrets
    /// Manager secret the cluster associated. Nothing here dials out.
    emulator: EmbeddedEmulator,
}

impl ClusterBroker {
    /// The endpoint of one cluster's access path, over the store and the emulator that
    /// announced it.
    pub fn new(
        cluster_arn: String,
        region: String,
        path: AccessPath,
        state: ::engine::ServiceState<crate::state::ComAmazonawsKafkaKafkaState>,
        upgrader: Option<TlsUpgrader>,
        emulator: EmbeddedEmulator,
    ) -> Self {
        Self {
            cluster_arn,
            region,
            path,
            state,
            upgrader,
            emulator,
        }
    }
}

impl NativeHandler for ClusterBroker {
    /// One call per accepted connection. The handles are cloned into the connection's
    /// own future, which is what the core's `'static` handler contract asks for.
    fn serve(&self, conn: ListenerConn) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let broker = Broker {
            cluster_arn: self.cluster_arn.clone(),
            region: self.region.clone(),
            path: self.path,
            state: self.state.clone(),
            emulator: self.emulator.clone(),
        };
        let upgrader = self.upgrader.clone();
        Box::pin(async move {
            // The connection's first byte says whether the client opened with TLS: a
            // TLS record is handed to the core's own TLS server and everything after
            // it is read decrypted, and anything else is read as it stands — so the
            // encrypted paths and the plaintext one are each served as the client
            // dialled them, with no key or certificate in this crate.
            let Some(conn) = ::service_common::in_band_tls::negotiated(conn, upgrader).await else {
                return;
            };
            broker.serve_requests(conn).await;
        })
    }
}

/// The handles one connection answers from.
#[derive(Clone)]
struct Broker {
    cluster_arn: String,
    region: String,
    path: AccessPath,
    state: ::engine::ServiceState<crate::state::ComAmazonawsKafkaKafkaState>,
    emulator: EmbeddedEmulator,
}

/// What one connection has established: which SASL mechanism it chose, how its tokens
/// travel, and whether the exchange has finished.
#[derive(Default)]
struct Session {
    /// The mechanism the client selected with `SaslHandshake`.
    mechanism: Option<Mechanism>,
    /// Whether this connection's SASL tokens travel UNWRAPPED.
    ///
    /// `SaslHandshake` has two framings and the version the client selected its
    /// mechanism with chooses between them: version 0 exchanges each SASL token as a
    /// bare, length-prefixed byte string, while from version 1 on the tokens travel
    /// inside `SaslAuthenticate` requests and responses. A client that handshakes at
    /// version 0 therefore sends no API key at all for its tokens.
    unwrapped_sasl: bool,
    /// The SASL/SCRAM exchange in flight, between the client's first message and its
    /// proof.
    scram: Option<sasl::ScramExchange>,
    /// Who the connection is authenticated as, once the exchange has succeeded.
    identity: Option<Authenticated>,
}

impl Session {
    /// Whether the NEXT frame on this connection is a bare SASL token rather than a
    /// Kafka request: the client selected its mechanism with the version of the
    /// handshake that exchanges tokens unwrapped, and the exchange has not finished.
    fn expects_sasl_token(&self) -> bool {
        self.unwrapped_sasl && self.mechanism.is_some() && self.identity.is_none()
    }
}

impl Broker {
    /// Read frames off `conn` and answer each one until the client closes it, the
    /// frame is one this endpoint cannot read, or the core severs the connection.
    async fn serve_requests(&self, mut conn: ListenerConn) {
        let mut session = Session::default();
        loop {
            let mut length = [0u8; 4];
            if conn.read_exact(&mut length).await.is_err() {
                return;
            }
            let length = u32::from_be_bytes(length) as usize;
            // A frame this endpoint will not buffer: the connection ends rather than
            // the process holding a caller-sized allocation for it. A bare SASL token
            // carries no request header, so it is shorter than the smallest request
            // frame — one byte is a whole token in some mechanisms.
            let smallest = match session.expects_sasl_token() {
                true => 1,
                false => 8,
            };
            if !(smallest..=MAX_FRAME_BYTES).contains(&length) {
                return;
            }
            let mut frame = vec![0u8; length];
            if conn.read_exact(&mut frame).await.is_err() {
                return;
            }
            let frame = Bytes::from(frame);
            let answered = match session.expects_sasl_token() {
                true => self.sasl_token(&mut session, &frame).await,
                false => self.answer(&mut session, frame).await,
            };
            let Some(response) = answered else {
                return;
            };
            let framed = u32::try_from(response.len()).unwrap_or(u32::MAX);
            if conn.write_all(&framed.to_be_bytes()).await.is_err()
                || conn.write_all(&response).await.is_err()
                || conn.flush().await.is_err()
            {
                return;
            }
        }
    }

    /// One leg of an UNWRAPPED SASL exchange: the token the client sent, answered with
    /// the challenge or the final message as bare bytes, with no response header.
    ///
    /// That framing carries no error code, so a refused exchange is answered by closing
    /// the connection — which is what the version of the handshake that selects it
    /// defines, and what a client of it waits for.
    async fn sasl_token(&self, session: &mut Session, token: &Bytes) -> Option<Vec<u8>> {
        match self.sasl_leg(session, token.as_ref()).await {
            Ok(sasl::Leg::Continue(challenge)) => Some(challenge.to_vec()),
            Ok(sasl::Leg::Done { response, identity }) => {
                session.identity = Some(identity);
                Some(response.to_vec())
            }
            Err(_) => None,
        }
    }

    /// Answer one request frame, or `None` when the connection cannot be served any
    /// further (an unreadable frame, a version this endpoint does not serve, or a
    /// request made before the path's authentication was completed — which is where
    /// Apache Kafka closes the connection too).
    async fn answer(&self, session: &mut Session, frame: Bytes) -> Option<Vec<u8>> {
        let api_key = i16::from_be_bytes([frame[0], frame[1]]);
        let api_version = i16::from_be_bytes([frame[2], frame[3]]);
        let correlation_id = i32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]);
        let key = ApiKey::try_from(api_key).ok()?;
        // Only the versions advertised in `ApiVersions` are read. A client that
        // negotiates with a newer `ApiVersions` is answered `UNSUPPORTED_VERSION` in
        // the oldest form of that response, which is what it then re-reads.
        if !served(key, api_version) {
            return self.unsupported_version(key, correlation_id);
        }
        let mut body = frame;
        let header =
            RequestHeader::decode(&mut body, key.request_header_version(api_version)).ok()?;
        // Before the exchange this path requires, only the frames that negotiate it
        // are served; anything else ends the connection, as an unauthenticated
        // connection to a broker does.
        if !self.path.mechanisms().is_empty() && session.identity.is_none() {
            match key {
                ApiKey::ApiVersions | ApiKey::SaslHandshake | ApiKey::SaslAuthenticate => {}
                _ => return None,
            }
        }
        match key {
            ApiKey::ApiVersions => {
                let _request = ApiVersionsRequest::decode(&mut body, api_version).ok()?;
                self.encode(key, api_version, &header, &api_versions())
            }
            ApiKey::SaslHandshake => {
                let request = SaslHandshakeRequest::decode(&mut body, api_version).ok()?;
                let response = self.sasl_handshake(session, &request, api_version);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::SaslAuthenticate => {
                let request = SaslAuthenticateRequest::decode(&mut body, api_version).ok()?;
                let response = self.sasl_authenticate(session, &request).await;
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::Metadata => {
                let request = MetadataRequest::decode(&mut body, api_version).ok()?;
                let response = self.metadata(&request);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::Produce => {
                let request = ProduceRequest::decode(&mut body, api_version).ok()?;
                let response = self.produce(&request, session.identity.as_ref());
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::Fetch => {
                let request = FetchRequest::decode(&mut body, api_version).ok()?;
                let response = self.fetch(&request, session.identity.as_ref());
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::ListOffsets => {
                let request = ListOffsetsRequest::decode(&mut body, api_version).ok()?;
                let response = self.list_offsets(&request);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::DescribeConfigs => {
                let request = DescribeConfigsRequest::decode(&mut body, api_version).ok()?;
                let response = self.describe_configs(&request);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::FindCoordinator => {
                let request = FindCoordinatorRequest::decode(&mut body, api_version).ok()?;
                let response = self.find_coordinator(&request, api_version);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::InitProducerId => {
                let _request = InitProducerIdRequest::decode(&mut body, api_version).ok()?;
                let response = self.init_producer_id();
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::JoinGroup => {
                let request = JoinGroupRequest::decode(&mut body, api_version).ok()?;
                let response = self
                    .group(&request.group_id.0, true, |group| group.join(&request))
                    .unwrap_or_else(|| {
                        ::kafka_protocol::messages::JoinGroupResponse::default()
                            .with_error_code(no_coordinator())
                    });
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::SyncGroup => {
                let request = SyncGroupRequest::decode(&mut body, api_version).ok()?;
                let response = self
                    .group(&request.group_id.0, false, |group| group.sync(&request))
                    .unwrap_or_else(|| {
                        ::kafka_protocol::messages::SyncGroupResponse::default()
                            .with_error_code(no_coordinator())
                    });
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::Heartbeat => {
                let request = HeartbeatRequest::decode(&mut body, api_version).ok()?;
                let response = self
                    .group(&request.group_id.0, false, |group| group.heartbeat(&request))
                    .unwrap_or_else(|| {
                        ::kafka_protocol::messages::HeartbeatResponse::default()
                            .with_error_code(no_coordinator())
                    });
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::LeaveGroup => {
                let request = LeaveGroupRequest::decode(&mut body, api_version).ok()?;
                let response = self
                    .group(&request.group_id.0, false, |group| group.leave(&request))
                    .unwrap_or_else(|| {
                        ::kafka_protocol::messages::LeaveGroupResponse::default()
                            .with_error_code(no_coordinator())
                    });
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::OffsetCommit => {
                let request = OffsetCommitRequest::decode(&mut body, api_version).ok()?;
                let response = self
                    .group(&request.group_id.0, true, |group| {
                        group.commit(&request, MAX_REQUEST_ENTRIES)
                    })
                    .unwrap_or_default();
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::OffsetFetch => {
                let request = OffsetFetchRequest::decode(&mut body, api_version).ok()?;
                // A group nobody has joined has committed nothing, which is what the
                // protocol's "no committed offset" answers — reading it registers no
                // group.
                let response = self
                    .group(&request.group_id.0, false, |group| {
                        group.fetch_offsets(&request)
                    })
                    .unwrap_or_else(|| groups::uncommitted_offsets(&request));
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::CreateAcls => {
                let request = CreateAclsRequest::decode(&mut body, api_version).ok()?;
                let mut store = self.state.lock();
                let held = store.broker_acls.entry(self.cluster_arn.clone()).or_default();
                let response = acls::create(held, &request, MAX_REQUEST_ENTRIES);
                drop(store);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::DescribeAcls => {
                let request = DescribeAclsRequest::decode(&mut body, api_version).ok()?;
                let mut store = self.state.lock();
                let held = store.broker_acls.entry(self.cluster_arn.clone()).or_default();
                let response = acls::describe(held, &request);
                drop(store);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::CreateTopics => {
                let request = CreateTopicsRequest::decode(&mut body, api_version).ok()?;
                // The ACL decision is taken BEFORE the store is locked: it reads the
                // bindings under the same lock, so the two cannot be held at once.
                let withheld = self.withheld_topics(
                    session.identity.as_ref(),
                    request.topics.iter().map(|topic| topic.name.0.to_string()),
                    super::OPERATION_CREATE,
                );
                let mut store = self.state.lock();
                let response = topics::create(
                    &mut store,
                    &self.region,
                    &self.cluster_arn,
                    &request,
                    &withheld,
                    MAX_REQUEST_ENTRIES,
                    SystemTime::now(),
                );
                drop(store);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::DeleteTopics => {
                let request = DeleteTopicsRequest::decode(&mut body, api_version).ok()?;
                let withheld = self.withheld_topics(
                    session.identity.as_ref(),
                    request
                        .topic_names
                        .iter()
                        .map(|name| name.0.to_string())
                        .chain(request.topics.iter().filter_map(|topic| {
                            topic.name.as_ref().map(|name| name.0.to_string())
                        })),
                    super::OPERATION_DELETE,
                );
                let mut store = self.state.lock();
                let response = topics::delete(
                    &mut store,
                    &self.cluster_arn,
                    &request,
                    &withheld,
                    MAX_REQUEST_ENTRIES,
                    SystemTime::now(),
                );
                drop(store);
                self.encode(key, api_version, &header, &response)
            }
            ApiKey::DeleteAcls => {
                let request = DeleteAclsRequest::decode(&mut body, api_version).ok()?;
                let mut store = self.state.lock();
                let held = store.broker_acls.entry(self.cluster_arn.clone()).or_default();
                let response = acls::delete(held, &request, MAX_REQUEST_ENTRIES);
                drop(store);
                self.encode(key, api_version, &header, &response)
            }
            // Unreachable: `served` admits only the keys above.
            _ => None,
        }
    }

    /// Frame one response: its header in the version this API and version pair
    /// declares, then the response itself.
    fn encode<R: Encodable>(
        &self,
        key: ApiKey,
        api_version: i16,
        header: &RequestHeader,
        response: &R,
    ) -> Option<Vec<u8>> {
        let mut encoded = BytesMut::new();
        ResponseHeader::default()
            .with_correlation_id(header.correlation_id)
            .encode(&mut encoded, key.response_header_version(api_version))
            .ok()?;
        response.encode(&mut encoded, api_version).ok()?;
        Some(encoded.to_vec())
    }

    /// What this endpoint answers a frame whose version it does not serve: a client
    /// negotiating with `ApiVersions` is told `UNSUPPORTED_VERSION` together with the
    /// versions this endpoint does serve, in the oldest form of that response, which
    /// is the one form every client can read. For any other API there is no answer
    /// this endpoint could frame in a version it cannot write, so the connection ends.
    fn unsupported_version(&self, key: ApiKey, correlation_id: i32) -> Option<Vec<u8>> {
        if key != ApiKey::ApiVersions {
            return None;
        }
        let mut encoded = BytesMut::new();
        ResponseHeader::default()
            .with_correlation_id(correlation_id)
            .encode(&mut encoded, 0)
            .ok()?;
        api_versions()
            .with_error_code(ResponseError::UnsupportedVersion.code())
            .encode(&mut encoded, 0)
            .ok()?;
        Some(encoded.to_vec())
    }

    /// The SASL mechanisms this path offers: the ones its port is for, narrowed to the
    /// ones the CLUSTER's client authentication still turns on
    /// ([`sasl::advertised`]) — so a client that picks another, or one that dials a
    /// SASL path of a cluster whose SASL authentication has been turned off, is told
    /// so.
    fn mechanisms(&self) -> Vec<Mechanism> {
        let enabled = self
            .state
            .lock()
            .cluster_in(&self.region, &self.cluster_arn)
            .map(sasl::advertised)
            .unwrap_or_default();
        self.path
            .mechanisms()
            .iter()
            .copied()
            .filter(|mechanism| enabled.contains(mechanism))
            .collect()
    }

    /// The authority the core bound for this path, which is the address a client
    /// reaches this broker at.
    fn authority(&self) -> String {
        self.state
            .lock()
            .cluster_endpoints
            .get(&(self.cluster_arn.clone(), self.path))
            .map(|endpoint| endpoint.authority().to_string())
            .unwrap_or_default()
    }

    /// `SaslHandshake`: the mechanisms this path offers, and whether the one the
    /// client named is among them.
    ///
    /// The VERSION a client handshakes at also chooses how its tokens then travel
    /// ([`Session::unwrapped_sasl`]), so it is remembered here: the exchange that
    /// follows is read in the framing the client selected.
    fn sasl_handshake(
        &self,
        session: &mut Session,
        request: &SaslHandshakeRequest,
        api_version: i16,
    ) -> SaslHandshakeResponse {
        let offered = self.mechanisms();
        let chosen = offered
            .iter()
            .copied()
            .find(|mechanism| mechanism.name() == request.mechanism.as_str());
        session.mechanism = chosen;
        session.unwrapped_sasl = api_version == 0;
        session.scram = None;
        SaslHandshakeResponse::default()
            .with_error_code(match chosen {
                Some(_) => 0,
                None => ResponseError::UnsupportedSaslMechanism.code(),
            })
            .with_mechanisms(
                offered
                    .iter()
                    .map(|mechanism| StrBytes::from_static_str(mechanism.name()))
                    .collect(),
            )
    }

    /// One leg of the exchange the chosen mechanism defines, whichever framing carried
    /// the token — so a client that handshaked at version 0 and one that wraps its
    /// tokens in `SaslAuthenticate` are authenticated by the same exchange.
    async fn sasl_leg(
        &self,
        session: &mut Session,
        auth_bytes: &[u8],
    ) -> Result<sasl::Leg, sasl::Refusal> {
        let chosen = session
            .mechanism
            .filter(|mechanism| self.mechanisms().contains(mechanism));
        let outcome = match chosen {
            Some(Mechanism::ScramSha512) => {
                let source = sasl::CredentialSource {
                    emulator: self.emulator.clone(),
                    region: self.region.clone(),
                    secret_arns: self.scram_secret_arns(),
                };
                let mut exchange = session.scram.take().unwrap_or_default();
                let leg = exchange.leg(&source, auth_bytes).await;
                session.scram = Some(exchange);
                leg
            }
            Some(Mechanism::AwsMskIam) => sasl::iam_leg(&self.region, auth_bytes),
            Some(Mechanism::OAuthBearer) => sasl::oauth_bearer_leg(&self.region, auth_bytes),
            // A client that authenticates before choosing a mechanism this path
            // offers is in no state to be authenticated, which is what
            // `ILLEGAL_SASL_STATE` says.
            None => Err(sasl::Refusal::IllegalState),
        };
        if outcome.is_err() {
            session.scram = None;
        }
        outcome
    }

    /// `SaslAuthenticate`: one leg of that exchange, as the versions of the handshake
    /// from 1 on carry it — inside this request, and answered inside its response.
    async fn sasl_authenticate(
        &self,
        session: &mut Session,
        request: &SaslAuthenticateRequest,
    ) -> SaslAuthenticateResponse {
        match self.sasl_leg(session, request.auth_bytes.as_ref()).await {
            Ok(sasl::Leg::Continue(challenge)) => {
                SaslAuthenticateResponse::default().with_auth_bytes(challenge)
            }
            Ok(sasl::Leg::Done { response, identity }) => {
                session.identity = Some(identity);
                SaslAuthenticateResponse::default().with_auth_bytes(response)
            }
            Err(refusal) => SaslAuthenticateResponse::default()
                .with_error_code(refusal.code())
                .with_error_message(Some(StrBytes::from_static_str(refusal.message()))),
        }
    }

    /// The cluster's associated SASL/SCRAM secret ARNs, which is where the credential a
    /// client authenticates with lives — so a secret the cluster no longer has
    /// associated authenticates nobody.
    fn scram_secret_arns(&self) -> Vec<String> {
        self.state
            .lock()
            .cluster_in(&self.region, &self.cluster_arn)
            .map(|cluster| cluster.scram_secret_arns.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Answer one group call out of the group `group_id` names on this cluster.
    ///
    /// A group exists from the moment a member JOINS it, so `create` is true only for
    /// that leg: every other leg is answered out of the group the cluster already
    /// holds, and `None` for one it does not — a read must not write a record a later
    /// caller would see, nor take one of the slots below.
    ///
    /// A group id is the CLIENT's string, so the cluster holds at most
    /// [`groups::MAX_GROUPS_PER_CLUSTER`] of them and reads no id longer than
    /// [`groups::MAX_GROUP_ID_BYTES`]: past either, the call is answered `None` rather
    /// than growing the store.
    fn group<R>(
        &self,
        group_id: &StrBytes,
        create: bool,
        answer: impl FnOnce(&mut groups::ConsumerGroup) -> R,
    ) -> Option<R> {
        if group_id.len() > groups::MAX_GROUP_ID_BYTES {
            return None;
        }
        let key = (self.cluster_arn.clone(), group_id.to_string());
        let mut store = self.state.lock();
        if !store.consumer_groups.contains_key(&key) {
            // The ceiling is THIS cluster's: the store is keyed by (cluster, group), so
            // the groups of one cluster are the ones counted against it.
            let held_here = store
                .consumer_groups
                .keys()
                .filter(|(cluster, _)| *cluster == self.cluster_arn)
                .count();
            if !create || held_here >= groups::MAX_GROUPS_PER_CLUSTER {
                return None;
            }
            store.consumer_groups.insert(key.clone(), Default::default());
        }
        store.consumer_groups.get_mut(&key).map(answer)
    }

    /// `InitProducerId`: the id an IDEMPOTENT producer writes under, which the broker
    /// assigns.
    ///
    /// The id comes from the store's own counter, so two producers of one emulator are
    /// never given the same one, and the epoch starts at zero as it does for an id
    /// nobody has fenced yet.
    fn init_producer_id(&self) -> InitProducerIdResponse {
        let mut store = self.state.lock();
        let producer_id = store.next_producer_id;
        store.next_producer_id += 1;
        InitProducerIdResponse::default()
            .with_producer_id(::kafka_protocol::messages::ProducerId(producer_id))
            .with_producer_epoch(0)
    }

    /// Whether an ACL binding this cluster's brokers hold withholds `topic` from the
    /// connection's principal for `operation`.
    fn withholds(
        &self,
        identity: Option<&Authenticated>,
        topic: &str,
        operation: i8,
    ) -> bool {
        self.state
            .lock()
            .broker_acls
            .get(&self.cluster_arn)
            .is_some_and(|held| {
                held.denies(
                    identity.map(|identity| identity.principal.as_str()),
                    topic,
                    operation,
                )
            })
    }

    /// The topics among `named` an ACL binding this cluster's brokers hold withholds
    /// from the connection's principal for `operation`.
    ///
    /// Resolved for the whole request before the store is written, since the bindings
    /// are read under the same lock a write to the registry takes. Bounded like every
    /// other per-request list: at most [`MAX_REQUEST_ENTRIES`] names are decided.
    fn withheld_topics(
        &self,
        identity: Option<&Authenticated>,
        named: impl Iterator<Item = String>,
        operation: i8,
    ) -> BTreeSet<String> {
        named
            .take(MAX_REQUEST_ENTRIES)
            .filter(|name| self.withholds(identity, name, operation))
            .collect()
    }

    /// `Metadata`: the brokers a client reaches this cluster at, and the topics it
    /// asked about.
    ///
    /// Every broker node the cluster holds is reported, each at the address the CORE
    /// bound for this path — so a client that asks one node for its configuration, or
    /// re-resolves the cluster from its metadata, reaches this emulator rather than a
    /// name nothing serves. The cluster's partitions are led by its FIRST node, which
    /// is the broker that holds the log.
    fn metadata(&self, request: &MetadataRequest) -> MetadataResponse {
        let now = SystemTime::now();
        let (host, port) = split_authority(&self.authority());
        let (held, broker_ids) = {
            let mut store = self.state.lock();
            // A serverless cluster has no broker nodes of its own, and the one endpoint
            // it publishes is one broker to its clients — so the node this endpoint
            // reports itself under stands in for it.
            let ids = store
                .cluster_in(&self.region, &self.cluster_arn)
                .map(|cluster| cluster.brokers.iter().map(|broker| broker.id).collect())
                .filter(|ids: &Vec<i32>| !ids.is_empty())
                .unwrap_or_else(|| vec![BROKER_NODE_ID]);
            (store.data_plane_topics(&self.cluster_arn, now), ids)
        };
        // A request naming no topic list at all asks for every topic of the cluster,
        // which is what a null list means in this API.
        let wanted: Option<Vec<String>> = request.topics.as_ref().map(|topics| {
            topics
                .iter()
                .filter_map(|topic| topic.name.as_ref().map(|name| name.0.to_string()))
                .collect()
        });
        let leader = broker_ids.first().copied().unwrap_or(BROKER_NODE_ID);
        let reported: Vec<MetadataResponseTopic> = match wanted {
            None => held
                .iter()
                .map(|(name, partitions)| topic_metadata(name, *partitions, leader, 0))
                .collect(),
            Some(names) => names
                .iter()
                .take(MAX_REQUEST_ENTRIES)
                .map(|name| match held.iter().find(|(held, _)| held == name) {
                    Some((name, partitions)) => topic_metadata(name, *partitions, leader, 0),
                    None => topic_metadata(
                        name,
                        0,
                        leader,
                        ResponseError::UnknownTopicOrPartition.code(),
                    ),
                })
                .collect(),
        };
        MetadataResponse::default()
            .with_brokers(
                broker_ids
                    .iter()
                    .map(|id| {
                        MetadataResponseBroker::default()
                            .with_node_id(BrokerId(*id))
                            .with_host(StrBytes::from_string(host.clone()))
                            .with_port(port)
                    })
                    .collect(),
            )
            .with_cluster_id(Some(StrBytes::from_string(self.cluster_arn.clone())))
            .with_controller_id(BrokerId(leader))
            .with_topics(reported)
    }

    /// `Produce`: append each submitted record batch to its partition's log, unless an
    /// ACL binding withholds the topic from the connection's principal.
    ///
    /// A records payload this endpoint cannot READ is answered `CORRUPT_MESSAGE` — the
    /// code Apache Kafka answers a batch it cannot parse — rather than with the offset of
    /// an append that did not happen: a producer is told its records were not written,
    /// instead of a success no log holds.
    ///
    /// Bounded as a whole like [`Self::fetch`]: at most [`MAX_REQUEST_ENTRIES`]
    /// (topic, partition) entries are served, with the store's lock taken and released
    /// per entry.
    fn produce(
        &self,
        request: &ProduceRequest,
        identity: Option<&Authenticated>,
    ) -> ProduceResponse {
        let now = SystemTime::now();
        let mut responses = Vec::new();
        let mut entries = 0usize;
        for topic in request.topic_data.iter() {
            let name = topic.name.0.to_string();
            let withheld = self.withholds(identity, &name, super::OPERATION_WRITE);
            let mut partitions = Vec::new();
            for partition in topic.partition_data.iter() {
                if entries >= MAX_REQUEST_ENTRIES {
                    continue;
                }
                entries += 1;
                if withheld {
                    partitions.push(
                        PartitionProduceResponse::default()
                            .with_index(partition.index)
                            .with_error_code(
                                ResponseError::TopicAuthorizationFailed.code(),
                            )
                            .with_base_offset(-1),
                    );
                    continue;
                }
                let Some(submitted) = decode_records(partition.records.clone()) else {
                    partitions.push(
                        PartitionProduceResponse::default()
                            .with_index(partition.index)
                            .with_error_code(ResponseError::CorruptMessage.code())
                            .with_base_offset(-1),
                    );
                    continue;
                };
                let appended = self.state.lock().data_plane_produce(
                    &self.cluster_arn,
                    &name,
                    partition.index,
                    submitted,
                    now,
                );
                partitions.push(match appended {
                    Ok(base_offset) => PartitionProduceResponse::default()
                        .with_index(partition.index)
                        .with_base_offset(base_offset),
                    Err(refusal) => PartitionProduceResponse::default()
                        .with_index(partition.index)
                        .with_error_code(refusal.code())
                        .with_base_offset(-1),
                });
            }
            responses.push(
                TopicProduceResponse::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_partition_responses(partitions),
            );
        }
        ProduceResponse::default().with_responses(responses)
    }

    /// `Fetch`: the records each requested partition holds from the requested offset
    /// on, with the ones the topic's retention has aged out already gone — unless an
    /// ACL binding withholds the topic from the connection's principal.
    ///
    /// BOUNDED as a whole, not per entry: at most [`MAX_REQUEST_ENTRIES`] (topic,
    /// partition) entries are served, each one once, and the records rendered across
    /// the whole response stop at [`MAX_FETCHED_RECORDS_PER_REQUEST`]. The store's lock
    /// is taken and released per entry, so one request cannot hold it for a whole
    /// batch.
    fn fetch(&self, request: &FetchRequest, identity: Option<&Authenticated>) -> FetchResponse {
        let now = SystemTime::now();
        let mut responses = Vec::new();
        let mut entries = 0usize;
        let mut rendered = 0usize;
        let mut served: BTreeSet<(String, i32)> = BTreeSet::new();
        for topic in request.topics.iter() {
            let name = topic.topic.0.to_string();
            let withheld = self.withholds(identity, &name, super::OPERATION_READ);
            let mut partitions = Vec::new();
            for partition in topic.partitions.iter() {
                if entries >= MAX_REQUEST_ENTRIES
                    || !served.insert((name.clone(), partition.partition))
                {
                    continue;
                }
                entries += 1;
                if withheld {
                    partitions.push(
                        PartitionData::default()
                            .with_partition_index(partition.partition)
                            .with_error_code(ResponseError::TopicAuthorizationFailed.code())
                            .with_high_watermark(-1)
                            .with_records(Some(Bytes::new())),
                    );
                    continue;
                }
                let remaining = MAX_FETCHED_RECORDS_PER_REQUEST.saturating_sub(rendered);
                let read = self.state.lock().data_plane_fetch(
                    &self.cluster_arn,
                    &name,
                    partition.partition,
                    partition.fetch_offset,
                    MAX_FETCHED_RECORDS.min(remaining),
                    now,
                );
                partitions.push(match read {
                    Ok(fetched) => {
                        rendered += fetched.records.len();
                        PartitionData::default()
                            .with_partition_index(partition.partition)
                            .with_high_watermark(fetched.high_watermark)
                            .with_last_stable_offset(fetched.high_watermark)
                            .with_log_start_offset(fetched.log_start_offset)
                            .with_records(encode_records(&fetched.records))
                    }
                    Err(refusal) => PartitionData::default()
                        .with_partition_index(partition.partition)
                        .with_error_code(refusal.code())
                        .with_high_watermark(-1)
                        .with_records(Some(Bytes::new())),
                });
            }
            responses.push(
                FetchableTopicResponse::default()
                    .with_topic(TopicName(StrBytes::from_string(name)))
                    .with_partitions(partitions),
            );
        }
        FetchResponse::default().with_responses(responses)
    }

    /// `ListOffsets`: where each requested partition's log begins and ends.
    ///
    /// The two timestamps a client asks with are the protocol's own
    /// ([`OFFSET_EARLIEST`], [`OFFSET_LATEST`]) and are answered from the log's own
    /// bounds without rendering a record; a real timestamp selects the first record at
    /// or after it. Bounded as a whole like [`Self::fetch`]: at most
    /// [`MAX_REQUEST_ENTRIES`] entries, each one once, with the store's lock taken and
    /// released per entry.
    fn list_offsets(&self, request: &ListOffsetsRequest) -> ListOffsetsResponse {
        let now = SystemTime::now();
        let mut topics = Vec::new();
        let mut entries = 0usize;
        let mut served: BTreeSet<(String, i32)> = BTreeSet::new();
        for topic in request.topics.iter() {
            let name = topic.name.0.to_string();
            let mut partitions = Vec::new();
            for partition in topic.partitions.iter() {
                if entries >= MAX_REQUEST_ENTRIES
                    || !served.insert((name.clone(), partition.partition_index))
                {
                    continue;
                }
                entries += 1;
                let answered = match partition.timestamp {
                    OFFSET_EARLIEST | OFFSET_LATEST => self
                        .state
                        .lock()
                        .data_plane_offsets(
                            &self.cluster_arn,
                            &name,
                            partition.partition_index,
                            now,
                        )
                        .map(|(earliest, latest)| match partition.timestamp {
                            OFFSET_EARLIEST => earliest,
                            _ => latest,
                        }),
                    wanted => self
                        .state
                        .lock()
                        .data_plane_fetch(
                            &self.cluster_arn,
                            &name,
                            partition.partition_index,
                            0,
                            MAX_FETCHED_RECORDS,
                            now,
                        )
                        .map(|fetched| {
                            fetched
                                .records
                                .iter()
                                .find(|record| record.timestamp_ms >= wanted)
                                .map(|record| record.offset)
                                .unwrap_or(-1)
                        }),
                };
                partitions.push(match answered {
                    Ok(offset) => ListOffsetsPartitionResponse::default()
                        .with_partition_index(partition.partition_index)
                        .with_timestamp(partition.timestamp)
                        .with_offset(offset),
                    Err(refusal) => ListOffsetsPartitionResponse::default()
                        .with_partition_index(partition.partition_index)
                        .with_error_code(refusal.code())
                        .with_offset(-1),
                });
            }
            topics.push(
                ListOffsetsTopicResponse::default()
                    .with_name(TopicName(StrBytes::from_string(name)))
                    .with_partitions(partitions),
            );
        }
        ListOffsetsResponse::default().with_topics(topics)
    }

    /// `DescribeConfigs`: the configuration of a topic of this cluster, or of this
    /// broker.
    ///
    /// A topic's entries are the ones `CreateTopic`/`UpdateTopic` stored, reported as
    /// the topic's own, over Apache Kafka's documented defaults, reported as defaults —
    /// so a client that set `retention.ms` reads back what it set, and one that asks
    /// about a value nobody set reads the default a broker would apply.
    ///
    /// Bounded as a whole, like [`Self::fetch`]: at most [`MAX_REQUEST_ENTRIES`]
    /// resources are served, the broker's defaults are built ONCE for the request
    /// rather than per resource, and the store's lock is taken and released per
    /// resource, so one request cannot hold it for a whole batch.
    fn describe_configs(&self, request: &DescribeConfigsRequest) -> DescribeConfigsResponse {
        let now = SystemTime::now();
        let broker_defaults: BTreeMap<String, String> = super::BROKER_CONFIG_DEFAULTS
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        let mut results = Vec::new();
        for resource in request.resources.iter().take(MAX_REQUEST_ENTRIES) {
            let name = resource.resource_name.to_string();
            let wanted: Option<Vec<String>> = resource
                .configuration_keys
                .as_ref()
                .map(|keys| keys.iter().map(ToString::to_string).collect());
            let entries = match resource.resource_type {
                RESOURCE_TYPE_TOPIC => self
                    .state
                    .lock()
                    .data_plane_topic_configuration(&self.cluster_arn, &name, now)
                    .map(|configuration| {
                        rendered_configuration(
                            &configuration.effective,
                            &configuration.set,
                            wanted.as_deref(),
                        )
                    }),
                RESOURCE_TYPE_BROKER => Some(rendered_configuration(
                    &broker_defaults,
                    &BTreeMap::new(),
                    wanted.as_deref(),
                )),
                _ => None,
            };
            results.push(match entries {
                Some(configs) => DescribeConfigsResult::default()
                    .with_resource_type(resource.resource_type)
                    .with_resource_name(StrBytes::from_string(name))
                    .with_configs(configs),
                None => DescribeConfigsResult::default()
                    .with_error_code(ResponseError::UnknownTopicOrPartition.code())
                    .with_error_message(Some(StrBytes::from_static_str(
                        "This server does not host this topic-partition.",
                    )))
                    .with_resource_type(resource.resource_type)
                    .with_resource_name(StrBytes::from_string(name)),
            });
        }
        DescribeConfigsResponse::default().with_results(results)
    }

    /// `FindCoordinator`: the broker that coordinates the group or transaction a client
    /// named — this endpoint, which is the broker that holds the group.
    ///
    /// Answered in the shape the NEGOTIATED version has: the single coordinator a request
    /// up to version 3 asks about, carried in the response's own members, and from
    /// version 4 on the per-key list, which is the only place those versions carry it.
    fn find_coordinator(
        &self,
        request: &FindCoordinatorRequest,
        api_version: i16,
    ) -> FindCoordinatorResponse {
        let (host, port) = split_authority(&self.authority());
        if api_version >= 4 {
            return FindCoordinatorResponse::default().with_coordinators(
                request
                    .coordinator_keys
                    .iter()
                    .take(MAX_REQUEST_ENTRIES)
                    .map(|key| {
                        ::kafka_protocol::messages::find_coordinator_response::Coordinator::default()
                            .with_key(key.clone())
                            .with_node_id(BrokerId(BROKER_NODE_ID))
                            .with_host(StrBytes::from_string(host.clone()))
                            .with_port(port)
                    })
                    .collect(),
            );
        }
        FindCoordinatorResponse::default()
            .with_node_id(BrokerId(BROKER_NODE_ID))
            .with_host(StrBytes::from_string(host))
            .with_port(port)
    }
}

/// The error a group call is answered with when this endpoint will not hold the group
/// the client named: the protocol's `COORDINATOR_NOT_AVAILABLE`, which is what a client
/// retries on or reports.
fn no_coordinator() -> i16 {
    ResponseError::CoordinatorNotAvailable.code()
}

/// Whether `api_version` of `key` is one this endpoint serves.
fn served(key: ApiKey, api_version: i16) -> bool {
    SERVED_APIS
        .iter()
        .any(|(served, min, max)| *served == key && api_version >= *min && api_version <= *max)
}

/// The `ApiVersions` answer: exactly the APIs and versions [`SERVED_APIS`] names.
fn api_versions() -> ApiVersionsResponse {
    ApiVersionsResponse::default().with_api_keys(
        SERVED_APIS
            .iter()
            .map(|(key, min, max)| {
                ApiVersion::default()
                    .with_api_key(*key as i16)
                    .with_min_version(*min)
                    .with_max_version(*max)
            })
            .collect(),
    )
}

/// One topic as `Metadata` reports it: every partition led by `leader`, the first
/// broker node the cluster holds, which is the broker that holds the log.
fn topic_metadata(
    name: &str,
    partitions: i32,
    leader: i32,
    error_code: i16,
) -> MetadataResponseTopic {
    MetadataResponseTopic::default()
        .with_error_code(error_code)
        .with_name(Some(TopicName(StrBytes::from_string(name.to_string()))))
        .with_partitions(
            (0..partitions)
                .map(|index| {
                    MetadataResponsePartition::default()
                        .with_partition_index(index)
                        .with_leader_id(BrokerId(leader))
                        .with_replica_nodes(vec![BrokerId(leader)])
                        .with_isr_nodes(vec![BrokerId(leader)])
                })
                .collect(),
        )
}

/// `host:port` split into the two members `Metadata` reports them as, with an IPv6
/// address unbracketed — the form a client dials.
fn split_authority(authority: &str) -> (String, i32) {
    match authority.rsplit_once(':') {
        Some((host, port)) => (
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_string(),
            port.parse().unwrap_or_default(),
        ),
        None => (authority.to_string(), 0),
    }
}

/// The configuration entries a `DescribeConfigs` result reports: every entry of
/// `effective`, each marked as the resource's own when `set` carries it and as a
/// default otherwise, narrowed to `wanted` when the request named keys.
fn rendered_configuration(
    effective: &BTreeMap<String, String>,
    set: &BTreeMap<String, String>,
    wanted: Option<&[String]>,
) -> Vec<DescribeConfigsResourceResult> {
    effective
        .iter()
        .filter(|(name, _)| wanted.is_none_or(|keys| keys.iter().any(|key| key == *name)))
        .map(|(name, value)| {
            let configured = set.contains_key(name);
            DescribeConfigsResourceResult::default()
                .with_name(StrBytes::from_string(name.clone()))
                .with_value(Some(StrBytes::from_string(value.clone())))
                .with_read_only(false)
                .with_config_source(match configured {
                    true => CONFIG_SOURCE_SET_BY_RESOURCE,
                    false => CONFIG_SOURCE_DEFAULT,
                })
        })
        .collect()
}

/// The records a produce submitted, as `(timestamp, key, value)` triples. A request that
/// carries NO records payload submits none, which is nothing to append; `None` is a
/// payload that is present and that this endpoint cannot read, which is a produce it
/// refuses rather than reports as appended.
fn decode_records(records: Option<Bytes>) -> Option<Vec<super::SubmittedRecord>> {
    let Some(mut encoded) = records else {
        return Some(Vec::new());
    };
    if encoded.is_empty() {
        return Some(Vec::new());
    }
    let decoded = RecordBatchDecoder::decode(&mut encoded).ok()?;
    Some(
        decoded
            .records
            .iter()
            .map(|record| {
                (
                    record.timestamp,
                    record.key.as_ref().map(|key| key.to_vec()),
                    record.value.as_ref().map(|value| value.to_vec()),
                )
            })
            .collect(),
    )
}

/// The records a fetch answers with, in the record-batch form the protocol carries
/// them in. A fetch that read nothing answers an EMPTY record set rather than a null
/// one: that is what a broker sends a client whose offset is at the end of the log, and
/// a client reads the field without first asking whether it is there.
fn encode_records(records: &[super::StoredRecord]) -> Option<Bytes> {
    if records.is_empty() {
        return Some(Bytes::new());
    }
    let batch: Vec<Record> = records
        .iter()
        .map(|record| Record {
            transactional: false,
            control: false,
            partition_leader_epoch: 0,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: record.offset,
            sequence: i32::try_from(record.offset).unwrap_or(i32::MAX),
            timestamp: record.timestamp_ms,
            key: record.key.clone().map(Bytes::from),
            value: record.value.clone().map(Bytes::from),
            headers: Default::default(),
            // A record this endpoint writes is never one the compactor has already
            // passed its tombstone horizon, which is what this flag marks.
            delete_horizon: false,
        })
        .collect();
    let mut encoded = BytesMut::new();
    let options = RecordEncodeOptions {
        version: RECORD_BATCH_VERSION,
        compression: Compression::None,
    };
    RecordBatchEncoder::encode(&mut encoded, batch.iter(), &options).ok()?;
    Some(encoded.freeze())
}
