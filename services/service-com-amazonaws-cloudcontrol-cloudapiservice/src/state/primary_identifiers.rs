// SPDX-License-Identifier: CC0-1.0

//! How the handler that owns a resource type NAMES the resource inside its own diagnostic.
//!
//! A resource type's registry schema points `primaryIdentifier` at the property that
//! identifies one of its resources, and a request may carry that identifier either as the
//! bare value or as the schema's identifier map — "Primary identifiers can be specified as
//! a string or JSON" (`GetResource$Identifier`). Which of the two a HANDLER echoes back
//! when it reports a resource it could not find is that handler's own business: one built
//! on the generic resource-provider base class repeats the identifier the request carried,
//! while one that resolves that identifier against the type's schema first names the
//! resource as the schema's identifier map, `{"/properties/<property>":"<value>"}`.
//!
//! So the rendering is PER TYPE, and this module is where that is stated: the types whose
//! handler is recorded naming the resource as the map, with the property their schema's
//! `primaryIdentifier` points at. Every other type is named by the identifier the request
//! carried.
//!
//! The same schema member decides WHAT the primary identifier of a resource IS. A schema
//! may point `primaryIdentifier` at more than one property — the resource of such a type is
//! identified by a COMBINATION of values, not by one — and the identifier of one of its
//! resources is those values joined by the registry's delimiter
//! ([`COMPOUND_IDENTIFIER_DELIMITER`]). That is the one form every answer of this service
//! names such a resource by and the one form a request addresses it by, so it is derived
//! here, from the type's own schema, rather than guessed per operation.

use super::*;

/// The delimiter the values of a COMPOUND primary identifier are joined by.
///
/// A resource type whose schema points `primaryIdentifier` at several properties is
/// identified by all of them together, and Cloud Control API carries that combination as one
/// `Identifier` string: the values in the order the schema lists the pointers, separated by
/// `|`. It is the form the console and the CLI show such an identifier in, and the form
/// `GetResource`/`UpdateResource`/`DeleteResource` take it back in.
pub const COMPOUND_IDENTIFIER_DELIMITER: char = '|';

/// The properties a resource type's registry schema points `primaryIdentifier` at, for the
/// types whose identifier is made of MORE THAN ONE of them — in the order the schema lists
/// the pointers, which is the order their values are joined in.
///
/// Catalog source: the `primaryIdentifier` list each type's published resource provider
/// definition schema declares, with the `/properties/` prefix of each pointer dropped. A row
/// is a statement about the TYPE, not about what this emulator's own CloudFormation registry
/// derives for it: that registry serves a schema built from the properties a type publishes
/// as return values, which is a different (narrower) record. Every type the CloudFormation
/// crate records as composite —
/// `service-com-amazonaws-cloudformation-cloudformation/src/state/template/return_values.rs`
/// (`composite_identity_properties`), which joins those same values with the same delimiter —
/// is here with the same properties in the same order, and has to stay that way.
///
/// A type absent from here is identified by ONE property, which is the rule
/// [`super::derive_identifier`] applies: the property the type's schema points its single
/// pointer at.
const COMPOUND_PRIMARY_IDENTIFIERS: &[(&str, &[&str])] = &[
    (
        "AWS::ACMPCA::Permission",
        &["CertificateAuthorityArn", "Principal"],
    ),
    (
        "AWS::AmplifyUIBuilder::Component",
        &["AppId", "EnvironmentName", "Id"],
    ),
    (
        "AWS::AmplifyUIBuilder::Form",
        &["AppId", "EnvironmentName", "Id"],
    ),
    (
        "AWS::AmplifyUIBuilder::Theme",
        &["AppId", "EnvironmentName", "Id"],
    ),
    (
        "AWS::ApplicationAutoScaling::ScalableTarget",
        &["ResourceId", "ScalableDimension", "ServiceNamespace"],
    ),
    (
        "AWS::Cognito::IdentityPoolPrincipalTag",
        &["IdentityPoolId", "IdentityProviderName"],
    ),
    ("AWS::EKS::AccessEntry", &["ClusterName", "PrincipalArn"]),
    ("AWS::Logs::LogStream", &["LogGroupName", "LogStreamName"]),
    (
        "AWS::Logs::SubscriptionFilter",
        &["FilterName", "LogGroupName"],
    ),
    ("AWS::WAFv2::IPSet", WAFV2_ENTITY_IDENTIFIER),
    ("AWS::WAFv2::RegexPatternSet", WAFV2_ENTITY_IDENTIFIER),
    ("AWS::WAFv2::RuleGroup", WAFV2_ENTITY_IDENTIFIER),
    ("AWS::WAFv2::WebACL", WAFV2_ENTITY_IDENTIFIER),
    (
        "AWS::WAFv2::WebACLAssociation",
        &["ResourceArn", "WebACLArn"],
    ),
];

