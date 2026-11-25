// SPDX-License-Identifier: CC0-1.0

//! FLEET PROVISIONING over the reserved MQTT topics: how a device that holds only a claim
//! certificate obtains its own identity and registers its thing.
//!
//! A device being provisioned by claim calls no HTTP API. It connects to the address
//! `DescribeEndpoint` published with the claim certificate it was given
//! (`CreateProvisioningClaim`) and runs the whole exchange on the broker's reserved topics:
//!
//! * `$aws/certificates/create/json` — the device asks for a certificate of its own, and is
//!   answered on `…/create/json/accepted` with the certificate, its id, the PRIVATE KEY (the
//!   only time it exists) and a `certificateOwnershipToken` that proves the certificate is
//!   its.
//! * `$aws/certificates/create-from-csr/json` — the same, for a device that generated its
//!   own key pair and sends a certificate signing request instead, so no private key is
//!   answered.
//! * `$aws/provisioning-templates/{templateName}/provision/json` — the device hands back
//!   that ownership token and the template's parameters, and the template is carried out:
//!   the thing, the policy and the certificate's own status, exactly as `RegisterThing`
//!   carries out a template body ([`super::thing_provisioning::provision_template`], which
//!   is the one provisioning walk both paths run). The answer on `…/provision/json/accepted`
//!   carries the thing that was registered and the template's `DeviceConfiguration`.
//!
//! The certificate a device creates here is NOT usable yet: it is recorded
//! [`PENDING_ACTIVATION`] — the status AWS IoT leaves a fleet-provisioned certificate in
//! until the template activates it — so the `Status` the template's certificate resource
//! asks for is what makes the device able to connect with it.
//!
//! Every answer goes out through the broker's own fan-out
//! ([`::service_common::iot_mqtt_broker::publish_to_subscribers`]), on the `accepted` or
//! `rejected` topic of the request's own topic, which is where the device subscribed: a
//! request that cannot be carried out is answered with a refusal rather than left silent.
//!
//! The CBOR half of each topic (`…/cbor`) is not served, exactly as the reserved commands
//! topics read a device's own report as JSON ([`::service_common::iot_mqtt_broker::commands`]).
//!
//! WHAT IS NOT PINNED BY A RECORDING: the topics, the members of each answer and the
//! certificate's pending status are AWS's; the `errorCode` tokens and the sentences the
//! refusals read with are this emulator's own ([`Refusal`]) — they state which part of the
//! request could not be carried out, so a device learns why, but a device library matching
//! other tokens will not find them.

use std::collections::HashMap;
use std::time::SystemTime;

use ::serde_json::{json, Map, Value};

use super::thing_registration::{render_text, ProvisioningTemplate};
use super::{ComAmazonawsIotAwsiotserviceState, ProvisioningRefusal};

/// The reserved topic a device asks for a certificate of its own on.
pub const CREATE_CERTIFICATE_TOPIC: &str = "$aws/certificates/create/json";

/// The reserved topic a device that generated its own key pair asks for a certificate on.
pub const CREATE_CERTIFICATE_FROM_CSR_TOPIC: &str = "$aws/certificates/create-from-csr/json";

/// The first two levels of the reserved namespace one provisioning template's own topics
/// hang off.
pub const PROVISIONING_TEMPLATES_PREFIX: &str = "$aws/provisioning-templates";

/// The levels that name the exchange within one template's namespace: the request topic is
/// `{prefix}/{templateName}/provision/json`.
pub const PROVISION_LEVEL: &str = "provision";
/// See [`PROVISION_LEVEL`].
pub const JSON_FORMAT: &str = "json";

/// The level a request that was carried out is answered on.
pub const ACCEPTED_LEVEL: &str = "accepted";
/// The level a request that was refused is answered on.
pub const REJECTED_LEVEL: &str = "rejected";

/// The QoS these answers are published at: the highest the broker delivers at, so a device
/// that asked for an acknowledged delivery of its answer gets one.
const ANSWER_QOS: i32 = ::service_common::iot_mqtt_broker::wire::MAX_GRANTED_QOS;

/// The status a certificate created over this exchange is recorded in.
///
/// AWS IoT creates a fleet-provisioned certificate in `PENDING_ACTIVATION` and leaves it
/// there until the provisioning template's certificate resource asks for another status, so
/// a device cannot connect with a certificate it created but never provisioned. It is one of
/// the statuses no caller may ASK for ([`super::INTERNAL_CERTIFICATE_STATUSES`]) — it is the
/// service's own to set, which is what this exchange is.
pub const PENDING_ACTIVATION: &str = "PENDING_ACTIVATION";

