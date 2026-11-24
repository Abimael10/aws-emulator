// SPDX-License-Identifier: CC0-1.0

//! Why the ENTITY a `PutLogEvents` request carried was rejected, when it was.
//!
//! "The entity associated with the log events in a `PutLogEvents` call" is telemetry
//! metadata about what produced the events, not part of them: the events of a request whose
//! entity cannot be read are still ingested, and the response says why the entity was not —
//! `PutLogEventsResponse$rejectedEntityInfo`, "information about why the entity is rejected
//! when calling PutLogEvents. Only returned when the entity is rejected." So an entity this
//! module rejects costs the batch nothing, and an entity it admits adds no member to the
//! response at all.
//!
//! What makes an entity readable is the contract of its own two maps:
//!
//! * `Entity$keyAttributes`, "the attributes of the entity which identify the specific
//!   entity […] There are five allowed attributes (key names): `Type`, `ResourceType`,
//!   `Identifier`, `Name`, and `Environment`" — so the `Type` says which kind of object the
//!   rest identifies, the attributes that kind is identified by have to be there, an
//!   attribute that states ANOTHER kind's identity does not belong beside it, and a key name
//!   outside those five is not an attribute of an entity at all;
//! * `Entity$attributes`, "additional attributes of the entity that are not used to specify
//!   the identity of the entity", which identify nothing: which names a producer may fill in
//!   there is published outside this model (the guide that member links to), and a producer's
//!   OWN name carries whatever text it likes — but a name that guide publishes a set of values
//!   for states one of those values, and nothing else can be read as that attribute
//!   ([`ATTRIBUTE_VALUE_CATALOGUE`]).
//!
//! The REQUIRED fields of the identity map are read first, its extra key names after them, and
//! the additional attributes beside it last ([`error_type`]), so an entity that identifies
//! nothing is reported for what it is missing rather than for what it carries besides.
//!
//! Every value the rejection can carry is one of the modelled
//! [`EntityRejectionErrorType`](crate::ops::types::ENTITY_REJECTION_ERROR_TYPE_VALUES)
//! values, each naming the part of the entity that could not be read — which is the whole
//! diagnostic a caller gets, since the call still answers 200.
//!
//! The two maps are also held to the bounds the model gives them — `keyAttributes` 2..4
//! entries, `attributes` 0..10, each key and value bounded — but those are modelled
//! constraints of the REQUEST, so the framework's own validation refuses a request that
//! breaks one before this module is reached: the rejections stated here are the ones left for
//! the service to take.

use std::collections::HashMap;

use crate::ops::types::{Entity, RejectedEntityInfo};

/// The `keyAttributes` key that says which kind of object the entity is: "There are five
/// allowed attributes (key names): `Type`, `ResourceType`, `Identifier`, `Name`, and
/// `Environment`" (`Entity$keyAttributes`), of which this one is the entity's type.
const TYPE_ATTRIBUTE: &str = "Type";

/// The `keyAttributes` key naming the type of the AWS resource a `Resource` entity is.
const RESOURCE_TYPE_ATTRIBUTE: &str = "ResourceType";

/// The `keyAttributes` key naming the identifier of the AWS resource a `Resource` entity is.
const IDENTIFIER_ATTRIBUTE: &str = "Identifier";

/// The `keyAttributes` key naming a `Service` entity.
const NAME_ATTRIBUTE: &str = "Name";

/// The `keyAttributes` key naming the environment an entity runs in.
const ENVIRONMENT_ATTRIBUTE: &str = "Environment";

/// The five key names `Entity$keyAttributes` admits, quoted on [`TYPE_ATTRIBUTE`].
const ALLOWED_KEY_ATTRIBUTES: [&str; 5] = [
    TYPE_ATTRIBUTE,
    RESOURCE_TYPE_ATTRIBUTE,
    IDENTIFIER_ATTRIBUTE,
    NAME_ATTRIBUTE,
    ENVIRONMENT_ATTRIBUTE,
];

