// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-pinpoint-pinpoint service.
//!
//! The store is keyed the way the API is: applications (what the Pinpoint console
//! calls projects) by `ApplicationId`, and each application owns its endpoints by
//! `EndpointId`, its channels by `ChannelType` and its live one-time passcodes by the
//! pair a verification matches on. Every map is a [`BTreeMap`] so every listing — and
//! the pagination token built from it — has one deterministic order.
//!
//! Handlers reach it through [`service_state`] and share the small helpers below
//! (id minting, the ISO 8601 clock, the signing region) rather than each growing
//! its own copy.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use crate::ops::types::{
    ApplicationResponse, BaiduChannelResponse, EndpointDemographic, EndpointLocation,
    EndpointResponse, EndpointUser,
};

/// The ARN namespace Pinpoint signs and builds ARNs with (`sigv4Name` /
/// `awsService.arnNamespace` in the Smithy model), which is NOT its endpoint
/// prefix (`pinpoint`).
pub const ARN_NAMESPACE: &str = "mobiletargeting";

/// The region an unsigned request is attributed to. A signed request carries its
/// own region in the SigV4 credential scope; see [`region_of`].
pub const DEFAULT_REGION: &str = "us-east-1";

/// The largest page `GetApps` serves, and the page it serves when the caller asks
/// for no particular size.
///
/// `GetApps`'s `page-size` is a free-form string in the model (no `@range` for the
/// boundary validator to enforce), so this is the service's OWN bound: one response
/// carries at most this many applications, and the work done under the state lock is
/// independent of how many are stored. A caller that asks for more is served this
/// many plus a `NextToken` for the rest.
pub const APP_PAGE_LIMIT: usize = 100;

/// The most applications this store holds at once.
///
/// A bound on how far the store can grow under repeated creates: a `CreateApp` past
/// it is refused through this service's client-error channel rather than accepted, so
/// a caller cannot grow the process's memory without limit. AWS publishes its own
/// quota for projects per account and it is not this number; this is the emulator's
/// own ceiling, chosen well above what a client script needs. `DeleteApp` frees the
/// slot, so the store's size is bounded by the live applications and nothing else.
pub const MAX_APPS: usize = 200;

/// The most endpoints one application holds at once, for the same reason and with the
/// same refusal. It also bounds the same-address scan `UpdateEndpoint` runs, so the
/// work one write costs cannot grow without limit either.
pub const MAX_ENDPOINTS_PER_APP: usize = 200;

/// The most `Attributes` keys one stored endpoint holds.
///
/// Endpoint writes MERGE, so without a ceiling a caller supplying fresh keys on each
/// write could grow one stored endpoint without limit. A write whose merged result
/// would pass the ceiling is refused and nothing is stored. Again the emulator's own
/// ceiling, not an AWS-published one.
pub const MAX_ENDPOINT_ATTRIBUTES: usize = 250;

/// The `Metrics` counterpart of [`MAX_ENDPOINT_ATTRIBUTES`].
pub const MAX_ENDPOINT_METRICS: usize = 250;

/// The most destinations one direct-message send may name.
///
/// A send answers one result per destination and, for an address, scans the
/// application's endpoints for the one that carries it, so without a ceiling one
/// request could ask for work bounded only by its own size. A send past it is refused
/// through this service's client-error channel and nothing is recorded. The emulator's
/// own ceiling, chosen well above what a client script needs.
pub const MAX_DESTINATIONS_PER_SEND: usize = 100;

/// The most accepted direct-message sends one application keeps a record of.
///
/// A send is recorded so a later read sees what the service actually did with it, and
/// the oldest record is dropped once the log is full: however many messages a caller
/// sends, the store's size stays bounded by the live applications. Again the
/// emulator's own ceiling, not an AWS-published one.
pub const MAX_SENDS_PER_APP: usize = 100;

/// The most one-time passcode challenges one application holds at once.
///
/// `SendOTPMessage` stores one challenge per reference identifier and destination, so
/// without a ceiling a caller sending codes for fresh reference identifiers could grow
/// one application without limit. The challenge nearest the end of its own validity is
/// dropped once the store is full, so however many codes a caller sends, the store's
/// size stays bounded by the live applications. The emulator's own ceiling, not an
/// AWS-published one.
pub const MAX_OTPS_PER_APP: usize = 100;

