// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-endusermessaging-endusermessaging service.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{SystemTime, UNIX_EPOCH};

/// The one-time passcode verifications the send/validate pair owns.
mod verifications;
pub use verifications::*;

/// The attributes a brand profile holds, and the default template a created
/// profile is seeded with.
mod brand_profile_attributes;
pub use brand_profile_attributes::*;

/// Which resource a `resourceArn` names, and the tags that resource carries.
mod tags;
pub use tags::*;

/// The service's own ARN namespace / sigv4 signing name, as the Smithy model's
/// `aws.api#service` trait declares it (`arnNamespace`/`endpointPrefix`
/// `end-user-messaging`).
pub const ARN_NAMESPACE: &str = "end-user-messaging";

/// The region used when a request carries no SigV4 credential scope to read one
/// from (an anonymous, host-routed request). Every generated emulator defaults
/// here.
pub const DEFAULT_REGION: &str = "us-east-1";

/// The `brand-profile` resource segment of a brand-profile ARN.
const BRAND_PROFILE_RESOURCE: &str = "brand-profile";

/// The prefix a `brandProfileId` carries, as `BrandProfileIdOrArn` documents the
/// bare identifier (`bp-abc12345678901234`).
const BRAND_PROFILE_ID_PREFIX: &str = "bp";

/// How many characters follow a `brandProfileId`'s prefix and hyphen, as the bare
/// identifier `BrandProfileIdOrArn` documents is written.
const BRAND_PROFILE_ID_BODY_LENGTH: usize = 17;

/// The `notify-code-configuration` resource segment of a notify-code-configuration
/// ARN, as `NotifyCodeConfigurationIdOrArn` documents it.
const NOTIFY_CODE_CONFIGURATION_RESOURCE: &str = "notify-code-configuration";

/// A persisted brand profile: everything the brand-profile operations report.
#[derive(Debug, Clone)]
pub struct BrandProfile {
    /// The minted `brandProfileId` (`bp-` + 17 chars, per `BrandProfileIdOrArn`).
    pub id: String,
    /// `brandProfileArn`, built once at create time from the request's region.
    pub arn: String,
    pub name: String,
    /// `status`, one of [`crate::ops::types::STATUS_VALUES`]. A profile AWS
    /// returns from `CreateBrandProfile` is already `ACTIVE`.
    pub status: String,
    pub deletion_protection_enabled: bool,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The tags this profile carries, in the order they were written: those supplied
    /// at create time, as `ListTagsForResource` reports them.
    pub tags: Vec<(String, String)>,
    /// The profile's attributes, keyed by `attributeName`: the default template of
    /// [`default_brand_profile_attributes`] as created, plus whatever the attribute
    /// operations have since written. A `BTreeMap` so
    /// `ListBrandProfileAttributes` pages in a stable, resumable order.
    pub attributes: BTreeMap<String, BrandProfileAttribute>,
    /// How many default attributes were seeded into this profile when it was
    /// created, which is what `attributesCreated` reports. A property of the
    /// create, so it is recorded on the record rather than recomputed from a map
    /// that later attribute calls change.
    pub default_attributes_created: usize,
}

/// A persisted notify code configuration: the reusable passcode policy the
/// notify-code-configuration operations report.
///
/// `code_configuration_parameters` and `channel_parameters` hold the modelled
/// structures exactly as the request supplied them, member for member. The API
/// documents every one of their members as optional and applies no value at create
/// time for an omitted one — the passcode defaults are applied when a code is sent —
/// so a member absent from the request stays absent in the store and in every
/// projection of this record.
#[derive(Debug, Clone)]
pub struct NotifyCodeConfigurationRecord {
    /// The minted `notifyCodeConfigurationId`.
    pub id: String,
    /// `notifyCodeConfigurationArn`, built once at create time from the request's
    /// region.
    pub arn: String,
    pub name: String,
    pub code_configuration_parameters: Option<crate::ops::types::CodeConfigurationParameters>,
    pub channel_parameters: Option<crate::ops::types::ChannelParameters>,
    pub deletion_protection_enabled: bool,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The tags this configuration carries, in the order they were written: those
    /// supplied at create time, as `ListTagsForResource` reports them.
    pub tags: Vec<(String, String)>,
    /// The `clientToken` of the request that minted this record, when it carried
    /// one. Kept on the record rather than in a map beside it, so the token lives
    /// and dies with the resource it names and is bounded by the same ceiling.
    pub client_token: Option<String>,
}