/// The pointers each of the four AWS WAF (WAFv2) ENTITY types declares its `primaryIdentifier`
/// as: the entity's name, the `Id` AWS WAF assigned it and the `Scope` of the namespace it
/// lives in — the triple every call of that service on one entity is addressed by.
///
/// One row for the four, since the four schemas declare the same three pointers in the same
/// order (a web ACL, a rule group, an IP set and a regex pattern set each live in a scope,
/// under a name, with an id that service minted).
const WAFV2_ENTITY_IDENTIFIER: &[&str] = &["Name", "Id", "Scope"];

/// The properties `type_name`'s schema points `primaryIdentifier` at, for a type whose
/// identifier is made of several of them; `None` for every other type, whose identifier is
/// the value of one property.
pub fn compound_identifier_properties(type_name: &str) -> Option<&'static [&'static str]> {
    COMPOUND_PRIMARY_IDENTIFIERS
        .iter()
        .find(|(name, _)| *name == type_name)
        .map(|(_, properties)| *properties)
}

/// Every property a resource of `type_name` carries its primary identifier in: the pointers
/// its schema's `primaryIdentifier` names where that is a combination, and otherwise the one
/// property `identifier_property` names.
///
/// The ONE reading of "which properties identify this resource", so the refusal that keeps a
/// patch from changing the identifier and the create-only set a patch is judged against
/// cannot disagree about a type.
pub fn identifier_properties(type_name: &str, identifier_property: &str) -> Vec<String> {
    match compound_identifier_properties(type_name) {
        Some(properties) => properties
            .iter()
            .map(|property| (*property).to_string())
            .collect(),
        None => vec![identifier_property.to_string()],
    }
}

/// The primary identifier the resource model `properties` describes, by the rule
/// `type_name`'s schema states: the values of the pointers of a compound identifier joined by
/// [`COMPOUND_IDENTIFIER_DELIMITER`], and otherwise the value `identifier_property` carries.
///
/// `None` when the model does not carry an identifier at all — a property missing, or
/// carrying something that cannot be one ([`super::identifier_text`]).
pub fn model_identifier(
    type_name: &str,
    properties: &Value,
    identifier_property: &str,
) -> Option<String> {
    let Some(pointers) = compound_identifier_properties(type_name) else {
        return properties.get(identifier_property).and_then(identifier_text);
    };
    let mut segments = Vec::with_capacity(pointers.len());
    for pointer in pointers {
        let (_, value) = property_named(properties, pointer)?;
        segments.push(identifier_text(value)?);
    }
    Some(segments.join(&COMPOUND_IDENTIFIER_DELIMITER.to_string()))
}

/// The primary identifier of the resource `properties` describes, for a type whose schema
/// points `primaryIdentifier` at several properties — or which of them keeps the model from
/// identifying a resource at all.
pub fn derive_compound_identifier(
    properties: &Value,
    pointers: &'static [&'static str],
) -> DerivedIdentifier {
    let mut segments = Vec::with_capacity(pointers.len());
    for pointer in pointers {
        // A segment the model does not carry, or carries something that cannot be an
        // identifier in, is named as the offending property: the combination identifies a
        // resource only when every one of its parts does.
        let value = property_named(properties, pointer).and_then(|(_, value)| identifier_text(value));
        match value {
            Some(segment) => segments.push(segment),
            None => {
                return DerivedIdentifier::Unusable {
                    property: (*pointer).to_string(),
                };
            }
        }
    }
    DerivedIdentifier::Carried {
        // Of the properties the identifier is made of, the first is the one recorded as
        // carrying it: the whole combination is the type's, which
        // [`identifier_properties`] answers from the type's own schema wherever the set is
        // needed.
        property: pointers.first().copied().unwrap_or_default().to_string(),
        identifier: segments.join(&COMPOUND_IDENTIFIER_DELIMITER.to_string()),
    }
}

