// SPDX-License-Identifier: CC0-1.0

//! The `DomainName` a path of an unmodelled route carries, held to the constraints the
//! MODEL declares for a domain name.
//!
//! These routes have no modelled input, so nothing validates their label before they are
//! reached — and a label a modelled route would have refused must not be refused
//! differently here, or two paths naming one domain would disagree about which names exist.
//! So the constraints are READ OFF the model (the `DomainName` member of a modelled
//! operation's generated input schema), the check is the framework's own validating walk,
//! and the words are the ones [`crate::service::validation`] states for every other
//! constraint refusal of this service — which is how the recorded refusal of a malformed
//! name is worded.
//!
//! NOTHING about constraint checking is reimplemented here: the member's own traits are
//! re-declared in the framework's own form on a shadow member schema of the same name, the
//! value is written into it the way a codec writes a string member, and
//! [`validate`] is the walk the request boundary itself runs.

use std::sync::LazyLock;

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use ::aws_smithy_schema_server::constraints::{validate, LengthTrait, PatternTrait};

use super::Refusal;

/// This service's Smithy namespace, for the shadow member schema below.
const NAMESPACE: &str = "com.amazonaws.opensearch";

/// The member the domain name is carried at, as the model names it — the name the refusal's
/// clause states, lowerCamelled by the framework's own path spelling.
const MEMBER: &str = "DomainName";

/// The modelled input the `DomainName` constraints are read off: the operation that binds
/// the same label on the same resource (`GET /2021-01-01/opensearch/domain/{DomainName}`),
/// so what a name of a domain IS comes from the model rather than from a copy here.
static MODELLED_INPUT: &::aws_smithy_schema::Schema =
    crate::ops::types::DESCRIBE_DOMAIN_INPUT_SCHEMA;

/// The shadow member's constraints: the `@pattern` and the `@length` the model declares for
/// that member, and nothing else.
static MEMBER_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    let declared = MODELLED_INPUT
        .members()
        .iter()
        .find(|member| member.member_name() == Some(MEMBER))
        .and_then(|member| member.traits());
    if let Some(pattern) = declared
        .and_then(|declared| declared.get(&PatternTrait::TRAIT_ID))
        .and_then(|declared| declared.as_any().downcast_ref::<PatternTrait>())
    {
        traits.insert(Box::new(PatternTrait::new(pattern.regex)));
    }
    if let Some(length) = declared
        .and_then(|declared| declared.get(&LengthTrait::TRAIT_ID))
        .and_then(|declared| declared.as_any().downcast_ref::<LengthTrait>())
    {
        traits.insert(Box::new(*length));
    }
    traits
});

/// The shadow member the walk reads: its name gives the violation's path and its trait map
/// carries the constraints.
static M_DOMAIN_NAME: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.opensearch#UnmodelledDomainName$DomainName",
        NAMESPACE,
        MEMBER,
    ),
    ShapeType::String,
    MEMBER,
    0,
)
.with_traits(&MEMBER_TRAITS);

/// The root of the shadow input the walk is driven over. Its member list is empty on
/// purpose: the walk visits what [`Label`] WRITES, and a root struct schema carries no
/// member name, so it contributes no path segment.
static LABEL_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.opensearch#UnmodelledDomainName",
        NAMESPACE,
        "UnmodelledDomainName",
    ),
    ShapeType::Structure,
    &[],
);

/// One request's domain-name label, written for the validating walk the way a codec writes a
/// string member.
struct Label<'a> {
    name: &'a str,
}

impl SerializableStruct for Label<'_> {
    fn serialize_members(&self, s: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        s.write_string(&M_DOMAIN_NAME, self.name)
    }
}

/// The refusal a domain name outside the model's constraints for one earns, and `None` for
/// a name inside them.
#[must_use]
pub fn refusal(name: &str) -> Option<Refusal> {
    let violations = validate(&Label { name }, &LABEL_SCHEMA);
    // The clauses, their order and the envelope are the ones every constraint refusal of
    // this service states, including the expression a `@pattern` clause QUOTES — the
    // service's own rendering of the model's text.
    crate::service::validation::stated_report(&violations).map(|refused| Refusal::validation(refused.message))
}
