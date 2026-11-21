// SPDX-License-Identifier: CC0-1.0

//! The resources the tagging trio addresses by ARN, and the `resourceArn` check the
//! published `@pattern` for that member cannot make.
//!
//! `TagResource` (`POST /tags/{resourceArn}`), `ListTagsForResource`
//! (`GET /tags/{resourceArn}`) and `UntagResource` (`DELETE /tags/{resourceArn}`) all name
//! their subject by ARN: `resourceArn` targets `TagArn`, `@length(0..256)` plus
//! `@pattern(^arn:[a-z0-9]{1,10}:profile$)`
//! (`smithy_query(query="operation", operation="TagResource")`). That expression closes
//! with a whole-value anchor immediately after the service segment, and Smithy pattern
//! semantics are a FIND, so it admits only the seven-to-sixteen character string
//! `arn:<partition>:profile` and nothing that continues into a region, an account and a
//! resource — which every ARN of a real Customer Profiles resource does. Enforced as
//! written it refuses the whole tagging surface.
//!
//! So the three inputs carry `skip_validation = ["resourceArn"]` in `src/ops/types/`, and
//! [`check_resource_arn`] holds the member to its modelled `@length` plus the expression
//! THIS MODEL publishes for one of its ARNs — `Arn`, `^arn:([a-z\d-]+):profile:.*:.*:.+$`
//! (`smithy_query(query="shape", shape="Arn")`) — under which exactly the ARNs the service
//! mints match. Nothing about constraint checking is reimplemented: the constraints are
//! re-declared in the framework's own form (a [`TraitMap`] of
//! [`LengthTrait`]/[`PatternTrait`]) on a member schema of the same name, and the check is
//! [`validate`] with the envelope [`envelope`] writes — the same walk and the same sentence
//! the request boundary refuses a member with.
//!
//! The ARN's RESOURCE part is the path the resource's own operations are bound to
//! (`/domains/{DomainName}`, `/domains/{DomainName}/object-types/{ObjectTypeName}`, …), so
//! one parse reaches every kind of resource this domain tree holds tags for, and
//! "domains, profile object types, and integrations can be tagged"
//! (`ListTagsForResource`) needs no per-operation table.

use std::collections::HashMap;
use std::sync::LazyLock;

use ::aws_smithy_schema::serde::{SerdeError, SerializableStruct, ShapeSerializer};
use ::aws_smithy_schema::{Schema, ShapeId, ShapeType, TraitMap};
use ::aws_smithy_schema_server::constraints::{
    envelope, validate, LengthTrait, PatternTrait, Violation,
};

use super::ComAmazonawsCustomerprofilesCustomerprofiles20200815State as ServiceState;

/// This service's Smithy namespace, for the member schema below.
const NAMESPACE: &str = "com.amazonaws.customerprofiles";

/// The expression a `resourceArn` is held to: the one this model publishes for a Customer
/// Profiles ARN (`Arn`). It is the published `TagArn` expression's own first segments —
/// `arn`, a partition, this service — continuing into the region, account and resource
/// part every ARN of a stored resource carries.
const RESOURCE_ARN_PATTERN: &str = r"^arn:([a-z\d-]+):profile:.*:.*:.+$";

/// `TagArn`'s modelled `@length` plus [`RESOURCE_ARN_PATTERN`].
static RESOURCE_ARN_TRAITS: LazyLock<TraitMap> = LazyLock::new(|| {
    let mut traits = TraitMap::new();
    traits.insert(Box::new(LengthTrait::new(Some(0), Some(MAX_TAG_ARN_LENGTH))));
    traits.insert(Box::new(PatternTrait::new(RESOURCE_ARN_PATTERN)));
    traits
});

/// The longest a `resourceArn` may be: the `@length` maximum `TagArn` declares.
const MAX_TAG_ARN_LENGTH: i64 = 256;

/// The most tags one resource carries: the `@length` maximum of `TagMap`, which is what
/// one request may carry and therefore what a resource may hold.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The `resourceArn` member schema the walk reads: its name gives the violation path and
/// its trait map carries the constraints.
static M_RESOURCE_ARN: Schema = Schema::new_member(
    ShapeId::from_static(
        "com.amazonaws.customerprofiles#TaggedResource$resourceArn",
        NAMESPACE,
        "resourceArn",
    ),
    ShapeType::String,
    "resourceArn",
    0,
)
.with_traits(&RESOURCE_ARN_TRAITS);

/// The root of the shadow input the walk is driven over. Its member list is empty on
/// purpose: the walk visits what [`TaggedResourceArn`] WRITES, and a root struct schema
/// carries no member name, so it contributes no path segment.
static TAGGED_RESOURCE_SCHEMA: Schema = Schema::new_struct(
    ShapeId::from_static(
        "com.amazonaws.customerprofiles#TaggedResource",
        NAMESPACE,
        "TaggedResource",
    ),
    ShapeType::Structure,
    &[],
);