/// The entity types that identify an object by its NAME: a service of the account, a service
/// it calls, and the AWS-managed form of one. The CloudWatch entity `Type` catalogue is one
/// across the services that take an entity; its values are the ones the sibling Application
/// Signals model documents for the same `Type` attribute
/// (`smithy_query(query="doc",shape="KeyAttributes")` there).
const NAMED_ENTITY_TYPES: [&str; 3] = ["Service", "RemoteService", "AWS::Service"];

/// The entity types that identify an object by its RESOURCE TYPE AND IDENTIFIER. See
/// [`NAMED_ENTITY_TYPES`] for where the catalogue comes from.
const RESOURCE_ENTITY_TYPES: [&str; 2] = ["Resource", "AWS::Resource"];

/// The error type for an entity whose `keyAttributes` carry a key name that is not one of the
/// five the member admits.
const INVALID_KEY_ATTRIBUTES: &str = "InvalidKeyAttributes";

/// The error type for an entity whose `Type` is not one of the entity types
/// ([`NAMED_ENTITY_TYPES`], [`RESOURCE_ENTITY_TYPES`]).
const INVALID_TYPE_VALUE: &str = "InvalidTypeValue";

/// The error type for an entity that leaves out an attribute its own type is identified by.
const MISSING_REQUIRED_FIELDS: &str = "MissingRequiredFields";

/// The error type for an entity whose `keyAttributes` carry an attribute that states ANOTHER
/// entity type's identity than the one the entity declared, and for an entity whose
/// `attributes` state a value outside the set published for the attribute they are under
/// ([`ATTRIBUTE_VALUE_CATALOGUE`]).
const INVALID_ATTRIBUTES: &str = "InvalidAttributes";

/// The `attributes` names whose values are drawn from a PUBLISHED set, each with that set.
///
/// `Entity$attributes` links its names to "How to add related information to telemetry" in the
/// CloudWatch User Guide
/// (`smithy_query(query="doc",shape="Entity",member="attributes")`), where the table of
/// attributes gives these two their valid values: the platform an entity runs on, and where
/// the name of a service entity came from. Every other name in that table — the Kubernetes,
/// EC2 and host coordinates, and a producer's own additions — carries free text, which is why
/// only these two are listed here.
const ATTRIBUTE_VALUE_CATALOGUE: [(&str, &[&str]); 2] = [
    (
        "PlatformType",
        &[
            "AWS::EC2",
            "AWS::ECS",
            "AWS::EKS",
            "AWS::Lambda",
            "K8s",
            "Generic",
        ],
    ),
    (
        "AWS.ServiceNameSource",
        &[
            "Instrumentation",
            "UserConfiguration",
            "ClientIamRole",
            "K8sWorkload",
            "RdsDbInstance",
            "CloudFormationStack",
        ],
    ),
];

/// Why `entity` was rejected, or `None` for a request that carried no entity or carried one
/// this service can read — in which case the response reports no `rejectedEntityInfo` at all
/// (see the module docs).
#[must_use]
pub fn rejection(entity: Option<&Entity>) -> Option<RejectedEntityInfo> {
    let entity = entity?;
    error_type(entity).map(|error_type| RejectedEntityInfo {
        error_type: error_type.to_string(),
    })
}

