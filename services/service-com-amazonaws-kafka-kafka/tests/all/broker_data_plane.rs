// SPDX-License-Identifier: CC0-1.0

//! The DATA PLANE of an Amazon MSK cluster: that every `host:port` the control plane
//! announces is an address THIS emulator accepts an Apache Kafka connection on, and
//! that a client taking the string verbatim reads the topic configuration the control
//! plane stored, produces a record and fetches it back — and no longer fetches one the
//! topic's retention has aged out.
//!
//! RULE under test: an address a service publishes for a caller to connect to is one
//! the service itself answers on, speaking the protocol the published member is for.
//! For `GetBootstrapBrokers` that is the Apache Kafka wire protocol, so each row below
//! dials a published string and talks to it.
//!
//! The tables span what the rule generalises over — which ACCESS PATH the string names
//! (plaintext, TLS, SASL/SCRAM, SASL/IAM, and their public and multi-VPC siblings),
//! whether the wire is ENCRYPTED, which CONFIGURATION the topic carries, how many
//! RECORDS are produced and how OLD they are against the topic's retention, and which
//! CREDENTIAL the SASL exchange is offered. The recorded case (a SASL endpoint, a
//! topic with `retention.ms` set, one record read from the earliest offset) is one row
//! of it.
//!
//! Every assertion goes over a real socket the core opened for the cluster, dialled at
//! the address `GetBootstrapBrokers` published; nothing here reaches into the
//! service's state.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ::bytes::{Bytes, BytesMut};
use ::kafka_protocol::messages::{
    describe_configs_request::DescribeConfigsResource,
    fetch_request::{FetchPartition, FetchTopic},
    list_offsets_request::{ListOffsetsPartition, ListOffsetsTopic},
    produce_request::{PartitionProduceData, TopicProduceData},
    ApiKey, ApiVersionsRequest, ApiVersionsResponse, DescribeConfigsRequest,
    DescribeConfigsResponse, FetchRequest, FetchResponse, ListOffsetsRequest, ListOffsetsResponse,
    MetadataRequest, MetadataResponse, ProduceRequest, ProduceResponse, RequestHeader,
    ResponseHeader, SaslAuthenticateRequest, SaslAuthenticateResponse, SaslHandshakeRequest,
    SaslHandshakeResponse, TopicName,
};
use ::kafka_protocol::protocol::{Decodable, Encodable, StrBytes};
use ::kafka_protocol::records::{
    Compression, Record, RecordBatchDecoder, RecordBatchEncoder, RecordEncodeOptions, TimestampType,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::kafka_harness::{pct, sleep_past_activation, ClusterRequest, Emulator, REGION};

/// The interface the core binds a resource listener on by default, which is the
/// interface every published bootstrap address resolves to on this machine: a broker
/// host is published in the zone a resolver answers with that interface (RFC 6761
/// reserves `localhost` and every name under it for it), so this is the address the
/// dialling below opens its connection to.
pub(crate) const LOOPBACK: &str = "127.0.0.1";

/// The client id every connection below identifies itself with.
pub(crate) const CLIENT_ID: &str = "sim2real-data-plane";

/// The SASL mechanism names a client selects, as Amazon MSK's brokers offer them.
pub(crate) const SCRAM_SHA_512: &str = "SCRAM-SHA-512";
pub(crate) const AWS_MSK_IAM: &str = "AWS_MSK_IAM";
pub(crate) const OAUTHBEARER: &str = "OAUTHBEARER";

/// The username and password the SASL/SCRAM secret in this suite carries.
const SCRAM_USER: &str = "scram-user";
const SCRAM_PASSWORD: &str = "scram-Secret-99";

/// A customer managed KMS key, which is the only kind of key a secret a cluster
/// associates may be encrypted with.
const CUSTOMER_KEY_ARN: &str =
    "arn:aws:kms:us-west-2:000000000000:key/9b1dca1e-7c4f-4a6e-8f0a-5b2d7c3e1f40";

/// A byte stream a Kafka session runs on: a plaintext socket, or the TLS stream over
/// one.
type Session = Box<dyn Stream>;

/// What a session needs of its connection.
trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Stream for T {}

/// One client on one published address.
///
/// The suites of this crate that drive a cluster's Apache Kafka endpoint share it, so a
/// new rule about the data plane adds its own suite rather than a second client.
pub(crate) struct Client {
    session: Session,
    correlation: i32,
    /// The name this client goes by, which every request frame it sends carries and a
    /// reader of a consumer group reports its member under.
    client_id: String,
    /// Whether this connection's SASL tokens travel UNWRAPPED, which is what the
    /// version it handshaked at selects: bare, length-prefixed byte strings at version 0
    /// and `SaslAuthenticate` requests from version 1 on.
    unwrapped_sasl: bool,
}

impl Client {
    /// Dial `authority` in the clear, after asserting it is an address of this
    /// machine: the core binds its listeners on the operator-chosen interface,
    /// loopback by default, so a published bootstrap address is this machine's own —
    /// and that is the address dialled.
    pub(crate) async fn plaintext(case: &str, authority: &str) -> Self {
        Self {
            session: Box::new(dial(case, authority).await),
            correlation: 0,
            client_id: CLIENT_ID.to_string(),
            unwrapped_sasl: false,
        }
    }

    /// The same client under the name `client_id`, which is how a suite drives two
    /// members a reader of the group must be able to tell apart.
    pub(crate) fn identified_as(mut self, client_id: &str) -> Self {
        self.client_id = client_id.to_string();
        self
    }

    /// The same address over TLS, which is how a client reaches one of the encrypted
    /// paths — the wire the core terminates.
    pub(crate) async fn tls(case: &str, authority: &str) -> Self {
        let conn = dial(case, authority).await;
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(Arc::clone(&provider))
            .with_safe_default_protocol_versions()
            .expect("the provider supports the default protocol versions")
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptTheEmulatorsCertificate(provider)))
            .with_no_client_auth();
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls::pki_types::ServerName::try_from("localhost")
            .expect("localhost is a server name");
        let stream = connector
            .connect(server_name, conn)
            .await
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: the cluster's socket must complete a TLS handshake, got {error}"
                )
            });
        Self {
            session: Box::new(stream),
            correlation: 0,
            client_id: CLIENT_ID.to_string(),
            unwrapped_sasl: false,
        }
    }

    /// Send one request and read its response, framed exactly as the protocol frames
    /// them: a length, the request header for this API and version, and the body.
    pub(crate) async fn call<Request: Encodable, Response: Decodable>(
        &mut self,
        case: &str,
        key: ApiKey,
        version: i16,
        request: &Request,
    ) -> Response {
        self.correlation += 1;
        let correlation = self.correlation;
        let mut frame = BytesMut::new();
        RequestHeader::default()
            .with_request_api_key(key as i16)
            .with_request_api_version(version)
            .with_correlation_id(correlation)
            .with_client_id(Some(StrBytes::from_string(self.client_id.clone())))
            .encode(&mut frame, key.request_header_version(version))
            .unwrap_or_else(|error| panic!("case {case:?}: the header must encode: {error}"));
        request
            .encode(&mut frame, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the request must encode: {error}"));
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        self.session
            .write_all(&length.to_be_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        self.session
            .write_all(&frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be written: {error}"));
        let mut answered = self.read_frame(case).await.unwrap_or_else(|| {
            panic!("case {case:?}: the broker must answer a {key:?} v{version} request")
        });
        ResponseHeader::decode(&mut answered, key.response_header_version(version))
            .unwrap_or_else(|error| panic!("case {case:?}: the response header must decode: {error}"));
        Response::decode(&mut answered, version)
            .unwrap_or_else(|error| panic!("case {case:?}: the response must decode: {error}"))
    }

    /// The next response frame, or `None` when the broker closed the connection
    /// instead of answering.
    async fn read_frame(&mut self, case: &str) -> Option<Bytes> {
        let mut length = [0u8; 4];
        if self.session.read_exact(&mut length).await.is_err() {
            return None;
        }
        let length = u32::from_be_bytes(length) as usize;
        assert!(
            length > 0 && length < 8 * 1024 * 1024,
            "case {case:?}: the broker answered a frame of {length} bytes"
        );
        let mut frame = vec![0u8; length];
        self.session
            .read_exact(&mut frame)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the frame must be read: {error}"));
        Some(Bytes::from(frame))
    }

    /// `ApiVersions`, the first call every Kafka client makes.
    pub(crate) async fn api_versions(&mut self, case: &str) -> ApiVersionsResponse {
        self.call(
            case,
            ApiKey::ApiVersions,
            3,
            &ApiVersionsRequest::default()
                .with_client_software_name(StrBytes::from_static_str(CLIENT_ID))
                .with_client_software_version(StrBytes::from_static_str("1")),
        )
        .await
    }

    /// `Metadata` for every topic of the cluster.
    pub(crate) async fn metadata(&mut self, case: &str) -> MetadataResponse {
        self.call(
            case,
            ApiKey::Metadata,
            9,
            &MetadataRequest::default().with_topics(None),
        )
        .await
    }

    /// Complete the SASL exchange `mechanism` defines, answering the error code of its
    /// last leg: `0` for an authenticated connection.
    ///
    /// The tokens travel inside `SaslAuthenticate`, which is the framing a handshake at
    /// version 1 selects; [`Client::authenticate_at`] drives the other one.
    pub(crate) async fn authenticate(&mut self, case: &str, mechanism: &str, password: &str) -> i16 {
        self.authenticate_as(case, mechanism, SCRAM_USER, password)
            .await
    }

    /// The same exchange offered as `user`, which is the SASL/SCRAM user the credential
    /// belongs to — the name the cluster's associated secrets are searched for. The IAM
    /// exchanges carry their principal in the signed payload instead, so they ignore it.
    pub(crate) async fn authenticate_as(
        &mut self,
        case: &str,
        mechanism: &str,
        user: &str,
        password: &str,
    ) -> i16 {
        self.authenticate_at(case, mechanism, user, password, 1)
            .await
    }

    /// The same exchange, with the tokens in the framing a `SaslHandshake` at
    /// `handshake_version` selects: bare, length-prefixed byte strings at version 0 and
    /// `SaslAuthenticate` requests from version 1 on.
    async fn authenticate_at(
        &mut self,
        case: &str,
        mechanism: &str,
        user: &str,
        password: &str,
        handshake_version: i16,
    ) -> i16 {
        let handshake: SaslHandshakeResponse = self
            .call(
                case,
                ApiKey::SaslHandshake,
                handshake_version,
                &SaslHandshakeRequest::default()
                    .with_mechanism(StrBytes::from_string(mechanism.to_string())),
            )
            .await;
        if handshake.error_code != 0 {
            return handshake.error_code;
        }
        self.unwrapped_sasl = handshake_version == 0;
        match mechanism {
            OAUTHBEARER => {
                // RFC 7628 §3.1: the GS2 header, then `auth=Bearer <token>`. The token
                // Amazon MSK's auth-token signer produces is the signed
                // `kafka-cluster:Connect` request, base64url-encoded.
                let signed = format!(
                    "https://kafka.{REGION}.amazonaws.com/?Action=kafka-cluster%3AConnect\
                     &X-Amz-Algorithm=AWS4-HMAC-SHA256\
                     &X-Amz-Credential=AKIDEXAMPLE%2F20261101%2F{REGION}%2Fkafka-cluster\
                     %2Faws4_request&X-Amz-Date=20261101T000000Z&X-Amz-Expires=900\
                     &X-Amz-SignedHeaders=host&X-Amz-Signature={password}"
                );
                let token = ::aws_smithy_types::base64::encode(&signed)
                    .replace('+', "-")
                    .replace('/', "_")
                    .trim_end_matches('=')
                    .to_string();
                match self
                    .sasl_token(
                        case,
                        format!("n,,\x01auth=Bearer {token}\x01\x01").into_bytes(),
                    )
                    .await
                {
                    Ok(_) => 0,
                    Err(code) => code,
                }
            }
            AWS_MSK_IAM => {
                let payload = json!({
                    "version": "2020_10_22",
                    "host": LOOPBACK,
                    "user-agent": CLIENT_ID,
                    "action": "kafka-cluster:Connect",
                    "x-amz-algorithm": "AWS4-HMAC-SHA256",
                    "x-amz-credential":
                        format!("AKIDEXAMPLE/20261101/{REGION}/kafka-cluster/aws4_request"),
                    "x-amz-date": "20261101T000000Z",
                    "x-amz-signedheaders": "host",
                    "x-amz-signature": password,
                });
                match self
                    .sasl_token(case, payload.to_string().into_bytes())
                    .await
                {
                    Ok(_) => 0,
                    Err(code) => code,
                }
            }
            _ => self.scram(case, user, password).await,
        }
    }

    /// One leg of the exchange: `token` sent in this connection's framing, answered with
    /// the server's token.
    ///
    /// `Err` carries the error code the leg was refused with. The unwrapped framing has
    /// nowhere to carry one — a broker refusing such an exchange closes the connection,
    /// which is what version 0 of the handshake defines — so a closed connection is
    /// reported as the authentication failure it is.
    async fn sasl_token(&mut self, case: &str, token: Vec<u8>) -> Result<Bytes, i16> {
        if !self.unwrapped_sasl {
            let answered: SaslAuthenticateResponse = self
                .call(
                    case,
                    ApiKey::SaslAuthenticate,
                    2,
                    &SaslAuthenticateRequest::default().with_auth_bytes(Bytes::from(token)),
                )
                .await;
            return match answered.error_code {
                0 => Ok(answered.auth_bytes),
                code => Err(code),
            };
        }
        let length = u32::try_from(token.len()).expect("a SASL token fits in a u32");
        self.session
            .write_all(&length.to_be_bytes())
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the token must be written: {error}"));
        self.session
            .write_all(&token)
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: the token must be written: {error}"));
        self.server_token()
            .await
            .ok_or_else(|| ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code())
    }

    /// The server's next UNWRAPPED token: its length, then that many bytes. An EMPTY
    /// token is a token — it is how a mechanism whose last leg carries no data says the
    /// exchange succeeded — so a zero length is read as one rather than as a closed
    /// connection.
    async fn server_token(&mut self) -> Option<Bytes> {
        let mut length = [0u8; 4];
        if self.session.read_exact(&mut length).await.is_err() {
            return None;
        }
        let length = u32::from_be_bytes(length) as usize;
        if length > 8 * 1024 * 1024 {
            return None;
        }
        let mut token = vec![0u8; length];
        self.session.read_exact(&mut token).await.ok()?;
        Some(Bytes::from(token))
    }

    /// The SCRAM-SHA-512 exchange of RFC 5802, client side, with each leg sent in this
    /// connection's token framing.
    async fn scram(&mut self, case: &str, user: &str, password: &str) -> i16 {
        let client_nonce = "fyko+d2lbbFgONRv9qkxdawL";
        let client_first_bare = format!("n={user},r={client_nonce}");
        let first = match self
            .sasl_token(case, format!("n,,{client_first_bare}").into_bytes())
            .await
        {
            Ok(answered) => answered,
            Err(code) => return code,
        };
        let server_first = String::from_utf8(first.to_vec())
            .unwrap_or_else(|_| panic!("case {case:?}: the server-first message must be text"));
        let mut combined_nonce = String::new();
        let mut salt = Vec::new();
        let mut iterations = 0u32;
        for attribute in server_first.split(',') {
            match attribute.split_once('=') {
                Some(("r", value)) => combined_nonce = value.to_string(),
                Some(("s", value)) => {
                    salt = ::aws_smithy_types::base64::decode(value).unwrap_or_else(|error| {
                        panic!("case {case:?}: the salt must be base64: {error}")
                    })
                }
                Some(("i", value)) => iterations = value.parse().unwrap_or_default(),
                _ => {}
            }
        }
        assert!(
            combined_nonce.starts_with(client_nonce),
            "case {case:?}: the server nonce must extend the client's, got {combined_nonce:?}"
        );
        assert!(
            iterations >= 4096,
            "case {case:?}: RFC 5802 requires at least 4096 iterations, got {iterations}"
        );
        let without_proof = format!("c=biws,r={combined_nonce}");
        let auth_message = format!("{client_first_bare},{server_first},{without_proof}");
        let mut salted = [0u8; 64];
        ::pbkdf2::pbkdf2_hmac::<::sha2::Sha512>(
            password.as_bytes(),
            &salt,
            iterations,
            &mut salted,
        );
        let client_key = hmac_sha512(&salted, b"Client Key");
        let stored_key = {
            use ::sha2::Digest;
            ::sha2::Sha512::digest(&client_key).to_vec()
        };
        let client_signature = hmac_sha512(&stored_key, auth_message.as_bytes());
        let proof: Vec<u8> = client_key
            .iter()
            .zip(client_signature.iter())
            .map(|(key, signature)| key ^ signature)
            .collect();
        let client_final = format!(
            "{without_proof},p={}",
            ::aws_smithy_types::base64::encode(&proof)
        );
        let last = self
            .sasl_token(case, client_final.into_bytes())
            .await;
        match last {
            Ok(answered) => {
                let server_final = String::from_utf8(answered.to_vec()).unwrap_or_default();
                let server_key = hmac_sha512(&salted, b"Server Key");
                let expected = ::aws_smithy_types::base64::encode(hmac_sha512(
                    &server_key,
                    auth_message.as_bytes(),
                ));
                assert_eq!(
                    server_final,
                    format!("v={expected}"),
                    "case {case:?}: the server must prove it holds the same credential"
                );
                0
            }
            Err(code) => code,
        }
    }
}

/// `HMAC(key, data)` over SHA-512, the client half of the SCRAM computation.
fn hmac_sha512(key: &[u8], data: &[u8]) -> Vec<u8> {
    use ::hmac::digest::KeyInit;
    use ::hmac::Mac;

    let mut mac = <::hmac::Hmac<::sha2::Sha512> as KeyInit>::new_from_slice(key)
        .expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// Open a TCP connection to a published authority, asserting first that the address is
/// one of this emulator's own.
///
/// A published pair names the BROKER NODE in its host and the access method in its port
/// (Amazon MSK publishes one port per method, shared by every node), and the host sits in
/// the zone that names the interface the core bound the socket on — so the host is
/// resolved to that interface here, exactly as a resolver answers a name under the
/// loopback zone, and the port is what picks the socket out.
async fn dial(case: &str, authority: &str) -> TcpStream {
    let (host, port) = authority
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} is not a host:port pair"));
    assert!(
        ::service_common::data_plane_addressing::names_the_loopback_interface(host),
        "case {case:?}: a published bootstrap address must name this emulator's own \
         interface, got {authority:?}"
    );
    let port: u16 = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: {port:?} is not a port"));
    // The loopback literal is named here rather than through [`LOOPBACK`] so the address
    // this connection goes to is readable on the line that opens it.
    TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {authority} must accept a connection, got {error}")
        })
}