/// One request's `resourceArn`, written for the validating walk the way the codec writes a
/// string member.
struct TaggedResourceArn<'a> {
    resource_arn: &'a str,
}

impl SerializableStruct for TaggedResourceArn<'_> {
    fn serialize_members(&self, serializer: &mut dyn ShapeSerializer) -> Result<(), SerdeError> {
        serializer.write_string(&M_RESOURCE_ARN, self.resource_arn)
    }
}

/// The refusal sentence for a `resourceArn` that breaks the member's length bound or the
/// expression above, or `None` for one that satisfies both.
///
/// The violations and the envelope are the framework's, so a refusal made here reads
/// exactly like one the walk at the request boundary writes for any other member.
pub fn resource_arn_violation(resource_arn: &str) -> Option<String> {
    let violations: Vec<Violation> =
        validate(&TaggedResourceArn { resource_arn }, &TAGGED_RESOURCE_SCHEMA);
    envelope(&violations)
}

/// The resource part of an ARN: everything after `arn:<partition>:<service>:<region>:<account>:`.
fn resource_part(resource_arn: &str) -> Option<&str> {
    resource_arn.splitn(6, ':').nth(5)
}

/// The Region part of an ARN: the fourth field, which names the Region the resource it
/// addresses lives in.
///
/// Every resource this surface tags is nested under a domain, and a domain is a regional
/// resource, so the Region the ARN names is the Region its record is looked for in: an ARN
/// naming another Region addresses no resource of this one.
fn region_part(resource_arn: &str) -> Option<&str> {
    resource_arn.split(':').nth(3)
}

/// One resource the tagging operations can address, as its ARN's resource part names it.
#[derive(Debug, PartialEq, Eq)]
enum Addressed<'a> {
    /// `domains/<DomainName>`.
    Domain(&'a str),
    /// `domains/<DomainName>/object-types/<ObjectTypeName>`.
    ObjectType(&'a str, &'a str),
    /// `domains/<DomainName>/calculated-attributes/<CalculatedAttributeName>`.
    CalculatedAttribute(&'a str, &'a str),
    /// `domains/<DomainName>/recommender-schemas/<RecommenderSchemaName>`.
    RecommenderSchema(&'a str, &'a str),
}

/// The resource an ARN's resource part names, or `None` for a path naming no kind of
/// resource this service holds.
///
/// Every path is the URI the resource's own operations are bound to, less its leading
/// slash: a domain, and the resources nested under one.
fn addressed(resource_part: &str) -> Option<Addressed<'_>> {
    let under_domains = resource_part.strip_prefix("domains/")?;
    let Some((domain_name, nested)) = under_domains.split_once('/') else {
        return (!under_domains.is_empty()).then_some(Addressed::Domain(under_domains));
    };
    let (collection, name) = nested.split_once('/')?;
    if domain_name.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    match collection {
        "object-types" => Some(Addressed::ObjectType(domain_name, name)),
        "calculated-attributes" => Some(Addressed::CalculatedAttribute(domain_name, name)),
        "recommender-schemas" => Some(Addressed::RecommenderSchema(domain_name, name)),
        _ => None,
    }
}

/// The tag map of the resource `resource_arn` names, for writing, or `None` when this
/// emulator holds no such resource.
///
/// One resolution for every tag mutation, so `TagResource` and `UntagResource` cannot
/// disagree about which record an ARN reaches.
pub fn resource_tags_mut<'s>(
    store: &'s mut ServiceState,
    resource_arn: &str,
) -> Option<&'s mut Option<HashMap<String, String>>> {
    let region = region_part(resource_arn)?.to_string();
    match addressed(resource_part(resource_arn)?)? {
        Addressed::Domain(domain_name) => {
            Some(&mut store.domain_mut(&region, domain_name)?.tags)
        }
        Addressed::ObjectType(domain_name, object_type_name) => Some(
            &mut store
                .domain_mut(&region, domain_name)?
                .object_types
                .get_mut(object_type_name)?
                .tags,
        ),
        Addressed::CalculatedAttribute(domain_name, calculated_attribute_name) => Some(
            &mut store
                .domain_mut(&region, domain_name)?
                .calculated_attributes
                .get_mut(calculated_attribute_name)?
                .tags,
        ),
        Addressed::RecommenderSchema(domain_name, recommender_schema_name) => Some(
            &mut store
                .domain_mut(&region, domain_name)?
                .recommender_schemas
                .get_mut(recommender_schema_name)?
                .tags,
        ),
    }
}

