// SPDX-License-Identifier: CC0-1.0

//! The attributes a brand profile holds, the default attribute template every
//! created profile is seeded with, and where the media of one is downloaded from.
//!
//! `CreateBrandProfile` reports `attributesCreated`, "the number of default
//! attributes that were created for the brand profile", so a profile is minted with
//! a template of attribute slots already in place and `ListBrandProfileAttributes`
//! pages exactly those entries back. The template lives here, next to the record it
//! fills, so the create path and every reader of an attribute agree on one
//! rendering of one stored attribute.
//!
//! An `IMAGE` or `DOCUMENT` attribute also holds MEDIA, which a caller downloads
//! rather than reads out of a response: `mediaDownloadUrl` is "a presigned Amazon S3
//! URL that you can use to download the attribute media", and the location this
//! emulator keeps that media at is this emulator itself. The address is therefore
//! built and read back here, in one place ([`attribute_media_path`] and
//! [`parse_attribute_media_path`]), so the URL an operation hands out and the request
//! `crate::service`'s media plane serves cannot disagree about which attribute it
//! names.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// One brand-profile attribute, keyed in [`super::BrandProfile::attributes`] by its
/// name (`BrandProfileAttributeName` is unique within a profile).
#[derive(Debug, Clone)]
pub struct BrandProfileAttribute {
    /// `attributeName` — unique within the owning brand profile.
    pub name: String,
    /// `attributeType`, one of [`crate::ops::types::BRAND_PROFILE_ATTRIBUTE_TYPE_VALUES`].
    pub attribute_type: String,
    /// `attributeValue` for a `TEXT` attribute; `None` for an attribute carrying no
    /// value yet and for uploaded media.
    pub value: Option<String>,
    /// `description`, absent until a request supplies one.
    pub description: Option<String>,
    /// `category`, absent until a request supplies one.
    pub category: Option<String>,
    /// The media an `IMAGE` or `DOCUMENT` attribute holds, as the request that
    /// supplied it through `attachmentBody` sent it. `None` for a `TEXT` attribute,
    /// which carries its value inline, and for a media attribute nothing has
    /// supplied media for yet: no bytes are invented for one.
    pub media: Option<BrandProfileAttributeMedia>,
    /// The `clientToken` of the request that created this attribute, when it carried
    /// one. Kept on the record rather than in a map beside it, so the token lives and
    /// dies with the attribute it names and is bounded by the same ceiling.
    pub client_token: Option<String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// The media one attribute holds: the uploaded bytes and the media type they
/// announce.
#[derive(Debug, Clone)]
pub struct BrandProfileAttributeMedia {
    /// The `attachmentBody` exactly as it arrived, so the download serves the very
    /// bytes that were uploaded.
    pub bytes: Vec<u8>,
    /// `mediaContentType`, "The MIME content type of the attribute media": read from
    /// the bytes themselves, because the request declares no type for them.
    pub content_type: String,
}

impl BrandProfileAttributeMedia {
    /// The media `bytes` make up, under the media type they announce.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        let content_type = media_content_type(&bytes).to_string();
        Self {
            bytes,
            content_type,
        }
    }

    /// `mediaSizeBytes`, "The size of the attribute media, in bytes".
    #[must_use]
    pub fn size_bytes(&self) -> i64 {
        i64::try_from(self.bytes.len()).unwrap_or(i64::MAX)
    }
}

/// The media type `bytes` announce in their own leading bytes.
///
/// `mediaContentType` is reported for media no request declares a type for — the
/// input carries `attachmentBody` and nothing beside it — so the type is read from
/// the content's own magic-number signature by the `infer` crate, which matches the
/// published signature of each format it knows rather than guessing from a name.
/// Bytes announcing no format at all are reported as unnamed binary rather than as a
/// format they are not.
fn media_content_type(bytes: &[u8]) -> &'static str {
    ::infer::get(bytes)
        .map(|kind| kind.mime_type())
        .unwrap_or(::service_common::web_media_type::GENERIC_MEDIA_TYPE)
}

/// The `attributeType` values whose content is uploaded MEDIA rather than an inline
/// value: `BrandProfileAttributeType` documents `IMAGE` and `DOCUMENT` as the types
/// that "store binary media that you upload", and `TEXT` as the one that "stores an
/// inline value".
pub const MEDIA_ATTRIBUTE_TYPES: [&str; 2] = ["IMAGE", "DOCUMENT"];

