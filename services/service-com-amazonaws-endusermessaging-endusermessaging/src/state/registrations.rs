// SPDX-License-Identifier: CC0-1.0

//! The registrations this service holds, which values name one, and how the fields a
//! registration carries are imported into a brand profile's attributes.
//!
//! A registration is a resource this API maps a brand profile to and from: one is created
//! from a profile with its fields prefilled from that profile's attributes
//! (`CreateRegistrationsFromBrandProfile`), and the fields a registration carries are
//! imported back into a profile's attributes by `UpdateBrandProfileFromRegistration`. The
//! record here is therefore what both directions agree on — the registration's identity,
//! the profile it was derived from, and the field values it carries — so one registration
//! has one meaning whichever operation reads it.
//!
//! `RegistrationIdOrArn` accepts either a bare identifier or the ARN of one, so a value
//! reaching no stored registration does so for one of two reasons, and they are different
//! failures to a caller: a value that is neither form can name a registration in NO state
//! and is a bad request, while an identifier of the documented form that this state holds
//! no record for is a missing resource. Classifying the value therefore happens BEFORE any
//! lookup, and it lives here rather than in one handler so every operation that takes such
//! an identifier tells the two apart the same way — as the brand-profile and
//! notify-code-configuration identifier gates beside it do.

use std::collections::BTreeMap;
use std::time::SystemTime;

use super::ComAmazonawsEndusermessagingEndusermessagingState;

/// The `registration` resource segment of a registration ARN, which is also the
/// `resourceType` a refusal about a registration reports: the hyphenated resource
/// vocabulary this API names its resources by, as
/// [`super::BRAND_PROFILE_RESOURCE_TYPE`] is spelled in.
pub const REGISTRATION_RESOURCE: &str = "registration";

/// The prefix this service mints a registration identifier with, as this API's own example
/// writes a bare one (`reg-abc12345678901234`, the `UpdateBrandProfileFromRegistration`
/// example). What makes a value an identifier at all is the FORM
/// ([`is_registration_id`]), not this prefix: a registration created elsewhere in the
/// account carries its own.
pub const REGISTRATION_ID_PREFIX: &str = "reg";

/// The member a `registrationId` arrives on, as this service's own gates name a member at
/// fault in `ValidationExceptionField$path`: by the member's own name.
pub const REGISTRATION_ID_MEMBER: &str = "registrationId";

/// Whether `smartMatch` is in force for a request that does not say.
///
/// `UpdateBrandProfileFromRegistration$smartMatch` and the same member of every other
/// operation that maps between a profile and a registration document it as "semantic field
/// mapping between brand profile attributes and registration fields. The default is true."
pub const SMART_MATCH_DEFAULT: bool = true;

/// The `onAttributeConflict` value an attribute that already has a value is overwritten
/// under: `OnAttributeConflict` documents `REPLACE` as "New registration value replaces the
/// existing attribute value (default)", so it is also what a request that omits the member
/// asks for.
pub const REPLACE_ON_ATTRIBUTE_CONFLICT: &str = "REPLACE";

/// The most registrations this emulator keeps for one account.
///
/// A ceiling, not a guess at AWS's published quota: it exists so a loop of asynchronous
/// calls cannot grow the emulator's heap without bound. It is as many as a reader of a
/// profile's registrations can page back at all, the modelled `@length` ceiling of
/// `ListRegistrationsFromBrandProfile`'s own `registrationAssociations` list (0..100). The
/// oldest registration is dropped once the store is full, so the registrations a caller can
/// still reach are the most recent ones.
pub const MAX_REGISTRATIONS: usize = 100;

/// One registration: everything the operations that map between a registration and a brand
/// profile read.
#[derive(Debug, Clone)]
pub struct Registration {
    /// The minted `registrationId` this record is keyed by.
    pub id: String,
    /// The registration's ARN, built from the Region of the request that created it.
    pub arn: String,
    /// `registrationType` — "the type of the registration, for example
    /// US_TOLL_FREE_REGISTRATION or SENDER_ID".
    pub registration_type: String,
    /// The `brandProfileId` of the profile this registration was created from, which is
    /// what `ListRegistrationsFromBrandProfile` reports a registration under.
    pub brand_profile_id: String,
    pub created_at: SystemTime,
    /// `smartMatchUsed` — "whether smart matching was used to create the association".
    pub smart_match_used: bool,
    /// The field values this registration carries, keyed by the brand-profile attribute
    /// name each one was prefilled from. Ordered by that name, so an import applies them
    /// in one order whichever registration it reads.
    pub fields: BTreeMap<String, String>,
}

