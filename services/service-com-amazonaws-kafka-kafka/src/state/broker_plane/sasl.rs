// SPDX-License-Identifier: CC0-1.0

//! The SASL exchanges a client completes against a cluster's Apache Kafka endpoint.
//!
//! Which mechanisms a cluster offers is the client authentication it turns on — for its
//! brokers, or for its multi-VPC connectivity ([`advertised`]) — and each one is
//! answered as its specification defines it:
//!
//! * `SCRAM-SHA-512` is RFC 5802 over SHA-512 — the client proves it holds the
//!   password without sending it, and the server proves it holds the same one. The
//!   credential is NOT this service's: it lives in the AWS Secrets Manager secret the
//!   cluster associated (`BatchAssociateScramSecret`), so it is read from there
//!   ([`CredentialSource`]) through the in-process callback. A user no associated
//!   secret names cannot authenticate, which is what makes a disassociated secret stop
//!   working.
//! * `AWS_MSK_IAM` is the exchange Amazon MSK's IAM access control defines: the client
//!   sends a SigV4-signed JSON payload naming the `kafka-cluster:Connect` action, and
//!   the broker answers with the version and a request id. `OAUTHBEARER` (RFC 7628) is
//!   the same access control as a non-Java client speaks it, its token the signed
//!   `Connect` request Amazon MSK's auth-token signer produces. What is checked of
//!   either is its FORM: the documented version, that action, a signature, and a
//!   credential scoped to this cluster's Region and to the MSK data plane's signing
//!   name. The SIGNATURE itself is not verified and the policies behind the access key
//!   are not evaluated — this emulator evaluates no IAM — so a payload that is not a
//!   signed `Connect` request is refused and one that is, is served.
//!
//! Nothing here is waved through: a refusal is the protocol's own
//! `SASL_AUTHENTICATION_FAILED`, and an exchange that has not finished leaves the
//! connection unauthenticated, which is what the handler gates every other API on.
//! Which FRAMING carried a token is the client's choice and the handler's business
//! ([`super::wire`]): the exchanges here read and write token BYTES, whether those
//! bytes arrived bare or inside a `SaslAuthenticate` request.

use std::collections::BTreeMap;

use ::bytes::Bytes;
use ::service_common::jwt::decode_base64url;
use ::smithy_server_runtime::EmbeddedEmulator;

use super::super::Cluster;

/// The SASL mechanism names Amazon MSK's brokers offer, as a client names them in
/// `SaslHandshake`.
const SCRAM_SHA_512: &str = "SCRAM-SHA-512";
/// See [`SCRAM_SHA_512`].
const AWS_MSK_IAM: &str = "AWS_MSK_IAM";
/// See [`SCRAM_SHA_512`]: the mechanism a non-Java client authenticates with IAM
/// through (RFC 7628).
const OAUTHBEARER: &str = "OAUTHBEARER";

/// The iteration count the server-first message publishes for the SCRAM exchange.
/// RFC 5802 requires at least 4096; Amazon MSK's own SCRAM credentials are stored at
/// that count, so that is what this endpoint salts at.
const SCRAM_ITERATIONS: u32 = 4096;

/// How many bytes of salt the server-first message carries.
const SCRAM_SALT_BYTES: usize = 16;

/// The most bytes a SASL username and a client nonce this endpoint reads may carry.
/// CHOSEN as process-safety bounds, not protocol limits: both come from the client and
/// are carried in the message the proof is computed over, so without a ceiling one
/// connection would size that work. Sized well above the Apache Kafka user names and
/// the RFC 5802 nonces a client really sends.
const MAX_SASL_USERNAME_BYTES: usize = 256;
/// See [`MAX_SASL_USERNAME_BYTES`].
const MAX_SCRAM_NONCE_BYTES: usize = 256;

/// The version an `AWS_MSK_IAM` payload declares, and the one the answer carries
/// ("the version of the authentication payload", Amazon MSK IAM access control).
const IAM_PAYLOAD_VERSION: &str = "2020_10_22";

/// The action an `AWS_MSK_IAM` payload is signed for when a client opens a connection
/// (Amazon MSK IAM access control actions).
const IAM_CONNECT_ACTION: &str = "kafka-cluster:Connect";

/// The service the SigV4 credential in such a payload is scoped to.
const IAM_SIGNING_SERVICE: &str = "kafka-cluster";