/// Whether an attribute of `attribute_type` carries uploaded media
/// ([`MEDIA_ATTRIBUTE_TYPES`]), which is what decides both where its content comes
/// from — `attachmentBody` rather than `attributeValue` — and whether a
/// `mediaDownloadUrl` is reported for it.
#[must_use]
pub fn carries_media(attribute_type: &str) -> bool {
    MEDIA_ATTRIBUTE_TYPES.contains(&attribute_type)
}

/// The most attributes this emulator holds for one brand profile: the modelled
/// `@length` ceiling of `ListBrandProfileAttributes`' own
/// `brandProfileAttributes` list (0..100), which is as many as a reader of a profile's
/// attributes can page back. `CreateBrandProfileAttributes` models
/// `ServiceQuotaExceededException`, which is the refusal once a profile is full.
pub const MAX_BRAND_PROFILE_ATTRIBUTES: usize = 100;

/// The most attribute media this emulator holds across the whole store, in bytes.
///
/// A ceiling, not a guess at AWS's published quota: `AttachmentBody` admits 5 MiB per
/// attribute, so it exists so a create loop cannot grow the emulator's heap without
/// bound. A create that would push the store past it is refused with
/// `ServiceQuotaExceededException`.
pub const MAX_ATTRIBUTE_MEDIA_BYTES: usize = 64 * 1024 * 1024;

/// How long a `mediaDownloadUrl` stays valid: `MediaDownloadUrl` documents it as "A
/// presigned Amazon S3 URL that you can use to download the attribute media. The URL
/// is valid for one hour".
pub const MEDIA_DOWNLOAD_URL_EXPIRY: Duration = Duration::from_secs(60 * 60);

/// The `resourceType` an attribute error reports, in the hyphenated resource
/// vocabulary this API names its resources by — the same vocabulary
/// [`super::BRAND_PROFILE_RESOURCE_TYPE`] is spelled in, with the attribute as the
/// resource rather than the profile that holds it.
pub const BRAND_PROFILE_ATTRIBUTE_RESOURCE_TYPE: &str = "brand-profile-attribute";

/// The conflict `Reason` token for a create refused because the name it asks for is
/// already taken: `attributeName` "is unique within a brand profile", so the state
/// that causes the conflict is that the resource already exists.
pub const RESOURCE_ALREADY_EXISTS_REASON: &str = "RESOURCE_ALREADY_EXISTS";

/// `ResourceNotFoundException` for a `BrandProfileAttributeName` the addressed
/// profile holds no attribute under.
///
/// The name is reported in the `resourceId` member and NOT in the message:
/// `BrandProfileAttributeName` is `@sensitive`, so the refusal states the value in the
/// member modelled for it rather than in its own text
/// ([`super::resource_not_found_of_sensitive_id`]).
#[must_use]
pub fn brand_profile_attribute_not_found(
    attribute_name: &str,
) -> crate::ops::types::ResourceNotFoundException {
    super::resource_not_found_of_sensitive_id(
        BRAND_PROFILE_ATTRIBUTE_RESOURCE_TYPE,
        attribute_name,
    )
}

/// `ConflictException` for a create of an attribute whose name is already taken
/// within the profile.
#[must_use]
pub fn brand_profile_attribute_exists_conflict(
    attribute_name: &str,
) -> crate::ops::types::ConflictException {
    super::resource_conflict(
        RESOURCE_ALREADY_EXISTS_REASON,
        BRAND_PROFILE_ATTRIBUTE_RESOURCE_TYPE,
        attribute_name,
    )
}

/// The `category` values this service supports.
///
/// `BrandProfileAttributeCategory` is modelled as a plain 1..64 string and documented
/// "Category for the attribute. Validated against supported values at runtime", so the
/// supported set is not in the model: it is the set the service answers for, listed in
/// the order its own refusal states it. A value outside it is refused
/// ([`invalid_category`]) rather than stored, so no read reports a category the service
/// cannot hold.
pub const SUPPORTED_ATTRIBUTE_CATEGORIES: [&str; 11] = [
    "BUSINESS_IDENTITY",
    "BUSINESS_ADDRESS",
    "BUSINESS_CONTACT",
    "MESSAGING_USE_CASE",
    "OPT_IN_CONSENT",
    "MESSAGE_CONTENT",
    "COMPLIANCE_KEYWORDS",
    "TERMS_AND_POLICY",
    "BRAND_DISPLAY",
    "SUPPORTING_DOCUMENT",
    "OTHER",
];