/// The longest a bare `registrationId` is, as `RegistrationId`'s `@length(1..256)` declares
/// it (`smithy_query(query="shape",shape="RegistrationId")`).
const REGISTRATION_ID_MAX_LENGTH: usize = 256;

/// Whether `candidate` is a bare `registrationId`: a system-generated identifier written
/// the way this API writes one — a short alphanumeric resource prefix, a hyphen, and an
/// alphanumeric body — inside `RegistrationId`'s length.
///
/// `RegistrationId` documents the value as a bare, service-generated identifier and declares
/// no body length for one, so the identifier is an opaque token and no particular length is
/// required of it: what makes a value an identifier at all is being written in that form,
/// which this API's own example writes (`reg-abc12345678901234`, the
/// `UpdateBrandProfileFromRegistration` example). The prefix is NOT required to be the one
/// this service mints a registration with: a registration is created in the account's
/// registration service as well as from a brand profile here, so an identifier this service
/// did not mint is still an identifier — it simply names no registration this state holds,
/// which is a missing resource rather than a bad request. A value not written in that form
/// can name a registration in no state at all, and is refused as input.
fn is_registration_id(candidate: &str) -> bool {
    candidate.len() <= REGISTRATION_ID_MAX_LENGTH
        && candidate.split_once('-').is_some_and(|(prefix, body)| {
            !prefix.is_empty()
                && prefix.bytes().all(|byte| byte.is_ascii_alphanumeric())
                && !body.is_empty()
                && body.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
}

/// The `registrationId` a well-formed `RegistrationIdOrArn` names, whichever of the two
/// forms the shape documents it took, or `None` for a value that is neither.
///
/// A bare identifier names itself; an ARN of this service's `registration` resource names
/// the identifier in its resource field. The ARN form carries the same bare identifier, so
/// a value is well formed only when the identifier it names is. Whether that identifier
/// reaches a stored registration is a separate question, asked after this one.
pub fn registration_id_of(id_or_arn: &str) -> Option<&str> {
    if is_registration_id(id_or_arn) {
        return Some(id_or_arn);
    }
    let id = super::resource_id_from_arn(id_or_arn, REGISTRATION_RESOURCE)?;
    is_registration_id(id).then_some(id)
}

/// `ValidationException` for a `RegistrationIdOrArn` that is not an identifier of a
/// registration at all — neither a bare `registrationId` nor the ARN of one.
///
/// Such a value can name no registration in any state, so it is a refusal about the input
/// rather than about a missing resource. This API states it as a sentence about the member
/// the value arrived on — the identifier reaches different operations on different members —
/// and echoes no value of the caller's: the whole of "must be a valid registration
/// identifier" is this refusal's fixed text, and the member's own name, as the model spells
/// it, is the only variable part. `fieldList` names that member the way this service's own
/// refusals name a top-level member at fault, by its own name, as the identifier gates
/// beside it do ([`super::invalid_brand_profile_id`]), and states that one sentence about
/// it: the gate has one thing to say about the member it refused.
pub fn invalid_registration_id(member: &str) -> crate::ops::types::ValidationException {
    let stated = format!("{member} must be a valid registration identifier");
    crate::ops::types::ValidationException {
        message: stated.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: member.to_string(),
            message: stated,
        }]),
    }
}

/// `ResourceNotFoundException` for a `RegistrationIdOrArn` that names no stored
/// registration. The message is built from the request, so it names the value given.
pub fn registration_not_found(id_or_arn: &str) -> crate::ops::types::ResourceNotFoundException {
    super::resource_not_found(REGISTRATION_RESOURCE, id_or_arn)
}

/// The ARN of a registration in `region`.
pub fn registration_arn(region: &str, registration_id: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_NAMESPACE,
        region,
        &format!("{REGISTRATION_RESOURCE}/{registration_id}"),
    )
}