/// One SASL mechanism this endpoint can complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mechanism {
    /// RFC 5802 over SHA-512, against the cluster's associated Secrets Manager
    /// secrets.
    ScramSha512,
    /// Amazon MSK's IAM access control exchange, as a Java client speaks it.
    AwsMskIam,
    /// The same IAM access control, as a NON-Java client speaks it: RFC 7628
    /// `OAUTHBEARER`, whose token is the signed `kafka-cluster:Connect` request Amazon
    /// MSK's auth-token signer produces. Amazon MSK's IAM port offers both, which is
    /// why a Python or C client authenticates with IAM at all.
    OAuthBearer,
}

impl Mechanism {
    /// The name a client selects this mechanism by.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::ScramSha512 => SCRAM_SHA_512,
            Self::AwsMskIam => AWS_MSK_IAM,
            Self::OAuthBearer => OAUTHBEARER,
        }
    }
}

/// The mechanisms `cluster`'s brokers offer: one per SASL method the cluster admits
/// clients on, in the order MSK's brokers advertise them. The IAM port offers the Java
/// exchange and the `OAUTHBEARER` one a non-Java client uses, since both authenticate
/// the same `kafka-cluster:Connect` signature.
///
/// A scheme is admitted when the cluster turns it on in EITHER of the two places it can
/// ([`Cluster::scram_clients_admitted`]): its own client authentication, or that of its
/// multi-VPC connectivity — because the control plane publishes a bootstrap string for
/// each of them (`BootstrapBrokerStringSaslScram` for the first,
/// `BootstrapBrokerStringVpcConnectivitySaslScram` for the second), both naming this
/// path's socket, and a published endpoint is one whose own SASL exchange reaches an
/// authentication verdict rather than one that answers no mechanism at all.
///
/// What a given PATH offers is this set narrowed to the path's own mechanisms
/// ([`super::AccessPath::mechanisms`]), so a client that offers SCRAM on the IAM port is
/// still told that port does not take it.
#[must_use]
pub fn advertised(cluster: &Cluster) -> Vec<Mechanism> {
    let mut mechanisms = Vec::new();
    if cluster.scram_clients_admitted() {
        mechanisms.push(Mechanism::ScramSha512);
    }
    if cluster.iam_clients_admitted() {
        mechanisms.push(Mechanism::AwsMskIam);
        mechanisms.push(Mechanism::OAuthBearer);
    }
    mechanisms
}

/// Who a completed exchange authenticated the connection as.
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// The principal the exchange established: the SCRAM user, or the access key id
    /// an IAM payload was signed with.
    pub principal: String,
}

/// One leg of an exchange: the challenge to send back, or the final message plus the
/// identity it established.
#[derive(Debug)]
pub enum Leg {
    /// The exchange needs another round trip; these bytes go back to the client.
    Continue(Bytes),
    /// The exchange is complete.
    Done {
        /// The last message of the exchange, which the client verifies.
        response: Bytes,
        /// Who the connection now speaks for.
        identity: Authenticated,
    },
}

/// Why an exchange was refused, as the protocol's own error code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// A client that authenticated before selecting a mechanism, or sent a leg the
    /// exchange was not at: `ILLEGAL_SASL_STATE`.
    IllegalState,
    /// The credential did not authenticate: `SASL_AUTHENTICATION_FAILED`.
    AuthenticationFailed,
}

impl Refusal {
    /// The protocol error code this refusal is reported under.
    #[must_use]
    pub fn code(self) -> i16 {
        match self {
            Self::IllegalState => ::kafka_protocol::ResponseError::IllegalSaslState.code(),
            Self::AuthenticationFailed => {
                ::kafka_protocol::ResponseError::SaslAuthenticationFailed.code()
            }
        }
    }

    /// The sentence the refusal carries. Fixed text: it says what failed and nothing
    /// about the credential that was offered.
    #[must_use]
    pub fn message(self) -> &'static str {
        match self {
            Self::IllegalState => "Request is not valid given the current SASL state.",
            Self::AuthenticationFailed => "Authentication failed.",
        }
    }
}

/// Where a SASL/SCRAM credential comes from: the Secrets Manager secrets the cluster
/// associated, read through the emulator the endpoint was opened by.
#[derive(Clone)]
pub struct CredentialSource {
    /// The emulator the secret is read through — an IN-PROCESS handle, so nothing
    /// here dials out.
    pub emulator: EmbeddedEmulator,
    /// The Region whose Secrets Manager endpoint holds the cluster's secrets.
    pub region: String,
    /// The secret ARNs the cluster currently has associated. A credential is only
    /// read out of one of these, so a disassociated secret authenticates nothing.
    pub secret_arns: Vec<String>,
}