/// The member a device names its certificate signing request under on
/// [`CREATE_CERTIFICATE_FROM_CSR_TOPIC`].
const CERTIFICATE_SIGNING_REQUEST_MEMBER: &str = "certificateSigningRequest";

/// The members a device names its certificate and the template's parameters under on a
/// provision request.
const CERTIFICATE_OWNERSHIP_TOKEN_MEMBER: &str = "certificateOwnershipToken";
/// See [`CERTIFICATE_OWNERSHIP_TOKEN_MEMBER`].
const PARAMETERS_MEMBER: &str = "parameters";

/// The parameter a template's certificate resource names the device's own certificate
/// through: `{"Ref": "AWS::IoT::Certificate::Id"}` resolves to the id of the certificate the
/// device proved ownership of, which is how one template body provisions every device.
pub const CERTIFICATE_ID_PARAMETER: &str = "AWS::IoT::Certificate::Id";

/// One certificate ownership token: the certificate it was minted for, and when.
#[derive(Debug, Clone)]
pub struct CertificateOwnership {
    /// The id of the certificate this token proves ownership of — the certificate store's
    /// own key.
    pub certificate_id: String,
    /// When the token was minted, which is when its certificate was created — the instant
    /// [`OWNERSHIP_TOKEN_VALIDITY`] is counted from.
    pub minted_at: SystemTime,
}

/// How long a certificate ownership token proves ownership for.
///
/// CHOSEN, not a span AWS publishes: the token is the one credential a device presents when
/// it provisions, and a token kept for ever is a credential that never expires, so it is
/// short-lived here. An hour is far longer than the exchange it belongs to — a device creates
/// its certificate and provisions with it in the same session — and short enough that a token
/// a device dropped stops proving anything.
pub const OWNERSHIP_TOKEN_VALIDITY: ::std::time::Duration =
    ::std::time::Duration::from_secs(60 * 60);

impl CertificateOwnership {
    /// Whether this token still proves ownership at `now`.
    #[must_use]
    pub fn is_valid_at(&self, now: SystemTime) -> bool {
        now.duration_since(self.minted_at)
            .is_ok_and(|held| held < OWNERSHIP_TOKEN_VALIDITY)
    }
}

/// Why a request on one of these topics was answered on its `rejected` topic.
///
/// The three cases are three different answers, and each carries the status a caller of the
/// equivalent API would be refused with: a request body this exchange cannot read is a fact
/// about the REQUEST (400), a template or a token that names nothing the account holds is a
/// miss (404), and a template the account holds but has not ENABLED is a template no device
/// may provision with (403).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The request body is no request this exchange can carry out.
    InvalidRequest(String),
    /// It names a resource the account does not hold.
    NotFound(String),
    /// It names a template the account holds but has not enabled.
    Forbidden(String),
    /// The exchange could not complete the request through no fault of the device's.
    InternalFailure(String),
}

impl Refusal {
    /// The HTTP status the equivalent API refusal carries, which is what the answer's
    /// `statusCode` reports.
    #[must_use]
    pub fn status_code(&self) -> i64 {
        match self {
            Self::InvalidRequest(_) => 400,
            Self::Forbidden(_) => 403,
            Self::NotFound(_) => 404,
            Self::InternalFailure(_) => 500,
        }
    }

    /// The `errorCode` token the answer carries: the name of the error class a caller of the
    /// equivalent API is refused with.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::InvalidRequest(_) => "InvalidRequestException",
            Self::Forbidden(_) => "AccessDeniedException",
            Self::NotFound(_) => "ResourceNotFoundException",
            Self::InternalFailure(_) => "InternalFailureException",
        }
    }

    /// What the refusal says: the rule the request broke.
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidRequest(reason)
            | Self::NotFound(reason)
            | Self::Forbidden(reason)
            | Self::InternalFailure(reason) => reason,
        }
    }

    /// The body the `rejected` topic carries: the status, the error code and the message —
    /// the three members a fleet-provisioning error response is read through.
    #[must_use]
    pub fn body(&self) -> Value {
        json!({
            "statusCode": self.status_code(),
            "errorCode": self.error_code(),
            "errorMessage": self.message(),
        })
    }
}

/// How this exchange states that a request body is no JSON document at all.
#[must_use]
pub fn unreadable_request_message() -> String {
    "The request payload is not a JSON document".to_string()
}

/// How this exchange states that a provision request names no certificate ownership token.
#[must_use]
pub fn missing_ownership_token_message() -> String {
    format!("The request names no {CERTIFICATE_OWNERSHIP_TOKEN_MEMBER}")
}