/// The verifier a client of the emulator's OWN socket uses: the certificate is the one
/// the core generated for its listeners, self-signed and naming loopback, and this
/// client is the emulator's own test connecting to a port the same process opened — so
/// there is no authority to check it against.
#[derive(Debug)]
struct AcceptTheEmulatorsCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptTheEmulatorsCertificate {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// Create a topic on `cluster`, with `configs` as the Base64 topic-configuration
/// string the API takes, and wait out its `CREATING` span.
pub(crate) async fn given_topic(
    emulator: &Emulator,
    cluster: &str,
    name: &str,
    partitions: i64,
    configs: Option<Value>,
) {
    let mut body = json!({
        "topicName": name,
        "partitionCount": partitions,
        "replicationFactor": 1,
    });
    if let Some(configs) = configs {
        body["configs"] = json!(::aws_smithy_types::base64::encode(configs.to_string()));
    }
    let reply = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/topics", pct(cluster)),
            Some(body),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "setup: CreateTopic({name}) must be accepted, got {} {}",
        reply.status, reply.body
    );
    std::thread::sleep(Duration::from_millis(750));
}

/// Every `host:port` pair of every bootstrap string a cluster publishes, member by
/// member: one pair per broker node of the cluster, in the order the string lists them.
async fn published_pairs(
    emulator: &Emulator,
    cluster: &str,
    case: &str,
) -> BTreeMap<String, Vec<String>> {
    let reply = emulator.bootstrap_brokers(cluster).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: GetBootstrapBrokers must answer, got {} {}",
        reply.status, reply.body
    );
    reply
        .body
        .as_object()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(member, value)| {
            value.as_str().map(|published| {
                (
                    member,
                    published
                        .split(',')
                        .map(|pair| pair.trim().to_string())
                        .collect(),
                )
            })
        })
        .collect()
}

/// The FIRST `host:port` pair of each bootstrap string a cluster publishes, member by
/// member: a client bootstraps against any one of the brokers the string lists, and this
/// is the one the suites below dial.
async fn published(emulator: &Emulator, cluster: &str, case: &str) -> BTreeMap<String, String> {
    published_pairs(emulator, cluster, case)
        .await
        .into_iter()
        .filter_map(|(member, pairs)| pairs.into_iter().next().map(|first| (member, first)))
        .collect()
}

/// RULE: every `host:port` `GetBootstrapBrokers` publishes is an address this emulator
/// accepts an Apache Kafka connection on, and the broker it reports in `Metadata` is
/// that same address — so a client that re-resolves the cluster from its metadata
/// keeps talking to the emulator.
#[tokio::test]
async fn every_published_bootstrap_address_answers_the_kafka_protocol() {
    // The rows differ along the access paths the cluster enables, which is what
    // decides WHICH members are published and what each one requires of a client.
    let rows: &[(&str, ClusterRequest)] = &[
        (
            "a cluster reached in the clear and over TLS",
            ClusterRequest::named("plane-plaintext"),
        ),
        (
            "a TLS-only cluster",
            ClusterRequest {
                client_broker: Some("TLS"),
                ..ClusterRequest::named("plane-tls")
            },
        ),
        (
            "a cluster authenticating clients with SASL/SCRAM",
            ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                ..ClusterRequest::named("plane-scram")
            },
        ),
        (
            "a cluster authenticating clients with IAM access control",
            ClusterRequest {
                client_broker: Some("TLS"),
                sasl_iam: Some(true),
                ..ClusterRequest::named("plane-iam")
            },
        ),
        (
            "a public, dual-stack cluster with both SASL schemes",
            ClusterRequest {
                client_broker: Some("TLS"),
                sasl_scram: Some(true),
                sasl_iam: Some(true),
                public_access: Some("SERVICE_PROVIDED_EIPS"),
                network_type: Some("DUAL"),
                vpc_tls: Some(true),
                vpc_iam: Some(true),
                ..ClusterRequest::named("plane-public")
            },
        ),
    ];
    for (case, request) in rows {
        let emulator = Emulator::with_secrets_manager();
        let cluster = emulator.create_ok(request).await;
        sleep_past_activation();
        if request.sasl_scram == Some(true) {
            given_secret_associated(&emulator, &cluster, "plane-secret", case).await;
        }
        given_topic(&emulator, &cluster, "orders", 1, None).await;

        let addresses = published_pairs(&emulator, &cluster, case).await;
        assert!(
            !addresses.is_empty(),
            "case {case:?}: a cluster with brokers must publish a bootstrap string"
        );
        for (member, pairs) in &addresses {
            // The FIRST broker of the member's string is the one dialled here; that every
            // pair of it is dialable is `bootstrap_broker_cardinality`'s subject.
            let authority = pairs
                .first()
                .unwrap_or_else(|| panic!("case {case:?}: {member} carries no host:port pair"));
            let probe = format!("{case} / {member}");
            // Every published member is dialled, and the ones a client of AWS reaches
            // over TLS are dialled over TLS.
            let mut client = match member.contains("Tls") || member.contains("Sasl") {
                true => Client::tls(&probe, authority).await,
                false => Client::plaintext(&probe, authority).await,
            };
            let negotiated = client.api_versions(&probe).await;
            assert_eq!(
                negotiated.error_code, 0,
                "case {probe:?}: ApiVersions must be answered, got error {}",
                negotiated.error_code
            );
            assert!(
                !negotiated.api_keys.is_empty(),
                "case {probe:?}: the broker must advertise the APIs it serves"
            );
            if member.contains("Sasl") {
                // Which mechanism is the member's own: a SASL/SCRAM endpoint
                // authenticates with the cluster's secret, a SASL/IAM one with a
                // signed IAM payload.
                let mechanism = match member.contains("SaslScram") {
                    true => SCRAM_SHA_512,
                    false => AWS_MSK_IAM,
                };
                let code = client.authenticate(&probe, mechanism, SCRAM_PASSWORD).await;
                assert_eq!(
                    code, 0,
                    "case {probe:?}: the {mechanism} exchange must authenticate the connection"
                );
            }
            let metadata = client.metadata(&probe).await;
            let brokers: Vec<String> = metadata
                .brokers
                .iter()
                .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
                .collect();
            assert!(
                brokers.contains(authority),
                "case {probe:?}: Metadata must report the address the client dialled \
                 ({authority}), got {brokers:?}"
            );
            // And it reports every OTHER pair of the same string too: the brokers the
            // cluster holds are one list, whichever of them the client reached.
            assert!(
                pairs.iter().all(|pair| brokers.contains(pair)),
                "case {probe:?}: Metadata must report every published broker {pairs:?}, got \
                 {brokers:?}"
            );
            let topics: Vec<String> = metadata
                .topics
                .iter()
                .filter_map(|topic| topic.name.as_ref().map(|name| name.0.to_string()))
                .collect();
            assert_eq!(
                topics,
                vec!["orders".to_string()],
                "case {probe:?}: Metadata must report the cluster's topics, got {topics:?}"
            );
        }
    }

    // And the one access path an MSK SERVERLESS cluster has: its clients authenticate
    // with IAM access control over TLS, and the single string it publishes names the
    // socket this emulator serves that path on. A serverless cluster keeps its client
    // authentication in its own `Serverless` record, which is where the mechanisms
    // offered on that socket come from.
    let case = "a serverless cluster's one access path";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_serverless_ok("plane-serverless", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    sleep_past_activation();
    let addresses = published(&emulator, &cluster, case).await;
    assert_eq!(
        addresses.keys().cloned().collect::<Vec<_>>(),
        vec!["bootstrapBrokerStringSaslIam".to_string()],
        "case {case:?}: a serverless cluster publishes exactly the SASL/IAM string, got \
         {addresses:?}"
    );
    let authority = addresses
        .values()
        .next()
        .expect("the serverless cluster published its one address")
        .clone();
    // The mechanisms that socket offers are the ones the SERVERLESS record's own client
    // authentication turns on — IAM access control, as the Java exchange and as the
    // `OAUTHBEARER` token a non-Java client uses — and nothing else.
    for (mechanism, expected) in [
        (AWS_MSK_IAM, 0),
        (OAUTHBEARER, 0),
        (
            SCRAM_SHA_512,
            ::kafka_protocol::ResponseError::UnsupportedSaslMechanism.code(),
        ),
    ] {
        let mut client = Client::tls(case, &authority).await;
        client.api_versions(case).await;
        let code = client.authenticate(case, mechanism, "a-signature").await;
        assert_eq!(
            code, expected,
            "case {case:?}: expected the {mechanism} exchange to answer {expected}, got {code}"
        );
    }
    // And an authenticated connection on it is served: the metadata reports the address
    // the client dialled.
    let mut client = Client::tls(case, &authority).await;
    client.api_versions(case).await;
    assert_eq!(
        client.authenticate(case, AWS_MSK_IAM, "a-signature").await,
        0,
        "case {case:?}: the IAM exchange must authenticate the connection"
    );
    let metadata = client.metadata(case).await;
    let brokers: Vec<String> = metadata
        .brokers
        .iter()
        .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
        .collect();
    assert!(
        brokers.contains(&authority),
        "case {case:?}: Metadata must report the address the client dialled ({authority}), got \
         {brokers:?}"
    );
}