impl CredentialSource {
    /// The password the user `username` authenticates with, or `None` when no
    /// associated secret names that user.
    ///
    /// Each associated secret is asked for in turn and read as the `{"username":
    /// ..., "password": ...}` pair Amazon MSK documents a SASL/SCRAM secret to carry.
    pub async fn password_for(&self, username: &str) -> Option<String> {
        let endpoint = super::super::network::peer_endpoint("secretsmanager", &self.region)?;
        let config = ::aws_sdk_secretsmanager::Config::builder()
            .behavior_version(::aws_sdk_secretsmanager::config::BehaviorVersion::latest())
            .http_client(self.emulator.http_client())
            .credentials_provider(self.emulator.aws_credentials_provider())
            .region(::aws_sdk_secretsmanager::config::Region::new(
                self.region.clone(),
            ))
            .endpoint_url(endpoint)
            // The transport is in-process: there is nothing to retry, time out or
            // watch for a stalled stream.
            .retry_config(::aws_sdk_secretsmanager::config::retry::RetryConfig::disabled())
            .timeout_config(::aws_sdk_secretsmanager::config::timeout::TimeoutConfig::disabled())
            .identity_cache(::aws_sdk_secretsmanager::config::IdentityCache::no_cache())
            .stalled_stream_protection(
                ::aws_sdk_secretsmanager::config::StalledStreamProtectionConfig::disabled(),
            )
            .build();
        let client = ::aws_sdk_secretsmanager::Client::from_conf(config);
        for arn in &self.secret_arns {
            let answered = client.get_secret_value().secret_id(arn).send().await;
            let Ok(value) = answered else {
                continue;
            };
            let Some(document) = value.secret_string() else {
                continue;
            };
            let Ok(pair) = ::serde_json::from_str::<BTreeMap<String, String>>(document) else {
                continue;
            };
            if pair.get("username").map(String::as_str) == Some(username) {
                return pair.get("password").cloned();
            }
        }
        None
    }
}

/// The SCRAM exchange one connection is in the middle of.
#[derive(Debug, Default)]
pub struct ScramExchange {
    /// What the client sent first, without its GS2 header, and what the server
    /// answered: the two halves of the message the proof is computed over.
    started: Option<ScramStarted>,
}

/// The state the server-first message left behind.
#[derive(Debug)]
struct ScramStarted {
    /// The user the client named.
    username: String,
    /// The client's first message without its GS2 header.
    client_first_bare: String,
    /// The server's first message, verbatim.
    server_first: String,
    /// The salt the server published.
    salt: Vec<u8>,
}

impl ScramExchange {
    /// A fresh exchange.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Answer one leg of the exchange with `auth_bytes`: the client's first message
    /// when the exchange has not started, and its proof when it has.
    pub async fn leg(
        &mut self,
        source: &CredentialSource,
        auth_bytes: &[u8],
    ) -> Result<Leg, Refusal> {
        let message = std::str::from_utf8(auth_bytes).map_err(|_| Refusal::AuthenticationFailed)?;
        match self.started.take() {
            None => self.start(message),
            Some(started) => self.finish(source, started, message).await,
        }
    }

    /// The server-first message for a client-first message: the combined nonce, the
    /// salt and the iteration count RFC 5802 §5 defines.
    fn start(&mut self, message: &str) -> Result<Leg, Refusal> {
        // `n,,n=<user>,r=<nonce>`: the GS2 header is the part before the second comma,
        // and what the proof is computed over is everything after it.
        let bare = match message.split_once(",,") {
            Some((_gs2, bare)) => bare,
            None => return Err(Refusal::AuthenticationFailed),
        };
        let mut username = None;
        let mut client_nonce = None;
        for attribute in bare.split(',') {
            match attribute.split_once('=') {
                Some(("n", value)) => username = Some(decode_saslname(value)),
                Some(("r", value)) => client_nonce = Some(value.to_string()),
                _ => {}
            }
        }
        let (Some(username), Some(client_nonce)) = (username, client_nonce) else {
            return Err(Refusal::AuthenticationFailed);
        };
        if username.len() > MAX_SASL_USERNAME_BYTES || client_nonce.len() > MAX_SCRAM_NONCE_BYTES
        {
            return Err(Refusal::AuthenticationFailed);
        }
        // RFC 5802 requires the server to choose an unpredictable salt and nonce, so an
        // exchange this emulator cannot draw randomness for is refused rather than run
        // with a guessable one.
        let (Some(salt), Some(server_nonce)) = (random_bytes(SCRAM_SALT_BYTES), nonce()) else {
            return Err(Refusal::AuthenticationFailed);
        };
        let server_first = format!(
            "r={client_nonce}{server_nonce},s={},i={SCRAM_ITERATIONS}",
            ::aws_smithy_types::base64::encode(&salt)
        );
        self.started = Some(ScramStarted {
            username,
            client_first_bare: bare.to_string(),
            server_first: server_first.clone(),
            salt,
        });
        Ok(Leg::Continue(Bytes::from(server_first.into_bytes())))
    }

