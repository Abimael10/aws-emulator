// SPDX-License-Identifier: CC0-1.0

//! The attributes a brand profile holds, and the default attribute template every
//! created profile is seeded with.
//!
//! `CreateBrandProfile` reports `attributesCreated`, "the number of default
//! attributes that were created for the brand profile", so a profile is minted with
//! a template of attribute slots already in place and `ListBrandProfileAttributes`
//! pages exactly those entries back. The template lives here, next to the record it
//! fills, so the create path and every reader of an attribute agree on one
//! rendering of one stored attribute.

use std::collections::BTreeMap;
use std::time::SystemTime;

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
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
}

/// One slot of the default attribute template: the attribute's name and the type
/// of value it takes.
#[derive(Debug, Clone, Copy)]
pub struct DefaultBrandProfileAttribute {
    /// The seeded `attributeName`, within `BrandProfileAttributeName`'s
    /// `@length(1..256)` and `^[A-Za-z0-9_ -]*[A-Za-z0-9_-][A-Za-z0-9_ -]*$` pattern.
    pub name: &'static str,
    /// The seeded `attributeType`, one of
    /// [`crate::ops::types::BRAND_PROFILE_ATTRIBUTE_TYPE_VALUES`].
    pub attribute_type: &'static str,
}

/// The default attribute template seeded into every brand profile this service
/// creates.
///
/// One slot per kind of brand identity information `CreateBrandProfile` documents a
/// profile as holding — company information, addresses, compliance documents and
/// logos — typed as that documentation types them: an inline `TEXT` value for the
/// company details, and uploaded media for the document and the logo
/// (`BrandProfileAttributeType` documents `IMAGE`/`DOCUMENT` as the uploaded-media
/// types). Each slot is created empty: the template is the set of names a profile
/// starts with, and a value arrives with the request that sets one.
pub const DEFAULT_BRAND_PROFILE_ATTRIBUTES: &[DefaultBrandProfileAttribute] = &[
    DefaultBrandProfileAttribute {
        name: "CompanyName",
        attribute_type: "TEXT",
    },
    DefaultBrandProfileAttribute {
        name: "CompanyAddress",
        attribute_type: "TEXT",
    },
    DefaultBrandProfileAttribute {
        name: "ComplianceDocument",
        attribute_type: "DOCUMENT",
    },
    DefaultBrandProfileAttribute {
        name: "BrandLogo",
        attribute_type: "IMAGE",
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
                    category: None,
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