/// How this exchange states that a certificate ownership token proves ownership of nothing:
/// no certificate of this account was created under it, or the certificate it named is gone.
#[must_use]
pub fn unknown_ownership_token_message() -> String {
    format!("The {CERTIFICATE_OWNERSHIP_TOKEN_MEMBER} is not valid")
}

/// How this exchange states that a template the account holds is not one a device may
/// provision with: `CreateProvisioningTemplate$enabled` is "True to enable the provisioning
/// template, otherwise false", so a template left disabled provisions nothing.
#[must_use]
pub fn disabled_template_message(template_name: &str) -> String {
    format!("Provisioning template {template_name} is not enabled")
}

/// Which exchange a topic addresses, or `None` for a topic this module does not serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exchange {
    /// [`CREATE_CERTIFICATE_TOPIC`].
    CreateCertificate,
    /// [`CREATE_CERTIFICATE_FROM_CSR_TOPIC`].
    CreateCertificateFromCsr,
    /// `$aws/provisioning-templates/{templateName}/provision/json`.
    Provision {
        /// The template the device is provisioning with.
        template_name: String,
    },
}

/// The exchange `topic` addresses, and `None` for every other topic — including the
/// `accepted` and `rejected` topics of these very exchanges, which are the service's own to
/// write and no request of a device's.
#[must_use]
pub fn addressed(topic: &str) -> Option<Exchange> {
    if topic == CREATE_CERTIFICATE_TOPIC {
        return Some(Exchange::CreateCertificate);
    }
    if topic == CREATE_CERTIFICATE_FROM_CSR_TOPIC {
        return Some(Exchange::CreateCertificateFromCsr);
    }
    let within = topic.strip_prefix(PROVISIONING_TEMPLATES_PREFIX)?;
    let mut levels = within.strip_prefix('/')?.split('/');
    let template_name = levels.next()?;
    if levels.next() != Some(PROVISION_LEVEL)
        || levels.next() != Some(JSON_FORMAT)
        || levels.next().is_some()
        || template_name.is_empty()
    {
        return None;
    }
    Some(Exchange::Provision {
        template_name: template_name.to_string(),
    })
}

/// The topic one answer to a request on `topic` is published on.
#[must_use]
pub fn answer_topic(topic: &str, accepted: bool) -> String {
    let level = if accepted {
        ACCEPTED_LEVEL
    } else {
        REJECTED_LEVEL
    };
    format!("{topic}/{level}")
}

/// Serve one publication a device made on a reserved provisioning topic, answering it on
/// that topic's own `accepted` or `rejected` topic.
///
/// `false` for a topic this exchange does not serve, which the caller then treats as the
/// ordinary publication it is. A request on one it DOES serve is always answered, including
/// one this emulator could not carry out because it vends no store to carry it out against:
/// a device waiting on an answer is told, never left silent.
pub async fn served_provisioning_request<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    region: &str,
    topic: &str,
    payload: &[u8],
) -> bool {
    let Some(exchange) = addressed(topic) else {
        return false;
    };
    let answered = match super::service_state(engine) {
        None => Err(Refusal::InternalFailure(NO_STORE_MESSAGE.to_string())),
        Some(state) => match exchange {
            Exchange::CreateCertificate => created_certificate(&state, region, None).await,
            Exchange::CreateCertificateFromCsr => match signing_request_of(payload) {
                Ok(signing_request) => {
                    created_certificate(&state, region, Some(signing_request)).await
                }
                Err(refusal) => Err(refusal),
            },
            Exchange::Provision { template_name } => {
                provisioned_device(&state, region, &template_name, payload)
            }
        },
    };
    let (accepted, body) = match answered {
        Ok(body) => (true, body),
        Err(refusal) => (false, refusal.body()),
    };
    answer(engine, &answer_topic(topic, accepted), &body);
    true
}

/// What a request this emulator holds no IoT store for is refused with: the exchange writes
/// the account's own registries, so there is nothing to carry the request out against.
pub const NO_STORE_MESSAGE: &str = "The IoT emulator has no state store attached.";

/// Publish one answer on `topic`: it goes to every session subscribed to a filter covering
/// it, which is where the device that made the request is listening.
fn answer<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    topic: &str,
    body: &Value,
) {
    let Some(broker) = ::service_common::iot_mqtt_broker::broker_state(engine) else {
        // Only an engine with no broker store at all, which is one no device can have
        // published on: the publication reached this exchange over that very broker.
        ::tracing::debug!(
            topic,
            "no broker store answered the reserved provisioning topic"
        );
        return;
    };
    ::service_common::iot_mqtt_broker::publish_to_subscribers(
        &broker,
        &::service_common::iot_mqtt_broker::DirectMessage {
            topic: topic.to_string(),
            payload: ::serde_json::to_vec(body).unwrap_or_default(),
            qos: ANSWER_QOS,
            delivered_at_millis: ::service_common::iot_mqtt_broker::now_millis(),
            ..::service_common::iot_mqtt_broker::DirectMessage::default()
        },
    );
}