/// The most notify code configurations this emulator stores for one account.
///
/// A ceiling, not a guess at AWS's published quota: it exists so a create loop
/// cannot grow the emulator's heap without bound. `CreateNotifyCodeConfiguration`
/// models `ServiceQuotaExceededException`, which is the refusal once the store is
/// full.
pub const MAX_NOTIFY_CODE_CONFIGURATIONS: usize = 100;

/// The most brand profiles this emulator stores for one account.
///
/// A ceiling, not a guess at AWS's published quota: it exists so a create loop
/// cannot grow the emulator's heap without bound. `CreateBrandProfile` models
/// `ServiceQuotaExceededException`, which is the refusal once the store is full.
pub const MAX_BRAND_PROFILES: usize = 100;

/// The most `clientToken` entries kept for `CreateBrandProfile` idempotency.
///
/// At most one token is recorded per stored profile and a token is dropped with
/// the profile it minted, so this cap is never the binding one in practice; it is
/// enforced anyway, by evicting the oldest token, so the map cannot accumulate
/// caller-chosen strings independently of the profiles they name.
pub const MAX_CREATE_TOKENS: usize = MAX_BRAND_PROFILES;

/// Per-service state for com-amazonaws-endusermessaging-endusermessaging. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsEndusermessagingEndusermessagingState {
    /// Every brand profile, keyed by `brandProfileId`. A `BTreeMap` so
    /// `ListBrandProfiles` pages in a stable, resumable order. Bounded by
    /// [`MAX_BRAND_PROFILES`].
    pub brand_profiles: BTreeMap<String, BrandProfile>,
    /// Every notify code configuration, keyed by `notifyCodeConfigurationId`. A
    /// `BTreeMap` so `ListNotifyCodeConfigurations` pages in a stable, resumable
    /// order. Bounded by [`MAX_NOTIFY_CODE_CONFIGURATIONS`].
    pub notify_code_configurations: BTreeMap<String, NotifyCodeConfigurationRecord>,
    /// Every one-time passcode verification in flight, keyed by
    /// `verificationId`. A `BTreeMap` so a scan over the verifications of one
    /// recipient is in a stable order. Bounded by [`MAX_VERIFICATIONS`], and
    /// pruned of elapsed validity windows by [`Self::expire_verifications`].
    pub verifications: BTreeMap<String, Verification>,
    /// `CreateBrandProfile` idempotency: `clientToken` -> `brandProfileId`.
    /// Bounded by [`MAX_CREATE_TOKENS`]; write it through
    /// [`Self::record_create_token`], never directly.
    pub create_tokens: HashMap<String, String>,
    /// The order [`Self::create_tokens`] was written in, so the cap evicts the
    /// oldest token rather than an arbitrary one.
    create_token_order: VecDeque<String>,
    /// Per-state random-ish prefix for minted ids, set on the first mint.
    id_seed: u64,
    /// Monotonic counter making each minted id unique within this state.
    next_id: u64,
}

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// Mint a fresh, unused `brandProfileId`.
    ///
    /// The shape is the one `BrandProfileIdOrArn` documents — a `bp-` prefix
    /// followed by 17 lowercase alphanumerics — and it satisfies that shape's
    /// `^[A-Za-z0-9_:/-]+$` pattern. The clock-derived seed keeps ids from
    /// different runs apart; the counter keeps ids within one run unique, and
    /// the loop re-draws rather than ever handing back an id already in use.
    pub fn mint_brand_profile_id(&mut self) -> String {
        loop {
            let id = self.next_candidate_id(BRAND_PROFILE_ID_PREFIX);
            if !self.brand_profiles.contains_key(&id) {
                return id;
            }
        }
    }

    /// Mint a fresh, unused `notifyCodeConfigurationId`.
    ///
    /// `NotifyCodeConfigurationIdOrArn` documents the identifier as an opaque token
    /// of no particular length or format, so it takes the same minted shape as every
    /// other id this state hands out — a resource prefix plus clock-seeded and
    /// counted hex digits — which satisfies `NotifyCodeConfigurationId`'s
    /// `@length(1..64)` and `^[A-Za-z0-9_-]+$` pattern. The loop re-draws rather than
    /// ever handing back an id already in use.
    pub fn mint_notify_code_configuration_id(&mut self) -> String {
        loop {
            let id = self.next_candidate_id("ncc");
            if !self.notify_code_configurations.contains_key(&id) {
                return id;
            }
        }
    }

    /// The next candidate id for a resource addressed with `prefix`.
    ///
    /// The clock-derived seed keeps ids from different runs apart; the counter keeps
    /// ids within one run unique. Whether the candidate is free is the caller's
    /// question, since it is the caller that knows which map the id goes in.
    fn next_candidate_id(&mut self, prefix: &str) -> String {
        if self.id_seed == 0 {
            // 36 bits of the current nanosecond clock -> 9 hex digits.
            self.id_seed = (nanos_since_epoch() as u64) & 0x0000_000F_FFFF_FFFF;
        }
        self.next_id = self.next_id.wrapping_add(1);
        format!(
            "{prefix}-{:09x}{:08x}",
            self.id_seed,
            self.next_id & 0xFFFF_FFFF
        )
    }

    /// Record `token` as having minted `brand_profile_id`, evicting the oldest
    /// token first if that would push the map past [`MAX_CREATE_TOKENS`].
    pub fn record_create_token(&mut self, token: String, brand_profile_id: &str) {
        while self.create_tokens.len() >= MAX_CREATE_TOKENS {
            match self.create_token_order.pop_front() {
                Some(oldest) => {
                    self.create_tokens.remove(&oldest);
                }
                // The order queue and the map are written together, so this is
                // unreachable; clearing rather than looping forever keeps the cap
                // an invariant even if they ever drift.
                None => {
                    self.create_tokens.clear();
                    break;
                }
            }
        }
        if self
            .create_tokens
            .insert(token.clone(), brand_profile_id.to_string())
            .is_none()
        {
            self.create_token_order.push_back(token);
        }
    }

    /// Drop every idempotency token that minted `brand_profile_id`, so a token
    /// never outlives the profile it names.
    pub fn forget_create_tokens_for(&mut self, brand_profile_id: &str) {
        self.create_tokens.retain(|_, minted| minted != brand_profile_id);
        let live = &self.create_tokens;
        self.create_token_order.retain(|token| live.contains_key(token));
    }

    /// Resolve a `BrandProfileIdOrArn` to the id of a stored profile.
    ///
    /// The shape accepts either a bare id or the profile's full ARN, so every
    /// operation taking one resolves both. The two ways a value reaches no profile
    /// are different failures, so they are told apart before any lookup: a value
    /// that is not an identifier of this resource at all cannot name a profile in
    /// any state, and is refused as invalid input; an identifier of the documented
    /// form that this state holds no record for is refused as not found.
    pub fn resolve_brand_profile_id(&self, id_or_arn: &str) -> Result<&str, UnreachableResource> {
        let Some(id) = brand_profile_id_of(id_or_arn) else {
            return Err(UnreachableResource::Invalid(invalid_brand_profile_id(
                id_or_arn,
            )));
        };
        self.brand_profiles
            .get_key_value(id)
            .map(|(id, _)| id.as_str())
            .ok_or_else(|| UnreachableResource::NotFound(brand_profile_not_found(id_or_arn)))
    }

    /// Resolve a `NotifyCodeConfigurationIdOrArn` to the id of a stored
    /// configuration.
    ///
    /// The shape accepts either a bare id or the resource's full ARN, so every
    /// operation taking one resolves both; an unknown value answers `None`, which
    /// the caller renders as `ResourceNotFoundException`.
    pub fn resolve_notify_code_configuration_id(&self, id_or_arn: &str) -> Option<&str> {
        if let Some((id, _)) = self.notify_code_configurations.get_key_value(id_or_arn) {
            return Some(id.as_str());
        }
        let id = resource_id_from_arn(id_or_arn, NOTIFY_CODE_CONFIGURATION_RESOURCE)?;
        self.notify_code_configurations
            .get_key_value(id)
            .map(|(id, _)| id.as_str())
    }

    /// The stored notify code configuration a previous create minted for `token`,
    /// or `None` when no live record carries it.
    ///
    /// `clientToken` is `@idempotencyToken`, so a repeat of a request already
    /// served reports the record it minted instead of creating a second one. The
    /// scan is over a map bounded by [`MAX_NOTIFY_CODE_CONFIGURATIONS`].
    pub fn notify_code_configuration_minted_by(
        &self,
        token: &str,
    ) -> Option<&NotifyCodeConfigurationRecord> {
        self.notify_code_configurations
            .values()
            .find(|record| record.client_token.as_deref() == Some(token))
    }
}