/// The number of digits a minted one-time passcode carries when the request names no
/// `CodeLength` ("the number of characters in the generated OTP"): six, the length an
/// OTP message carries unless the caller asks for another.
pub const DEFAULT_CODE_LENGTH: i32 = 6;

/// The fewest digits a minted one-time passcode can carry: a code of no characters
/// could never be supplied for verification, so a request asking for fewer than this
/// is refused instead of served.
pub const MIN_CODE_LENGTH: usize = 1;

/// The most digits a minted one-time passcode can carry, so the work one send costs
/// and the state it leaves behind cannot grow with the number the request asks for.
/// The emulator's own ceiling, not an AWS-published one.
pub const MAX_CODE_LENGTH: usize = 10;

/// The verifications one challenge answers when the request names no
/// `AllowedAttempts` ("the attempts allowed to validate an OTP").
pub const DEFAULT_ALLOWED_ATTEMPTS: i32 = 5;

/// The minutes a minted code stays valid for when the request names no
/// `ValidityPeriod` ("the time in minutes before the OTP is no longer valid"). A
/// request that names one gets exactly the period it named; this default is the
/// emulator's own.
pub const DEFAULT_VALIDITY_PERIOD_MINUTES: i32 = 30;

/// The `ChannelType` value of the SMS channel, which is the only channel an OTP
/// message is sent over: `SendOTPMessageRequestParameters$Channel` is "channel type
/// for the OTP message. Supported values: [SMS]".
pub const CHANNEL_TYPE_SMS: &str = "SMS";

/// The `DeliveryStatus` values a send reports for one destination, over the values the
/// model's `DeliveryStatus` enum lists. Every send operation answers out of this one
/// vocabulary, so two of them cannot describe the same outcome differently.
///
/// The message was accepted for delivery to the destination.
pub const DELIVERY_SUCCESSFUL: &str = "SUCCESSFUL";

/// The destination cannot take the message, and a retry of the same send cannot
/// change that.
pub const DELIVERY_PERMANENT_FAILURE: &str = "PERMANENT_FAILURE";

/// The destination's user has opted out of receiving messages.
pub const DELIVERY_OPT_OUT: &str = "OPT_OUT";

/// The destination's address already took delivery in this same send.
pub const DELIVERY_DUPLICATE: &str = "DUPLICATE";

/// `MessageResult$StatusCode` is "the downstream service status code for delivering
/// the message": accepted, refused by the request or the destination's own state,
/// refused because the destination's user opted out, or no such destination or channel.
pub const STATUS_OK: i32 = 200;
pub const STATUS_REFUSED: i32 = 400;
pub const STATUS_FORBIDDEN: i32 = 403;
pub const STATUS_NOT_FOUND: i32 = 404;

/// The number of hex characters in a minted id, and so in an `ApplicationId`.
const MINTED_ID_LEN: usize = 32;

/// The `ChannelType` value of the Baidu (Baidu Cloud Push) channel, which is also the
/// `Platform` its response reports: `BaiduChannelResponse$Platform` is "the type of
/// messaging or notification platform for the channel. For the Baidu channel, this
/// value is BAIDU".
pub const CHANNEL_TYPE_BAIDU: &str = "BAIDU";

/// The deprecated `Id` of a channel: the segment that names the channel in its own
/// resource URI, `/v1/apps/{ApplicationId}/channels/<id>` — `baidu` for the Baidu
/// channel, `gcm` for the GCM one, `apns_sandbox` for the APNs sandbox one — which is
/// the `ChannelType` value lowercased.
///
/// `BaiduChannelResponse$Id` is "(Deprecated) An identifier for the Baidu channel":
/// an identifier of the CHANNEL, not of the application, which the response carries
/// separately as `ApplicationId`.
pub fn channel_id(channel_type: &str) -> String {
    channel_type.to_ascii_lowercase()
}

/// The service's text for a request that names a channel an existing application has
/// not enabled — the `NotFoundException` message a channel's Get or Delete answers
/// with, and the per-destination `StatusMessage` a direct send reports for such a
/// destination.
///
/// It classifies the missing resource as a channel, says what to do about it, and
/// names the application the channel has to be enabled on, which is what separates it
/// from the application-level "not found" text.
pub fn channel_not_found_message(channel_type: &str, application_id: &str) -> String {
    format!(
        "{channel_type} channel not found. Please enable the {channel_type} channel for this application {application_id}."
    )
}