/// The certificate signing request a `create-from-csr` body names, or why the body names
/// none.
fn signing_request_of(payload: &[u8]) -> Result<String, Refusal> {
    let body = request_body(payload)?;
    body.get(CERTIFICATE_SIGNING_REQUEST_MEMBER)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            Refusal::InvalidRequest(format!(
                "The request names no {CERTIFICATE_SIGNING_REQUEST_MEMBER}"
            ))
        })
}

/// One request body as the JSON object it has to be. An EMPTY payload is an empty object:
/// the certificate-create request carries no member at all, so a device that publishes
/// nothing is making that request.
fn request_body(payload: &[u8]) -> Result<Map<String, Value>, Refusal> {
    if payload.iter().all(u8::is_ascii_whitespace) {
        return Ok(Map::new());
    }
    match ::serde_json::from_slice::<Value>(payload) {
        Ok(Value::Object(body)) => Ok(body),
        _ => Err(Refusal::InvalidRequest(unreadable_request_message())),
    }
}

/// Create the certificate a device asked for: minted here and recorded
/// [`PENDING_ACTIVATION`], with the ownership token the provision request then proves it
/// with.
///
/// `signing_request` is the device's own certificate signing request where it sent one, in
/// which case the key pair is the device's and no private key is answered.
///
/// Generating a key pair and signing a certificate is hundreds of milliseconds of CPU, so it
/// runs on the blocking pool with the store's lock RELEASED, exactly as the control-plane
/// creates do: `spawn_blocking(..).await` is a real await point, so one certificate cannot
/// starve the worker that serves every other request.
async fn created_certificate(
    state: &::engine::ServiceState<ComAmazonawsIotAwsiotserviceState>,
    region: &str,
    signing_request: Option<String>,
) -> Result<Value, Refusal> {
    // The store holds every certificate in memory, so this exchange stops issuing them at
    // the same ceiling the control plane refuses past. Read BEFORE anything is signed, so a
    // request past it costs no CPU either.
    if state.lock().certificates.len() >= super::MAX_CERTIFICATES {
        return Err(Refusal::InternalFailure(format!(
            "this emulator stores at most {} certificates",
            super::MAX_CERTIFICATES
        )));
    }
    let now = SystemTime::now();
    let (certificate, private_key) = match signing_request {
        None => {
            let region = region.to_string();
            let minted = ::tokio::task::spawn_blocking(move || {
                super::mint_keys_and_certificate(
                    &region,
                    ::engine::emulator_account_id(),
                    false,
                    now,
                )
            })
            .await;
            let Ok(Some(minted)) = minted else {
                return Err(Refusal::InternalFailure(
                    "the key pair and certificate could not be issued".to_string(),
                ));
            };
            (minted.certificate, minted.key_pair.private_key)
        }
        Some(signing_request) => {
            let read = super::read_certificate_signing_request(&signing_request)
                .map_err(|refusal| Refusal::InvalidRequest(refusal.message().to_string()))?;
            let authority = issuing_authority(state).await?;
            let region = region.to_string();
            let issued = ::tokio::task::spawn_blocking(move || {
                authority.issue_certificate(
                    &region,
                    ::engine::emulator_account_id(),
                    &read,
                    false,
                    now,
                )
            })
            .await;
            let Ok(Some(issued)) = issued else {
                return Err(Refusal::InternalFailure(
                    "the certificate could not be issued".to_string(),
                ));
            };
            // The key pair is the DEVICE's own, so there is no private key to answer with.
            (issued, None)
        }
    };

    let mut store = state.lock();
    // Re-read under the write lock: another request may have filled the store while this
    // one was signing.
    if store.certificates.len() >= super::MAX_CERTIFICATES {
        return Err(Refusal::InternalFailure(format!(
            "this emulator stores at most {} certificates",
            super::MAX_CERTIFICATES
        )));
    }
    let mut recorded = certificate;
    // A certificate created here is not usable until a template activates it.
    recorded.status = PENDING_ACTIVATION.to_string();
    let certificate_id = recorded.certificate_id.clone();
    let certificate_pem = recorded.certificate_pem.clone();
    store.certificates.insert(certificate_id.clone(), recorded);
    let token = minted_ownership_token(&mut store, &certificate_id, now);

    let mut body = Map::new();
    body.insert("certificateId".to_string(), json!(certificate_id));
    body.insert("certificatePem".to_string(), json!(certificate_pem));
    if let Some(private_key) = private_key {
        body.insert("privateKey".to_string(), json!(private_key));
    }
    body.insert(
        CERTIFICATE_OWNERSHIP_TOKEN_MEMBER.to_string(),
        json!(token),
    );
    Ok(Value::Object(body))
}