/// Whether the brand-profile attribute named `attribute_name` takes part in a mapping
/// between a profile and a registration done with `smart_match`.
///
/// `smartMatch` is "semantic field mapping between brand profile attributes and
/// registration fields. The default is true. When false, the service maps fields using a
/// fixed set of standard field types": with it on, every attribute of the profile maps,
/// including one a caller added; with it off, only the service's own standard fields do,
/// which are the attribute slots every profile is seeded with
/// ([`super::DEFAULT_BRAND_PROFILE_ATTRIBUTES`]).
#[must_use]
pub fn maps_attribute(smart_match: bool, attribute_name: &str) -> bool {
    smart_match
        || super::DEFAULT_BRAND_PROFILE_ATTRIBUTES
            .iter()
            .any(|slot| slot.name == attribute_name)
}

/// The fields a registration created from `profile` is prefilled with: the value of every
/// attribute that both maps under `smart_match` and has one.
///
/// An attribute holding no value prefills nothing — no value is invented for one — and an
/// `IMAGE` or `DOCUMENT` attribute holds uploaded media rather than an inline value, so it
/// carries no field value either.
#[must_use]
pub fn registration_fields(
    profile: &super::BrandProfile,
    smart_match: bool,
) -> BTreeMap<String, String> {
    profile
        .attributes
        .values()
        .filter(|attribute| maps_attribute(smart_match, &attribute.name))
        .filter_map(|attribute| {
            attribute
                .value
                .as_ref()
                .map(|value| (attribute.name.clone(), value.clone()))
        })
        .collect()
}