/// RULE: the brokers a client learns from `Metadata` are the broker nodes the cluster
/// holds — one entry per node, each at its OWN address, the one the bootstrap string
/// publishes for that node, with the first of them the controller — so a client sent to
/// any node of the cluster reaches the endpoint this emulator serves that node on.
///
/// The rows differ along the number of brokers the cluster holds, which is what the rule
/// counts over.
#[tokio::test]
async fn metadata_reports_every_broker_node_the_cluster_holds() {
    // (case, the brokers the cluster is created with, the subnets they are placed in)
    let rows: &[(&str, i64, &[&str])] = &[
        ("one broker", 1, &["subnet-a"]),
        ("two brokers", 2, &["subnet-a", "subnet-b"]),
        ("three brokers", 3, &["subnet-a", "subnet-b", "subnet-c"]),
    ];
    for (case, brokers, subnets) in rows {
        let emulator = Emulator::new();
        let cluster = emulator
            .create_ok(&ClusterRequest {
                client_broker: Some("PLAINTEXT"),
                brokers: *brokers,
                subnets: subnets.to_vec(),
                ..ClusterRequest::named(&format!("plane-brokers-{brokers}"))
            })
            .await;
        // The addresses the cluster publishes once it has settled — one per broker node —
        // and the nodes the control plane then reports for it, the ones a client must be
        // told about.
        let authority = published_address(&emulator, &cluster, "bootstrapBrokerString", case).await;
        let pairs = published_pairs(&emulator, &cluster, case)
            .await
            .remove("bootstrapBrokerString")
            .unwrap_or_else(|| panic!("case {case:?}: the cluster published no plaintext string"));
        let nodes = emulator
            .call(
                "GET",
                &format!("/v1/clusters/{}/nodes", pct(&cluster)),
                None,
            )
            .await;
        let node_ids: Vec<i64> = nodes
            .at("/nodeInfoList")
            .as_array()
            .map(|listed| {
                listed
                    .iter()
                    .filter_map(|node| node.pointer("/brokerNodeInfo/brokerId"))
                    .filter_map(Value::as_f64)
                    .map(|id| id as i64)
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            node_ids.len(),
            usize::try_from(*brokers).unwrap_or_default(),
            "case {case:?}: the cluster must hold {brokers} broker nodes, got {}",
            nodes.body
        );

        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;
        let metadata = client.metadata(case).await;
        let reported: Vec<i64> = metadata
            .brokers
            .iter()
            .map(|broker| i64::from(broker.node_id.0))
            .collect();
        assert_eq!(
            reported, node_ids,
            "case {case:?}: Metadata must report the cluster's own broker nodes, got \
             {reported:?} for {node_ids:?}"
        );
        // Each node at the pair the bootstrap string publishes for it, in the same order:
        // the string lists one address per node, so the two readings of the cluster's
        // brokers name the same addresses.
        let addressed: Vec<String> = metadata
            .brokers
            .iter()
            .map(|broker| format!("{}:{}", broker.host.as_str(), broker.port))
            .collect();
        assert_eq!(
            addressed, pairs,
            "case {case:?}: each broker node must be reachable at its own published \
             address {pairs:?}, got {addressed:?}"
        );
        assert_eq!(
            i64::from(metadata.controller_id.0),
            node_ids.first().copied().unwrap_or_default(),
            "case {case:?}: the controller must be a node of the cluster, got {}",
            metadata.controller_id.0
        );
    }
}

/// The `ConfigResource.Type` a `DescribeConfigs` request names a topic by, and the one
/// it names a broker by (Apache Kafka protocol).
const RESOURCE_TYPE_TOPIC: i8 = 2;
const RESOURCE_TYPE_BROKER: i8 = 4;

/// The timestamps a `ListOffsets` request asks the earliest and the latest offset with.
const OFFSET_EARLIEST: i64 = -2;
const OFFSET_LATEST: i64 = -1;

impl Client {
    /// `DescribeConfigs` for one resource, answering the entries it reports as
    /// `name -> value`.
    async fn configs(
        &mut self,
        case: &str,
        resource_type: i8,
        resource_name: &str,
        keys: Option<Vec<&str>>,
    ) -> BTreeMap<String, String> {
        let answered: DescribeConfigsResponse = self
            .call(
                case,
                ApiKey::DescribeConfigs,
                4,
                &DescribeConfigsRequest::default().with_resources(vec![
                    DescribeConfigsResource::default()
                        .with_resource_type(resource_type)
                        .with_resource_name(StrBytes::from_string(resource_name.to_string()))
                        .with_configuration_keys(keys.map(|keys| {
                            keys.into_iter()
                                .map(|key| StrBytes::from_string(key.to_string()))
                                .collect()
                        })),
                ]),
            )
            .await;
        let result = answered
            .results
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeConfigs reported no result"))
            .clone();
        assert_eq!(
            result.error_code, 0,
            "case {case:?}: DescribeConfigs({resource_name}) must be answered, got error {} {:?}",
            result.error_code, result.error_message
        );
        result
            .configs
            .iter()
            .map(|entry| {
                (
                    entry.name.to_string(),
                    entry
                        .value
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    /// `Produce` one record batch to one partition, answering the base offset the
    /// broker assigned.
    ///
    /// Each record carries the timestamp the row gives it, which is what a producer
    /// sets and what the broker's retention ages a record out by.
    pub(crate) async fn produce(
        &mut self,
        case: &str,
        topic: &str,
        partition: i32,
        records: &[(i64, Option<&str>, &str)],
    ) -> i64 {
        let batch: Vec<Record> = records
            .iter()
            .enumerate()
            .map(|(index, (timestamp, key, value))| Record {
                transactional: false,
                control: false,
                partition_leader_epoch: 0,
                producer_id: -1,
                producer_epoch: -1,
                timestamp_type: TimestampType::Creation,
                offset: index as i64,
                sequence: index as i32,
                timestamp: *timestamp,
                key: key.map(|key| Bytes::from(key.as_bytes().to_vec())),
                value: Some(Bytes::from(value.as_bytes().to_vec())),
                headers: Default::default(),
                delete_horizon: false,
            })
            .collect();
        let mut encoded = BytesMut::new();
        RecordBatchEncoder::encode(
            &mut encoded,
            batch.iter(),
            &RecordEncodeOptions {
                version: 2,
                compression: Compression::None,
            },
        )
        .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
        let answered: ProduceResponse = self
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
                            .with_index(partition)
                            .with_records(Some(encoded.freeze()))])]),
            )
            .await;
        let partition_response = answered
            .responses
            .first()
            .and_then(|topic| topic.partition_responses.first())
            .unwrap_or_else(|| panic!("case {case:?}: Produce reported no partition"))
            .clone();
        assert_eq!(
            partition_response.error_code, 0,
            "case {case:?}: Produce must be accepted, got error {}",
            partition_response.error_code
        );
        partition_response.base_offset
    }

    /// `Fetch` one partition from `offset`, answering the records it read as
    /// `(offset, key, value)`.
    pub(crate) async fn fetch(
        &mut self,
        case: &str,
        topic: &str,
        partition: i32,
        offset: i64,
    ) -> Vec<(i64, Option<String>, String)> {
        let answered: FetchResponse = self
            .call(
                case,
                ApiKey::Fetch,
                11,
                &FetchRequest::default()
                    .with_max_wait_ms(100)
                    .with_min_bytes(0)
                    .with_max_bytes(1_048_576)
                    .with_topics(vec![FetchTopic::default()
                        .with_topic(TopicName(StrBytes::from_string(topic.to_string())))
                        .with_partitions(vec![FetchPartition::default()
                            .with_partition(partition)
                            .with_fetch_offset(offset)
                            .with_partition_max_bytes(1_048_576)])]),
            )
            .await;
        let partition_data = answered
            .responses
            .first()
            .and_then(|topic| topic.partitions.first())
            .unwrap_or_else(|| panic!("case {case:?}: Fetch reported no partition"))
            .clone();
        assert_eq!(
            partition_data.error_code, 0,
            "case {case:?}: Fetch must be answered, got error {}",
            partition_data.error_code
        );
        let Some(mut records) = partition_data.records.clone() else {
            return Vec::new();
        };
        // A fetch that read nothing answers an EMPTY record set, which decodes to no
        // records rather than to a batch.
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

    /// `ListOffsets` for one partition at `timestamp`.
    async fn offset(&mut self, case: &str, topic: &str, partition: i32, timestamp: i64) -> i64 {
        let answered: ListOffsetsResponse = self
            .call(
                case,
                ApiKey::ListOffsets,
                5,
                &ListOffsetsRequest::default()
                    .with_replica_id(::kafka_protocol::messages::BrokerId(-1))
                    .with_topics(vec![ListOffsetsTopic::default()
                        .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                        .with_partitions(vec![ListOffsetsPartition::default()
                            .with_partition_index(partition)
                            .with_timestamp(timestamp)])]),
            )
            .await;
        let partition_response = answered
            .topics
            .first()
            .and_then(|topic| topic.partitions.first())
            .unwrap_or_else(|| panic!("case {case:?}: ListOffsets reported no partition"))
            .clone();
        assert_eq!(
            partition_response.error_code, 0,
            "case {case:?}: ListOffsets must be answered, got error {}",
            partition_response.error_code
        );
        partition_response.offset
    }
}

/// A cluster whose brokers are reached in the clear, so a suite about the PROTOCOL
/// dials one socket without a credential, and the address its plaintext path is
/// published at.
async fn given_plaintext_cluster(emulator: &Emulator, name: &str) -> String {
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named(name)
        })
        .await;
    sleep_past_activation();
    cluster
}

/// The address a cluster publishes for its plaintext path.
async fn plaintext_address(emulator: &Emulator, cluster: &str, case: &str) -> String {
    published(emulator, cluster, case)
        .await
        .get("bootstrapBrokerString")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the cluster published no plaintext address"))
}

/// One row of the configuration table: the case, the topic, the `Configs` the topic is
/// created with, and the entries the broker must report for it.
type ConfigRow<'a> = (&'a str, &'a str, Option<Value>, &'a [(&'a str, &'a str)]);

/// One row of the record table: the case, the topic, and the records produced to it as
/// `(key, value)` pairs.
type RecordRow<'a> = (&'a str, &'a str, &'a [(Option<&'a str>, &'a str)]);

/// RULE: the topic configuration the control plane stored is what the broker reports
/// over the published endpoint, over Apache Kafka's documented defaults — so a client
/// reads back the `retention.ms`, `segment.ms` and `cleanup.policy` it set through
/// `CreateTopic`, and the broker's own retention-sweep period for what it did not set.
#[tokio::test]
async fn a_topics_configuration_is_readable_over_the_published_endpoint() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-configs").await;
    // The rows differ along WHICH entries the topic sets: the whole retention set a
    // time-to-live test configures, one entry on its own, and none at all (so every
    // value reported is a documented default).
    let rows: &[ConfigRow] = &[
        (
            "the retention set a time-to-live client configures",
            "ttl-full",
            Some(json!({
                "retention.ms": "5000",
                "segment.bytes": "1048576",
                "segment.ms": "1000",
                "cleanup.policy": "delete",
                "file.delete.delay.ms": "0",
            })),
            &[
                ("retention.ms", "5000"),
                ("segment.bytes", "1048576"),
                ("segment.ms", "1000"),
                ("cleanup.policy", "delete"),
                ("file.delete.delay.ms", "0"),
            ],
        ),
        (
            "a longer retention on its own",
            "ttl-retention",
            Some(json!({ "retention.ms": "30000" })),
            // The entries the topic did not set are Apache Kafka's documented
            // topic-level defaults.
            &[
                ("retention.ms", "30000"),
                ("cleanup.policy", "delete"),
                ("segment.ms", "604800000"),
                ("file.delete.delay.ms", "60000"),
            ],
        ),
        (
            "a topic that sets nothing",
            "ttl-default",
            None,
            &[
                ("retention.ms", "604800000"),
                ("cleanup.policy", "delete"),
                ("max.message.bytes", "1048588"),
            ],
        ),
    ];
    for (case, topic, configs, expected) in rows {
        given_topic(&emulator, &cluster, topic, 1, configs.clone()).await;
        let authority = plaintext_address(&emulator, &cluster, case).await;
        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;
        let reported = client
            .configs(case, RESOURCE_TYPE_TOPIC, topic, None)
            .await;
        for (name, value) in *expected {
            assert_eq!(
                reported.get(*name).map(String::as_str),
                Some(*value),
                "case {case:?}: the broker must report {name}={value}, got {:?}",
                reported.get(*name)
            );
        }
        // The topic's own configuration is read back whichever entries the request
        // asked about, so a client that names one key gets that key.
        let narrowed = client
            .configs(case, RESOURCE_TYPE_TOPIC, topic, Some(vec!["retention.ms"]))
            .await;
        assert_eq!(
            narrowed.keys().cloned().collect::<Vec<_>>(),
            vec!["retention.ms".to_string()],
            "case {case:?}: a request naming one key must be answered about that key alone"
        );
    }

    // The broker's own configuration, which is where the retention sweep's period
    // lives: Apache Kafka's documented default for `log.retention.check.interval.ms`.
    let authority = plaintext_address(&emulator, &cluster, "broker configs").await;
    let mut client = Client::plaintext("broker configs", &authority).await;
    client.api_versions("broker configs").await;
    let reported = client
        .configs("broker configs", RESOURCE_TYPE_BROKER, "1", None)
        .await;
    assert_eq!(
        reported
            .get("log.retention.check.interval.ms")
            .map(String::as_str),
        Some("300000"),
        "the broker must report its retention-sweep period, got {reported:?}"
    );
}

/// RULE: a record produced to a topic is readable from the earliest offset the
/// partition still holds, in the order it was produced — so a client that produces and
/// then consumes from the beginning reads what it wrote.
#[tokio::test]
async fn a_produced_record_is_readable_from_the_earliest_offset() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-records").await;
    // The rows differ along how many records are produced and whether they carry a
    // key, which is what the rule generalises over.
    let rows: &[RecordRow] = &[
        ("one record with no key", "records-one", &[(None, "first")]),
        (
            "three keyed records",
            "records-three",
            &[
                (Some("a"), "alpha"),
                (Some("b"), "beta"),
                (Some("c"), "gamma"),
            ],
        ),
        (
            "a record whose value is empty",
            "records-empty",
            &[(Some("k"), "")],
        ),
    ];
    for (case, topic, records) in rows {
        given_topic(&emulator, &cluster, topic, 1, None).await;
        let authority = plaintext_address(&emulator, &cluster, case).await;
        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;

        let now = epoch_millis();
        let batch: Vec<(i64, Option<&str>, &str)> = records
            .iter()
            .map(|(key, value)| (now, *key, *value))
            .collect();
        let base = client.produce(case, topic, 0, &batch).await;
        assert_eq!(
            base, 0,
            "case {case:?}: the first batch of an empty partition starts at offset 0"
        );
        assert_eq!(
            client.offset(case, topic, 0, OFFSET_EARLIEST).await,
            0,
            "case {case:?}: the earliest offset of a partition nothing aged out is 0"
        );
        assert_eq!(
            client.offset(case, topic, 0, OFFSET_LATEST).await,
            records.len() as i64,
            "case {case:?}: the latest offset is the number of records produced"
        );
        let read = client.fetch(case, topic, 0, 0).await;
        let expected: Vec<(i64, Option<String>, String)> = records
            .iter()
            .enumerate()
            .map(|(index, (key, value))| {
                (
                    index as i64,
                    key.map(ToString::to_string),
                    (*value).to_string(),
                )
            })
            .collect();
        assert_eq!(
            read, expected,
            "case {case:?}: a fetch from the earliest offset must read what was produced"
        );
    }
}

/// RULE: a record older than its topic's `retention.ms` is no longer served, and the
/// partition's earliest readable offset moves past it — while a record inside the
/// window, and every record of a topic whose retention is unlimited, stays readable.
#[tokio::test]
async fn records_past_the_topics_retention_are_no_longer_fetched() {
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-retention").await;
    // The rows differ along the topic's retention and the AGE of the record against
    // it: the age is the record's own timestamp, which is what a producer sets and
    // what the broker ages a record out by, so no row waits for a clock.
    // (case, topic, retention.ms as the topic sets it, the record's age, still readable)
    let rows: &[(&str, &str, Option<&str>, Duration, bool)] = &[
        (
            "a record inside a five-second retention",
            "retention-fresh",
            Some("5000"),
            Duration::from_millis(0),
            true,
        ),
        (
            "a record older than a five-second retention",
            "retention-aged",
            Some("5000"),
            Duration::from_secs(60),
            false,
        ),
        (
            "a record older than a thirty-second retention",
            "retention-aged-longer",
            Some("30000"),
            Duration::from_secs(3_600),
            false,
        ),
        (
            "a record far inside the default week",
            "retention-default",
            None,
            Duration::from_secs(3_600),
            true,
        ),
        (
            "a record of a topic that keeps everything",
            "retention-unlimited",
            Some("-1"),
            Duration::from_secs(90_000),
            true,
        ),
    ];
    for (case, topic, retention, age, readable) in rows {
        let configs = retention.map(|retention| json!({ "retention.ms": retention }));
        given_topic(&emulator, &cluster, topic, 1, configs).await;
        let authority = plaintext_address(&emulator, &cluster, case).await;
        let mut client = Client::plaintext(case, &authority).await;
        client.api_versions(case).await;

        let stamped = epoch_millis() - i64::try_from(age.as_millis()).unwrap_or_default();
        client
            .produce(case, topic, 0, &[(stamped, Some("key"), "payload")])
            .await;
        let read = client.fetch(case, topic, 0, 0).await;
        assert_eq!(
            !read.is_empty(),
            *readable,
            "case {case:?}: expected the record to be {}, got {read:?}",
            match readable {
                true => "readable",
                false => "aged out",
            }
        );
        let earliest = client.offset(case, topic, 0, OFFSET_EARLIEST).await;
        let latest = client.offset(case, topic, 0, OFFSET_LATEST).await;
        assert_eq!(
            latest, 1,
            "case {case:?}: retention moves the log's start, never its end"
        );
        assert_eq!(
            earliest,
            match readable {
                true => 0,
                false => 1,
            },
            "case {case:?}: the earliest readable offset must move past an aged-out record"
        );
    }
}