/// Whether `category` is one of [`SUPPORTED_ATTRIBUTE_CATEGORIES`].
///
/// Compared exactly: the supported values are spelled one way, so a differently cased
/// spelling of one of them is not a supported value.
#[must_use]
pub fn is_supported_attribute_category(category: &str) -> bool {
    SUPPORTED_ATTRIBUTE_CATEGORIES.contains(&category)
}

/// `ValidationException` for a `category` outside [`SUPPORTED_ATTRIBUTE_CATEGORIES`],
/// reported against the member at `pointer` — the JSONPointer of the `category` member
/// in the request document, which is what `ValidationExceptionField$path` documents.
///
/// The fixed text is the `Invalid category '…'. Supported values: …` sentence over the
/// supported set; its one variable part is the rejected value, taken from the request.
/// The `fieldList` entry states the same sentence about the member it names, so a
/// caller sending a list of attributes is told which entry to change.
#[must_use]
pub fn invalid_category(pointer: &str, category: &str) -> crate::ops::types::ValidationException {
    let message = format!(
        "Invalid category '{category}'. Supported values: {}",
        SUPPORTED_ATTRIBUTE_CATEGORIES.join(", ")
    );
    crate::ops::types::ValidationException {
        message: message.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: pointer.to_string(),
            message,
        }]),
    }
}

/// `ValidationException` for a request supplying `member` for an attribute whose
/// `attribute_type` does not take it, reported against the member at `pointer`.
///
/// Which member carries an attribute's content follows from its type: `attachmentBody`
/// is "The binary content for an attribute of type IMAGE or DOCUMENT" and
/// `attributeValue` "applies to attributes of type TEXT. For attributes of type IMAGE or
/// DOCUMENT, provide the media through the attachment body instead". The fixed text is
/// the `… cannot be provided for … attribute` sentence; its variable parts are the
/// member the request supplied and the stored attribute's own type.
#[must_use]
pub fn member_not_for_attribute_type(
    pointer: &str,
    member: &str,
    attribute_type: &str,
) -> crate::ops::types::ValidationException {
    let message = format!("{member} cannot be provided for {attribute_type} attribute");
    crate::ops::types::ValidationException {
        message: message.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: pointer.to_string(),
            message,
        }]),
    }
}

/// The sentence a request is refused with when it would push the store past
/// [`MAX_ATTRIBUTE_MEDIA_BYTES`]: one wording of this emulator's own ceiling, whichever
/// operation would have crossed it.
#[must_use]
pub fn attribute_media_ceiling_exceeded() -> String {
    format!(
        "The maximum amount of attribute media ({MAX_ATTRIBUTE_MEDIA_BYTES} bytes) would be exceeded."
    )
}

/// One slot of the default attribute template: the attribute's name, the type of
/// value it takes and the category it is grouped under.
#[derive(Debug, Clone, Copy)]
pub struct DefaultBrandProfileAttribute {
    /// The seeded `attributeName`, within `BrandProfileAttributeName`'s
    /// `@length(1..256)` and `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern
    /// — which admits the interior spaces these names carry.
    pub name: &'static str,
    /// The seeded `attributeType`, one of
    /// [`crate::ops::types::BRAND_PROFILE_ATTRIBUTE_TYPE_VALUES`].
    pub attribute_type: &'static str,
    /// The seeded `category`, one of [`SUPPORTED_ATTRIBUTE_CATEGORIES`]: a default
    /// attribute is classified by the service that seeded it, so every reader of one
    /// reports the member rather than leaving it unset.
    pub category: &'static str,
}

