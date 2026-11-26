// SPDX-License-Identifier: CC0-1.0

//! The shapes a hosted-zone projection is answered with: the `HostedZoneConfig` it
//! carries, the `HostedZoneFeatures` beside it, and the per-guardrail entry inside that.
//!
//! WHY THIS MODULE EXISTS. `#[smithy_macros::smithy(shape = ...)]` derives a structure's
//! `Schema`, its serializer and its deserializer from the on-disk model's member list, and
//! it admits EXACTLY that list: a Rust field with no member behind it is a compile error,
//! so a member the service really answers with that the on-disk model omits cannot be
//! declared through the derive at all. A hosted-zone projection carries two such members —
//! the zone's TIER inside `Config` and the zone's per-guardrail statuses inside `Features`
//! — so the shapes that carry them are written out here the way the derive writes one: one
//! member schema per member (index = that member's position in the member list), a
//! `SCHEMA` naming them in that order, and the `SerializableStruct` /
//! `DeserializableStruct` pair. The model's own members keep the order and the indices the
//! model gives them, and a member the model does not declare is appended LAST, so a later
//! model refresh that declares one only has to move it into place and put the derive back.
//!
//! The REQUEST's config keeps the derive. `HostedZoneConfig` is a member of
//! `CreateHostedZoneRequest` as well as of every answer, and the derive is what captures
//! the `@length` on its `Comment` for the constraint walk that runs on an operation's
//! input — so the projection's config is its own, answer-only type
//! ([`ProjectedHostedZoneConfig`]) and the request keeps
//! [`super::shared_types::HostedZoneConfig`] exactly as the model declares it.
//!
//! WHAT IS DELIBERATELY ABSENT, because the derive's own output would be inert here too:
//!
//! * Constraint traits (`@length`, `@pattern`, `@range`). The walk that enforces them runs
//!   on operation INPUTS only, and every shape in this module is answered, never read from
//!   a request.
//! * The `@http` chain, which the derive bakes on only when the shape is an operation's
//!   INPUT.
//! * `@xmlName`, on the shapes and on every member of them: each one's wire element IS its
//!   member name. The one exception is the element name of a LIST's items, which is a
//!   different name from the member's own and travels, as in the derive's output, on the
//!   member's `XmlListTrait`.

use super::*;

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use ::aws_smithy_schema_server::xml::XmlListTrait;
use ::aws_smithy_schema_server::DeserializableStruct;
use ::std::sync::LazyLock;

/// The namespace every shape of this service is declared in.
const NAMESPACE: &str = "com.amazonaws.route53";

/// The element one entry of a zone's change-guardrail set is serialised as, inside the
/// `ChangeGuardrails` container that holds them.
const CHANGE_GUARDRAIL_ELEMENT: &str = "ChangeGuardrail";

/// The `HostedZoneConfig` a hosted-zone projection carries: the `Comment` and
/// `PrivateZone` the model declares, plus the `HostedZoneTier` the answer carries that the
/// on-disk model does not declare (see this module's own docs).
///
/// Answer-only: a request's `HostedZoneConfig` is
/// [`super::shared_types::HostedZoneConfig`], which keeps the derive and with it the
/// `Comment` length bound the constraint walk reads.
#[derive(Debug, Clone, Default)]
pub struct ProjectedHostedZoneConfig {
    /// `HostedZoneConfig$Comment`, reported only for a zone that holds one.
    pub comment: Option<String>,
    /// `HostedZoneConfig$PrivateZone`, a boolean with `@default false`, so every zone has
    /// one to report.
    pub private_zone: bool,
    /// The tier the zone is in, for a zone that has one.
    pub hosted_zone_tier: Option<String>,
}

#[allow(non_upper_case_globals)]
impl ProjectedHostedZoneConfig {
    const M_COMMENT: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneConfig$Comment",
            NAMESPACE,
            "Comment",
        ),
        ShapeType::String,
        "Comment",
        0,
    );

    const M_PRIVATE_ZONE: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneConfig$PrivateZone",
            NAMESPACE,
            "PrivateZone",
        ),
        ShapeType::Boolean,
        "PrivateZone",
        1,
    );

    const M_HOSTED_ZONE_TIER: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneConfig$HostedZoneTier",
            NAMESPACE,
            "HostedZoneTier",
        ),
        ShapeType::String,
        "HostedZoneTier",
        2,
    );

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneConfig",
            NAMESPACE,
            "HostedZoneConfig",
        ),
        ShapeType::Structure,
        &[
            &Self::M_COMMENT,
            &Self::M_PRIVATE_ZONE,
            &Self::M_HOSTED_ZONE_TIER,
        ],
    );
}