/// RULE: a SASL path serves only a connection that completes ITS exchange with a
/// credential the cluster holds — so a wrong password, a secret the cluster no longer
/// has associated and a payload that is not a signed IAM one are each refused, and a
/// request made before the exchange gets no answer at all.
#[tokio::test]
async fn only_a_credential_the_cluster_holds_is_served_on_a_sasl_path() {
    let emulator = Emulator::with_secrets_manager();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_scram: Some(true),
            sasl_iam: Some(true),
            ..ClusterRequest::named("plane-credentials")
        })
        .await;
    sleep_past_activation();
    let secret = given_secret_associated(&emulator, &cluster, "plane-credential", "setup").await;
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let addresses = published(&emulator, &cluster, "credentials").await;
    let scram_address = addresses
        .get("bootstrapBrokerStringSaslScram")
        .cloned()
        .expect("a SASL/SCRAM cluster publishes a SASL/SCRAM address");
    let iam_address = addresses
        .get("bootstrapBrokerStringSaslIam")
        .cloned()
        .expect("a SASL/IAM cluster publishes a SASL/IAM address");

    // The protocol's own codes: 58 is `SASL_AUTHENTICATION_FAILED` and 34 is
    // `ILLEGAL_SASL_STATE`, which is what a client that offers the wrong mechanism for
    // a path is told.
    let authentication_failed = ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code();
    let illegal_state = ::kafka_protocol::ResponseError::IllegalSaslState.code();
    let unsupported_mechanism =
        ::kafka_protocol::ResponseError::UnsupportedSaslMechanism.code();

    // The rows differ along which credential is offered on which path.
    // (case, address, mechanism, secret the connection offers, expected code)
    let rows: &[(&str, &str, &str, &str, i16)] = &[
        (
            "the password the cluster's secret holds",
            &scram_address,
            SCRAM_SHA_512,
            SCRAM_PASSWORD,
            0,
        ),
        (
            "a password no secret of the cluster holds",
            &scram_address,
            SCRAM_SHA_512,
            "not-the-password",
            authentication_failed,
        ),
        (
            "a signed IAM payload",
            &iam_address,
            AWS_MSK_IAM,
            "a-signature",
            0,
        ),
        (
            "the OAUTHBEARER token a non-Java client authenticates with IAM through",
            &iam_address,
            OAUTHBEARER,
            "a-signature",
            0,
        ),
        (
            "an OAUTHBEARER token carrying no signature",
            &iam_address,
            OAUTHBEARER,
            "",
            authentication_failed,
        ),
        (
            "an IAM payload carrying no signature",
            &iam_address,
            AWS_MSK_IAM,
            "",
            authentication_failed,
        ),
        (
            "the SCRAM mechanism offered on the IAM path",
            &iam_address,
            SCRAM_SHA_512,
            SCRAM_PASSWORD,
            unsupported_mechanism,
        ),
    ];
    for (case, address, mechanism, password, expected) in rows {
        let mut client = Client::tls(case, address).await;
        client.api_versions(case).await;
        let code = client.authenticate(case, mechanism, password).await;
        assert_eq!(
            code, *expected,
            "case {case:?}: expected the exchange to answer code {expected}, got {code}"
        );
    }

    // A `SaslAuthenticate` sent before any handshake is in no state to be
    // authenticated, which is what `ILLEGAL_SASL_STATE` says.
    let mut unnegotiated = Client::tls("before the handshake", &scram_address).await;
    unnegotiated.api_versions("before the handshake").await;
    let answered: SaslAuthenticateResponse = unnegotiated
        .call(
            "before the handshake",
            ApiKey::SaslAuthenticate,
            2,
            &SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from_static(b"n,,n=scram-user,r=nonce")),
        )
        .await;
    assert_eq!(
        answered.error_code, illegal_state,
        "a credential offered before a mechanism is chosen must answer ILLEGAL_SASL_STATE, got {}",
        answered.error_code
    );

    // A request on a SASL path before the exchange is not served at all, which is
    // where a broker closes the connection.
    let mut unauthenticated = Client::tls("before the exchange", &scram_address).await;
    unauthenticated.api_versions("before the exchange").await;
    let answered = unauthenticated.metadata_or_closed("before the exchange").await;
    assert!(
        answered.is_none(),
        "an unauthenticated connection must not be served, got {answered:?}"
    );

    // And once the cluster no longer holds the secret, the credential in it stops
    // authenticating: the broker reads the credential from the secrets the cluster HAS
    // associated.
    let disassociated = emulator
        .call(
            "PATCH",
            &format!("/v1/clusters/{}/scram-secrets", pct(&cluster)),
            Some(json!({ "secretArnList": [secret] })),
        )
        .await;
    assert_eq!(
        disassociated.status, 200,
        "BatchDisassociateScramSecret must be accepted, got {} {}",
        disassociated.status, disassociated.body
    );
    let mut revoked = Client::tls("a disassociated secret", &scram_address).await;
    revoked.api_versions("a disassociated secret").await;
    let code = revoked
        .authenticate("a disassociated secret", SCRAM_SHA_512, SCRAM_PASSWORD)
        .await;
    assert_eq!(
        code, authentication_failed,
        "a credential the cluster no longer holds must not authenticate, got code {code}"
    );
}

/// The Apache Kafka ACL values these rows use: the resource type a topic binding
/// names, the pattern type a binding that names it exactly carries, the two operations
/// a client of a topic needs, and the two permissions a binding can carry.
const ACL_RESOURCE_TYPE_TOPIC: i8 = 2;
const ACL_PATTERN_TYPE_LITERAL: i8 = 3;
const ACL_PATTERN_TYPE_ANY: i8 = 1;
const ACL_OPERATION_READ: i8 = 3;
const ACL_OPERATION_WRITE: i8 = 4;
const ACL_OPERATION_ANY: i8 = 1;
const ACL_PERMISSION_DENY: i8 = 2;
const ACL_PERMISSION_ALLOW: i8 = 3;
const ACL_PERMISSION_ANY: i8 = 1;
const ACL_RESOURCE_TYPE_ANY: i8 = 1;

/// One ACL binding a row creates: the resource it is about, the principal, the
/// operation and whether it allows or denies.
type AclRow<'a> = (&'a str, &'a str, i8, i8);