/// The id named by an ARN of this service's `resource` type
/// (`arn:<partition>:end-user-messaging:<region>:<account>:<resource>/<id>`), or
/// `None` for anything that is not one.
///
/// The ARN itself is read by the workspace's one ARN parse
/// ([`::service_common::arn`]), once, so this service does not decide for itself what
/// an ARN is; what belongs here is only which vendor its ARNs carry and which
/// resource type the caller asked about. AWS writes a resource in either the
/// `<type>/<id>` or the `<type>:<id>` form, so both separators end the type — and
/// requiring one of them is what keeps a longer type name from matching a shorter
/// one.
fn resource_id_from_arn<'a>(candidate: &'a str, resource: &str) -> Option<&'a str> {
    let arn = ::service_common::arn::parse_of(candidate, ARN_NAMESPACE)?;
    let rest = arn.resource.strip_prefix(resource)?;
    let id = rest.strip_prefix('/').or_else(|| rest.strip_prefix(':'))?;
    (!id.is_empty()).then_some(id)
}

/// Whether `candidate` is a bare `brandProfileId`: the system-generated identifier
/// `BrandProfileIdOrArn` documents, its resource prefix followed by the alphanumeric
/// body of the documented length.
fn is_brand_profile_id(candidate: &str) -> bool {
    candidate
        .strip_prefix(BRAND_PROFILE_ID_PREFIX)
        .and_then(|rest| rest.strip_prefix('-'))
        .is_some_and(|body| {
            body.len() == BRAND_PROFILE_ID_BODY_LENGTH
                && body.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

/// The `brandProfileId` a well-formed `BrandProfileIdOrArn` names, whichever of the
/// two forms the shape documents it took, or `None` for a value that is neither.
///
/// A bare identifier names itself; an ARN of this service's `brand-profile` resource
/// names the identifier in its resource field. The ARN form carries the same bare
/// identifier, so a value is well formed only when the identifier it names is.
/// Whether that identifier reaches a stored profile is a separate question, asked
/// after this one.
pub fn brand_profile_id_of(id_or_arn: &str) -> Option<&str> {
    if is_brand_profile_id(id_or_arn) {
        return Some(id_or_arn);
    }
    let id = resource_id_from_arn(id_or_arn, BRAND_PROFILE_RESOURCE)?;
    is_brand_profile_id(id).then_some(id)
}

/// The JSON pointer naming the member a refused `brandProfileId` arrived on, as
/// `ValidationExceptionField$path` documents it.
const BRAND_PROFILE_ID_POINTER: &str = "/brandProfileId";

/// `ValidationException` for a `BrandProfileIdOrArn` that is not an identifier of a
/// brand profile at all — neither a bare `brandProfileId` nor the ARN of one.
///
/// Such a value can name no profile in any state, so it is a refusal about the input
/// rather than about a missing resource. The fixed text is this refusal's; the only
/// variable part is the value the request gave, and `fieldList` points at the member
/// it came in on.
fn invalid_brand_profile_id(id_or_arn: &str) -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: format!("invalid brand profile ID format: {id_or_arn}"),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: BRAND_PROFILE_ID_POINTER.to_string(),
            message: format!(
                "The value must be a brand profile identifier: the bare ID, or the full \
                 Amazon Resource Name of the form \
                 arn:<partition>:{ARN_NAMESPACE}:<region>:<account-id>:{BRAND_PROFILE_RESOURCE}/<id>"
            ),
        }]),
    }
}