/// The [`EntityRejectionErrorType`](crate::ops::types::ENTITY_REJECTION_ERROR_TYPE_VALUES)
/// value naming what cannot be read in `entity`.
///
/// The REQUIRED fields come first — the `Type` that says which kind of object the entity is,
/// and the attributes that type is identified by — because they are what the entity has to
/// carry to identify anything at all; only an entity that does identify something is then
/// held to what its identity map carries BESIDES that identity: a key name outside the five
/// the member admits, and one of the five that states another type's identity. So an entity
/// that identifies nothing is reported for what it is missing rather than for what it carries
/// beyond it. The ADDITIONAL attributes come last, because they identify nothing at all: only
/// a name the catalogue publishes values for can fail to be read there
/// ([`states_unpublished_value`]).
fn error_type(entity: &Entity) -> Option<&'static str> {
    let key_attributes = entity.key_attributes.as_ref()?;
    let Some(entity_type) = key_attributes
        .get(TYPE_ATTRIBUTE)
        .filter(|entity_type| !entity_type.trim().is_empty())
    else {
        // The `Type` is what the rest of the key attributes are read under, so an entity
        // without one identifies nothing.
        return Some(MISSING_REQUIRED_FIELDS);
    };
    let identified_by: &[&str] = if NAMED_ENTITY_TYPES.contains(&entity_type.as_str()) {
        &[NAME_ATTRIBUTE]
    } else if RESOURCE_ENTITY_TYPES.contains(&entity_type.as_str()) {
        &[RESOURCE_TYPE_ATTRIBUTE, IDENTIFIER_ATTRIBUTE]
    } else {
        // An entity type outside the catalogue identifies nothing either, and which
        // attributes it would be identified BY is unknown, so it is reported for the type.
        return Some(INVALID_TYPE_VALUE);
    };
    if identified_by
        .iter()
        .any(|attribute| missing(key_attributes, attribute))
    {
        return Some(MISSING_REQUIRED_FIELDS);
    }
    // What the entity carries BESIDES a complete identity: a key name outside the five the
    // member admits is no attribute of an entity at all, and one of the five that states
    // ANOTHER type's identity does not fit the type this entity declared — the environment
    // an entity runs in fits every type, since it states no identity of its own.
    if key_attributes
        .keys()
        .any(|key| !ALLOWED_KEY_ATTRIBUTES.contains(&key.as_str()))
    {
        return Some(INVALID_KEY_ATTRIBUTES);
    }
    if key_attributes.keys().any(|key| {
        ![TYPE_ATTRIBUTE, ENVIRONMENT_ATTRIBUTE].contains(&key.as_str())
            && !identified_by.contains(&key.as_str())
    }) {
        return Some(INVALID_ATTRIBUTES);
    }
    // The additional attributes identify nothing, so they are read last — and only the ones
    // whose values are published can fail to be read at all.
    states_unpublished_value(entity.attributes.as_ref()).then_some(INVALID_ATTRIBUTES)
}

/// Whether `attributes` states, under a name the attribute catalogue publishes values for, a
/// value that is not one of them — which is what makes an additional attribute unreadable.
///
/// A name the catalogue does not carry is a producer's own, and "additional attributes of the
/// entity that are not used to specify the identity of the entity" identify nothing, so such a
/// name's value is free text and is read as given.
fn states_unpublished_value(attributes: Option<&HashMap<String, String>>) -> bool {
    let Some(attributes) = attributes else {
        return false;
    };
    attributes.iter().any(|(name, value)| {
        ATTRIBUTE_VALUE_CATALOGUE
            .iter()
            .find(|(attribute, _)| *attribute == name.as_str())
            .is_some_and(|(_, published)| !published.contains(&value.as_str()))
    })
}