/// The service's text for a request that names an application this service does not
/// hold — the `NotFoundException` message of an operation addressed at the APPLICATION
/// itself.
///
/// It classifies the missing resource as an application and carries nothing out of the
/// request: the identifier the caller asked for is not part of the text, so the
/// application-level refusal reads the same whichever operation was asked and whichever
/// identifier it was asked for.
pub const APPLICATION_NOT_FOUND_MESSAGE: &str = "Application not found";

/// The service's generic text for a resource it does not hold — the
/// `NotFoundException` message a request for a CHANNEL of an application answers with
/// when the application that would hold it is not stored.
///
/// A channel is reached as a resource of an application, so a request for one of an
/// application that does not exist resolves no resource at all and is refused as such:
/// the refusal classifies neither the channel (there is no application to enable it on)
/// nor the application (the caller addressed a channel), and carries nothing out of the
/// request.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource not found";

/// A stored Pinpoint application (a "project").
#[derive(Debug, Clone, Default)]
pub struct App {
    pub id: String,
    pub name: String,
    pub arn: String,
    /// ISO 8601, set once when the application is created.
    pub creation_date: String,
    pub tags: HashMap<String, String>,
    pub endpoints: BTreeMap<String, Endpoint>,
    /// The channels configured on this application, by the `ChannelType` value each
    /// one is the channel for. A channel is a singleton per application, so one
    /// application holds at most one entry per channel and the map needs no ceiling.
    pub channels: BTreeMap<String, Channel>,
    /// The sends this application accepted, oldest first and at most
    /// [`MAX_SENDS_PER_APP`] of them.
    pub sends: Vec<MessageSend>,
    /// The live one-time passcode challenges of this application, by the pair a
    /// verification has to match: [`otp_key`]. At most [`MAX_OTPS_PER_APP`] of them.
    pub otps: BTreeMap<(String, String), OtpChallenge>,
}

impl App {
    /// The modelled `ApplicationResponse` view of this stored application. Every
    /// operation that returns an application builds it here, so CreateApp, GetApp,
    /// GetApps and DeleteApp cannot describe the same application differently.
    pub fn to_response(&self) -> ApplicationResponse {
        ApplicationResponse {
            arn: self.arn.clone(),
            id: self.id.clone(),
            name: self.name.clone(),
            tags: Some(self.tags.clone()),
            creation_date: Some(self.creation_date.clone()),
        }
    }

    /// Record one accepted send against this application, dropping the oldest record
    /// when the log is already at [`MAX_SENDS_PER_APP`].
    pub fn record_send(&mut self, send: MessageSend) {
        if self.sends.len() >= MAX_SENDS_PER_APP {
            let overflow = self.sends.len() + 1 - MAX_SENDS_PER_APP;
            self.sends.drain(..overflow);
        }
        self.sends.push(send);
    }

    /// Create or update the settings of one channel, the way a channel's Update
    /// operation does: the first write records the creation date, and every write
    /// bumps the version and the last-modified date. Returns the stored channel, so
    /// the operation answers with what is now in the store.
    ///
    /// Every channel's Update operation goes through here, so two channels cannot
    /// record the same settings differently.
    pub fn upsert_channel(
        &mut self,
        channel_type: &str,
        enabled: bool,
        credential: Option<String>,
    ) -> &Channel {
        let now = now_iso8601();
        let application_id = self.id.clone();
        let channel = self
            .channels
            .entry(channel_type.to_string())
            .or_insert_with(|| Channel {
                channel_type: channel_type.to_string(),
                application_id,
                creation_date: now.clone(),
                ..Channel::default()
            });
        channel.enabled = enabled;
        channel.credential = credential;
        channel.last_modified_date = now;
        channel.version = channel.version.saturating_add(1);
        channel
    }

    /// The channel `channel_type` is configured and deliverable over: present,
    /// enabled and not archived. A channel an application never configured, or one it
    /// disabled, is not deliverable over.
    pub fn deliverable_channel(&self, channel_type: &str) -> Option<&Channel> {
        self.channels
            .get(channel_type)
            .filter(|channel| channel.enabled && !channel.is_archived)
    }