/// The resource types whose handler names the resource as the PRIMARY-IDENTIFIER OBJECT
/// the type's registry schema declares, with the property that schema's
/// `primaryIdentifier` points at.
///
/// Catalog source: the refusal each type's handler is RECORDED answering through this API.
/// A type absent from here is named by the identifier the request carried, which is what
/// the handlers recorded for the other types of this emulator do.
const RENDERED_AS_SCHEMA_MAP: &[(&str, &str)] = &[("AWS::Logs::LogGroup", "LogGroupName")];

/// The resource as the handler that owns `type_name` names it: the identifier the request
/// carried, or the type's primary-identifier object where that handler renders one
/// ([`RENDERED_AS_SCHEMA_MAP`]).
pub fn handler_rendered_identifier(type_name: &str, identifier: &str) -> String {
    let Some((_, property)) = RENDERED_AS_SCHEMA_MAP
        .iter()
        .find(|(name, _)| *name == type_name)
    else {
        return identifier.to_string();
    };
    // The map is written by the JSON writer, so it carries the separators JSON gives it
    // and no whitespace of its own.
    let mut map = ::serde_json::Map::new();
    map.insert(
        format!("{PROPERTY_POINTER_PREFIX}{property}"),
        Value::String(identifier.to_string()),
    );
    Value::Object(map).to_string()
}

/// The identifier the resource ANSWERS TO for the reference a request addressed a resource
/// of `type_name` by — or why that reference names no resource of the type at all.
///
/// "Primary identifiers can be specified as a string or JSON"
/// (`GetResource$Identifier`), so a request may address a resource in either of two forms,
/// and both are read here, once, for every operation that takes an `Identifier`:
///
/// * the primary identifier itself, which for a type whose schema points
///   `primaryIdentifier` at several properties is their values joined by
///   [`COMPOUND_IDENTIFIER_DELIMITER`] — so a reference of that form carrying the wrong
///   number of values identifies no resource and is refused rather than looked up;
/// * a JSON object of the schema's identifier properties, keyed either by the property name
///   or by the pointer the schema spells it with (`/properties/<property>`), which is the
///   form a client builds from the type's registry schema. It names THOSE properties and no
///   others: a property that does not identify a resource of the type is refused rather
///   than resolved as if it did.
///
/// The identifier a JSON object may name is the type's PRIMARY one. `GetResource$Identifier`
/// also admits "any secondary identifier defined for the resource type in its resource
/// schema", and no type of this emulator declares one — no schema it serves carries
/// `additionalIdentifiers` — so the primary identifier is the whole set a reference can name
/// here, and the refusals below list exactly it.
///
/// `assigned_property` is the property the OWNING service's provider holds a single-pointer
/// type's identifier under, where one stands behind the type ([`native`]'s
/// `identifier_property`); for every other type the properties that identify a resource are
/// the ones the type's own name convention gives it
/// ([`super::identifier_property_candidates`]).
pub fn addressed_identifier(
    type_name: &str,
    reference: &str,
    assigned_property: Option<&str>,
) -> Result<String, String> {
    let pointers = compound_identifier_properties(type_name);
    if let Some(named) = identifier_object(reference) {
        // The identifier the type's schema declares, as the refusal lists it: the pointers
        // of a compound one, the property the owning service holds a single one under, or
        // the property the type's own name convention gives it.
        let declared: Vec<String> = match (pointers, assigned_property) {
            (Some(pointers), _) => pointers.iter().map(|p| (*p).to_string()).collect(),
            (None, Some(property)) => vec![property.to_string()],
            (None, None) => super::identifier_property_candidates(type_name)
                .into_iter()
                .take(1)
                .collect(),
        };
        // "You must specify exactly one identifier … and no additional properties": the
        // object names the identifier's properties and nothing else, so an entry that is
        // not one of them makes the whole object name no identifier.
        let accepted: Vec<String> = match (pointers, assigned_property) {
            (None, None) => super::identifier_property_candidates(type_name),
            _ => declared.clone(),
        };
        if named.len() != declared.len()
            || named.iter().any(|(property, _)| {
                !accepted
                    .iter()
                    .any(|accepted| accepted.eq_ignore_ascii_case(property))
            })
        {
            return Err(json_identifier_message(&declared));
        }
        let mut segments = Vec::with_capacity(declared.len());
        for property in &declared {
            // A compound identifier's segments come in the order the SCHEMA lists the
            // pointers, whichever order the object's members arrived in; a single one is
            // the one entry's value, under whichever of the type's identifier properties
            // the object named it.
            let carried = match (pointers, named.as_slice()) {
                (None, [(_, value)]) => identifier_text(value),
                _ => named
                    .iter()
                    .find(|(named, _)| named.eq_ignore_ascii_case(property))
                    .and_then(|(_, value)| identifier_text(value)),
            };
            match carried {
                Some(segment) => segments.push(segment),
                None => return Err(json_identifier_message(&declared)),
            }
        }
        return Ok(segments.join(&COMPOUND_IDENTIFIER_DELIMITER.to_string()));
    }
    let Some(pointers) = pointers else {
        return Ok(reference.to_string());
    };
    let segments: Vec<&str> = reference
        .split(COMPOUND_IDENTIFIER_DELIMITER)
        .collect();
    if segments.len() != pointers.len() || segments.iter().any(|segment| segment.is_empty()) {
        return Err(malformed_identifier(reference, pointers));
    }
    Ok(reference.to_string())
}