impl SerializableStruct for ProjectedHostedZoneConfig {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(comment) = self.comment.as_ref() {
            s.write_string(&Self::M_COMMENT, comment.as_str())?;
        }
        s.write_boolean(&Self::M_PRIVATE_ZONE, self.private_zone)?;
        if let Some(tier) = self.hosted_zone_tier.as_ref() {
            s.write_string(&Self::M_HOSTED_ZONE_TIER, tier.as_str())?;
        }
        Ok(())
    }
}

impl DeserializableStruct for ProjectedHostedZoneConfig {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> Result<Self, SerdeError> {
        let mut comment = None;
        let mut private_zone = None;
        let mut hosted_zone_tier = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => comment = Some(d.read_string(member)?),
                Some(1) => private_zone = Some(d.read_boolean(member)?),
                Some(2) => hosted_zone_tier = Some(d.read_string(member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            comment,
            // `@default false`: a document that leaves the member out means the default,
            // exactly as the derive reads one.
            private_zone: private_zone.unwrap_or_default(),
            hosted_zone_tier,
        })
    }
}

/// The traits the `ChangeGuardrails` member's schema carries: the XML layout of the list
/// it targets — wrapped, with each entry serialised as [`CHANGE_GUARDRAIL_ELEMENT`] — which
/// is where the restXml codec reads an item's element name from.
static M_CHANGE_GUARDRAILS_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(XmlListTrait::new(
        false,
        Some(CHANGE_GUARDRAIL_ELEMENT),
    )));
    traits
});

/// The `HostedZoneFeatures` a hosted-zone projection carries: the three members the model
/// declares, plus the `ChangeGuardrails` set the answer carries that the on-disk model does
/// not declare (see this module's own docs).
#[derive(Debug, Clone, Default)]
pub struct HostedZoneFeatures {
    /// `HostedZoneFeatures$AcceleratedRecoveryStatus`.
    pub accelerated_recovery_status: Option<String>,
    /// `HostedZoneFeatures$FailureReasons`.
    pub failure_reasons: Option<HostedZoneFailureReasons>,
    /// `HostedZoneFeatures$PublicDNSFallbackStatus`.
    pub public_dns_fallback_status: Option<String>,
    /// The status of each change guardrail the zone has one for, in the order Route 53
    /// publishes the guardrails in.
    pub change_guardrails: Option<Vec<ChangeGuardrail>>,
}

#[allow(non_upper_case_globals)]
impl HostedZoneFeatures {
    const M_ACCELERATED_RECOVERY_STATUS: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneFeatures$AcceleratedRecoveryStatus",
            NAMESPACE,
            "AcceleratedRecoveryStatus",
        ),
        ShapeType::String,
        "AcceleratedRecoveryStatus",
        0,
    );

    const M_FAILURE_REASONS: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneFeatures$FailureReasons",
            NAMESPACE,
            "FailureReasons",
        ),
        ShapeType::Structure,
        "FailureReasons",
        1,
    );

    const M_PUBLIC_DNS_FALLBACK_STATUS: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneFeatures$PublicDNSFallbackStatus",
            NAMESPACE,
            "PublicDNSFallbackStatus",
        ),
        ShapeType::String,
        "PublicDNSFallbackStatus",
        2,
    );

    const M_CHANGE_GUARDRAILS: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneFeatures$ChangeGuardrails",
            NAMESPACE,
            "ChangeGuardrails",
        ),
        ShapeType::List,
        "ChangeGuardrails",
        3,
    )
    .with_traits(&M_CHANGE_GUARDRAILS_TRAITS);

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.route53#HostedZoneFeatures",
            NAMESPACE,
            "HostedZoneFeatures",
        ),
        ShapeType::Structure,
        &[
            &Self::M_ACCELERATED_RECOVERY_STATUS,
            &Self::M_FAILURE_REASONS,
            &Self::M_PUBLIC_DNS_FALLBACK_STATUS,
            &Self::M_CHANGE_GUARDRAILS,
        ],
    );
}