/// `ValidationException` for an update request that carries no updatable member at
/// all.
///
/// Every updatable member of an update of this API is optional on its own — any one
/// of them is enough — but a request carrying none of them asks for no change, and
/// is refused before anything is resolved or written. Worded as a sentence, the way
/// this API words a refusal its model does not express, and about the request as a
/// whole rather than about one member, so no `fieldList` names a member at fault.
pub fn nothing_to_update() -> crate::ops::types::ValidationException {
    crate::ops::types::ValidationException {
        message: "At least one field must be provided for update".to_string(),
        field_list: None,
    }
}

/// The ARN of a brand profile in `region`.
pub fn brand_profile_arn(region: &str, brand_profile_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{BRAND_PROFILE_RESOURCE}/{brand_profile_id}"),
    )
}

/// The ARN of a notify code configuration in `region`.
pub fn notify_code_configuration_arn(region: &str, notify_code_configuration_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_NAMESPACE,
        region,
        &format!("{NOTIFY_CODE_CONFIGURATION_RESOURCE}/{notify_code_configuration_id}"),
    )
}

/// The region this request was signed for, or [`DEFAULT_REGION`] when it carries
/// no SigV4 credential scope.
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map(|scope| scope.region)
        .filter(|region| !region.is_empty())
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// A stored instant as the modelled `Timestamp` the wire carries: the whole epoch
/// second it falls in.
///
/// Every `Timestamp` member of this service is `smithy.api#Timestamp` with no
/// `@timestampFormat` override, so restJson1 carries it in the body in its default
/// epoch-seconds form, and this API — which documents each one as a time "in Unix
/// epoch time" — reports it as a whole count of seconds, with no sub-second
/// fraction. A stored instant is therefore reported as the second it falls in, and
/// the resolution of the host clock that recorded it never reaches the wire.
/// Truncating rather than rounding keeps the reported instant inside the second the
/// event happened in, so it never runs ahead of the clock that recorded it. Every
/// projection of every stored instant goes through here, so one instant has one
/// wire form whichever operation reports it.
pub fn timestamp(at: SystemTime) -> ::aws_smithy_types::DateTime {
    let since_epoch = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    ::aws_smithy_types::DateTime::from_secs(i64::try_from(since_epoch.as_secs()).unwrap_or(i64::MAX))
}