/// The authority a certificate issued over a device's own signing request is signed by:
/// this service's one issuing authority, minted on the first use that needs it — the same
/// record `CreateCertificateFromCsr` signs with, so a device and an API caller are issued
/// certificates by one issuer.
async fn issuing_authority(
    state: &::engine::ServiceState<ComAmazonawsIotAwsiotserviceState>,
) -> Result<::std::sync::Arc<super::IssuingCertificateAuthority>, Refusal> {
    if let Some(authority) = state.lock().issuing_certificate_authority.clone() {
        return Ok(authority);
    }
    let minted = ::tokio::task::spawn_blocking(super::IssuingCertificateAuthority::generate).await;
    let Ok(Some(minted)) = minted else {
        return Err(Refusal::InternalFailure(
            "the certificate could not be issued".to_string(),
        ));
    };
    let mut store = state.lock();
    Ok(store
        .issuing_certificate_authority
        .get_or_insert_with(|| ::std::sync::Arc::new(minted))
        .clone())
}

/// Mint the ownership token of `certificate_id` and record it, releasing the tokens whose
/// certificates the account no longer holds first — so the table is bounded by the
/// certificate store's own ceiling.
fn minted_ownership_token(
    store: &mut ComAmazonawsIotAwsiotserviceState,
    certificate_id: &str,
    now: SystemTime,
) -> String {
    released_ownership_tokens(store, now);
    let token = loop {
        let candidate = super::fresh_uuid();
        if !store.certificate_ownership_tokens.contains_key(&candidate) {
            break candidate;
        }
    };
    store.certificate_ownership_tokens.insert(
        token.clone(),
        CertificateOwnership {
            certificate_id: certificate_id.to_string(),
            minted_at: now,
        },
    );
    token
}

/// Release every ownership token that proves nothing any more at `now`: one whose certificate
/// the account no longer holds, and one older than [`OWNERSHIP_TOKEN_VALIDITY`]. A release the
/// CLOCK already made due — it was owed at this instant whether or not anybody published — and
/// what keeps the table bounded by the certificate store's own ceiling.
pub fn released_ownership_tokens(store: &mut ComAmazonawsIotAwsiotserviceState, now: SystemTime) {
    let held: Vec<String> = store
        .certificate_ownership_tokens
        .iter()
        .filter(|(_, owned)| {
            !owned.is_valid_at(now) || !store.certificates.contains_key(&owned.certificate_id)
        })
        .map(|(token, _)| token.clone())
        .collect();
    for token in held {
        store.certificate_ownership_tokens.remove(&token);
    }
}

/// Carry out `template_name` for the device whose provision request `payload` is: the thing,
/// the policy and the certificate status the template declares, and the answer the device
/// reads its own registration out of.
fn provisioned_device(
    state: &::engine::ServiceState<ComAmazonawsIotAwsiotserviceState>,
    region: &str,
    template_name: &str,
    payload: &[u8],
) -> Result<Value, Refusal> {
    let body = request_body(payload)?;
    let token = body
        .get(CERTIFICATE_OWNERSHIP_TOKEN_MEMBER)
        .and_then(Value::as_str)
        .ok_or_else(|| Refusal::InvalidRequest(missing_ownership_token_message()))?
        .to_string();
    let parameters = supplied_parameters(&body);
    let now = SystemTime::now();

    let mut store = state.lock();
    let Some(template) = store.provisioning_templates.get(template_name) else {
        return Err(Refusal::NotFound(
            super::missing_provisioning_template_message(template_name),
        ));
    };
    if !template.enabled {
        return Err(Refusal::Forbidden(disabled_template_message(template_name)));
    }
    let template_body = template.default_body().to_string();
    // The token is what proves the device owns the certificate the template will provision,
    // so one this account holds no live token for — it named no certificate it still holds,
    // or it is past its own validity — provisions nothing.
    released_ownership_tokens(&mut store, now);
    let certificate_id = store
        .certificate_ownership_tokens
        .get(&token)
        .map(|owned| owned.certificate_id.clone())
        .ok_or_else(|| Refusal::NotFound(unknown_ownership_token_message()))?;

    // The template's own parameters, plus the pseudo-parameter its certificate resource
    // names the device's certificate through — which is what lets ONE template body
    // provision every device.
    let mut substituted = parameters;
    substituted.insert(CERTIFICATE_ID_PARAMETER.to_string(), certificate_id);

    let provisioned = super::provision_template(&mut store, region, &template_body, &substituted, now)
        .map_err(|refusal| match refusal {
            // A template body this service cannot carry out is a fact about the TEMPLATE,
            // which the device did not write; a declaration the account's state would not
            // take is a registration that failed. Both leave the device with no thing, so
            // both are refusals of its request.
            ProvisioningRefusal::Template(reason) => Refusal::InvalidRequest(reason),
            ProvisioningRefusal::Resource(reason) => Refusal::InvalidRequest(reason),
        })?;
    drop(store);

    let mut body = Map::new();
    body.insert("thingName".to_string(), json!(provisioned.thing_name));
    body.insert(
        "deviceConfiguration".to_string(),
        device_configuration(&template_body, &substituted),
    );
    Ok(Value::Object(body))
}