/// The default attribute template seeded into every brand profile this service
/// creates: the service's own default attribute names, with the type each one takes
/// and the category it is grouped under.
///
/// `CreateBrandProfile`'s `attributesCreated` is "The number of default attributes
/// that were created for the brand profile", so the names here are the service's,
/// not a profile's: a caller addresses a default attribute by its service name right
/// after the create (`GetBrandProfileAttribute`, `UpdateBrandProfileAttribute` and
/// `DeleteBrandProfileAttribute` all key on `attributeName`, and nothing else
/// addresses one). The model does not enumerate the template, so each slot below is
/// a default attribute the service's own `ListBrandProfileAttributes` answer reports
/// for a freshly created profile — its name, its `attributeType` and its `category`
/// as that answer gives them — kept in the order `attributeName` sorts in, which is
/// the order the list reports and the order a `BTreeMap` keyed by name pages.
///
/// Each slot is created empty: the template is the set of names, types and
/// categories a profile starts with, and a value arrives with the request that sets
/// one.
///
/// PARTIAL: only the slots below are evidenced, and the service's own template holds
/// more of them, so a profile created here carries fewer default attributes than the
/// service seeds — which is what `attributesCreated` and the attribute list then
/// report, because both are computed from this template rather than from a count of
/// their own. Completing it is tracked by
/// `issues/.../endusermessaging-createbrandprofile-attributescreated-undercounts-the-service-default-attribute.md`;
/// a slot is added here only with the name, type and category the service's own
/// answer gives it, never invented to reach a count.
pub const DEFAULT_BRAND_PROFILE_ATTRIBUTES: &[DefaultBrandProfileAttribute] = &[
    DefaultBrandProfileAttribute {
        name: "Address Line 2",
        attribute_type: "TEXT",
        category: "BUSINESS_ADDRESS",
    },
    DefaultBrandProfileAttribute {
        name: "Business Type",
        attribute_type: "TEXT",
        category: "BUSINESS_IDENTITY",
    },
    DefaultBrandProfileAttribute {
        name: "Business Vertical",
        attribute_type: "TEXT",
        category: "BUSINESS_IDENTITY",
    },
    DefaultBrandProfileAttribute {
        name: "City",
        attribute_type: "TEXT",
        category: "BUSINESS_ADDRESS",
    },
    DefaultBrandProfileAttribute {
        name: "Company Address",
        attribute_type: "TEXT",
        category: "BUSINESS_ADDRESS",
    },
    DefaultBrandProfileAttribute {
        name: "Company Name",
        attribute_type: "TEXT",
        category: "BUSINESS_IDENTITY",
    },
    DefaultBrandProfileAttribute {
        name: "Contact Email",
        attribute_type: "TEXT",
        category: "BUSINESS_CONTACT",
    },
];

/// The default attribute template, as the attribute map a freshly created profile
/// holds: one entry per slot of [`DEFAULT_BRAND_PROFILE_ATTRIBUTES`], created and
/// last updated at the instant the profile was created.
pub fn default_brand_profile_attributes(
    created_at: SystemTime,
) -> BTreeMap<String, BrandProfileAttribute> {
    DEFAULT_BRAND_PROFILE_ATTRIBUTES
        .iter()
        .map(|slot| {
            (
                slot.name.to_string(),
                BrandProfileAttribute {
                    name: slot.name.to_string(),
                    attribute_type: slot.attribute_type.to_string(),
                    value: None,
                    description: None,
                    category: Some(slot.category.to_string()),
                    media: None,
                    client_token: None,
                    created_at,
                    updated_at: created_at,
                },
            )
        })
        .collect()
}

/// The modelled `BrandProfileAttributeSummary` a list reports, read back off a
/// stored attribute, so one stored attribute has one rendering whichever operation
/// returns it.
pub fn brand_profile_attribute_summary(
    attribute: &BrandProfileAttribute,
) -> crate::ops::types::BrandProfileAttributeSummary {
    crate::ops::types::BrandProfileAttributeSummary {
        attribute_name: attribute.name.clone(),
        attribute_type: attribute.attribute_type.clone(),
        description: attribute.description.clone(),
        category: attribute.category.clone(),
        created_at: super::timestamp(attribute.created_at),
        updated_at: super::timestamp(attribute.updated_at),
    }
}