/// The message for the one failure that is not about the request: this service was
/// mounted on an engine that holds no store for it.
pub const NO_STORE: &str =
    "the com-amazonaws-endusermessaging-endusermessaging store is not mounted on this engine";

/// `status` of a brand profile this emulator creates — a member of
/// `crate::ops::types::STATUS_VALUES`.
pub const ACTIVE_STATUS: &str = "ACTIVE";

/// The `resourceType` a brand-profile error reports: the hyphenated resource
/// vocabulary this API names the resource by, the same spelling it takes in a
/// brand-profile ARN. `ConflictException`/`ResourceNotFoundException` model
/// `resourceType` as a free `String`, not as the `JobResourceType` enum, so the
/// resource segment — not that enum's `BRAND_PROFILE` — is the token reported
/// here and quoted in the refusal messages below.
pub const BRAND_PROFILE_RESOURCE_TYPE: &str = BRAND_PROFILE_RESOURCE;

/// The `resourceType` a notify-code-configuration error reports, in that same
/// hyphenated resource vocabulary: the spelling the resource takes in its own ARN.
pub const NOTIFY_CODE_CONFIGURATION_RESOURCE_TYPE: &str = NOTIFY_CODE_CONFIGURATION_RESOURCE;

/// The conflict `Reason` token for a request refused because the resource it
/// names has deletion protection switched on.
pub const DELETION_PROTECTION_ENABLED_REASON: &str = "DELETION_PROTECTION_ENABLED";

/// `ResourceNotFoundException` for an identifier that names no stored resource of
/// `resource_type`.
///
/// The message is this error class's fixed template; its only variable parts are
/// the resource type and the identifier, which is the value the request gave.
pub fn resource_not_found(
    resource_type: &str,
    resource_id: &str,
) -> crate::ops::types::ResourceNotFoundException {
    crate::ops::types::ResourceNotFoundException {
        message: format!(
            "Resource not found - ResourceType=\"{resource_type}\" ResourceId=\"{resource_id}\""
        ),
        resource_id: Some(resource_id.to_string()),
        resource_type: Some(resource_type.to_string()),
    }
}