    /// Verify the client's proof and answer the server signature.
    async fn finish(
        &mut self,
        source: &CredentialSource,
        started: ScramStarted,
        message: &str,
    ) -> Result<Leg, Refusal> {
        let (without_proof, proof) = match message.rsplit_once(",p=") {
            Some((without_proof, proof)) => (without_proof, proof),
            None => return Err(Refusal::AuthenticationFailed),
        };
        let proof = ::aws_smithy_types::base64::decode(proof)
            .map_err(|_| Refusal::AuthenticationFailed)?;
        let password = source
            .password_for(&started.username)
            .await
            .ok_or(Refusal::AuthenticationFailed)?;
        let auth_message = format!(
            "{},{},{without_proof}",
            started.client_first_bare, started.server_first
        );
        let salted = salted_password(&password, &started.salt);
        let client_key = hmac(&salted, b"Client Key");
        let stored_key = sha512(&client_key);
        let client_signature = hmac(&stored_key, auth_message.as_bytes());
        if proof.len() != client_signature.len() {
            return Err(Refusal::AuthenticationFailed);
        }
        let recovered: Vec<u8> = proof
            .iter()
            .zip(client_signature.iter())
            .map(|(proof, signature)| proof ^ signature)
            .collect();
        if sha512(&recovered) != stored_key {
            return Err(Refusal::AuthenticationFailed);
        }
        let server_key = hmac(&salted, b"Server Key");
        let server_signature = hmac(&server_key, auth_message.as_bytes());
        let server_final = format!(
            "v={}",
            ::aws_smithy_types::base64::encode(&server_signature)
        );
        Ok(Leg::Done {
            response: Bytes::from(server_final.into_bytes()),
            identity: Authenticated {
                principal: started.username,
            },
        })
    }
}