/// The modelled `RegistrationAssociationSummary` a list reports, read back off a stored
/// registration, so one registration has one wire form whichever operation reports it.
pub fn registration_association_summary(
    registration: &Registration,
) -> crate::ops::types::RegistrationAssociationSummary {
    crate::ops::types::RegistrationAssociationSummary {
        registration_id: registration.id.clone(),
        registration_type: registration.registration_type.clone(),
        created_at: super::timestamp(registration.created_at),
        smart_match_used: registration.smart_match_used,
    }
}

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// Mint a fresh, unused `registrationId`.
    ///
    /// The form is the one this API's own example writes — the resource prefix
    /// ([`REGISTRATION_ID_PREFIX`]) plus a body of [`super::MINTED_ID_BODY_LENGTH`]
    /// characters drawn from fresh entropy ([`super::minted_id`]) — and it satisfies
    /// `RegistrationId`'s `@length(1..256)` and `^[A-Za-z0-9_-]+$` pattern. The loop
    /// re-draws rather than ever handing back an id already in use.
    pub fn mint_registration_id(&mut self) -> String {
        loop {
            let id = super::minted_id(REGISTRATION_ID_PREFIX);
            if !self.registrations.contains_key(&id) {
                return id;
            }
        }
    }

    /// Store `registration`, dropping the oldest registration first if that would push the
    /// store past [`MAX_REGISTRATIONS`].
    ///
    /// The ceiling is an invariant of the map rather than of its callers, so no number of
    /// asynchronous calls can grow the emulator's heap without bound.
    pub fn record_registration(&mut self, registration: Registration) {
        while self.registrations.len() >= MAX_REGISTRATIONS {
            let oldest = self
                .registrations
                .values()
                .min_by_key(|stored| (stored.created_at, stored.id.clone()))
                .map(|stored| stored.id.clone());
            match oldest {
                Some(id) => {
                    self.registrations.remove(&id);
                }
                None => break,
            }
        }
        self.registrations
            .insert(registration.id.clone(), registration);
    }

    /// Resolve a `RegistrationIdOrArn` to the id of a stored registration.
    ///
    /// The shape accepts either a bare id or the registration's full ARN, so every
    /// operation taking one resolves both. The two ways a value reaches no registration are
    /// different failures, so they are told apart before any lookup: a value that is not an
    /// identifier of this resource at all cannot name a registration in any state, and is
    /// refused as invalid input against the `member` it arrived on; an identifier of the
    /// documented form that this state holds no record for is refused as not found.
    pub fn resolve_registration_id(
        &self,
        member: &str,
        id_or_arn: &str,
    ) -> Result<&str, super::UnreachableResource> {
        let Some(id) = registration_id_of(id_or_arn) else {
            return Err(super::UnreachableResource::Invalid(invalid_registration_id(
                member,
            )));
        };
        self.registrations
            .get_key_value(id)
            .map(|(id, _)| id.as_str())
            .ok_or_else(|| super::UnreachableResource::NotFound(registration_not_found(id_or_arn)))
    }

    /// Import the fields of the registration `registration_id` into the attributes of the
    /// brand profile `brand_profile_id`, as of `at`, and report what that produced.
    ///
    /// This is the work of an `UpdateBrandProfileFromRegistration` job, performed when the
    /// clock makes that job's transition due rather than when the call that started it
    /// returned, because the operation "runs asynchronously": the profile changes when the
    /// job finishes.
    ///
    /// Every field the registration carries that maps under `smart_match` is written to the
    /// profile's attribute of that name, which is created when the profile holds none;
    /// `replace_existing_values` is what `onAttributeConflict` chose — `REPLACE` overwrites
    /// a value already there, `PRESERVE` keeps it. A resource the job cannot reach, and a
    /// profile that would be pushed past the ceiling on how many attributes one profile
    /// holds, are reported as the job having FAILED rather than as a partly applied import.
    pub(crate) fn import_registration_into_brand_profile(
        &mut self,
        brand_profile_id: &str,
        registration_id: &str,
        replace_existing_values: bool,
        smart_match: bool,
        at: SystemTime,
    ) -> super::JobOutcome {
        let Some(registration) = self.registrations.get(registration_id) else {
            return super::JobOutcome::failed(
                super::RESOURCE_NOT_FOUND_JOB_ERROR_CODE,
                &registration_not_found(registration_id).message,
            );
        };
        // Read off the registration before the profile is borrowed for writing, and
        // filtered here, so the mapping rule is applied once whatever the profile holds.
        let imported: Vec<(String, String)> = registration
            .fields
            .iter()
            .filter(|(name, _)| maps_attribute(smart_match, name))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();

        let Some(profile) = self.brand_profiles.get_mut(brand_profile_id) else {
            return super::JobOutcome::failed(
                super::RESOURCE_NOT_FOUND_JOB_ERROR_CODE,
                &super::brand_profile_not_found(brand_profile_id).message,
            );
        };
        // How many attributes the import would have to create, judged before any of them
        // is: an import is applied whole or not at all, so a profile is never left holding
        // some of a registration's fields.
        let created = imported
            .iter()
            .filter(|(name, _)| !profile.attributes.contains_key(name))
            .count();
        if profile.attributes.len() + created > super::MAX_BRAND_PROFILE_ATTRIBUTES {
            return super::JobOutcome::failed(
                super::SERVICE_QUOTA_EXCEEDED_JOB_ERROR_CODE,
                &super::brand_profile_attribute_ceiling_exceeded(),
            );
        }

        let mut changed = false;
        for (name, value) in imported {
            match profile.attributes.get_mut(&name) {
                Some(attribute) => {
                    // An `IMAGE` or `DOCUMENT` attribute's content is uploaded media rather
                    // than an inline value, so a registration field — which is text — can
                    // fill none of one, and the media it holds is left exactly as it was.
                    if super::carries_media(&attribute.attribute_type) {
                        continue;
                    }
                    // `PRESERVE` keeps the value that is already there; a value that is
                    // already the one being imported is no change either, so neither case
                    // reports the attribute as updated.
                    if attribute.value.is_some() && !replace_existing_values {
                        continue;
                    }
                    if attribute.value.as_deref() == Some(value.as_str()) {
                        continue;
                    }
                    attribute.value = Some(value);
                    attribute.updated_at = at;
                    changed = true;
                }
                None => {
                    profile.attributes.insert(
                        name.clone(),
                        super::BrandProfileAttribute {
                            name,
                            // A registration field is a text value, which is what a `TEXT`
                            // attribute stores.
                            attribute_type: super::TEXT_ATTRIBUTE_TYPE.to_string(),
                            value: Some(value),
                            // Nothing but the value arrives with an imported field: no
                            // description or category is invented for one, and an imported
                            // attribute holds no media.
                            description: None,
                            category: None,
                            media: None,
                            created_at: at,
                            updated_at: at,
                        },
                    );
                    changed = true;
                }
            }
        }
        // The profile is reported as updated only when the import actually changed it, and
        // as of the instant the job's work came due.
        if changed {
            profile.updated_at = at;
        }
        super::JobOutcome::succeeded(vec![crate::ops::types::JobResource {
            resource_type: super::BRAND_PROFILE_JOB_RESOURCE_TYPE.to_string(),
            resource_id: profile.id.clone(),
            resource_arn: profile.arn.clone(),
        }])
    }
}