impl super::ComAmazonawsEndusermessagingEndusermessagingState {
    /// How many bytes of attribute media this store holds, which is what
    /// [`MAX_ATTRIBUTE_MEDIA_BYTES`] bounds.
    ///
    /// Summed over the profiles rather than carried as a counter beside them, so the
    /// figure cannot drift from the media actually held: both maps are bounded, so
    /// the walk is bounded too.
    #[must_use]
    pub fn attribute_media_bytes(&self) -> usize {
        self.brand_profiles
            .values()
            .flat_map(|profile| profile.attributes.values())
            .filter_map(|attribute| attribute.media.as_ref())
            .map(|media| media.bytes.len())
            .sum()
    }

    /// The attribute `attribute_name` of the profile `brand_profile_id` names, when
    /// this store holds both.
    ///
    /// The profile is addressed by its STORED id here rather than by a
    /// `BrandProfileIdOrArn`, because this is the reader the media plane uses and a
    /// media address carries the resolved id.
    #[must_use]
    pub fn brand_profile_attribute(
        &self,
        brand_profile_id: &str,
        attribute_name: &str,
    ) -> Option<&BrandProfileAttribute> {
        self.brand_profiles
            .get(brand_profile_id)?
            .attributes
            .get(attribute_name)
    }
}

/// The plane segment every attribute-media address carries, under the reserved
/// data-plane namespace: `/_aws/brand-profile-attribute-media/…`.
pub const MEDIA_PLANE: &str = "brand-profile-attribute-media";

/// The path one attribute's media is addressed at: the plane, then the profile that
/// holds the attribute and the attribute's own name, DECODED as the model lays both
/// out (the signer encodes them).
///
/// ONE place states the form, so the URL this service hands out and the request its
/// media plane serves back cannot disagree about it (see
/// [`parse_attribute_media_path`]). The profile is named by its stored id, which is
/// minted and so carries none of the separators a `BrandProfileIdOrArn` may.
#[must_use]
pub fn attribute_media_path(brand_profile_id: &str, attribute_name: &str) -> String {
    format!(
        "/{}/{MEDIA_PLANE}/{brand_profile_id}/{attribute_name}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// The profile id and attribute name a media request's decoded path `segments` name,
/// or `None` when they address no attribute media at all.
///
/// Read from the same two parts [`attribute_media_path`] builds the published URL out
/// of. Any signature the URL carries is not part of WHICH media it names, so it plays
/// no part here.
#[must_use]
pub fn parse_attribute_media_path(segments: &[String]) -> Option<(String, String)> {
    let [namespace, plane, brand_profile_id, attribute_name] = segments else {
        return None;
    };
    if namespace != ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
        || plane != MEDIA_PLANE
        || brand_profile_id.is_empty()
        || attribute_name.is_empty()
    {
        return None;
    }
    Some((brand_profile_id.clone(), attribute_name.clone()))
}

/// `mediaDownloadUrl` for one attribute of one profile: where a caller downloads the
/// attribute's media from.
///
/// Signed through the framework's URL signer, which is what makes the URL both a
/// presigned one and one this emulator ANSWERS: it signs for the emulator's current
/// base URL, so the announced host is the host the caller reached this service on
/// rather than a host only AWS serves, and the signature is what authorises the fetch
/// for the one hour [`MEDIA_DOWNLOAD_URL_EXPIRY`] announces.
#[must_use]
pub fn attribute_media_download_url(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    brand_profile_id: &str,
    attribute_name: &str,
) -> String {
    let path = attribute_media_path(brand_profile_id, attribute_name);
    let request = smithy_server_runtime::SignedUrlRequest::new(
        smithy_server_runtime::http::Method::GET,
        ctx.identity().signing_name(),
        &path,
    );
    match ctx.url_signer().sign(request, MEDIA_DOWNLOAD_URL_EXPIRY) {
        Ok(signed) => signed.as_str().to_string(),
        // Nothing about a minted profile id, a modelled attribute name or this
        // emulator's own endpoint makes a request unsignable, so this is unreachable
        // in practice; the media is still served at that address, so the address is
        // what is reported rather than no URL at all. The endpoint is
        // `scheme://host[:port]` with no path of its own, and a trailing separator is
        // dropped so the media's path is the only one the URL carries.
        Err(_) => format!("{}{path}", ctx.endpoint().as_str().trim_end_matches('/')),
    }
}