impl Client {
    /// `CreateAcls` for each binding, answering the error code of each creation.
    async fn create_acls(&mut self, case: &str, bindings: &[AclRow<'_>]) -> Vec<i16> {
        let answered: ::kafka_protocol::messages::CreateAclsResponse = self
            .call(
                case,
                ApiKey::CreateAcls,
                1,
                &::kafka_protocol::messages::CreateAclsRequest::default().with_creations(
                    bindings
                        .iter()
                        .map(|(topic, principal, operation, permission)| {
                            ::kafka_protocol::messages::create_acls_request::AclCreation::default()
                                .with_resource_type(ACL_RESOURCE_TYPE_TOPIC)
                                .with_resource_name(StrBytes::from_string((*topic).to_string()))
                                .with_resource_pattern_type(ACL_PATTERN_TYPE_LITERAL)
                                .with_principal(StrBytes::from_string((*principal).to_string()))
                                .with_host(StrBytes::from_static_str("*"))
                                .with_operation(*operation)
                                .with_permission_type(*permission)
                        })
                        .collect(),
                ),
            )
            .await;
        answered
            .results
            .iter()
            .map(|result| result.error_code)
            .collect()
    }

    /// `DescribeAcls` over every binding the cluster holds, as
    /// `(resource name, principal, operation, permission)`.
    async fn describe_acls(&mut self, case: &str) -> Vec<(String, String, i8, i8)> {
        let answered: ::kafka_protocol::messages::DescribeAclsResponse = self
            .call(
                case,
                ApiKey::DescribeAcls,
                1,
                &::kafka_protocol::messages::DescribeAclsRequest::default()
                    .with_resource_type_filter(ACL_RESOURCE_TYPE_ANY)
                    // A null filter is the protocol's "any": a filter naming the empty
                    // string would select a binding about the empty name.
                    .with_resource_name_filter(None)
                    .with_pattern_type_filter(ACL_PATTERN_TYPE_ANY)
                    .with_principal_filter(None)
                    .with_host_filter(None)
                    .with_operation(ACL_OPERATION_ANY)
                    .with_permission_type(ACL_PERMISSION_ANY),
            )
            .await;
        assert_eq!(
            answered.error_code, 0,
            "case {case:?}: DescribeAcls must be answered, got error {}",
            answered.error_code
        );
        answered
            .resources
            .iter()
            .flat_map(|resource| {
                resource.acls.iter().map(|acl| {
                    (
                        resource.resource_name.to_string(),
                        acl.principal.to_string(),
                        acl.operation,
                        acl.permission_type,
                    )
                })
            })
            .collect()
    }

    /// `DeleteAcls` for the bindings on one topic with one permission, answering how
    /// many were removed.
    async fn delete_acls(&mut self, case: &str, topic: &str, permission: i8) -> usize {
        let answered: ::kafka_protocol::messages::DeleteAclsResponse = self
            .call(
                case,
                ApiKey::DeleteAcls,
                1,
                &::kafka_protocol::messages::DeleteAclsRequest::default().with_filters(vec![
                    ::kafka_protocol::messages::delete_acls_request::DeleteAclsFilter::default()
                        .with_resource_type_filter(ACL_RESOURCE_TYPE_TOPIC)
                        .with_resource_name_filter(Some(StrBytes::from_string(topic.to_string())))
                        .with_pattern_type_filter(ACL_PATTERN_TYPE_ANY)
                        .with_principal_filter(None)
                        .with_host_filter(None)
                        .with_operation(ACL_OPERATION_ANY)
                        .with_permission_type(permission),
                ]),
            )
            .await;
        answered
            .filter_results
            .iter()
            .map(|result| result.matching_acls.len())
            .sum()
    }

    /// `Produce` one record, answering the error code the broker reported for the
    /// partition rather than asserting it succeeded.
    async fn produce_outcome(&mut self, case: &str, topic: &str, value: &str) -> i16 {
        let record = Record {
            transactional: false,
            control: false,
            partition_leader_epoch: 0,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: 0,
            sequence: 0,
            timestamp: epoch_millis(),
            key: None,
            value: Some(Bytes::from(value.as_bytes().to_vec())),
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
        let answered: ProduceResponse = self
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
            .unwrap_or_default()
    }
}

/// RULE: the ACL bindings a client creates over the published endpoint are held by the
/// cluster's brokers — `DescribeAcls` reports what was created and `DeleteAcls` removes
/// it — and a binding that WITHHOLDS a topic from a principal is what the brokers then
/// refuse that principal's produce with, while a cluster holding no such binding serves
/// every authenticated client (Apache Kafka's `allow.everyone.if.no.acl.found`, which
/// an Amazon MSK cluster runs with).
#[tokio::test]
async fn the_acl_bindings_a_client_creates_are_held_and_decide_a_refusal() {
    let case = "acl bindings over the published endpoint";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-acls").await;
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    // Nothing is withheld from a cluster that holds no binding.
    assert!(
        client.describe_acls(case).await.is_empty(),
        "case {case:?}: a new cluster holds no ACL binding"
    );
    assert_eq!(
        client.produce_outcome(case, "orders", "before").await,
        0,
        "case {case:?}: a cluster holding no binding serves the produce"
    );

    // The bindings a client creates are the bindings it reads back.
    let created: &[AclRow] = &[
        ("orders", "User:*", ACL_OPERATION_READ, ACL_PERMISSION_ALLOW),
        ("orders", "User:*", ACL_OPERATION_WRITE, ACL_PERMISSION_DENY),
    ];
    assert_eq!(
        client.create_acls(case, created).await,
        vec![0, 0],
        "case {case:?}: both creations must be accepted"
    );
    let mut held = client.describe_acls(case).await;
    held.sort();
    assert_eq!(
        held,
        vec![
            (
                "orders".to_string(),
                "User:*".to_string(),
                ACL_OPERATION_READ,
                ACL_PERMISSION_ALLOW
            ),
            (
                "orders".to_string(),
                "User:*".to_string(),
                ACL_OPERATION_WRITE,
                ACL_PERMISSION_DENY
            ),
        ],
        "case {case:?}: DescribeAcls must report the bindings that were created"
    );

    // And the DENY is what the produce is refused with.
    assert_eq!(
        client.produce_outcome(case, "orders", "withheld").await,
        ::kafka_protocol::ResponseError::TopicAuthorizationFailed.code(),
        "case {case:?}: a binding that withholds the topic must refuse the produce"
    );
    assert_eq!(
        client.produce_outcome(case, "other", "unrelated").await,
        ::kafka_protocol::ResponseError::UnknownTopicOrPartition.code(),
        "case {case:?}: a topic the cluster does not hold is unknown, not withheld"
    );

    // Removing it serves the principal again, and the removal is reported.
    assert_eq!(
        client.delete_acls(case, "orders", ACL_PERMISSION_DENY).await,
        1,
        "case {case:?}: DeleteAcls must report the binding it removed"
    );
    assert_eq!(
        client.produce_outcome(case, "orders", "after").await,
        0,
        "case {case:?}: a produce is served once nothing withholds the topic"
    );
    assert_eq!(
        client.describe_acls(case).await.len(),
        1,
        "case {case:?}: the binding the delete did not select must still be held"
    );
}

/// RULE: the group a consuming client joins is coordinated by the broker it was sent
/// to — the member is admitted with an id and a generation, told the assignment the
/// group's leader computed for it, and the offset it commits is the offset a later
/// `OffsetFetch` reports — so a client that consumes as part of a group makes progress
/// and resumes where it left off.
#[tokio::test]
async fn the_group_a_consumer_joins_is_coordinated_by_the_broker() {
    let case = "a consumer group over the published endpoint";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-groups").await;
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let authority = plaintext_address(&emulator, &cluster, case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    // The coordinator of a group is the broker that answers for it, at the address the
    // client dialled.
    let coordinator: ::kafka_protocol::messages::FindCoordinatorResponse = client
        .call(
            case,
            ApiKey::FindCoordinator,
            2,
            &::kafka_protocol::messages::FindCoordinatorRequest::default()
                .with_key(StrBytes::from_static_str("readers"))
                .with_key_type(0),
        )
        .await;
    assert_eq!(
        format!("{}:{}", coordinator.host.as_str(), coordinator.port),
        authority,
        "case {case:?}: the coordinator must be the broker the client dialled"
    );

    // Joining admits the member, names it and hands it the group's generation.
    let joined: ::kafka_protocol::messages::JoinGroupResponse = client
        .call(
            case,
            ApiKey::JoinGroup,
            5,
            &::kafka_protocol::messages::JoinGroupRequest::default()
                .with_group_id(::kafka_protocol::messages::GroupId(
                    StrBytes::from_static_str("readers"),
                ))
                .with_session_timeout_ms(30_000)
                .with_rebalance_timeout_ms(30_000)
                .with_member_id(StrBytes::from_static_str(""))
                .with_protocol_type(StrBytes::from_static_str("consumer"))
                .with_protocols(vec![
                    ::kafka_protocol::messages::join_group_request::JoinGroupRequestProtocol::default()
                        .with_name(StrBytes::from_static_str("range"))
                        .with_metadata(Bytes::from_static(b"subscription")),
                ]),
        )
        .await;
    assert_eq!(
        joined.error_code, 0,
        "case {case:?}: the join must be admitted, got error {}",
        joined.error_code
    );
    assert!(
        !joined.member_id.is_empty() && joined.generation_id > 0,
        "case {case:?}: a joined member must carry an id and a generation, got {:?} / {}",
        joined.member_id,
        joined.generation_id
    );
    assert_eq!(
        joined.leader, joined.member_id,
        "case {case:?}: the only member of a group is its leader"
    );
    assert_eq!(
        joined
            .members
            .iter()
            .map(|member| member.metadata.clone())
            .collect::<Vec<_>>(),
        vec![Bytes::from_static(b"subscription")],
        "case {case:?}: the leader must be told the subscription every member joined with"
    );

    // And syncing hands the member the assignment the leader computed for it.
    let synced: ::kafka_protocol::messages::SyncGroupResponse = client
        .call(
            case,
            ApiKey::SyncGroup,
            3,
            &::kafka_protocol::messages::SyncGroupRequest::default()
                .with_group_id(::kafka_protocol::messages::GroupId(
                    StrBytes::from_static_str("readers"),
                ))
                .with_generation_id(joined.generation_id)
                .with_member_id(joined.member_id.clone())
                .with_assignments(vec![
                    ::kafka_protocol::messages::sync_group_request::SyncGroupRequestAssignment::default()
                        .with_member_id(joined.member_id.clone())
                        .with_assignment(Bytes::from_static(b"orders-0")),
                ]),
        )
        .await;
    assert_eq!(
        (synced.error_code, synced.assignment.clone()),
        (0, Bytes::from_static(b"orders-0")),
        "case {case:?}: the member must be handed its own assignment"
    );

    // A heartbeat on the group's generation keeps it; one on an older generation is
    // told to rejoin.
    for (generation, expected) in [
        (joined.generation_id, 0),
        (
            joined.generation_id - 1,
            ::kafka_protocol::ResponseError::IllegalGeneration.code(),
        ),
    ] {
        let beat: ::kafka_protocol::messages::HeartbeatResponse = client
            .call(
                case,
                ApiKey::Heartbeat,
                3,
                &::kafka_protocol::messages::HeartbeatRequest::default()
                    .with_group_id(::kafka_protocol::messages::GroupId(
                        StrBytes::from_static_str("readers"),
                    ))
                    .with_generation_id(generation)
                    .with_member_id(joined.member_id.clone()),
            )
            .await;
        assert_eq!(
            beat.error_code, expected,
            "case {case:?}: a heartbeat at generation {generation} must answer {expected}"
        );
    }

    // Nothing is committed until the group commits, and then the commit is what a
    // later fetch of the group's offsets reports.
    assert_eq!(
        group_offset(&mut client, case).await,
        -1,
        "case {case:?}: a group that has committed nothing reports no offset"
    );
    let committed: ::kafka_protocol::messages::OffsetCommitResponse = client
        .call(
            case,
            ApiKey::OffsetCommit,
            4,
            &::kafka_protocol::messages::OffsetCommitRequest::default()
                .with_group_id(::kafka_protocol::messages::GroupId(
                    StrBytes::from_static_str("readers"),
                ))
                .with_generation_id_or_member_epoch(joined.generation_id)
                .with_member_id(joined.member_id.clone())
                .with_topics(vec![
                    ::kafka_protocol::messages::offset_commit_request::OffsetCommitRequestTopic::default()
                        .with_name(TopicName(StrBytes::from_static_str("orders")))
                        .with_partitions(vec![
                            ::kafka_protocol::messages::offset_commit_request::OffsetCommitRequestPartition::default()
                                .with_partition_index(0)
                                .with_committed_offset(7),
                        ]),
                ]),
        )
        .await;
    assert_eq!(
        committed
            .topics
            .first()
            .and_then(|topic| topic.partitions.first())
            .map(|partition| partition.error_code),
        Some(0),
        "case {case:?}: the commit must be accepted"
    );
    assert_eq!(
        group_offset(&mut client, case).await,
        7,
        "case {case:?}: the offset a group committed is the one it reads back"
    );
}

/// RULE: the data plane a cluster is served by belongs to that cluster, so a cluster
/// that is gone serves nothing: the socket its access path was answered on is closed,
/// and nothing of its log, its ACL bindings or its groups is kept.
#[tokio::test]
async fn a_deleted_clusters_endpoint_stops_answering() {
    let case = "a deleted cluster's endpoint";
    let emulator = Emulator::new();
    let cluster = given_plaintext_cluster(&emulator, "plane-teardown").await;
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let authority = plaintext_address(&emulator, &cluster, case).await;

    // It answers while the cluster is there.
    let mut client = Client::plaintext(case, &authority).await;
    assert_eq!(
        client.api_versions(case).await.error_code,
        0,
        "case {case:?}: the endpoint answers while the cluster is registered"
    );
    client
        .produce(case, "orders", 0, &[(epoch_millis(), None, "kept")])
        .await;

    let deleted = emulator
        .call("DELETE", &format!("/v1/clusters/{}", pct(&cluster)), None)
        .await;
    assert_eq!(
        deleted.status, 200,
        "case {case:?}: DeleteCluster must be accepted, got {} {}",
        deleted.status, deleted.body
    );

    // Once the removal the clock owes the cluster is materialised, the socket it was
    // served on is closed: a client dialling the address it published is refused.
    // Polled for the condition rather than waited out, with a generous deadline.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority} is not a host:port pair"));
    loop {
        let described = emulator.describe(&cluster).await;
        let refused = TcpStream::connect(("127.0.0.1", port)).await.is_err();
        if described.status == 404 && refused {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: a deleted cluster's endpoint must stop answering; DescribeCluster \
             answered {} and the port was {}",
            described.status,
            match refused {
                true => "refused",
                false => "still accepting",
            }
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The offset the group `readers` has committed for partition 0 of `orders`, as
/// `OffsetFetch` reports it.
async fn group_offset(client: &mut Client, case: &str) -> i64 {
    let fetched: ::kafka_protocol::messages::OffsetFetchResponse = client
        .call(
            case,
            ApiKey::OffsetFetch,
            5,
            &::kafka_protocol::messages::OffsetFetchRequest::default()
                .with_group_id(::kafka_protocol::messages::GroupId(
                    StrBytes::from_static_str("readers"),
                ))
                .with_topics(Some(vec![
                    ::kafka_protocol::messages::offset_fetch_request::OffsetFetchRequestTopic::default()
                        .with_name(TopicName(StrBytes::from_static_str("orders")))
                        .with_partition_indexes(vec![0]),
                ])),
        )
        .await;
    fetched
        .topics
        .first()
        .and_then(|topic| topic.partitions.first())
        .map(|partition| partition.committed_offset)
        .unwrap_or_default()
}

/// Epoch milliseconds now, the unit a record's timestamp is in.
/// The current instant in epoch milliseconds, which is the timestamp a producer stamps
/// a record with.
pub(crate) fn epoch_millis() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or_default())
        .unwrap_or_default()
}

impl Client {
    /// `Metadata`, or `None` when the broker closed the connection rather than
    /// answering — which is what an unauthenticated connection to a SASL path gets.
    async fn metadata_or_closed(&mut self, case: &str) -> Option<String> {
        self.correlation += 1;
        let key = ApiKey::Metadata;
        let version = 9;
        let mut frame = BytesMut::new();
        RequestHeader::default()
            .with_request_api_key(key as i16)
            .with_request_api_version(version)
            .with_correlation_id(self.correlation)
            .with_client_id(Some(StrBytes::from_static_str(CLIENT_ID)))
            .encode(&mut frame, key.request_header_version(version))
            .expect("the header encodes");
        MetadataRequest::default()
            .with_topics(None)
            .encode(&mut frame, version)
            .expect("the request encodes");
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        if self.session.write_all(&length.to_be_bytes()).await.is_err()
            || self.session.write_all(&frame).await.is_err()
        {
            return None;
        }
        self.read_frame(case).await.map(|frame| format!("{frame:?}"))
    }
}

/// How long a probe below waits for a condition the clock owes the cluster — a topic's
/// removal — before the suite calls it a failure: far longer than the span the service
/// owes it, so the deadline says nothing about how fast the host runs.
const TOPIC_CONDITION_DEADLINE: Duration = Duration::from_secs(60);

/// How long such a probe waits between two looks, a probing cadence rather than a bound
/// on the transition.
const TOPIC_PROBE_INTERVAL: Duration = Duration::from_millis(50);

/// One topic-level configuration entry a `CreateTopics` request carries.
type WireConfig<'a> = (&'a str, &'a str);

/// One topic a `CreateTopics` request asks for: its name, the partition count and the
/// replication factor it names (`-1` asking for the broker's own default), and the
/// topic-level configuration it sets.
type WireTopic<'a> = (&'a str, i32, i16, &'a [WireConfig<'a>]);

/// What one `CreateTopics` result says: the topic it is about, the error code, the error
/// message, and the partitions and replication factor the broker reports it created.
type CreatedTopic = (String, i16, String, i32, i16);

impl Client {
    /// `CreateTopics` for each topic, answering one result per topic.
    ///
    /// Sent at the highest version this endpoint serves, the one that reports back the
    /// partitions and the replication factor the broker placed.
    pub(crate) async fn create_topics(
        &mut self,
        case: &str,
        topics: &[WireTopic<'_>],
        validate_only: bool,
    ) -> Vec<CreatedTopic> {
        let answered: ::kafka_protocol::messages::CreateTopicsResponse = self
            .call(
                case,
                ApiKey::CreateTopics,
                5,
                &::kafka_protocol::messages::CreateTopicsRequest::default()
                    .with_timeout_ms(1_000)
                    .with_validate_only(validate_only)
                    .with_topics(
                        topics
                            .iter()
                            .map(|(name, partitions, replication_factor, configs)| {
                                ::kafka_protocol::messages::create_topics_request::CreatableTopic::default()
                                    .with_name(TopicName(StrBytes::from_string((*name).to_string())))
                                    .with_num_partitions(*partitions)
                                    .with_replication_factor(*replication_factor)
                                    .with_configs(
                                        configs
                                            .iter()
                                            .map(|(key, value)| {
                                                ::kafka_protocol::messages::create_topics_request::CreatableTopicConfig::default()
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
                    result.error_code,
                    result
                        .error_message
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                    result.num_partitions,
                    result.replication_factor,
                )
            })
            .collect()
    }

    /// `DeleteTopics` for each name, answering `(topic, error code, error message)` per
    /// name.
    async fn delete_topics(
        &mut self,
        case: &str,
        names: &[&str],
    ) -> Vec<(String, i16, String)> {
        let answered: ::kafka_protocol::messages::DeleteTopicsResponse = self
            .call(
                case,
                ApiKey::DeleteTopics,
                // The version that carries a per-topic error MESSAGE, which this
                // endpoint's highest served version is.
                5,
                &::kafka_protocol::messages::DeleteTopicsRequest::default()
                    .with_timeout_ms(1_000)
                    .with_topic_names(
                        names
                            .iter()
                            .map(|name| TopicName(StrBytes::from_string((*name).to_string())))
                            .collect(),
                    ),
            )
            .await;
        answered
            .responses
            .iter()
            .map(|result| {
                (
                    result
                        .name
                        .as_ref()
                        .map(|name| name.0.to_string())
                        .unwrap_or_default(),
                    result.error_code,
                    result
                        .error_message
                        .as_ref()
                        .map(ToString::to_string)
                        .unwrap_or_default(),
                )
            })
            .collect()
    }

    /// The topics `Metadata` reports for the cluster, as `name -> partition count`.
    async fn metadata_topics(&mut self, case: &str) -> BTreeMap<String, usize> {
        self.metadata(case)
            .await
            .topics
            .iter()
            .filter_map(|topic| {
                topic
                    .name
                    .as_ref()
                    .map(|name| (name.0.to_string(), topic.partitions.len()))
            })
            .collect()
    }
}

/// The address a cluster publishes for one bootstrap member, once it publishes one —
/// through the harness's own poll, which every data-plane suite of this crate shares.
async fn published_address(
    emulator: &Emulator,
    cluster: &str,
    member: &str,
    case: &str,
) -> String {
    emulator.published_bootstrap(cluster, member, case).await
}

/// An authenticated client on the one address an MSK SERVERLESS cluster publishes: the
/// SASL/IAM path over TLS, which is the only way a client of such a cluster reaches its
/// brokers — and so the only way it manages its topics.
async fn serverless_client(case: &str, authority: &str) -> Client {
    let mut client = Client::tls(case, authority).await;
    client.api_versions(case).await;
    assert_eq!(
        client.authenticate(case, AWS_MSK_IAM, "a-signature").await,
        0,
        "case {case:?}: the IAM exchange must authenticate the connection"
    );
    client
}

/// RULE: topic administration over a published bootstrap address acts on the CLUSTER's
/// own topics — a topic an admin client creates there is the cluster's topic, carrying
/// the partitions and the configuration the request named, usable for records at once
/// and reported by that cluster's own readers.
///
/// The rows differ along what the rule generalises over: how many PARTITIONS the request
/// names (including `-1`, which asks for the broker's own default), what topic-level
/// CONFIGURATION it sets, and which ACCESS PATH the admin client reached the cluster on
/// — a provisioned cluster's plaintext path, and the SASL/IAM path that is the only one
/// an MSK Serverless cluster has, where an admin client is the only way to create a
/// topic at all.
#[tokio::test]
async fn an_admin_client_creates_the_clusters_own_topics_over_the_published_address() {
    // (case, the topic the request asks for, the partitions the broker must place, the
    // configuration entries it must report back)
    let rows: &[(&str, WireTopic, i32, &[WireConfig])] = &[
        (
            "a single-partition topic with no configuration of its own",
            ("admin-plain", 1, 1, &[]),
            1,
            &[],
        ),
        (
            "a multi-partition topic",
            ("admin-partitioned", 3, 1, &[]),
            3,
            &[],
        ),
        (
            // `-1` is what a client sends to ask for the broker's own
            // `num.partitions`/`default.replication.factor`, which this broker reports
            // as 1 in its own configuration.
            "a topic that asks for the broker's own defaults",
            ("admin-defaulted", -1, -1, &[]),
            1,
            &[],
        ),
        (
            "a topic that sets its retention and cleanup policy",
            (
                "admin-configured",
                2,
                1,
                &[("retention.ms", "60000"), ("cleanup.policy", "compact")],
            ),
            2,
            &[("retention.ms", "60000"), ("cleanup.policy", "compact")],
        ),
    ];

    let emulator = Emulator::new();
    let provisioned = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("admin-provisioned")
        })
        .await;
    let serverless = emulator
        .create_serverless_ok("admin-serverless", &[vec!["subnet-0a1b2c3d"]], Some(true))
        .await;
    let plaintext =
        published_address(&emulator, &provisioned, "bootstrapBrokerString", "provisioned").await;
    let sasl_iam = published_address(
        &emulator,
        &serverless,
        "bootstrapBrokerStringSaslIam",
        "serverless",
    )
    .await;

    // (which cluster, its published address, whether that path authenticates with IAM
    // over TLS, and whether the control plane's own topic readers serve that kind of
    // cluster — Amazon MSK Serverless has no control-plane topic management, which the
    // `topic_apis_serverless_gate` suite is about)
    let endpoints: &[(&str, &str, &str, bool, bool)] = &[
        ("a provisioned cluster", &provisioned, &plaintext, false, true),
        ("a serverless cluster", &serverless, &sasl_iam, true, false),
    ];

    for (endpoint, cluster, authority, sasl, control_plane_reads) in endpoints {
        for (row, (topic, partitions, replication_factor, configs), expected, reported) in rows {
            let case = format!("{endpoint} / {row}");
            let mut client = match sasl {
                true => serverless_client(&case, authority).await,
                false => {
                    let mut client = Client::plaintext(&case, authority).await;
                    client.api_versions(&case).await;
                    client
                }
            };
            let created = client
                .create_topics(&case, &[(topic, *partitions, *replication_factor, configs)], false)
                .await;
            assert_eq!(
                created.len(),
                1,
                "case {case:?}: expected one result per topic, got {created:?}"
            );
            let (name, code, message, placed, factor) = created[0].clone();
            assert_eq!(
                (name.as_str(), code, message.as_str()),
                (*topic, 0, ""),
                "case {case:?}: expected {topic} to be created, got {created:?}"
            );
            assert_eq!(
                placed, *expected,
                "case {case:?}: expected {expected} partitions placed, got {placed}"
            );
            assert_eq!(
                factor, 1,
                "case {case:?}: expected the replication factor 1 reported, got {factor}"
            );

            // The cluster's own brokers report it at once: the topic is there to produce
            // to as soon as the create is answered.
            let listed = client.metadata_topics(&case).await;
            assert_eq!(
                listed.get(*topic).copied(),
                Some(usize::try_from(*expected).unwrap_or_default()),
                "case {case:?}: Metadata must report {topic} with {expected} partitions, got \
                 {listed:?}"
            );
            // And the configuration the request set is the topic's own, over Apache
            // Kafka's documented defaults.
            let reported_configs = client
                .configs(&case, RESOURCE_TYPE_TOPIC, topic, None)
                .await;
            for (key, value) in *reported {
                assert_eq!(
                    reported_configs.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected {key}={value} on the created topic, got \
                     {reported_configs:?}"
                );
            }
            assert_eq!(
                reported_configs.get("min.insync.replicas").map(String::as_str),
                Some("1"),
                "case {case:?}: an entry the request did not set must be the documented \
                 default, got {reported_configs:?}"
            );

            // Records: the topic a client created over this address is one it can
            // produce to and fetch from straight away.
            let produced = client
                .produce(&case, topic, 0, &[(epoch_millis(), Some("k"), "payload")])
                .await;
            assert_eq!(
                produced, 0,
                "case {case:?}: the first record of a new topic takes offset 0, got {produced}"
            );
            let fetched = client.fetch(&case, topic, 0, 0).await;
            assert_eq!(
                fetched,
                vec![(0, Some("k".to_string()), "payload".to_string())],
                "case {case:?}: the record produced to the new topic must be fetched back, got \
                 {fetched:?}"
            );

            // And the control plane reads back the same topic, for a cluster whose
            // topics it serves at all.
            if *control_plane_reads {
                let described = emulator
                    .call(
                        "GET",
                        &format!("/v1/clusters/{}/topics/{}", pct(cluster), pct(topic)),
                        None,
                    )
                    .await;
                assert_eq!(
                    described.status, 200,
                    "case {case:?}: DescribeTopic must report the topic the wire created, got {} \
                     {}",
                    described.status, described.body
                );
                assert_eq!(
                    described.at("/partitionCount").as_i64(),
                    Some(i64::from(*expected)),
                    "case {case:?}: DescribeTopic must report {expected} partitions, got {}",
                    described.body
                );
            }
        }
    }
}

/// RULE: a topic-administration request the broker cannot place is REFUSED, with the
/// Apache Kafka error code and the sentence that error class carries — and it registers
/// nothing, so the cluster's topics are exactly those it could place.
///
/// The rows differ along what the rule generalises over: which part of the request the
/// broker will not take — the name, the partition count, the replication factor against
/// the brokers the cluster has, a name the cluster already holds — and a request that
/// asks only to be VALIDATED, which is answered without creating anything.
#[tokio::test]
async fn topic_administration_refuses_what_the_broker_cannot_place() {
    let invalid_topic = ::kafka_protocol::ResponseError::InvalidTopicException.code();
    let invalid_partitions = ::kafka_protocol::ResponseError::InvalidPartitions.code();
    let invalid_replication = ::kafka_protocol::ResponseError::InvalidReplicationFactor.code();
    let already_exists = ::kafka_protocol::ResponseError::TopicAlreadyExists.code();
    let invalid_request = ::kafka_protocol::ResponseError::InvalidRequest.code();

    let emulator = Emulator::new();
    // Three brokers, which is what bounds a topic's replication factor on this cluster.
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("admin-refusals")
        })
        .await;
    let authority =
        published_address(&emulator, &cluster, "bootstrapBrokerString", "refusals").await;
    let mut client = Client::plaintext("refusals", &authority).await;
    client.api_versions("refusals").await;

    // The name the cluster already holds, created over the same address.
    let held = client
        .create_topics("the held topic", &[("admin-held", 1, 1, &[])], false)
        .await;
    assert_eq!(
        held.first().map(|(_, code, _, _, _)| *code),
        Some(0),
        "setup: the first create of a name must be accepted, got {held:?}"
    );

    // (case, the topic the request asks for, the error code, the message)
    let rows: &[(&str, WireTopic, i16, &str)] = &[
        (
            "a name the cluster already holds",
            ("admin-held", 1, 1, &[]),
            already_exists,
            "Topic 'admin-held' already exists.",
        ),
        (
            "a name with a character no topic name may carry",
            ("admin bad name", 1, 1, &[]),
            invalid_topic,
            "Topic name \"admin bad name\" is illegal, it contains a character other than ASCII \
             alphanumerics, '.', '_' and '-'",
        ),
        (
            // Apache Kafka reserves both of these names.
            "the reserved name",
            ("..", 1, 1, &[]),
            invalid_topic,
            "Topic name cannot be \".\" or \"..\"",
        ),
        (
            "no partitions at all",
            ("admin-zero-partitions", 0, 1, &[]),
            invalid_partitions,
            "Number of partitions was set to an invalid non-positive value.",
        ),
        (
            // Past the partitions of one topic this broker serves, so the count is
            // refused rather than reported as a placement nothing is behind.
            "more partitions than this broker places",
            ("admin-over-partitioned", 1_025, 1, &[]),
            invalid_partitions,
            "This broker places at most 1024 partitions per topic.",
        ),
        (
            "no replicas at all",
            ("admin-zero-replicas", 1, 0, &[]),
            invalid_replication,
            "Replication factor must be larger than 0, or -1 to use the default value.",
        ),
        (
            // Apache Kafka cannot place more replicas of a partition than the cluster
            // has brokers to place them on, and this cluster has three.
            "more replicas than the cluster has brokers",
            ("admin-over-replicated", 1, 4, &[]),
            invalid_replication,
            "Replication factor: 4 larger than available brokers: 3.",
        ),
    ];
    for (case, (topic, partitions, replication_factor, configs), code, message) in rows {
        let answered = client
            .create_topics(case, &[(topic, *partitions, *replication_factor, configs)], false)
            .await;
        let (name, answered_code, answered_message, _, _) = answered
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported no result"));
        assert_eq!(
            (name.as_str(), answered_code, answered_message.as_str()),
            (*topic, *code, *message),
            "case {case:?}: expected the refusal ({code}, {message:?}), got {answered:?}"
        );
    }
    // Nothing of the refused rows was registered: the cluster holds the one topic it
    // could place.
    let listed = client.metadata_topics("the refused rows").await;
    assert_eq!(
        listed.keys().cloned().collect::<Vec<String>>(),
        vec!["admin-held".to_string()],
        "a refused create must register nothing, got {listed:?}"
    );

    // A request that asks only to be validated is answered as the create would have
    // been, and registers nothing either.
    let validated = client
        .create_topics("a validated name", &[("admin-validated", 2, 1, &[])], true)
        .await;
    assert_eq!(
        validated.first().map(|(_, code, _, _, _)| *code),
        Some(0),
        "a name the broker could place must validate, got {validated:?}"
    );
    let validated_conflict = client
        .create_topics("a validated held name", &[("admin-held", 1, 1, &[])], true)
        .await;
    assert_eq!(
        validated_conflict.first().map(|(_, code, _, _, _)| *code),
        Some(already_exists),
        "a name the cluster holds must fail validation too, got {validated_conflict:?}"
    );
    let after = client.metadata_topics("after the validation").await;
    assert_eq!(
        after.keys().cloned().collect::<Vec<String>>(),
        vec!["admin-held".to_string()],
        "a validated create must register nothing, got {after:?}"
    );

    // And a name the cluster does not hold is nothing to delete.
    let deleted = client
        .delete_topics("an absent topic", &["admin-absent"])
        .await;
    assert_eq!(
        deleted,
        vec![(
            "admin-absent".to_string(),
            ::kafka_protocol::ResponseError::UnknownTopicOrPartition.code(),
            "This server does not host this topic-partition.".to_string(),
        )],
        "deleting a name the cluster does not hold must be refused, got {deleted:?}"
    );

    // A request that names one topic TWICE says two things about one name: every entry
    // for it is refused and nothing is created, whether the request asks to create or
    // only to validate.
    let repeated_entries = "Found multiple entries for this topic.";
    for (case, validate_only) in [
        ("a name stated twice", false),
        ("a name stated twice, validated only", true),
    ] {
        let twice = client
            .create_topics(
                case,
                &[("admin-twice", 1, 1, &[]), ("admin-twice", 2, 1, &[])],
                validate_only,
            )
            .await;
        let answered: Vec<(String, i16, String)> = twice
            .iter()
            .map(|(name, code, message, _, _)| (name.clone(), *code, message.clone()))
            .collect();
        assert_eq!(
            answered,
            vec![
                (
                    "admin-twice".to_string(),
                    invalid_request,
                    repeated_entries.to_string()
                ),
                (
                    "admin-twice".to_string(),
                    invalid_request,
                    repeated_entries.to_string()
                ),
            ],
            "case {case:?}: both entries of a repeated name must be refused, got {twice:?}"
        );
    }
    // The delete side follows the same rule.
    let deleted_twice = client
        .delete_topics("a delete naming one topic twice", &["admin-held", "admin-held"])
        .await;
    assert_eq!(
        deleted_twice,
        vec![
            (
                "admin-held".to_string(),
                invalid_request,
                repeated_entries.to_string()
            ),
            (
                "admin-held".to_string(),
                invalid_request,
                repeated_entries.to_string()
            ),
        ],
        "both entries of a repeated name must be refused on the delete too, got \
         {deleted_twice:?}"
    );

    // The two BOUNDS the rows above sit outside of, at the bound and one past it: a
    // topic name is at most 249 characters (Apache Kafka's own maximum, which this
    // service reads as `MAX_TOPIC_NAME_LENGTH`), and this broker places at most 1024
    // partitions of one topic (`MAX_PARTITIONS_PER_TOPIC`).
    let longest_name = "n".repeat(249);
    let over_long_name = "n".repeat(250);
    // (case, the topic the request asks for, the error code, the message)
    let bounds: &[(&str, (&str, i32), i16, String)] = &[
        ("the longest name a topic may carry", (&longest_name, 1), 0, String::new()),
        (
            "one character past that name length",
            (&over_long_name, 1),
            invalid_topic,
            format!(
                "Topic name is illegal, it can't be longer than 249 characters, topic name: \
                 {over_long_name}"
            ),
        ),
        (
            "the most partitions this broker places",
            ("admin-at-the-partition-bound", 1_024),
            0,
            String::new(),
        ),
    ];
    for (case, (topic, partitions), code, message) in bounds {
        let answered = client
            .create_topics(case, &[(topic, *partitions, 1, &[])], false)
            .await;
        let (name, answered_code, answered_message, placed, _) = answered
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported no result"));
        assert_eq!(
            (name.as_str(), answered_code, answered_message.as_str()),
            (*topic, *code, message.as_str()),
            "case {case:?}: expected ({code}, {message:?}), got {answered:?}"
        );
        if *code == 0 {
            assert_eq!(
                placed, *partitions,
                "case {case:?}: expected {partitions} partitions placed, got {placed}"
            );
        }
    }
    // So the cluster holds exactly the names it could place: the held one and the two
    // at the bounds.
    let held_names = client.metadata_topics("at the bounds").await;
    assert_eq!(
        held_names.keys().cloned().collect::<Vec<String>>(),
        vec![
            "admin-at-the-partition-bound".to_string(),
            "admin-held".to_string(),
            longest_name.clone(),
        ],
        "only the names the broker could place may be registered, got {:?}",
        held_names.keys().collect::<Vec<&String>>()
    );
}

impl Client {
    /// `CreateTopics` for one topic whose replicas are laid out BY HAND: one entry per
    /// partition, each naming the brokers that partition's replicas sit on.
    ///
    /// `partitions` and `replication_factor` are sent as the request states them, so a
    /// row can show what a request that states both a placement and a count is answered.
    async fn create_topic_with_assignments(
        &mut self,
        case: &str,
        topic: &str,
        partitions: i32,
        replication_factor: i16,
        assignments: &[&[i32]],
    ) -> CreatedTopic {
        let answered: ::kafka_protocol::messages::CreateTopicsResponse = self
            .call(
                case,
                ApiKey::CreateTopics,
                5,
                &::kafka_protocol::messages::CreateTopicsRequest::default()
                    .with_timeout_ms(1_000)
                    .with_topics(vec![
                        ::kafka_protocol::messages::create_topics_request::CreatableTopic::default()
                            .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                            .with_num_partitions(partitions)
                            .with_replication_factor(replication_factor)
                            .with_assignments(
                                assignments
                                    .iter()
                                    .enumerate()
                                    .map(|(index, brokers)| {
                                        ::kafka_protocol::messages::create_topics_request::CreatableReplicaAssignment::default()
                                            .with_partition_index(i32::try_from(index).unwrap_or_default())
                                            .with_broker_ids(
                                                brokers
                                                    .iter()
                                                    .map(|broker| ::kafka_protocol::messages::BrokerId(*broker))
                                                    .collect(),
                                            )
                                    })
                                    .collect(),
                            ),
                    ]),
            )
            .await;
        let result = answered
            .topics
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: CreateTopics reported no result"));
        (
            result.name.0.to_string(),
            result.error_code,
            result
                .error_message
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            result.num_partitions,
            result.replication_factor,
        )
    }
}

/// One row of the accepted-placement table: the case, the topic, the placement (one
/// broker list per partition), and the partitions and replication factor the broker must
/// report for it.
type PlacedTopic<'a> = (&'a str, &'a str, &'a [&'a [i32]], i32, i16);

/// One row of the refused-placement table: the case, the topic, the partition count and
/// replication factor the request states beside the placement, the placement itself, and
/// the error code and message the broker must answer.
type RefusedPlacement<'a> = (&'a str, &'a str, i32, i16, &'a [&'a [i32]], i16, &'a str);

/// RULE: a manual replica assignment STATES the placement — one entry per partition,
/// each naming the brokers that partition's replicas sit on — so the topic it creates
/// carries one partition per entry and is replicated as many times as each entry names;
/// a placement the cluster cannot make is refused with the Apache Kafka error code for
/// that class and nothing is created.
///
/// The rows differ along what the rule generalises over: how many partitions the
/// placement lays out, how many replicas each carries, and each way a placement can
/// name something the cluster cannot do — a count stated beside it, a partition
/// replicated a different number of times, one partition placed twice on one broker, and
/// a broker the cluster does not have.
#[tokio::test]
async fn a_manual_replica_assignment_states_the_placement_or_is_refused() {
    let invalid_request = ::kafka_protocol::ResponseError::InvalidRequest.code();
    let invalid_assignment = ::kafka_protocol::ResponseError::InvalidReplicaAssignment.code();

    let emulator = Emulator::new();
    // Three brokers, which are the brokers 1, 2 and 3 a placement may name.
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("admin-assignments")
        })
        .await;
    let authority =
        published_address(&emulator, &cluster, "bootstrapBrokerString", "assignments").await;
    let mut client = Client::plaintext("assignments", &authority).await;
    client.api_versions("assignments").await;

    // (case, the topic, the placement, the partitions and replication factor the broker
    // must report)
    let placed: &[PlacedTopic] = &[
        (
            "one partition on one broker",
            "assigned-single",
            &[&[1]],
            1,
            1,
        ),
        (
            "two partitions, one replica each",
            "assigned-spread",
            &[&[1], &[2]],
            2,
            1,
        ),
        (
            "three partitions replicated twice",
            "assigned-replicated",
            &[&[1, 2], &[2, 3], &[3, 1]],
            3,
            2,
        ),
    ];
    for (case, topic, assignments, partitions, replication_factor) in placed {
        let (name, code, message, placed_partitions, placed_replicas) = client
            .create_topic_with_assignments(case, topic, -1, -1, assignments)
            .await;
        assert_eq!(
            (name.as_str(), code, message.as_str()),
            (*topic, 0, ""),
            "case {case:?}: the placement must be accepted, got ({code}, {message:?})"
        );
        assert_eq!(
            (placed_partitions, placed_replicas),
            (*partitions, *replication_factor),
            "case {case:?}: expected {partitions} partitions replicated {replication_factor} \
             time(s), got {placed_partitions} and {placed_replicas}"
        );
        // The partitions the placement laid out are the partitions the cluster serves.
        let listed = client.metadata_topics(case).await;
        assert_eq!(
            listed.get(*topic).copied(),
            Some(usize::try_from(*partitions).unwrap_or_default()),
            "case {case:?}: Metadata must report {partitions} partitions, got {listed:?}"
        );
    }

    // (case, the topic, the count the request states beside the placement, the
    // placement, the error code, the message)
    let refused: &[RefusedPlacement] = &[
        (
            "a partition count stated beside the placement",
            "assigned-and-counted",
            2,
            -1,
            &[&[1], &[2]],
            invalid_request,
            "Both numPartitions or replicationFactor and replicasAssignments were set. Both \
             cannot be used at the same time.",
        ),
        (
            "a replication factor stated beside the placement",
            "assigned-and-factored",
            -1,
            1,
            &[&[1], &[2]],
            invalid_request,
            "Both numPartitions or replicationFactor and replicasAssignments were set. Both \
             cannot be used at the same time.",
        ),
        (
            "one partition replicated twice and the next once",
            "assigned-ragged",
            -1,
            -1,
            &[&[1, 2], &[3]],
            invalid_assignment,
            "Inconsistent replication factor between partitions, partition 0 has 2 while \
             partition 1 has 1 replicas.",
        ),
        (
            "both replicas of a partition on one broker",
            "assigned-duplicated",
            -1,
            -1,
            &[&[1, 1]],
            invalid_assignment,
            "Duplicate brokers not allowed in replica assignment for partition 0.",
        ),
        (
            "a broker the cluster does not have",
            "assigned-unknown-broker",
            -1,
            -1,
            &[&[1], &[9]],
            invalid_assignment,
            "Unknown broker(s) in replica assignment for partition 1.",
        ),
        (
            "a partition placed on no broker at all",
            "assigned-empty",
            -1,
            -1,
            &[&[]],
            invalid_assignment,
            "Inconsistent replication factor between partitions, partition 0 has 0 while \
             partition 0 has 0 replicas.",
        ),
    ];
    for (case, topic, partitions, replication_factor, assignments, code, message) in refused {
        let (name, answered_code, answered_message, _, _) = client
            .create_topic_with_assignments(
                case,
                topic,
                *partitions,
                *replication_factor,
                assignments,
            )
            .await;
        assert_eq!(
            (name.as_str(), answered_code, answered_message.as_str()),
            (*topic, *code, *message),
            "case {case:?}: expected the refusal ({code}, {message:?}), got ({answered_code}, \
             {answered_message:?})"
        );
    }
    // Only the placements the cluster could make are registered.
    let held = client.metadata_topics("the placements").await;
    assert_eq!(
        held.keys().cloned().collect::<Vec<String>>(),
        vec![
            "assigned-replicated".to_string(),
            "assigned-single".to_string(),
            "assigned-spread".to_string(),
        ],
        "a refused placement must register nothing, got {held:?}"
    );
}

/// RULE: a topic an admin client DELETES over the published address stops being the
/// cluster's topic — it leaves the brokers' metadata and keeps no records, so a name
/// created again is a new, empty topic.
#[tokio::test]
async fn a_deleted_topic_leaves_the_cluster_and_keeps_no_records() {
    let case = "a deleted and recreated topic";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("PLAINTEXT"),
            ..ClusterRequest::named("admin-deletion")
        })
        .await;
    let authority = published_address(&emulator, &cluster, "bootstrapBrokerString", case).await;
    let mut client = Client::plaintext(case, &authority).await;
    client.api_versions(case).await;