/// What a reference that is not the primary identifier of the type is refused with: the
/// reference itself, and the identifier it is not valid for as the schema's own pointers.
///
/// RECORDED wording: "Identifier `<reference>` is not valid for identifier
/// [`<pointer>`, …]" — the reference the request carried, and the pointers the type's
/// `primaryIdentifier` declares, which is what tells a caller what to send instead.
fn malformed_identifier(reference: &str, properties: &[&str]) -> String {
    format!(
        "Identifier {reference} is not valid for identifier [{}]",
        pointer_list(properties.iter().map(|property| (*property).to_string()))
    )
}

/// What an identifier given in the JSON form is refused with when it does not name the
/// type's identifier: RECORDED wording, which states the rule — one identifier of the type's
/// own, and nothing beside it.
///
/// The identifiers are listed as a list OF identifiers, each the pointers one of them is
/// made of, because a type may declare more than one way to be addressed; a type of this
/// emulator declares the one its `primaryIdentifier` does, and no secondary identifier, so
/// the list carries that one.
///
/// Every word of the template but the pointer list is FIXED text, the closing clause
/// included: it is what states what is disallowed, so a caller reading the refusal learns
/// that nothing may be specified beside the one identifier.
fn json_identifier_message(declared: &[String]) -> String {
    format!(
        "When specifying the identifier as JSON, you must specify exactly one identifier \
         from [[{}]], and no additional properties can be specified",
        pointer_list(declared.iter().cloned())
    )
}

/// The properties as the pointers a type's schema spells them with, comma-separated — the
/// form every refusal above lists an identifier in.
fn pointer_list(properties: impl Iterator<Item = String>) -> String {
    properties
        .map(|property| format!("{PROPERTY_POINTER_PREFIX}{property}"))
        .collect::<Vec<String>>()
        .join(", ")
}