/// The parameters a provision request supplies, as the provisioning walk takes them: the
/// `parameters` member's own entries, each as the text it spells.
fn supplied_parameters(body: &Map<String, Value>) -> HashMap<String, String> {
    let Some(parameters) = body.get(PARAMETERS_MEMBER).and_then(Value::as_object) else {
        return HashMap::new();
    };
    parameters
        .iter()
        .filter_map(|(name, value)| scalar_text(value).map(|text| (name.clone(), text)))
        .collect()
}

/// The text one parameter value spells, and `None` for a value that spells no text: a
/// template substitutes TEXT into its properties, so an object or an array is no parameter
/// value.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(boolean) => Some(boolean.to_string()),
        _ => None,
    }
}

/// The `deviceConfiguration` the accepted answer carries: the template's own
/// `DeviceConfiguration` section with the request's parameters substituted into it, and an
/// empty object for a template that declares none.
///
/// The body is read ONCE, through the same reader the provisioning walk reads it with
/// ([`ProvisioningTemplate`]), so the section and the substitutions it is rendered against
/// come from one reading of the template.
fn device_configuration(template_body: &str, parameters: &HashMap<String, String>) -> Value {
    let Ok(template) = ProvisioningTemplate::read(template_body) else {
        return Value::Object(Map::new());
    };
    // The same substitutions every resource of the template is read with: the parameters the
    // request supplied over the `Default`s the template declares, and its own `Mappings`.
    let supplied: Map<String, Value> = parameters
        .iter()
        .map(|(name, value)| (name.clone(), Value::String(value.clone())))
        .collect();
    let substitutions = template.substitutions(&supplied);
    let mut configuration = Map::new();
    for (name, declared) in template.declared_device_configuration() {
        if let Some(text) = render_text(declared, &substitutions) {
            configuration.insert(name.clone(), Value::String(text));
        }
    }
    Value::Object(configuration)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an ownership token proves ownership only while it is live: for the validity it
    /// was minted with, and only while the account still holds the certificate it names.
    ///
    /// The rows span what the rule generalises over: a token just minted, one inside its
    /// validity, one exactly at it, one past it, and one whose instant is in the future
    /// (a store written before the clock moved back).
    #[test]
    fn an_ownership_token_proves_ownership_only_while_it_is_live() {
        let minted_at = SystemTime::UNIX_EPOCH + ::std::time::Duration::from_secs(1_800_000_000);
        let owned = CertificateOwnership {
            certificate_id: "abc".to_string(),
            minted_at,
        };
        let rows: &[(&str, ::std::time::Duration, bool)] = &[
            ("the instant it was minted", ::std::time::Duration::ZERO, true),
            ("inside its validity", ::std::time::Duration::from_secs(60), true),
            ("one second short of it", OWNERSHIP_TOKEN_VALIDITY - ::std::time::Duration::from_secs(1), true),
            ("exactly at it", OWNERSHIP_TOKEN_VALIDITY, false),
            ("past it", OWNERSHIP_TOKEN_VALIDITY + ::std::time::Duration::from_secs(1), false),
        ];
        for (case, held, expected) in rows {
            let valid = owned.is_valid_at(minted_at + *held);
            assert_eq!(
                valid, *expected,
                "case {case:?}: expected is_valid_at={expected}, got {valid}"
            );
        }
        // An instant BEFORE the token was minted is no age at all, so the token proves
        // nothing rather than proving ownership for ever.
        let earlier = minted_at - ::std::time::Duration::from_secs(1);
        assert!(
            !owned.is_valid_at(earlier),
            "case \"an instant before it was minted\": expected no proof of ownership"
        );
    }

    /// RULE: a reserved provisioning topic addresses one exchange — the two certificate
    /// creates, and one template's own provision topic — and every other topic addresses
    /// none, including the answers these exchanges themselves publish.
    ///
    /// The rows span what the rule generalises over: each served topic, a template name with
    /// the characters a name may carry, the `accepted`/`rejected` answers, the CBOR half this
    /// module does not serve, a truncated topic, a topic with a level too many and an
    /// ordinary application topic.
    #[test]
    fn a_reserved_provisioning_topic_addresses_one_exchange() {
        let rows: &[(&str, &str, Option<Exchange>)] = &[
            (
                "the certificate create",
                "$aws/certificates/create/json",
                Some(Exchange::CreateCertificate),
            ),
            (
                "the create from a signing request",
                "$aws/certificates/create-from-csr/json",
                Some(Exchange::CreateCertificateFromCsr),
            ),
            (
                "a template's provision topic",
                "$aws/provisioning-templates/fleet-1/provision/json",
                Some(Exchange::Provision {
                    template_name: "fleet-1".to_string(),
                }),
            ),
            (
                "a template name of the other characters a name carries",
                "$aws/provisioning-templates/Fleet_Template.2-x/provision/json",
                Some(Exchange::Provision {
                    template_name: "Fleet_Template.2-x".to_string(),
                }),
            ),
            (
                "the accepted answer of a create",
                "$aws/certificates/create/json/accepted",
                None,
            ),
            (
                "the rejected answer of a provision",
                "$aws/provisioning-templates/fleet-1/provision/json/rejected",
                None,
            ),
            (
                "the CBOR half of a create",
                "$aws/certificates/create/cbor",
                None,
            ),
            (
                "a provision topic naming no template",
                "$aws/provisioning-templates//provision/json",
                None,
            ),
            (
                "a template namespace with no provision level",
                "$aws/provisioning-templates/fleet-1/json",
                None,
            ),
            ("an ordinary application topic", "devices/sensor-1/state", None),
        ];
        for (case, topic, expected) in rows {
            let addressed = addressed(topic);
            assert_eq!(
                addressed, *expected,
                "case {case:?}: expected {expected:?} for {topic:?}, got {addressed:?}"
            );
        }
    }

    /// RULE: a request is answered on its own topic's `accepted` or `rejected` level, so a
    /// device subscribed to either hears about the request it made.
    #[test]
    fn an_answer_is_published_on_the_requests_own_topic() {
        let rows: &[(&str, &str, bool, &str)] = &[
            (
                "an accepted create",
                CREATE_CERTIFICATE_TOPIC,
                true,
                "$aws/certificates/create/json/accepted",
            ),
            (
                "a rejected create",
                CREATE_CERTIFICATE_TOPIC,
                false,
                "$aws/certificates/create/json/rejected",
            ),
            (
                "an accepted provision",
                "$aws/provisioning-templates/fleet-1/provision/json",
                true,
                "$aws/provisioning-templates/fleet-1/provision/json/accepted",
            ),
            (
                "a rejected create from a signing request",
                CREATE_CERTIFICATE_FROM_CSR_TOPIC,
                false,
                "$aws/certificates/create-from-csr/json/rejected",
            ),
        ];
        for (case, topic, accepted, expected) in rows {
            let answered = answer_topic(topic, *accepted);
            assert_eq!(
                answered, *expected,
                "case {case:?}: expected {expected:?}, got {answered:?}"
            );
        }
    }

    /// RULE: a refusal carries the status and the error class a caller of the equivalent API
    /// would be refused with, so a device can tell a bad request from a missing resource
    /// from a template it may not use.
    #[test]
    fn each_refusal_carries_its_own_status_and_error_code() {
        let rows: &[(&str, Refusal, i64, &str)] = &[
            (
                "a body this exchange cannot read",
                Refusal::InvalidRequest(unreadable_request_message()),
                400,
                "InvalidRequestException",
            ),
            (
                "a template the account has not enabled",
                Refusal::Forbidden(disabled_template_message("fleet-1")),
                403,
                "AccessDeniedException",
            ),
            (
                "a token that proves nothing",
                Refusal::NotFound(unknown_ownership_token_message()),
                404,
                "ResourceNotFoundException",
            ),
            (
                "a certificate nothing could issue",
                Refusal::InternalFailure("unissued".to_string()),
                500,
                "InternalFailureException",
            ),
        ];
        for (case, refusal, status, code) in rows {
            let body = refusal.body();
            assert_eq!(
                body["statusCode"],
                json!(status),
                "case {case:?}: expected statusCode {status}, got {body}"
            );
            assert_eq!(
                body["errorCode"],
                json!(code),
                "case {case:?}: expected errorCode {code:?}, got {body}"
            );
            assert_eq!(
                body["errorMessage"],
                json!(refusal.message()),
                "case {case:?}: expected the refusal's own message, got {body}"
            );
        }
    }

    /// RULE: the accepted answer's `deviceConfiguration` is the template's own section with
    /// the request's parameters substituted into it, and an empty object for a template that
    /// declares none.
    ///
    /// The rows span what the rule generalises over: no section at all, a literal entry, an
    /// entry built from a supplied parameter, one built from a parameter the template
    /// defaults, and a section that is no object.
    #[test]
    fn the_device_configuration_is_the_templates_section_with_the_parameters_in_it() {
        let body = |section: Value| {
            json!({
                "Parameters": {
                    "SerialNumber": {"Type": "String"},
                    "Site": {"Type": "String", "Default": "plant-9"},
                },
                "Resources": {
                    "thing": {
                        "Type": "AWS::IoT::Thing",
                        "Properties": {"ThingName": {"Ref": "SerialNumber"}},
                    },
                },
                "DeviceConfiguration": section,
            })
            .to_string()
        };
        let rows: &[(&str, Value, Value)] = &[
            (
                "a literal entry",
                json!({"fleet": "north"}),
                json!({"fleet": "north"}),
            ),
            (
                "an entry built from a supplied parameter",
                json!({"serial": {"Ref": "SerialNumber"}}),
                json!({"serial": "SN-7"}),
            ),
            (
                "an entry built from a defaulted parameter",
                json!({"site": {"Ref": "Site"}}),
                json!({"site": "plant-9"}),
            ),
            ("no entry at all", json!({}), json!({})),
            ("a section that is no object", json!("north"), json!({})),
        ];
        let parameters: HashMap<String, String> =
            HashMap::from([("SerialNumber".to_string(), "SN-7".to_string())]);
        for (case, section, expected) in rows {
            let configuration = device_configuration(&body(section.clone()), &parameters);
            assert_eq!(
                configuration, *expected,
                "case {case:?}: expected {expected}, got {configuration}"
            );
        }

        // A template that declares no section at all answers an empty configuration too.
        let without = json!({
            "Resources": {
                "thing": {"Type": "AWS::IoT::Thing", "Properties": {"ThingName": "device-1"}},
            },
        })
        .to_string();
        let configuration = device_configuration(&without, &parameters);
        assert_eq!(
            configuration,
            json!({}),
            "case \"no DeviceConfiguration section\": expected an empty object, got \
             {configuration}"
        );
    }

    /// RULE: a certificate-create request carries no member, so an empty payload IS that
    /// request; a provision request's parameters are the text each entry spells, and a body
    /// that is no JSON object is no request at all.
    #[test]
    fn a_request_body_is_read_as_the_object_it_is() {
        let rows: &[(&str, &[u8], bool)] = &[
            ("an empty payload", b"", true),
            ("whitespace alone", b" \n", true),
            ("an empty object", b"{}", true),
            ("an object", br#"{"certificateOwnershipToken":"t"}"#, true),
            ("an array", b"[]", false),
            ("a scalar", b"7", false),
            ("not JSON at all", b"certificateOwnershipToken=t", false),
        ];
        for (case, payload, readable) in rows {
            let read = request_body(payload);
            assert_eq!(
                read.is_ok(),
                *readable,
                "case {case:?}: expected readable={readable}, got {read:?}"
            );
        }

        let supplied = supplied_parameters(
            ::serde_json::from_str::<Value>(
                r#"{"parameters":{"SerialNumber":"SN-7","Count":45,"Live":true,"Nested":{}}}"#,
            )
            .expect("the body is JSON")
            .as_object()
            .expect("the body is an object"),
        );
        let mut expected: Vec<(String, String)> = vec![
            ("Count".to_string(), "45".to_string()),
            ("Live".to_string(), "true".to_string()),
            ("SerialNumber".to_string(), "SN-7".to_string()),
        ];
        expected.sort();
        let mut read: Vec<(String, String)> = supplied.into_iter().collect();
        read.sort();
        assert_eq!(
            read, expected,
            "case \"the parameters a request supplies\": expected {expected:?}, got {read:?}"
        );
    }
}