    let created = client
        .create_topics(case, &[("admin-recycled", 1, 1, &[])], false)
        .await;
    assert_eq!(
        created.first().map(|(_, code, _, _, _)| *code),
        Some(0),
        "case {case:?}: the topic must be created, got {created:?}"
    );
    client
        .produce(
            case,
            "admin-recycled",
            0,
            &[
                (epoch_millis(), None, "first"),
                (epoch_millis(), None, "second"),
            ],
        )
        .await;
    let before = client.fetch(case, "admin-recycled", 0, 0).await;
    assert_eq!(
        before.len(),
        2,
        "case {case:?}: both records must be in the log, got {before:?}"
    );

    let deleted = client.delete_topics(case, &["admin-recycled"]).await;
    assert_eq!(
        deleted,
        vec![("admin-recycled".to_string(), 0, String::new())],
        "case {case:?}: the delete must be accepted, got {deleted:?}"
    );

    // The removal is a transition the clock owes the topic, so the metadata is read
    // again until it no longer reports the name.
    let deadline = std::time::Instant::now() + TOPIC_CONDITION_DEADLINE;
    loop {
        let listed = client.metadata_topics(case).await;
        if !listed.contains_key("admin-recycled") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the deleted topic was still reported, got {listed:?}"
        );
        std::thread::sleep(TOPIC_PROBE_INTERVAL);
    }

    // The name is free again, and the topic created under it is empty: the records of
    // the topic that held the name are gone with it.
    let recreated = client
        .create_topics(case, &[("admin-recycled", 1, 1, &[])], false)
        .await;
    assert_eq!(
        recreated.first().map(|(_, code, _, _, _)| *code),
        Some(0),
        "case {case:?}: the name must be free to create again, got {recreated:?}"
    );
    let after = client.fetch(case, "admin-recycled", 0, 0).await;
    assert!(
        after.is_empty(),
        "case {case:?}: a recreated topic must hold no records, got {after:?}"
    );
    let offset = client
        .offset(case, "admin-recycled", 0, OFFSET_LATEST)
        .await;
    assert_eq!(
        offset, 0,
        "case {case:?}: a recreated topic's log must start at offset 0, got {offset}"
    );
}