/// The identifier properties and values `reference` names, for a reference given in the JSON
/// form — `None` for one that is not a JSON object at all, which is a primary identifier
/// given as the string it is.
///
/// A key may be the property's name or the pointer the type's schema spells it with
/// ([`PROPERTY_POINTER_PREFIX`]), because that is the form a client reading the schema has:
/// the two name the same property.
fn identifier_object(reference: &str) -> Option<Vec<(String, Value)>> {
    let trimmed = reference.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let named: Value = ::serde_json::from_str(trimmed).ok()?;
    let named = named.as_object()?;
    Some(
        named
            .iter()
            .map(|(property, value)| {
                let property = property
                    .strip_prefix(PROPERTY_POINTER_PREFIX)
                    .unwrap_or(property);
                (property.to_string(), value.clone())
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compound primary identifier is the values of the type's pointers in the schema's
    /// own order, joined by the registry's delimiter — and a model missing one of them
    /// identifies no resource, naming the part that is missing.
    #[test]
    fn a_compound_identifier_is_every_pointers_value_joined_by_the_delimiter() {
        let rows = [
            (
                "a log stream, whose identifier is its group and its own name",
                "AWS::Logs::LogStream",
                ::serde_json::json!({ "LogGroupName": "app", "LogStreamName": "host-1" }),
                Ok("app|host-1"),
            ),
            (
                "a subscription filter, an entry the delimiter rule is not stated for",
                "AWS::Logs::SubscriptionFilter",
                ::serde_json::json!({
                    "FilterName": "to-firehose",
                    "LogGroupName": "app",
                    "DestinationArn": "arn:aws:logs:us-east-1:000000000000:destination:d",
                }),
                Ok("to-firehose|app"),
            ),
            (
                "an access entry, identified by the cluster and the principal",
                "AWS::EKS::AccessEntry",
                ::serde_json::json!({
                    "ClusterName": "fleet",
                    "PrincipalArn": "arn:aws:iam::000000000000:role/r",
                }),
                Ok("fleet|arn:aws:iam::000000000000:role/r"),
            ),
            (
                "a scalable target, whose identifier embeds the resource it scales",
                "AWS::ApplicationAutoScaling::ScalableTarget",
                ::serde_json::json!({
                    "ResourceId": "table/orders",
                    "ScalableDimension": "dynamodb:table:ReadCapacityUnits",
                    "ServiceNamespace": "dynamodb",
                    "MinCapacity": 1,
                    "MaxCapacity": 10,
                }),
                Ok("table/orders|dynamodb:table:ReadCapacityUnits|dynamodb"),
            ),
            (
                "a web ACL, identified by its name, the id AWS WAF minted and its scope",
                "AWS::WAFv2::WebACL",
                ::serde_json::json!({
                    "Name": "edge-acl",
                    "Id": "0b1e6a0e-7c44-4a29-9a4e-4d9b0cdc4f21",
                    "Scope": "REGIONAL",
                    "DefaultAction": { "Allow": {} },
                }),
                Ok("edge-acl|0b1e6a0e-7c44-4a29-9a4e-4d9b0cdc4f21|REGIONAL"),
            ),
            (
                "an IP set, whose schema declares the same three pointers",
                "AWS::WAFv2::IPSet",
                ::serde_json::json!({
                    "Name": "listed",
                    "Id": "7f2d1a55-3c08-42b7-9f1e-2a6c58b0d913",
                    "Scope": "CLOUDFRONT",
                    "IPAddressVersion": "IPV4",
                }),
                Ok("listed|7f2d1a55-3c08-42b7-9f1e-2a6c58b0d913|CLOUDFRONT"),
            ),
            (
                "a model missing one part of the combination",
                "AWS::Logs::LogStream",
                ::serde_json::json!({ "LogStreamName": "host-1" }),
                Err("LogGroupName"),
            ),
            (
                "a model carrying a part that cannot be an identifier",
                "AWS::Logs::LogStream",
                ::serde_json::json!({ "LogGroupName": "app", "LogStreamName": "" }),
                Err("LogStreamName"),
            ),
        ];
        for (case, type_name, properties, expected) in rows {
            let pointers = compound_identifier_properties(type_name).unwrap_or_else(|| {
                panic!("case {case:?}: expected {type_name:?} to declare a compound identifier")
            });
            let derived = derive_compound_identifier(&properties, pointers);
            let expected = match expected {
                Ok(identifier) => DerivedIdentifier::Carried {
                    property: pointers[0].to_string(),
                    identifier: identifier.to_string(),
                },
                Err(property) => DerivedIdentifier::Unusable {
                    property: property.to_string(),
                },
            };
            assert_eq!(
                derived, expected,
                "case {case:?}: expected {expected:?}, got {derived:?}"
            );
            // The same identifier is read back off a model that carries it, which is what
            // lets an update check its patch left the identifier alone.
            if let DerivedIdentifier::Carried { identifier, .. } = &expected {
                let read = model_identifier(type_name, &properties, pointers[0]);
                assert_eq!(
                    read.as_deref(),
                    Some(identifier.as_str()),
                    "case {case:?}: expected the model to read back as {identifier:?}, got \
                     {read:?}"
                );
            }
        }
    }

    /// A type whose schema points `primaryIdentifier` at ONE property is identified by that
    /// property alone, so nothing here changes what every other type answers.
    #[test]
    fn a_single_pointer_type_is_identified_by_its_one_property() {
        for type_name in ["AWS::SQS::Queue", "AWS::Logs::LogGroup", "AWS::Kinesis::Stream"] {
            assert!(
                compound_identifier_properties(type_name).is_none(),
                "type {type_name:?}: expected no compound identifier"
            );
            assert_eq!(
                identifier_properties(type_name, "QueueUrl"),
                vec!["QueueUrl".to_string()],
                "type {type_name:?}: expected the one property the model carries"
            );
        }
        assert_eq!(
            identifier_properties("AWS::Logs::LogStream", "LogGroupName"),
            vec!["LogGroupName".to_string(), "LogStreamName".to_string()],
            "a compound identifier's properties are the schema's own pointers"
        );
    }

    /// A request may address a resource by its primary identifier or by a JSON object of the
    /// schema's identifier properties, and both resolve to the one identifier the resource
    /// answers to; a reference of neither form, or one naming a property that does not
    /// identify a resource of the type, identifies no resource.
    #[test]
    fn a_reference_resolves_to_the_identifier_the_resource_answers_to() {
        let rows = [
            (
                "a single-property type addressed by its identifier",
                "AWS::SQS::Queue",
                Some("QueueUrl"),
                "https://sqs.us-east-1.amazonaws.com/000000000000/orders",
                Ok("https://sqs.us-east-1.amazonaws.com/000000000000/orders"),
            ),
            (
                "the same type addressed as a JSON object of that property",
                "AWS::SQS::Queue",
                Some("QueueUrl"),
                "{\"QueueUrl\": \"https://sqs.us-east-1.amazonaws.com/000000000000/orders\"}",
                Ok("https://sqs.us-east-1.amazonaws.com/000000000000/orders"),
            ),
            (
                "the same object keyed by the pointer the schema spells",
                "AWS::Logs::LogGroup",
                Some("LogGroupName"),
                "{\"/properties/LogGroupName\": \"app\"}",
                Ok("app"),
            ),
            (
                "a type of this service's own store, by its conventional name property",
                "AWS::Kinesis::Stream",
                None,
                "{\"StreamName\": \"events\"}",
                Ok("events"),
            ),
            (
                "a compound type addressed by its delimited identifier",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "app|host-1",
                Ok("app|host-1"),
            ),
            (
                "a compound type addressed as a JSON object, in any member order",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "{\"LogStreamName\": \"host-1\", \"LogGroupName\": \"app\"}",
                Ok("app|host-1"),
            ),
            (
                "a three-part identifier as a JSON object",
                "AWS::ApplicationAutoScaling::ScalableTarget",
                None,
                "{\"ServiceNamespace\": \"dynamodb\", \"ResourceId\": \"table/orders\", \
                 \"ScalableDimension\": \"dynamodb:table:ReadCapacityUnits\"}",
                Ok("table/orders|dynamodb:table:ReadCapacityUnits|dynamodb"),
            ),
            (
                "a compound type addressed by one segment only",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "app",
                Err("Identifier app is not valid for identifier \
                     [/properties/LogGroupName, /properties/LogStreamName]"),
            ),
            (
                "a compound type addressed by too many segments",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "app|host-1|extra",
                Err("Identifier app|host-1|extra is not valid for identifier \
                     [/properties/LogGroupName, /properties/LogStreamName]"),
            ),
            (
                "a compound type addressed by a JSON object missing a part",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "{\"LogGroupName\":\"app\"}",
                Err("When specifying the identifier as JSON, you must specify exactly one \
                     identifier from [[/properties/LogGroupName, \
                     /properties/LogStreamName]], and no additional properties can be \
                     specified"),
            ),
            (
                "a compound type addressed by a JSON object carrying one property too many",
                "AWS::Logs::LogStream",
                Some("LogGroupName"),
                "{\"LogGroupName\":\"app\",\"LogStreamName\":\"host\",\"Extra\":\"x\"}",
                Err("When specifying the identifier as JSON, you must specify exactly one \
                     identifier from [[/properties/LogGroupName, \
                     /properties/LogStreamName]], and no additional properties can be \
                     specified"),
            ),
            (
                "a JSON object naming a property that identifies no resource",
                "AWS::SQS::Queue",
                Some("QueueUrl"),
                "{\"VisibilityTimeout\":\"30\"}",
                Err("When specifying the identifier as JSON, you must specify exactly one \
                     identifier from [[/properties/QueueUrl]], and no additional properties \
                     can be specified"),
            ),
            (
                "a JSON object naming two properties where one identifies the resource",
                "AWS::SQS::Queue",
                Some("QueueUrl"),
                "{\"QueueUrl\":\"https://queue/url\",\"QueueName\":\"orders\"}",
                Err("When specifying the identifier as JSON, you must specify exactly one \
                     identifier from [[/properties/QueueUrl]], and no additional properties \
                     can be specified"),
            ),
        ];
        for (case, type_name, assigned, reference, expected) in rows {
            let resolved = addressed_identifier(type_name, reference, assigned);
            match expected {
                Ok(identifier) => assert_eq!(
                    resolved.as_deref(),
                    Ok(identifier),
                    "case {case:?}: expected {identifier:?}, got {resolved:?}"
                ),
                Err(detail) => assert_eq!(
                    resolved.as_ref().err().map(String::as_str),
                    Some(detail),
                    "case {case:?}: expected the refusal {detail:?}, got {resolved:?}"
                ),
            }
        }
    }

    /// A type whose handler names the resource as the schema's identifier map is rendered
    /// as that map, built from the identifier the request carried; every other type is
    /// named by that identifier itself.
    #[test]
    fn the_identifier_is_rendered_as_the_owning_handler_names_it() {
        let rows = [
            (
                "AWS::Logs::LogGroup",
                "app-logs",
                "{\"/properties/LogGroupName\":\"app-logs\"}",
            ),
            (
                "AWS::Logs::LogGroup",
                "/aws/lambda/fn",
                "{\"/properties/LogGroupName\":\"/aws/lambda/fn\"}",
            ),
            ("AWS::SQS::Queue", "https://queue/url", "https://queue/url"),
            ("AWS::Kinesis::Stream", "events", "events"),
        ];
        for (type_name, identifier, expected) in rows {
            let rendered = handler_rendered_identifier(type_name, identifier);
            assert_eq!(
                rendered, expected,
                "type {type_name:?} with {identifier:?}: expected {expected:?}, got \
                 {rendered:?}"
            );
        }
    }

    /// The map is the JSON form a request may carry an identifier in, so it reads back as
    /// one object of the schema's pointer and the value.
    #[test]
    fn the_rendered_map_is_the_schemas_pointer_and_the_value() {
        let rendered = handler_rendered_identifier("AWS::Logs::LogGroup", "app-logs");
        let parsed: Value = ::serde_json::from_str(&rendered)
            .expect("the rendered identifier must be the JSON the reader takes");
        assert_eq!(
            parsed,
            ::serde_json::json!({ "/properties/LogGroupName": "app-logs" }),
            "unexpected identifier map, got {rendered:?}"
        );
    }
}