impl SerializableStruct for HostedZoneFeatures {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        if let Some(status) = self.accelerated_recovery_status.as_ref() {
            s.write_string(&Self::M_ACCELERATED_RECOVERY_STATUS, status.as_str())?;
        }
        if let Some(reasons) = self.failure_reasons.as_ref() {
            s.write_struct(&Self::M_FAILURE_REASONS, reasons)?;
        }
        if let Some(status) = self.public_dns_fallback_status.as_ref() {
            s.write_string(&Self::M_PUBLIC_DNS_FALLBACK_STATUS, status.as_str())?;
        }
        if let Some(guardrails) = self.change_guardrails.as_ref() {
            s.write_list(&Self::M_CHANGE_GUARDRAILS, &|list| {
                for guardrail in guardrails.iter() {
                    list.write_struct(&ChangeGuardrail::SCHEMA, guardrail)?;
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}

impl DeserializableStruct for HostedZoneFeatures {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> Result<Self, SerdeError> {
        let mut accelerated_recovery_status = None;
        let mut failure_reasons = None;
        let mut public_dns_fallback_status = None;
        let mut change_guardrails = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => accelerated_recovery_status = Some(d.read_string(member)?),
                Some(1) => {
                    failure_reasons =
                        Some(<HostedZoneFailureReasons as DeserializableStruct>::deserialize(d)?);
                }
                Some(2) => public_dns_fallback_status = Some(d.read_string(member)?),
                Some(3) => {
                    let mut read: Vec<ChangeGuardrail> = Vec::new();
                    d.read_list(member, &mut |item| {
                        read.push(<ChangeGuardrail as DeserializableStruct>::deserialize(item)?);
                        Ok(())
                    })?;
                    change_guardrails = Some(read);
                }
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            accelerated_recovery_status,
            failure_reasons,
            public_dns_fallback_status,
            change_guardrails,
        })
    }
}

/// One change guardrail's status on a hosted zone, as the `ChangeGuardrails` container
/// inside `Features` carries it: the guardrail's `Name` and its `Status` on that zone.
#[derive(Debug, Clone, Default)]
pub struct ChangeGuardrail {
    /// The guardrail's name.
    pub name: String,
    /// The guardrail's status on the zone the container belongs to.
    pub status: String,
}

#[allow(non_upper_case_globals)]
impl ChangeGuardrail {
    const M_NAME: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#ChangeGuardrail$Name",
            NAMESPACE,
            "Name",
        ),
        ShapeType::String,
        "Name",
        0,
    );

    const M_STATUS: Schema = Schema::new_member(
        ShapeId::from_static(
            "com.amazonaws.route53#ChangeGuardrail$Status",
            NAMESPACE,
            "Status",
        ),
        ShapeType::String,
        "Status",
        1,
    );

    pub const SCHEMA: Schema = Schema::new_struct(
        ShapeId::from_static(
            "com.amazonaws.route53#ChangeGuardrail",
            NAMESPACE,
            "ChangeGuardrail",
        ),
        ShapeType::Structure,
        &[&Self::M_NAME, &Self::M_STATUS],
    );
}

impl SerializableStruct for ChangeGuardrail {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&Self::M_NAME, self.name.as_str())?;
        s.write_string(&Self::M_STATUS, self.status.as_str())?;
        Ok(())
    }
}

impl DeserializableStruct for ChangeGuardrail {
    fn deserialize(
        d: &mut dyn ::aws_smithy_schema::serde::ShapeDeserializer,
    ) -> Result<Self, SerdeError> {
        let mut name = None;
        let mut status = None;
        d.read_struct(&Self::SCHEMA, &mut |member, d| {
            match member.member_index() {
                Some(0) => name = Some(d.read_string(member)?),
                Some(1) => status = Some(d.read_string(member)?),
                _ => {}
            }
            Ok(())
        })?;
        Ok(Self {
            name: name.ok_or_else(|| ::aws_smithy_schema_server::missing_member("Name"))?,
            status: status.ok_or_else(|| ::aws_smithy_schema_server::missing_member("Status"))?,
        })
    }
}