/// A SASL/SCRAM secret, minted through Secrets Manager's own `CreateSecret` and
/// associated with the cluster, which is where a broker reads the credential from.
async fn given_secret_associated(
    emulator: &Emulator,
    cluster: &str,
    name: &str,
    case: &str,
) -> String {
    given_secret_associated_as(emulator, cluster, name, SCRAM_USER, SCRAM_PASSWORD, case).await
}

/// The same, for the SASL/SCRAM user and password the secret is to carry — so a suite
/// whose rule is about WHICH credential a cluster holds can associate more than one.
pub(crate) async fn given_secret_associated_as(
    emulator: &Emulator,
    cluster: &str,
    name: &str,
    user: &str,
    password: &str,
    case: &str,
) -> String {
    let created = emulator
        .secrets_manager(
            REGION,
            "CreateSecret",
            json!({
                "Name": name,
                "ClientRequestToken": format!("00000000-0000-4000-8000-{:012x}", name.len() * 7919),
                "KmsKeyId": CUSTOMER_KEY_ARN,
                "SecretString":
                    format!("{{\"username\":\"{user}\",\"password\":\"{password}\"}}"),
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: Secrets Manager must create the secret, got {} {}",
        created.status, created.body
    );
    let arn = created
        .at("/ARN")
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: Secrets Manager reported no ARN"))
        .to_string();
    let associated = emulator
        .call(
            "POST",
            &format!("/v1/clusters/{}/scram-secrets", pct(cluster)),
            Some(json!({ "secretArnList": [arn.clone()] })),
        )
        .await;
    assert_eq!(
        associated.status, 200,
        "case {case:?}: BatchAssociateScramSecret must be accepted, got {} {}",
        associated.status, associated.body
    );
    assert_eq!(
        associated.at("/unprocessedScramSecrets").as_array().map(Vec::len),
        Some(0),
        "case {case:?}: the secret must be associated, got {}",
        associated.body
    );
    arn
}

/// One request/response exchange whose outcome is REPORTED rather than asserted, so a
/// sweep over many API versions can name every pair that went wrong instead of stopping
/// at the first.
impl Client {
    /// Send `request` at `version` and decode the response that version declares.
    ///
    /// `Err` carries what went wrong: a frame this client could not encode, a
    /// connection the broker closed instead of answering, or a response that does not
    /// decode as the version declares it.
    async fn exchange<Request: Encodable, Response: Decodable>(
        &mut self,
        key: ApiKey,
        version: i16,
        request: &Request,
    ) -> Result<Response, String> {
        self.correlation += 1;
        let mut frame = BytesMut::new();
        RequestHeader::default()
            .with_request_api_key(key as i16)
            .with_request_api_version(version)
            .with_correlation_id(self.correlation)
            .with_client_id(Some(StrBytes::from_static_str(CLIENT_ID)))
            .encode(&mut frame, key.request_header_version(version))
            .map_err(|error| format!("this suite could not encode the request header: {error}"))?;
        request
            .encode(&mut frame, version)
            .map_err(|error| format!("this suite could not encode the request: {error}"))?;
        let length = u32::try_from(frame.len()).expect("a request frame fits in a u32");
        self.session
            .write_all(&length.to_be_bytes())
            .await
            .map_err(|error| format!("the frame could not be written: {error}"))?;
        self.session
            .write_all(&frame)
            .await
            .map_err(|error| format!("the frame could not be written: {error}"))?;
        let mut answered = self
            .answer()
            .await
            .ok_or_else(|| "the broker closed the connection instead of answering".to_string())?;
        ResponseHeader::decode(&mut answered, key.response_header_version(version))
            .map_err(|error| format!("the response header does not decode: {error}"))?;
        Response::decode(&mut answered, version)
            .map_err(|error| format!("the response does not decode: {error}"))
    }

    /// The next response frame, or `None` when the broker closed the connection or
    /// framed something this client cannot read.
    async fn answer(&mut self) -> Option<Bytes> {
        let mut length = [0u8; 4];
        if self.session.read_exact(&mut length).await.is_err() {
            return None;
        }
        let length = u32::from_be_bytes(length) as usize;
        if length == 0 || length > 8 * 1024 * 1024 {
            return None;
        }
        let mut frame = vec![0u8; length];
        self.session.read_exact(&mut frame).await.ok()?;
        Some(Bytes::from(frame))
    }
}

/// Call `key` at `version` over `client`, with the smallest request that version
/// declares, reporting what went wrong instead of asserting.
///
/// One arm per API this endpoint advertises; an API no arm names is itself reported, so
/// the sweep below cannot silently skip a newly served one.
async fn call_at(
    client: &mut Client,
    key: ApiKey,
    version: i16,
    topic: &str,
) -> Result<(), String> {
    use ::kafka_protocol::messages as wire;

    let topic_name = TopicName(StrBytes::from_string(topic.to_string()));
    let group = wire::GroupId(StrBytes::from_static_str("version-sweep"));
    match key {
        ApiKey::ApiVersions => {
            let mut request = ApiVersionsRequest::default();
            // The client's own name travels only on the versions that carry it.
            if version >= 3 {
                request = request
                    .with_client_software_name(StrBytes::from_static_str(CLIENT_ID))
                    .with_client_software_version(StrBytes::from_static_str("1"));
            }
            client
                .exchange::<_, ApiVersionsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::SaslHandshake => {
            let request = SaslHandshakeRequest::default()
                .with_mechanism(StrBytes::from_static_str(SCRAM_SHA_512));
            client
                .exchange::<_, SaslHandshakeResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::SaslAuthenticate => {
            let request = SaslAuthenticateRequest::default()
                .with_auth_bytes(Bytes::from_static(b"n,,n=nobody,r=nonce"));
            client
                .exchange::<_, SaslAuthenticateResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::Metadata => {
            let request = MetadataRequest::default().with_topics(Some(vec![
                wire::metadata_request::MetadataRequestTopic::default()
                    .with_name(Some(topic_name.clone())),
            ]));
            client
                .exchange::<_, MetadataResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::Produce => {
            let record = Record {
                transactional: false,
                control: false,
                partition_leader_epoch: 0,
                producer_id: -1,
                producer_epoch: -1,
                timestamp_type: TimestampType::Creation,
                offset: 0,
                sequence: 0,
                timestamp: epoch_millis(),
                key: None,
                value: Some(Bytes::from_static(b"swept")),
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
            .map_err(|error| format!("this suite could not encode the batch: {error}"))?;
            let request = ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(topic_name.clone())
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(0)
                        .with_records(Some(encoded.freeze()))])]);
            client
                .exchange::<_, ProduceResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::Fetch => {
            let request = FetchRequest::default()
                .with_max_wait_ms(0)
                .with_min_bytes(1)
                .with_topics(vec![FetchTopic::default()
                    .with_topic(topic_name.clone())
                    .with_partitions(vec![FetchPartition::default()
                        .with_partition(0)
                        .with_fetch_offset(0)
                        .with_partition_max_bytes(1_048_576)])]);
            client
                .exchange::<_, FetchResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::ListOffsets => {
            let request = ListOffsetsRequest::default().with_topics(vec![
                ListOffsetsTopic::default()
                    .with_name(topic_name.clone())
                    .with_partitions(vec![ListOffsetsPartition::default()
                        .with_partition_index(0)
                        .with_timestamp(OFFSET_EARLIEST)]),
            ]);
            client
                .exchange::<_, ListOffsetsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DescribeConfigs => {
            let request = DescribeConfigsRequest::default().with_resources(vec![
                DescribeConfigsResource::default()
                    .with_resource_type(RESOURCE_TYPE_TOPIC)
                    .with_resource_name(StrBytes::from_string(topic.to_string())),
            ]);
            client
                .exchange::<_, DescribeConfigsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::FindCoordinator => {
            // The group a request names travels in the single `key` up to version 3 and
            // in the `coordinator_keys` list from version 4 on.
            let mut request = wire::FindCoordinatorRequest::default();
            request = match version >= 4 {
                true => request.with_coordinator_keys(vec![group.0.clone()]),
                false => request.with_key(group.0.clone()),
            };
            client
                .exchange::<_, wire::FindCoordinatorResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::InitProducerId => {
            let request = wire::InitProducerIdRequest::default().with_transaction_timeout_ms(-1);
            client
                .exchange::<_, wire::InitProducerIdResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::JoinGroup => {
            let request = wire::JoinGroupRequest::default()
                .with_group_id(group.clone())
                .with_session_timeout_ms(10_000)
                .with_protocol_type(StrBytes::from_static_str("consumer"))
                .with_protocols(vec![
                    wire::join_group_request::JoinGroupRequestProtocol::default()
                        .with_name(StrBytes::from_static_str("range"))
                        .with_metadata(Bytes::from_static(b"subscription")),
                ]);
            client
                .exchange::<_, wire::JoinGroupResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::SyncGroup => {
            let request = wire::SyncGroupRequest::default()
                .with_group_id(group.clone())
                .with_generation_id(1);
            client
                .exchange::<_, wire::SyncGroupResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::Heartbeat => {
            let request = wire::HeartbeatRequest::default()
                .with_group_id(group.clone())
                .with_generation_id(1);
            client
                .exchange::<_, wire::HeartbeatResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::LeaveGroup => {
            // The leaving member is named by `member_id` up to version 2 and by the
            // `members` list from version 3 on.
            let mut request = wire::LeaveGroupRequest::default().with_group_id(group.clone());
            request = match version >= 3 {
                true => request.with_members(vec![
                    wire::leave_group_request::MemberIdentity::default()
                        .with_member_id(StrBytes::from_static_str("version-sweep-1")),
                ]),
                false => request.with_member_id(StrBytes::from_static_str("version-sweep-1")),
            };
            client
                .exchange::<_, wire::LeaveGroupResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::OffsetCommit => {
            let request = wire::OffsetCommitRequest::default()
                .with_group_id(group.clone())
                .with_topics(vec![wire::offset_commit_request::OffsetCommitRequestTopic::default()
                    .with_name(topic_name.clone())
                    .with_partitions(vec![
                        wire::offset_commit_request::OffsetCommitRequestPartition::default()
                            .with_partition_index(0)
                            .with_committed_offset(0),
                    ])]);
            client
                .exchange::<_, wire::OffsetCommitResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::OffsetFetch => {
            let request = wire::OffsetFetchRequest::default()
                .with_group_id(group.clone())
                .with_topics(Some(vec![
                    wire::offset_fetch_request::OffsetFetchRequestTopic::default()
                        .with_name(topic_name.clone())
                        .with_partition_indexes(vec![0]),
                ]));
            client
                .exchange::<_, wire::OffsetFetchResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::CreateAcls => {
            let request = wire::CreateAclsRequest::default().with_creations(vec![
                wire::create_acls_request::AclCreation::default()
                    .with_resource_type(RESOURCE_TYPE_TOPIC)
                    .with_resource_name(StrBytes::from_string(topic.to_string()))
                    .with_resource_pattern_type(ACL_PATTERN_TYPE_LITERAL)
                    .with_principal(StrBytes::from_static_str("User:swept"))
                    .with_host(StrBytes::from_static_str("*"))
                    .with_operation(ACL_OPERATION_READ)
                    .with_permission_type(ACL_PERMISSION_ALLOW),
            ]);
            client
                .exchange::<_, wire::CreateAclsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DescribeAcls => {
            // The pattern-type filter exists from version 1 on; version 0 matches on the
            // literal name alone.
            let mut request = wire::DescribeAclsRequest::default()
                .with_resource_type_filter(RESOURCE_TYPE_TOPIC)
                .with_operation(ACL_OPERATION_ANY)
                .with_permission_type(ACL_PERMISSION_ANY);
            if version >= 1 {
                request = request.with_pattern_type_filter(ACL_PATTERN_TYPE_ANY);
            }
            client
                .exchange::<_, wire::DescribeAclsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DeleteAcls => {
            let mut filter = wire::delete_acls_request::DeleteAclsFilter::default()
                .with_resource_type_filter(RESOURCE_TYPE_TOPIC)
                .with_operation(ACL_OPERATION_ANY)
                .with_permission_type(ACL_PERMISSION_ANY);
            if version >= 1 {
                filter = filter.with_pattern_type_filter(ACL_PATTERN_TYPE_ANY);
            }
            let request = wire::DeleteAclsRequest::default().with_filters(vec![filter]);
            client
                .exchange::<_, wire::DeleteAclsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::CreateTopics => {
            // A name of its own per row, so no row depends on another's outcome.
            let request = wire::CreateTopicsRequest::default()
                .with_timeout_ms(1_000)
                .with_topics(vec![
                    wire::create_topics_request::CreatableTopic::default()
                        .with_name(TopicName(StrBytes::from_string(format!(
                            "swept-create-v{version}"
                        ))))
                        .with_num_partitions(1)
                        .with_replication_factor(1),
                ]);
            client
                .exchange::<_, wire::CreateTopicsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::CreatePartitions => {
            // A name of its own per row, so no row depends on another's outcome: the
            // topic is not there, which this API answers per topic.
            let request = wire::CreatePartitionsRequest::default()
                .with_timeout_ms(1_000)
                .with_topics(vec![
                    wire::create_partitions_request::CreatePartitionsTopic::default()
                        .with_name(TopicName(StrBytes::from_string(format!(
                            "swept-grow-v{version}"
                        ))))
                        .with_count(4)
                        .with_assignments(None),
                ]);
            client
                .exchange::<_, wire::CreatePartitionsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DeleteTopics => {
            let request = wire::DeleteTopicsRequest::default()
                .with_timeout_ms(1_000)
                .with_topic_names(vec![TopicName(StrBytes::from_string(format!(
                    "swept-delete-v{version}"
                )))]);
            client
                .exchange::<_, wire::DeleteTopicsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::OffsetForLeaderEpoch => {
            let request = wire::OffsetForLeaderEpochRequest::default().with_topics(vec![
                wire::offset_for_leader_epoch_request::OffsetForLeaderTopic::default()
                    .with_topic(topic_name.clone())
                    .with_partitions(vec![
                        wire::offset_for_leader_epoch_request::OffsetForLeaderPartition::default()
                            .with_partition(0)
                            .with_leader_epoch(0),
                    ]),
            ]);
            client
                .exchange::<_, wire::OffsetForLeaderEpochResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DescribeGroups => {
            let request = wire::DescribeGroupsRequest::default().with_groups(vec![group.clone()]);
            client
                .exchange::<_, wire::DescribeGroupsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::ListGroups => {
            let request = wire::ListGroupsRequest::default();
            client
                .exchange::<_, wire::ListGroupsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        ApiKey::DeleteGroups => {
            // A group name of its own per row, so no row depends on another's outcome.
            let request = wire::DeleteGroupsRequest::default().with_groups_names(vec![
                wire::GroupId(StrBytes::from_string(format!("swept-group-v{version}"))),
            ]);
            client
                .exchange::<_, wire::DeleteGroupsResponse>(key, version, &request)
                .await
                .map(drop)
        }
        other => Err(format!(
            "this suite does not know how to call {other:?}, which the broker advertises"
        )),
    }
}

/// RULE: a produce whose records payload the broker cannot READ is refused with the
/// protocol's `CORRUPT_MESSAGE` and appends nothing — a producer is told its records were
/// not written, rather than handed the offset of an append that never happened.
///
/// The rows span the ways a payload can be unreadable (bytes that are no batch at all, a
/// batch truncated mid-header, a batch declaring a format version this endpoint does not
/// write), and a batch the endpoint CAN read is the positive control that the refusal is
/// about the payload and not about the request.
#[tokio::test]
async fn a_produce_the_broker_cannot_read_is_refused_and_appends_nothing() {
    let case = "an unreadable produce";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest::named("plane-corrupt"))
        .await;
    sleep_past_activation();
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let addresses = published(&emulator, &cluster, case).await;
    let authority = addresses
        .get("bootstrapBrokerString")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the cluster must publish a plaintext bootstrap string")
        });

    let mut readable = BytesMut::new();
    RecordBatchEncoder::encode(
        &mut readable,
        [Record {
            transactional: false,
            control: false,
            partition_leader_epoch: 0,
            producer_id: -1,
            producer_epoch: -1,
            timestamp_type: TimestampType::Creation,
            offset: 0,
            sequence: 0,
            timestamp: epoch_millis(),
            key: None,
            value: Some(Bytes::from_static(b"readable")),
            headers: Default::default(),
            delete_horizon: false,
        }]
        .iter(),
        &RecordEncodeOptions {
            version: 2,
            compression: Compression::None,
        },
    )
    .unwrap_or_else(|error| panic!("case {case:?}: the batch must encode: {error}"));
    let readable = readable.freeze();
    // A record batch declares its format version in the byte after its base offset, its
    // length and its partition leader epoch (Apache Kafka's record batch header), so a
    // value no format uses is a batch no broker can read.
    let mut unknown_format = readable.to_vec();
    unknown_format[16] = 9;

    let rows: &[(&str, Bytes)] = &[
        (
            "bytes that are no record batch at all",
            Bytes::from_static(b"this is not a record batch"),
        ),
        ("a batch truncated mid-header", readable.slice(0..12)),
        (
            "a batch of an unknown format version",
            Bytes::from(unknown_format),
        ),
    ];
    for (row, payload) in rows {
        let probe = format!("{case} / {row}");
        let mut client = Client::plaintext(&probe, &authority).await;
        client.api_versions(&probe).await;
        let before = client.offset(&probe, "orders", 0, OFFSET_LATEST).await;
        let answered = produce_payload(&mut client, "orders", payload.clone()).await;
        assert_eq!(
            answered.0,
            ::kafka_protocol::ResponseError::CorruptMessage.code(),
            "case {probe:?}: expected the produce to be refused CORRUPT_MESSAGE, got error {} \
             and base offset {}",
            answered.0,
            answered.1
        );
        let after = client.offset(&probe, "orders", 0, OFFSET_LATEST).await;
        assert_eq!(
            after, before,
            "case {probe:?}: a refused produce must append nothing, but the log end moved from \
             {before} to {after}"
        );
    }

    // The positive control: the same request carrying a batch this endpoint reads is
    // appended, at the offset the log was at.
    let probe = format!("{case} / a batch the broker reads");
    let mut client = Client::plaintext(&probe, &authority).await;
    client.api_versions(&probe).await;
    let before = client.offset(&probe, "orders", 0, OFFSET_LATEST).await;
    let answered = produce_payload(&mut client, "orders", readable.clone()).await;
    assert_eq!(
        (answered.0, answered.1),
        (0, before),
        "case {probe:?}: expected the produce to be appended at offset {before}, got error {} \
         and base offset {}",
        answered.0,
        answered.1
    );
    let read = client.fetch(&probe, "orders", 0, before).await;
    assert_eq!(
        read.first().map(|(_, _, value)| value.clone()),
        Some("readable".to_string()),
        "case {probe:?}: the appended record must be readable back, got {read:?}"
    );
}

/// `Produce` one partition with `payload` as its records field EXACTLY as given, so a
/// suite can submit bytes a record-batch encoder would never produce. Answers the
/// `(error code, base offset)` the broker reported for the partition.
async fn produce_payload(client: &mut Client, topic: &str, payload: Bytes) -> (i16, i64) {
    let answered: ProduceResponse = client
        .exchange(
            ApiKey::Produce,
            9,
            &ProduceRequest::default()
                .with_acks(1)
                .with_timeout_ms(1_000)
                .with_topic_data(vec![TopicProduceData::default()
                    .with_name(TopicName(StrBytes::from_string(topic.to_string())))
                    .with_partition_data(vec![PartitionProduceData::default()
                        .with_index(0)
                        .with_records(Some(payload))])]),
        )
        .await
        .unwrap_or_else(|why| panic!("the broker must answer a Produce request: {why}"));
    let partition = answered
        .responses
        .first()
        .and_then(|topic| topic.partition_responses.first())
        .cloned()
        .expect("Produce must report the partition it was given");
    (partition.error_code, partition.base_offset)
}

/// RULE: a broker reads a connection's SASL tokens in the framing that connection's
/// `SaslHandshake` VERSION selects — bare, length-prefixed byte strings at version 0,
/// and `SaslAuthenticate` requests and responses from version 1 on — and either way the
/// completed exchange authenticates the connection, which is then served.
///
/// A client chooses that version, and the versions a broker advertises are what it
/// chooses from: a broker that advertises version 0 and then reads the token after it as
/// a Kafka request frame drops a connection it told the client to open that way, leaving
/// the client reconnecting with no error it can report.
///
/// The rows span what the rule generalises over: the handshake VERSION, and which
/// MECHANISM the exchange runs (the cluster's SASL/SCRAM credential, and IAM access
/// control as both the Java exchange and the `OAUTHBEARER` one a non-Java client
/// speaks). Each row proves the connection is usable afterwards by producing a record
/// and reading it back, and each mechanism is also offered a credential the cluster does
/// not hold, which must be refused rather than served.
#[tokio::test]
async fn a_sasl_exchange_is_served_in_the_framing_its_handshake_selects() {
    let case = "a sasl exchange's token framing";
    let emulator = Emulator::with_secrets_manager();
    let cluster = emulator
        .create_ok(&ClusterRequest {
            client_broker: Some("TLS"),
            sasl_scram: Some(true),
            sasl_iam: Some(true),
            ..ClusterRequest::named("plane-framing")
        })
        .await;
    sleep_past_activation();
    given_secret_associated(&emulator, &cluster, "plane-framing-secret", case).await;
    given_topic(&emulator, &cluster, "framed", 1, None).await;
    let addresses = published(&emulator, &cluster, case).await;
    let scram_endpoint = addresses
        .get("bootstrapBrokerStringSaslScram")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the cluster must publish its SASL/SCRAM string"));
    let iam_endpoint = addresses
        .get("bootstrapBrokerStringSaslIam")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the cluster must publish its SASL/IAM string"));

    // (mechanism, the endpoint that offers it, the credential it authenticates with),
    // each driven at both handshake versions.
    let rows: &[(&str, &str, &str)] = &[
        (SCRAM_SHA_512, &scram_endpoint, SCRAM_PASSWORD),
        (AWS_MSK_IAM, &iam_endpoint, "a-signature"),
        (OAUTHBEARER, &iam_endpoint, "a-signature"),
    ];
    for (mechanism, authority, credential) in rows {
        for handshake_version in [0, 1] {
            let probe = format!("{case} / {mechanism} / handshake v{handshake_version}");
            let mut client = Client::tls(&probe, authority).await;
            client.api_versions(&probe).await;
            let code = client
                .authenticate_at(&probe, mechanism, SCRAM_USER, credential, handshake_version)
                .await;
            assert_eq!(
                code, 0,
                "case {probe:?}: the exchange must authenticate the connection, got error {code}"
            );
            // The connection is served now: a record produced over it is read back over
            // it, which is what the exchange was for.
            let value = format!("framed-{mechanism}-v{handshake_version}");
            let offset = client
                .produce(&probe, "framed", 0, &[(epoch_millis(), None, &value)])
                .await;
            let read = client.fetch(&probe, "framed", 0, offset).await;
            assert_eq!(
                read.first().map(|(_, _, value)| value.clone()),
                Some(value.clone()),
                "case {probe:?}: the record produced over the authenticated connection must be \
                 read back, got {read:?}"
            );

            // And a credential the cluster does not hold is refused in either framing.
            // The unwrapped framing carries no error code, so a broker refusing one
            // closes the connection, which this client reports as the authentication
            // failure it is.
            let mut refused = Client::tls(&probe, authority).await;
            refused.api_versions(&probe).await;
            let code = refused
                .authenticate_at(&probe, mechanism, SCRAM_USER, "", handshake_version)
                .await;
            assert_eq!(
                code,
                ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code(),
                "case {probe:?}: a credential the cluster does not hold must be refused, got \
                 error {code}"
            );
        }
    }
}

/// RULE: every (API, version) pair a broker ADVERTISES in `ApiVersions` is answered with
/// the response that version declares.
///
/// A Kafka client negotiates what it sends from that advertisement, and Apache Kafka
/// answers a frame it cannot serve by closing the connection — so a pair the endpoint
/// announces and then does not answer leaves the client reconnecting against a socket
/// that drops it, with no error it can report. The recorded case is a client picking the
/// newest version of each API; the table here is the WHOLE advertised class, read from
/// the broker itself: every version from each advertised `min_version` to its
/// `max_version`.
#[tokio::test]
async fn every_advertised_api_version_is_answered() {
    let case = "every advertised api version";
    let emulator = Emulator::new();
    let cluster = emulator
        .create_ok(&ClusterRequest::named("plane-versions"))
        .await;
    sleep_past_activation();
    given_topic(&emulator, &cluster, "orders", 1, None).await;
    let addresses = published(&emulator, &cluster, case).await;
    let authority = addresses
        .get("bootstrapBrokerString")
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the cluster must publish a plaintext bootstrap string")
        });

    // A record in the log, so the read APIs below answer over a partition that holds
    // something rather than over an empty one.
    let mut writer = Client::plaintext(case, &authority).await;
    writer.api_versions(case).await;
    writer
        .produce(case, "orders", 0, &[(epoch_millis(), None, "swept")])
        .await;

    let advertised = Client::plaintext(case, &authority)
        .await
        .api_versions(case)
        .await;
    assert!(
        !advertised.api_keys.is_empty(),
        "case {case:?}: the broker must advertise the APIs it serves"
    );

    let mut unanswered: Vec<String> = Vec::new();
    for api in &advertised.api_keys {
        let key = ApiKey::try_from(api.api_key).unwrap_or_else(|_| {
            panic!(
                "case {case:?}: the broker advertised API key {}, which is not a Kafka API",
                api.api_key
            )
        });
        for version in api.min_version..=api.max_version {
            // One connection per row: a broker that drops the connection for one pair
            // must not decide the next row's outcome.
            let mut client = Client::plaintext(case, &authority).await;
            if let Err(why) = call_at(&mut client, key, version, "orders").await {
                unanswered.push(format!("{key:?} v{version}: {why}"));
            }
        }
    }
    assert!(
        unanswered.is_empty(),
        "case {case:?}: every advertised (API, version) pair must be answered with the \
         response that version declares; these were not: {unanswered:#?}"
    );
}