/// The `AWS_MSK_IAM` exchange: one leg, in which the client sends its signed payload
/// and the broker answers.
///
/// What is required of the payload is what Amazon MSK requires of it: the documented
/// version, the `kafka-cluster:Connect` action, and a SigV4 signature whose credential
/// is scoped to this cluster's Region and to the MSK data plane's signing name.
pub fn iam_leg(region: &str, auth_bytes: &[u8]) -> Result<Leg, Refusal> {
    let payload: BTreeMap<String, ::serde_json::Value> =
        ::serde_json::from_slice(auth_bytes).map_err(|_| Refusal::AuthenticationFailed)?;
    let field = |name: &str| {
        payload
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    if field("version") != IAM_PAYLOAD_VERSION || field("action") != IAM_CONNECT_ACTION {
        return Err(Refusal::AuthenticationFailed);
    }
    if field("x-amz-signature").is_empty() || field("x-amz-date").is_empty() {
        return Err(Refusal::AuthenticationFailed);
    }
    // `<access key id>/<date>/<region>/<service>/aws4_request`: the credential says
    // which key signed the payload and what it was scoped to — read by the same rule
    // the `OAUTHBEARER` token's credential is read by, so the two IAM exchanges cannot
    // disagree about which signatures are for this cluster's brokers.
    let access_key_id = scoped_access_key_id(&field("x-amz-credential"), region)?;
    let response = ::serde_json::json!({
        "version": IAM_PAYLOAD_VERSION,
        "request-id": nonce().ok_or(Refusal::AuthenticationFailed)?,
    });
    Ok(Leg::Done {
        response: Bytes::from(response.to_string().into_bytes()),
        identity: Authenticated {
            principal: access_key_id,
        },
    })
}

/// The `OAUTHBEARER` exchange (RFC 7628), which is how a non-Java client authenticates
/// with Amazon MSK's IAM access control: one leg, in which the client sends the token
/// the MSK auth-token signer produced and the broker answers an empty success.
///
/// That token is a SIGNED `kafka-cluster:Connect` request — the presigned URL the
/// signer builds — carried base64url-encoded (read by the tree's one base64url reader,
/// [`::service_common::jwt::decode_base64url`]), so what is required of it is what is
/// required of the Java payload: the `Connect` action and a SigV4 signature whose
/// credential is scoped to this cluster's Region and to the MSK data plane's signing
/// name. A token that is not that is refused.
pub fn oauth_bearer_leg(region: &str, auth_bytes: &[u8]) -> Result<Leg, Refusal> {
    let message =
        std::str::from_utf8(auth_bytes).map_err(|_| Refusal::AuthenticationFailed)?;
    // `n,,\x01auth=Bearer <token>\x01\x01` — the GS2 header, then the key/value fields
    // RFC 7628 §3.1 separates with `\x01`.
    let token = message
        .split('\x01')
        .filter_map(|field| field.strip_prefix("auth="))
        .map(|value| value.trim_start_matches("Bearer").trim())
        .next()
        .ok_or(Refusal::AuthenticationFailed)?;
    let signed = decode_base64url(token).ok_or(Refusal::AuthenticationFailed)?;
    let signed = String::from_utf8(signed).map_err(|_| Refusal::AuthenticationFailed)?;
    let query_value = |name: &str| {
        signed
            .split(['?', '&'])
            .filter_map(|pair| pair.split_once('='))
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            // The one percent-decoding this tree has, so an `Action` written
            // `kafka-cluster%3AConnect` reads as the action it names.
            .map(|(_, value)| ::smithy_server_runtime::percent_decode(value))
    };
    if query_value("Action").as_deref() != Some(IAM_CONNECT_ACTION) {
        return Err(Refusal::AuthenticationFailed);
    }
    if query_value("X-Amz-Signature").is_none_or(|signature| signature.is_empty()) {
        return Err(Refusal::AuthenticationFailed);
    }
    let credential = query_value("X-Amz-Credential").ok_or(Refusal::AuthenticationFailed)?;
    let access_key_id = scoped_access_key_id(&credential, region)?;
    // RFC 7628 §3.2.2: the server's answer to an accepted token carries no data.
    Ok(Leg::Done {
        response: Bytes::new(),
        identity: Authenticated {
            principal: access_key_id,
        },
    })
}

/// The access key id a SigV4 credential names, when the credential is scoped to
/// `region` and to the MSK data plane's signing name:
/// `<access key id>/<date>/<region>/<service>/aws4_request`. A credential scoped to
/// another Region or another service is not one for this cluster's brokers.
fn scoped_access_key_id(credential: &str, region: &str) -> Result<String, Refusal> {
    let mut scope = credential.split('/');
    let (Some(access_key_id), Some(_date), Some(scoped_region), Some(service)) =
        (scope.next(), scope.next(), scope.next(), scope.next())
    else {
        return Err(Refusal::AuthenticationFailed);
    };
    if access_key_id.is_empty() || scoped_region != region || service != IAM_SIGNING_SERVICE {
        return Err(Refusal::AuthenticationFailed);
    }
    Ok(access_key_id.to_string())
}

/// `Hi(password, salt, i)` of RFC 5802 §2.2 over SHA-512: PBKDF2-HMAC-SHA-512.
fn salted_password(password: &str, salt: &[u8]) -> Vec<u8> {
    let mut salted = [0u8; 64];
    ::pbkdf2::pbkdf2_hmac::<::sha2::Sha512>(
        password.as_bytes(),
        salt,
        SCRAM_ITERATIONS,
        &mut salted,
    );
    salted.to_vec()
}

/// `HMAC(key, data)` over SHA-512.
fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    use ::hmac::digest::KeyInit;
    use ::hmac::Mac;

    let mut mac = <::hmac::Hmac<::sha2::Sha512> as KeyInit>::new_from_slice(key)
        .expect("HMAC accepts a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// `H(data)`, the SHA-512 hash.
fn sha512(data: &[u8]) -> Vec<u8> {
    use ::sha2::Digest;

    ::sha2::Sha512::digest(data).to_vec()
}

/// `count` unpredictable bytes, for the salt and the nonce RFC 5802 requires the
/// server to choose. `None` when this machine's random source could not be read, which
/// is an exchange that is refused rather than run with a guessable salt.
fn random_bytes(count: usize) -> Option<Vec<u8>> {
    use ::rand::TryRngCore;

    let mut bytes = vec![0u8; count];
    ::rand::rngs::OsRng.try_fill_bytes(&mut bytes).ok()?;
    Some(bytes)
}

/// An unpredictable printable token: the server nonce of a SCRAM exchange, which RFC
/// 5802 requires to be unique per connection, and the request id an IAM exchange
/// answers with.
fn nonce() -> Option<String> {
    use std::fmt::Write;

    Some(
        random_bytes(16)?
            .iter()
            .fold(String::new(), |mut token, byte| {
                let _ = write!(token, "{byte:02x}");
                token
            }),
    )
}

/// The username a SCRAM `n=` attribute carries: RFC 5802 escapes `=` as `=3D` and `,`
/// as `=2C`, so a name carrying either is read back as it was sent.
fn decode_saslname(value: &str) -> String {
    value.replace("=3D", "=").replace("=2C", ",")
}