    /// Store one freshly minted OTP challenge against this application.
    ///
    /// A new code for a reference identifier and destination this application already
    /// holds a challenge for REPLACES it: the code last sent to a destination is the
    /// one that verifies, and an earlier code for the same pair is spent. When the
    /// store is already at [`MAX_OTPS_PER_APP`], the challenge nearest the end of its
    /// own validity is dropped to make room — the one that was going to stop verifying
    /// first anyway.
    pub fn put_otp(&mut self, challenge: OtpChallenge) {
        let key = otp_key(&challenge.reference_id, &challenge.destination_identity);
        while !self.otps.contains_key(&key) && self.otps.len() >= MAX_OTPS_PER_APP {
            // Iteration is in key order and the first minimum wins, so which of two
            // challenges expiring at the same instant is dropped is deterministic.
            let evicted = self
                .otps
                .iter()
                .min_by_key(|(_, held)| held.expires_at)
                .map(|(held_key, _)| held_key.clone());
            match evicted {
                Some(held_key) => {
                    self.otps.remove(&held_key);
                }
                None => break,
            }
        }
        self.otps.insert(key, challenge);
    }

    /// Answer one verification of a stored OTP challenge, as of `now`, and record what
    /// the verification did to it.
    ///
    /// A code is valid only while the challenge it was minted for is still held, has
    /// not run out of validity and still answers an attempt, and only for the exact
    /// code stored for that reference identifier and destination. Each verification
    /// that reaches the comparison spends one of the challenge's attempts, and a code
    /// that verifies is consumed: a one-time passcode verifies once.
    pub fn verify_otp(
        &mut self,
        reference_id: &str,
        destination_identity: &str,
        code: &str,
        now: SystemTime,
    ) -> bool {
        let key = otp_key(reference_id, destination_identity);
        let Some(challenge) = self.otps.get_mut(&key) else {
            // Nothing was sent for this pair — or what was sent has already been
            // consumed or evicted — so there is no code this request could match.
            return false;
        };
        if challenge.expires_at <= now {
            // Owed by the clock: the challenge stopped being valid at that instant
            // whether or not anybody verified it, so drop it now it is observed.
            self.otps.remove(&key);
            return false;
        }
        if challenge.attempts_remaining <= 0 {
            return false;
        }
        challenge.attempts_remaining = challenge.attempts_remaining.saturating_sub(1);
        if challenge.code != code {
            return false;
        }
        self.otps.remove(&key);
        true
    }
}

/// The key one OTP challenge is stored under: the developer-supplied reference
/// identifier together with the destination the code was sent to — the pair a
/// verification carries, since `SendOTPMessageRequestParameters$ReferenceId` is
/// "required to match during OTP verification" and `VerifyOTPMessageRequestParameters`
/// requires the destination as well.
pub fn otp_key(reference_id: &str, destination_identity: &str) -> (String, String) {
    (
        reference_id.to_string(),
        destination_identity.to_string(),
    )
}

/// One live one-time passcode of an application: what `SendOTPMessage` minted and
/// stored, and the only thing `VerifyOTPMessage` answers from.
#[derive(Clone)]
pub struct OtpChallenge {
    /// The caller's own reference identifier for the code.
    pub reference_id: String,
    /// The destination the code was sent to.
    pub destination_identity: String,
    /// The code the message carried. No operation of this service reports it: it is
    /// only ever compared against the code a verification supplies.
    pub code: String,
    /// The verifications this challenge still answers: `AllowedAttempts` when it was
    /// minted, one fewer after each verification that reaches the comparison.
    pub attempts_remaining: i32,
    /// ISO 8601, the instant the code was sent.
    pub sent_at: String,
    /// The instant the code stops being valid: `ValidityPeriod` minutes after it was
    /// sent.
    pub expires_at: SystemTime,
}