/// The tags the resource `resource_arn` names carries, as a report states them: PRESENT
/// and empty for a resource carrying none, and `None` when this emulator holds no such
/// resource — the same distinction a domain projection draws
/// ([`super::Domain::reported_tags`]).
pub fn reported_resource_tags(
    store: &ServiceState,
    resource_arn: &str,
) -> Option<HashMap<String, String>> {
    let region = region_part(resource_arn)?;
    let held = match addressed(resource_part(resource_arn)?)? {
        Addressed::Domain(domain_name) => &store.domain(region, domain_name)?.tags,
        Addressed::ObjectType(domain_name, object_type_name) => {
            &store
                .domain(region, domain_name)?
                .object_types
                .get(object_type_name)?
                .tags
        }
        Addressed::CalculatedAttribute(domain_name, calculated_attribute_name) => {
            &store
                .domain(region, domain_name)?
                .calculated_attributes
                .get(calculated_attribute_name)?
                .tags
        }
        Addressed::RecommenderSchema(domain_name, recommender_schema_name) => {
            &store
                .domain(region, domain_name)?
                .recommender_schemas
                .get(recommender_schema_name)?
                .tags
        }
    };
    Some(held.clone().unwrap_or_default())
}

/// The sentence the TAGGING surface words an absent resource with: entirely FIXED text,
/// interpolating nothing — not the ARN the request carried, nor the kind of resource it
/// named. It travels with the error code that surface answers, `NotFoundException`
/// ([`crate::scaffolded::Scaffolded::not_found`]), rather than with the
/// `ResourceNotFoundException` the domain-scoped operations answer: the tagging layer in
/// front of the service reports an absent resource in its own words, which these three
/// operations therefore share.
pub const TAGGED_RESOURCE_NOT_FOUND: &str = "Resource was not found";

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: an ARN's resource part is the URI path of the resource it names, so a domain
    /// and each kind of resource nested under one resolves, and anything else does not.
    #[test]
    fn an_arn_resolves_to_the_resource_its_uri_path_names() {
        let resolved = [
            (
                "a domain",
                "arn:aws:profile:us-east-1:000000000000:domains/D",
                Addressed::Domain("D"),
            ),
            (
                "a profile object type",
                "arn:aws:profile:eu-west-1:000000000000:domains/D/object-types/CTR",
                Addressed::ObjectType("D", "CTR"),
            ),
            (
                "a calculated attribute definition",
                "arn:aws-cn:profile:cn-north-1:000000000000:domains/D/calculated-attributes/Spend",
                Addressed::CalculatedAttribute("D", "Spend"),
            ),
            (
                "a recommender schema",
                "arn:aws:profile:us-east-1:000000000000:domains/D/recommender-schemas/S",
                Addressed::RecommenderSchema("D", "S"),
            ),
        ];
        for (case, resource_arn, expected) in resolved {
            let part = resource_part(resource_arn)
                .unwrap_or_else(|| panic!("case {case:?}: no resource part"));
            assert_eq!(
                addressed(part),
                Some(expected),
                "case {case:?}: expected the resource its path names, got {:?}",
                addressed(part)
            );
        }

        for (case, resource_part_text) in [
            ("no resource at all", ""),
            ("a domain with no name", "domains/"),
            ("a collection this service does not nest", "domains/D/widgets/W"),
            ("a nested resource with no name", "domains/D/object-types/"),
            ("a path that is not under domains", "profiles/P"),
        ] {
            assert_eq!(
                addressed(resource_part_text),
                None,
                "case {case:?}: expected no resource, got {:?}",
                addressed(resource_part_text)
            );
        }
    }

    /// RULE: the member is held to its modelled length and to the expression the model
    /// publishes for one of its ARNs, so every ARN of a stored resource passes and a value
    /// that is not one of this service's ARNs is refused.
    #[test]
    fn a_resource_arn_is_checked_against_the_published_service_arn_expression() {
        for (case, resource_arn) in [
            (
                "a domain ARN",
                "arn:aws:profile:us-east-1:000000000000:domains/D".to_string(),
            ),
            (
                "a China-partition ARN",
                "arn:aws-cn:profile:cn-north-1:000000000000:domains/D".to_string(),
            ),
            (
                "a nested resource ARN",
                "arn:aws:profile:us-east-1:000000000000:domains/D/object-types/CTR".to_string(),
            ),
        ] {
            assert_eq!(
                resource_arn_violation(&resource_arn),
                None,
                "case {case:?}: expected the ARN to satisfy the member, got {:?}",
                resource_arn_violation(&resource_arn)
            );
        }

        for (case, resource_arn) in [
            ("not an ARN at all", "domains/D".to_string()),
            (
                "another service's ARN",
                "arn:aws:connect:us-east-1:000000000000:instance/i".to_string(),
            ),
            (
                "no resource part",
                "arn:aws:profile:us-east-1:000000000000:".to_string(),
            ),
            (
                "past the modelled length bound",
                format!(
                    "arn:aws:profile:us-east-1:000000000000:domains/{}",
                    "d".repeat(256)
                ),
            ),
        ] {
            assert!(
                resource_arn_violation(&resource_arn).is_some(),
                "case {case:?}: expected the ARN to be refused, got no violation"
            );
        }
    }
}