/// `ConflictException` for a request the current state of `resource_id` forbids.
///
/// The message is this error class's fixed template: the quoted `Reason` token
/// names the state that caused the conflict and the quoted `ResourceType` the
/// resource's type; no request-supplied value appears in it, so the identifier is
/// reported in the `resourceId` member alone.
pub fn resource_conflict(
    reason: &str,
    resource_type: &str,
    resource_id: &str,
) -> crate::ops::types::ConflictException {
    crate::ops::types::ConflictException {
        message: format!("Conflict occurred - Reason=\"{reason}\" ResourceType=\"{resource_type}\""),
        resource_id: Some(resource_id.to_string()),
        resource_type: Some(resource_type.to_string()),
    }
}

/// `ResourceNotFoundException` for a `BrandProfileIdOrArn` that names no stored
/// profile. The message is built from the request, so it names the value given.
pub fn brand_profile_not_found(id_or_arn: &str) -> crate::ops::types::ResourceNotFoundException {
    resource_not_found(BRAND_PROFILE_RESOURCE_TYPE, id_or_arn)
}

/// `ConflictException` for a brand-profile request refused by that profile's
/// deletion protection.
pub fn brand_profile_deletion_protection_conflict(
    brand_profile_id: &str,
) -> crate::ops::types::ConflictException {
    resource_conflict(
        DELETION_PROTECTION_ENABLED_REASON,
        BRAND_PROFILE_RESOURCE_TYPE,
        brand_profile_id,
    )
}

/// `ResourceNotFoundException` for a `NotifyCodeConfigurationIdOrArn` that names no
/// stored configuration. The message is built from the request, so it names the
/// value given.
pub fn notify_code_configuration_not_found(
    id_or_arn: &str,
) -> crate::ops::types::ResourceNotFoundException {
    resource_not_found(NOTIFY_CODE_CONFIGURATION_RESOURCE_TYPE, id_or_arn)
}

/// The modelled `NotifyCodeConfiguration` resource, read back off a stored record.
///
/// Every operation that projects a notify code configuration reports it through
/// here, so one record has one rendering whichever operation returns it.
pub fn notify_code_configuration_view(
    record: &NotifyCodeConfigurationRecord,
) -> crate::ops::types::NotifyCodeConfiguration {
    crate::ops::types::NotifyCodeConfiguration {
        notify_code_configuration_id: record.id.clone(),
        notify_code_configuration_arn: record.arn.clone(),
        notify_code_configuration_name: record.name.clone(),
        code_configuration_parameters: record.code_configuration_parameters.clone(),
        channel_parameters: record.channel_parameters.clone(),
        deletion_protection_enabled: record.deletion_protection_enabled,
        created_at: timestamp(record.created_at),
        updated_at: timestamp(record.updated_at),
    }
}

/// An opaque pagination continuation token for a page that ended at
/// `last_returned_key`.
///
/// `marker` names the issuing operation and token version, so a token one paginated
/// operation issued is never read as a resume point by another and any value the
/// service did not issue is recognisable; hex keeps the token inside `NextToken`'s
/// `^.+$` pattern and `@length(1..2048)` whatever the key contains, and makes it
/// opaque to a client while staying decodable by the next call. Every paginated
/// operation of this service pages through here, so one token format is defined
/// once.
pub fn encode_page_token(marker: &str, last_returned_key: &str) -> String {
    ::hex::encode(format!("{marker}{last_returned_key}"))
}

/// The key a token issued by [`encode_page_token`] under `marker` resumes strictly
/// after, or `None` for any value that operation never issued.
pub fn decode_page_token(marker: &str, token: &str) -> Option<String> {
    let decoded = String::from_utf8(::hex::decode(token).ok()?).ok()?;
    let resume_after = decoded.strip_prefix(marker)?;
    (!resume_after.is_empty()).then(|| resume_after.to_string())
}

fn nanos_since_epoch() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsEndusermessagingEndusermessagingState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsEndusermessagingEndusermessagingState>)
}