/// Whether `attributes` states nothing under `attribute`: an attribute present with no text
/// names nothing, so it is as absent as one that was left out.
fn missing(attributes: &HashMap<String, String>, attribute: &str) -> bool {
    attributes
        .get(attribute)
        .is_none_or(|value| value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One entity, from its key attributes alone.
    fn entity(key_attributes: &[(&str, &str)]) -> Entity {
        Entity {
            key_attributes: Some(
                key_attributes
                    .iter()
                    .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                    .collect(),
            ),
            attributes: None,
        }
    }

    /// One row of the table below: the case, the entity's key attributes, and the error type
    /// the rejection carries (`None` for an entity that is read).
    type Row = (&'static str, &'static [(&'static str, &'static str)], Option<&'static str>);

    /// The entries of one map of an entity, as a row below writes them.
    type Attributes = Vec<(String, String)>;

    /// RULE: an entity is read by its `Type` and the attributes that type is identified by,
    /// and the rejection names the part that could not be read — the key names first, then
    /// the type, then the attributes it is identified by.
    #[test]
    fn an_entity_is_rejected_for_the_part_that_cannot_be_read() {
        let rows: &[Row] = &[
            (
                "a service entity",
                &[("Type", "Service"), ("Name", "orders"), ("Environment", "prod")],
                None,
            ),
            (
                "a resource entity",
                &[
                    ("Type", "Resource"),
                    ("ResourceType", "AWS::EC2::Instance"),
                    ("Identifier", "i-0123456789abcdef0"),
                ],
                None,
            ),
            (
                "an AWS-managed service entity",
                &[("Type", "AWS::Service"), ("Name", "orders")],
                None,
            ),
            (
                "a type outside the catalogue",
                &[("Type", "Widget"), ("Name", "orders")],
                Some(INVALID_TYPE_VALUE),
            ),
            (
                "a type whose case does not match the catalogue",
                &[("Type", "service"), ("Name", "orders")],
                Some(INVALID_TYPE_VALUE),
            ),
            (
                "a type carrying no text",
                &[("Type", ""), ("Name", "orders")],
                Some(MISSING_REQUIRED_FIELDS),
            ),
            (
                "a key name outside the five allowed, beside a complete identity",
                &[("Type", "Service"), ("Name", "orders"), ("Owner", "team-a")],
                Some(INVALID_KEY_ATTRIBUTES),
            ),
            (
                "a key name outside the five allowed, with no identity at all",
                &[("Type", "Service"), ("Owner", "team-a")],
                Some(MISSING_REQUIRED_FIELDS),
            ),
            (
                "no type at all",
                &[("Name", "orders"), ("Environment", "prod")],
                Some(MISSING_REQUIRED_FIELDS),
            ),
            (
                "a service entity with no name",
                &[("Type", "Service"), ("Environment", "prod")],
                Some(MISSING_REQUIRED_FIELDS),
            ),
            (
                "a resource entity with no identifier",
                &[("Type", "Resource"), ("ResourceType", "AWS::EC2::Instance")],
                Some(MISSING_REQUIRED_FIELDS),
            ),
            (
                "a resource entity whose identifier carries no text",
                &[
                    ("Type", "Resource"),
                    ("ResourceType", "AWS::EC2::Instance"),
                    ("Identifier", "  "),
                ],
                Some(MISSING_REQUIRED_FIELDS),
            ),
        ];
        for (case, key_attributes, expected) in rows {
            let rejected = rejection(Some(&entity(key_attributes)));
            let actual = rejected.as_ref().map(|info| info.error_type.as_str());
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the rejection {expected:?}, got {actual:?}"
            );
            if let Some(error_type) = actual {
                assert!(
                    crate::ops::types::ENTITY_REJECTION_ERROR_TYPE_VALUES.contains(&error_type),
                    "case {case:?}: {error_type:?} is not a modelled rejection error type"
                );
            }
        }
        // A request that carried no entity has nothing to reject.
        assert!(
            rejection(None).is_none(),
            "case \"no entity at all\": expected no rejection"
        );
    }

    /// RULE: beside a complete identity, the identity map is held to the five names it may
    /// carry and to the ones that state the TYPE it declared — while an `attributes` name the
    /// catalogue publishes no values for carries whatever text it is given.
    #[test]
    fn the_identity_map_is_held_to_the_names_it_may_carry() {
        let identity = [
            ("Type".to_string(), "Service".to_string()),
            ("Name".to_string(), "orders".to_string()),
        ];
        let with_attributes = |attributes: Vec<(String, String)>| Entity {
            key_attributes: Some(identity.iter().cloned().collect()),
            attributes: Some(attributes.into_iter().collect()),
        };
        // (case, the attributes beside the identity, the rejection)
        let rows: Vec<(&str, Attributes, Option<&str>)> = vec![
            (
                "an attribute of the telemetry guide's own table",
                vec![(
                    "AWS.ServiceNameSource".to_string(),
                    "UserConfiguration".to_string(),
                )],
                None,
            ),
            (
                "two attributes of a producer's own",
                vec![
                    ("Host".to_string(), "i-0".to_string()),
                    ("PlatformType".to_string(), "AWS::EC2".to_string()),
                ],
                None,
            ),
            ("no attributes at all", Vec::new(), None),
        ];
        for (case, attributes, expected) in rows {
            let rejected = rejection(Some(&with_attributes(attributes)));
            let actual = rejected.as_ref().map(|info| info.error_type.as_str());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the rejection {expected:?}, got {actual:?}"
            );
        }

        // The identity map carries its own five names, and only the ones that state the type
        // it declared.
        let key_attributes = |entries: Vec<(String, String)>| Entity {
            key_attributes: Some(entries.into_iter().collect()),
            attributes: None,
        };
        for (case, entries, expected) in [
            (
                "as many key attributes as the map admits",
                vec![
                    ("Type".to_string(), "Resource".to_string()),
                    ("ResourceType".to_string(), "AWS::EC2::Instance".to_string()),
                    ("Identifier".to_string(), "i-0".to_string()),
                    ("Environment".to_string(), "prod".to_string()),
                ],
                None,
            ),
            (
                "an attribute of the OTHER map among the key attributes",
                vec![
                    ("Type".to_string(), "Service".to_string()),
                    ("Name".to_string(), "orders".to_string()),
                    ("Host".to_string(), "i-0".to_string()),
                ],
                Some(INVALID_KEY_ATTRIBUTES),
            ),
            (
                "a resource entity's identifier beside a service entity's name",
                vec![
                    ("Type".to_string(), "Service".to_string()),
                    ("Name".to_string(), "orders".to_string()),
                    ("Identifier".to_string(), "i-0".to_string()),
                ],
                Some(INVALID_ATTRIBUTES),
            ),
            (
                "a service entity's name beside a resource entity's identity",
                vec![
                    ("Type".to_string(), "Resource".to_string()),
                    ("ResourceType".to_string(), "AWS::EC2::Instance".to_string()),
                    ("Identifier".to_string(), "i-0".to_string()),
                    ("Name".to_string(), "orders".to_string()),
                ],
                Some(INVALID_ATTRIBUTES),
            ),
        ] {
            let rejected = rejection(Some(&key_attributes(entries)));
            let actual = rejected.as_ref().map(|info| info.error_type.as_str());
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the rejection {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: an additional attribute whose name the catalogue publishes values for states one
    /// of those values; anything else cannot be read as that attribute.
    ///
    /// Every published value of every catalogued name is admitted, and each name is also given
    /// a value outside its set — including one that belongs to the OTHER catalogued name, so a
    /// value is read under the name it is stated for rather than against the catalogue as a
    /// whole.
    #[test]
    fn an_additional_attribute_is_held_to_the_values_published_for_it() {
        let with_attributes = |attributes: Attributes| Entity {
            key_attributes: Some(
                [
                    ("Type".to_string(), "Service".to_string()),
                    ("Name".to_string(), "orders".to_string()),
                ]
                .into_iter()
                .collect(),
            ),
            attributes: Some(attributes.into_iter().collect()),
        };
        let one = |name: &str, value: &str| -> Attributes {
            vec![(name.to_string(), value.to_string())]
        };
        for (name, published) in ATTRIBUTE_VALUE_CATALOGUE {
            for value in published {
                let case = format!("{name}={value}, a published value");
                let rejected = rejection(Some(&with_attributes(one(name, value))));
                let actual = rejected.as_ref().map(|info| info.error_type.as_str());
                assert_eq!(
                    actual, None,
                    "case {case:?}: expected no rejection, got {actual:?}"
                );
            }
            // A value of the OTHER catalogued name, and one no name publishes.
            let unpublished: Vec<&str> = ATTRIBUTE_VALUE_CATALOGUE
                .iter()
                .filter(|(other, _)| *other != name)
                .flat_map(|(_, values)| values.iter().copied())
                .chain(["", "  ", "not-a-published-value", "aws::ec2"])
                .filter(|value| !published.contains(value))
                .collect();
            for value in unpublished {
                let case = format!("{name}={value:?}, not a published value");
                let rejected = rejection(Some(&with_attributes(one(name, value))));
                let actual = rejected.as_ref().map(|info| info.error_type.as_str());
                assert_eq!(
                    actual,
                    Some(INVALID_ATTRIBUTES),
                    "case {case:?}: expected the rejection {INVALID_ATTRIBUTES:?}, got {actual:?}"
                );
            }
            // A name the catalogue does not carry takes the same text as free text.
            let case = format!("a producer's own name carrying {name}'s rejected value");
            let rejected = rejection(Some(&with_attributes(one(
                "Producer.OwnAttribute",
                "not-a-published-value",
            ))));
            let actual = rejected.as_ref().map(|info| info.error_type.as_str());
            assert_eq!(
                actual, None,
                "case {case:?}: expected no rejection, got {actual:?}"
            );
        }
    }
}