/// Written by hand rather than derived so the code itself cannot reach a formatter:
/// everything else about a challenge is safe to print, and the code is the one part
/// that is only ever compared.
impl ::std::fmt::Debug for OtpChallenge {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("OtpChallenge")
            .field("reference_id", &self.reference_id)
            .field("destination_identity", &self.destination_identity)
            .field("code_length", &self.code.chars().count())
            .field("attempts_remaining", &self.attempts_remaining)
            .field("sent_at", &self.sent_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// A channel configured on one application: what the channel's Update operation
/// stored, what its Get reports, and what a send over that channel is checked
/// against.
#[derive(Debug, Clone, Default)]
pub struct Channel {
    /// The `ChannelType` value this is the channel for, e.g. `BAIDU` — also the
    /// `Platform` the channel's response reports.
    pub channel_type: String,
    pub application_id: String,
    pub enabled: bool,
    /// The credential the channel's response member carries, when the caller supplied
    /// one (the API key, client id or equivalent the channel is modelled with).
    pub credential: Option<String>,
    /// Set when the channel is deleted: deleting a channel archives it, and the
    /// settings the delete answers with report that.
    pub is_archived: bool,
    /// ISO 8601, set once when the channel is first configured.
    pub creation_date: String,
    /// ISO 8601, refreshed on every write to the channel.
    pub last_modified_date: String,
    pub last_modified_by: Option<String>,
    /// 1 after the first write, incremented by every later one.
    pub version: i32,
}

impl Channel {
    /// Whether the channel currently holds a credential to communicate with, which is
    /// what its response's `HasCredential` member reports: a configured channel holds
    /// the one its Update was given, and a channel whose settings have been deleted —
    /// and so archived — holds none any more, whatever it was configured with.
    pub fn holds_credential(&self) -> bool {
        self.credential.is_some() && !self.is_archived
    }

    /// The modelled `BaiduChannelResponse` view of this stored channel. Every
    /// operation that returns the Baidu channel builds it here, so UpdateBaiduChannel,
    /// GetBaiduChannel and DeleteBaiduChannel cannot describe the same channel
    /// differently.
    pub fn to_baidu_response(&self) -> BaiduChannelResponse {
        BaiduChannelResponse {
            application_id: Some(self.application_id.clone()),
            creation_date: Some(self.creation_date.clone()),
            credential: self.credential.clone().unwrap_or_default(),
            enabled: Some(self.enabled),
            has_credential: Some(self.holds_credential()),
            id: Some(channel_id(&self.channel_type)),
            is_archived: Some(self.is_archived),
            last_modified_by: self.last_modified_by.clone(),
            last_modified_date: Some(self.last_modified_date.clone()),
            platform: self.channel_type.clone(),
            version: Some(self.version),
        }
    }
}

/// One destination's outcome inside a recorded send: what this service answered for
/// one endpoint id or one address, and the message id it minted for it.
#[derive(Debug, Clone, Default)]
pub struct MessageDelivery {
    /// The endpoint the message was addressed to, when it was addressed by endpoint id.
    pub endpoint_id: Option<String>,
    /// The address the message was addressed to, as far as it is known.
    pub address: Option<String>,
    /// The channel the destination was resolved to, when one could be resolved.
    pub channel_type: Option<String>,
    /// A `DeliveryStatus` value.
    pub delivery_status: String,
    pub status_code: i32,
    /// Minted only for a destination the message was accepted for.
    pub message_id: Option<String>,
}

/// One accepted direct-message send against an application.
#[derive(Debug, Clone, Default)]
pub struct MessageSend {
    /// The `RequestId` the send answered with.
    pub request_id: String,
    /// The caller's own `TraceId`, when it supplied one.
    pub trace_id: Option<String>,
    /// ISO 8601, the instant the send was accepted.
    pub sent_at: String,
    /// One entry per destination the request named, in the order they were answered.
    pub deliveries: Vec<MessageDelivery>,
}

/// A stored endpoint of one application.
#[derive(Debug, Clone, Default)]
pub struct Endpoint {
    pub id: String,
    pub application_id: String,
    pub address: Option<String>,
    pub channel_type: Option<String>,
    pub attributes: Option<HashMap<String, Vec<String>>>,
    pub metrics: Option<HashMap<String, f64>>,
    pub user: Option<EndpointUser>,
    pub location: Option<EndpointLocation>,
    pub demographic: Option<EndpointDemographic>,
    pub opt_out: Option<String>,
    pub endpoint_status: Option<String>,
    pub request_id: Option<String>,
    /// 0-99, assigned once when the endpoint is created.
    pub cohort_id: String,
    /// ISO 8601, set once when the endpoint is created.
    pub creation_date: String,
    /// ISO 8601, refreshed on every write to the endpoint.
    pub effective_date: String,
}

impl Endpoint {
    /// The modelled `EndpointResponse` view of this stored endpoint.
    pub fn to_response(&self) -> EndpointResponse {
        EndpointResponse {
            address: self.address.clone(),
            application_id: Some(self.application_id.clone()),
            attributes: self.attributes.clone(),
            channel_type: self.channel_type.clone(),
            cohort_id: Some(self.cohort_id.clone()),
            creation_date: Some(self.creation_date.clone()),
            demographic: self.demographic.clone(),
            effective_date: Some(self.effective_date.clone()),
            endpoint_status: self.endpoint_status.clone(),
            id: Some(self.id.clone()),
            location: self.location.clone(),
            metrics: self.metrics.clone(),
            opt_out: self.opt_out.clone(),
            request_id: self.request_id.clone(),
            user: self.user.clone(),
        }
    }
}

/// Per-service state for com-amazonaws-pinpoint-pinpoint. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsPinpointPinpointState {
    /// Applications by `ApplicationId`.
    pub apps: BTreeMap<String, App>,
    /// Monotonic counter mixed into every minted id, so two ids minted inside the
    /// same clock tick still differ. Only [`Self::mint_id`] reads it.
    mint_counter: u64,
}

impl ComAmazonawsPinpointPinpointState {
    /// Mint a fresh server-generated id: 32 lowercase hex characters, the shape a
    /// Pinpoint `ApplicationId` has.
    ///
    /// Uniqueness comes from the monotonic counter, not from the clock, so ids
    /// minted within one tick (or after the clock steps back) are still distinct.
    /// It is hashed only so callers cannot read a sequence number out of an id and
    /// predict the next one.
    pub fn mint_id(&mut self) -> String {
        self.mint_counter = self.mint_counter.wrapping_add(1);
        let seed = format!("{}:{}", self.mint_counter, nanos_since_epoch());
        let digest = Sha256::digest(seed.as_bytes());
        hex::encode(&digest[..MINTED_ID_LEN / 2])
    }

    /// Mint a fresh one-time passcode of `length` decimal digits.
    ///
    /// Drawn the same way as [`Self::mint_id`], so two codes minted inside one clock
    /// tick still differ and a caller holding one code cannot work out the next.
    /// `length` is bounded by [`MAX_CODE_LENGTH`], which is well inside the digest's
    /// own width, so the code always has as many digits as was asked for.
    pub fn mint_otp_code(&mut self, length: usize) -> String {
        self.mint_counter = self.mint_counter.wrapping_add(1);
        let seed = format!("otp:{}:{}", self.mint_counter, nanos_since_epoch());
        let digest = Sha256::digest(seed.as_bytes());
        digest
            .iter()
            .copied()
            .take(length)
            .map(|byte| char::from(b'0' + byte % 10))
            .collect()
    }
}

/// Whether `id` has the form [`ComAmazonawsPinpointPinpointState::mint_id`] mints:
/// [`MINTED_ID_LEN`] lowercase hex characters. Lets an operation tell an identifier
/// this service could have issued from a string that cannot name one of its
/// resources at all.
pub fn is_application_id(id: &str) -> bool {
    id.len() == MINTED_ID_LEN && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsPinpointPinpointState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsPinpointPinpointState>)
}

/// Nanoseconds since the Unix epoch, saturating at 0 if the host clock predates it.
fn nanos_since_epoch() -> u128 {
    ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos())
}

/// The current time in the ISO 8601 form Pinpoint's date members carry
/// (`CreationDate`, `EffectiveDate`).
pub fn now_iso8601() -> String {
    let stamp = ::aws_smithy_types::DateTime::from(::std::time::SystemTime::now());
    stamp
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|_| String::from("1970-01-01T00:00:00Z"))
}

/// The region this request was signed for, from its SigV4 credential scope;
/// [`DEFAULT_REGION`] when the request carries no scope (an unsigned call).
pub fn region_of(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map_or_else(|| DEFAULT_REGION.to_string(), |scope| scope.region)
}

/// The ARN of an application: `arn:aws:mobiletargeting:<region>:<account>:apps/<id>`.
pub fn app_arn(region: &str, application_id: &str) -> String {
    ::engine::emulator_arn(ARN_NAMESPACE, region, &format!("apps/{application_id}"))
}

/// The cohort an endpoint belongs to: a number from 0-99, as `EndpointResponse$CohortId`
/// documents ("a number from 0-99 that represents the cohort that the endpoint is
/// assigned to ... each cohort contains approximately 1 percent of the endpoints").
///
/// Derived from a hash of the endpoint's identity so the spread over the 100 cohorts
/// is even and an endpoint keeps the cohort it was assigned.
pub fn cohort_id(application_id: &str, endpoint_id: &str) -> String {
    let digest = Sha256::digest(format!("{application_id}/{endpoint_id}").as_bytes());
    let bucket = u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) % 100;
    bucket.to_string()
}
