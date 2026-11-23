// SPDX-License-Identifier: CC0-1.0

//! `AWS::IoT::Thing`: the thing a stack declares, registered in AWS IoT's own thing registry.
//!
//! This is the per-type half of provisioning for one resource type, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::iot_topic_rule`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the thing is really created
//!
//! A thing is a RECORD of the AWS IoT registry, and the registry is what every later
//! control-plane call about the device resolves the name against: `DescribeThing`,
//! `UpdateThing`, `AttachThingPrincipal`, `ListThingPrincipals`, `AddThingToThingGroup` and
//! `DeleteThing` each take a `thingName` and each answer `ResourceNotFoundException` for a
//! name the registry does not hold. So a stack that reported a thing created while nothing
//! created it publishes a name that resolves to nothing: the certificate a client then
//! attaches to the deployed device is refused, no group can hold it, and a second stack
//! declaring the same name is accepted where AWS IoT refuses it with
//! `ResourceAlreadyExistsException`. Registering it where it belongs is what makes every one
//! of those answers the owning service's own — including the refusal of a `DeleteThing` while
//! a principal is still attached, which is what a stack teardown has to report.
//!
//! ## The properties
//!
//! The type publishes `ThingName` — the name the thing is registered UNDER, which is this
//! resource's physical id and what its `Ref` resolves to — and the `AttributePayload` whose
//! `Attributes` map `CreateThing` and `UpdateThing` take. A declaration that names no thing is
//! registered under the identifier CloudFormation computed for the resource.
//!
//! A declared property this arm does not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a thing reported created with less than its declaration
//! asked for is the shallow success this arm exists to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{call_rest_json, percent_encoded, Created, Readiness, Refusal, Removal};
use crate::state::StoredResource;

/// The resource type this module provisions.
pub(crate) const IOT_THING: &str = "AWS::IoT::Thing";

/// The SigV4 signing name of the service that owns it, which is what routes the in-process
/// call to AWS IoT's own handlers (that model's `sigv4Name`/`endpointPrefix`,
/// `smithy_query(query="service_info")`). It speaks `restJson1`, so each call is a method and
/// a URI rather than an `X-Amz-Target`.
const IOT_SIGNING_NAME: &str = "iot";

/// The code AWS IoT answers a read of a thing it does not hold with
/// (`DescribeThing`'s modelled `ResourceNotFoundException`, HTTP 404), which is what says the
/// registry no longer holds the resource.
const NOT_FOUND: &str = "ResourceNotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The property naming the thing, which is the name it is registered under rather than a
/// member of the create's body (`CreateThing` binds `thingName` as its `@httpLabel`).
const THING_NAME: &str = "ThingName";

/// The property carrying the thing's attributes, and the map inside it: `AttributePayload`'s
/// own published member.
const ATTRIBUTE_PAYLOAD: &str = "AttributePayload";
const ATTRIBUTES: &str = "Attributes";

/// Every property `AWS::IoT::Thing` publishes. A declared property this list does not carry
/// is one no call here applies, and is refused rather than dropped.
const THING_PROPERTIES: &[&str] = &[ATTRIBUTE_PAYLOAD, THING_NAME];

/// Every property the `AttributePayload` of this type publishes, judged the same way: the
/// attribute map is all a declared payload states, since `merge` "is only valid when calling
/// UpdateThing or UpdateThingGroup" and is the deployment's own business rather than the
/// template's.
const ATTRIBUTE_PAYLOAD_PROPERTIES: &[&str] = &[ATTRIBUTES];

/// The members of the request `CreateThing` and `UpdateThing` take the attribute map under
/// (`AttributePayload$attributes`).
const ATTRIBUTE_PAYLOAD_MEMBER: &str = "attributePayload";
const ATTRIBUTES_MEMBER: &str = "attributes";

/// The attributes `AWS::IoT::Thing` publishes through `Fn::GetAtt` (`Arn`, "the Amazon
/// Resource Name of the thing", and `Id`, "the thing ID"), read off the members AWS IoT's own
/// answers carry them under.
const ARN_ATTRIBUTE: &str = "Arn";
const ID_ATTRIBUTE: &str = "Id";
const THING_ARN_MEMBER: &str = "thingArn";
const THING_ID_MEMBER: &str = "thingId";

/// The member a read of a thing answers its attribute map under
/// (`DescribeThing$attributes`).
const REPORTED_ATTRIBUTES_MEMBER: &str = "attributes";

/// Register the thing an `AWS::IoT::Thing` declares, in AWS IoT itself — so the name the stack
/// publishes is one that service answers for, and every later call about the device resolves
/// it exactly as it resolves an API-created thing's.
///
/// `Err` is the owning service's own refusal of the create, which is what the resource fails
/// with: a name the account already holds, a name outside the pattern that service models, an
/// attribute set it will not take.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_declared_properties(&resource.properties, &resource.resource_type)?;
    let name = resource.physical_resource_id.clone();
    let request = create_request(&resource.properties);
    // `CreateThing` answers the thing's own ARN and id, so the attributes the type publishes
    // are the ones that service minted rather than values built here.
    let answered = call_iot(ctx, region, "POST", &thing_uri(&name), Some(&request)).await?;
    Ok(Created::attributes(published_attributes(&answered)))
}

/// Whether the owning service reports the registered thing ready.
///
/// Nothing is asked, and nothing waits: a thing EXISTS from the moment `CreateThing` answers —
/// `DescribeThing` resolves it and a principal can be attached to it at once — and AWS IoT
/// reports no transient state a registered thing settles out of.
pub(super) fn ready() -> Readiness {
    Readiness::Ready
}

/// Whether the owning service has finished removing a deleted thing.
///
/// Nothing is asked: `DeleteThing` takes the record away as it answers, and AWS IoT reports no
/// state a deleted thing is still being removed in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the thing the stack already has, through `UpdateThing` — the
/// call that takes the thing's own name, so the record keeps its identity, its ARN and its id,
/// and nothing is replaced.
///
/// The thing's NAME is its identity, and a declaration that changes it is a different
/// resource: [`super::super::template`] computes a new physical id for it, so the deployment
/// replaces the thing rather than reaching here.
///
/// The attribute list is sent whether or not the new declaration states one, and NOT as a
/// merge: a revision that drops `AttributePayload` asks for a thing with no attributes, and
/// `AttributePayload$merge` left alone is the API's own default — "the list of attributes
/// provided in the `AttributePayload`" overwrites the stored ones rather than merging into
/// them — so the registry ends up holding exactly what the declaration asks for.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    // The DECLARED document, not the record's: by the time this is reached the resource
    // carries the properties it was CREATED with, so a property the new declaration
    // introduces is only in `declared`.
    refuse_declared_properties(declared, &resource.resource_type)?;
    let name = resource.physical_resource_id.clone();
    call_iot(
        ctx,
        region,
        "PATCH",
        &thing_uri(&name),
        Some(&update_request(declared)),
    )
    .await?;
    // `UpdateThing` answers no members at all, so the attributes the type publishes are read
    // back off the thing that now exists rather than invented here.
    let described = read_thing(ctx, region, &name).await?;
    Ok(Created::attributes(published_attributes(&described)))
}

/// Delete the thing a stack registered, addressed by the name it was registered under.
///
/// Every refusal is the owning service's own and fails the resource, which is the point of
/// deleting it there: a thing a principal is still attached to is refused — AWS IoT requires
/// detach-before-delete — so a teardown that would tear the device's credential out from under
/// it reports that rather than a removal nothing performed.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    call_iot(
        ctx,
        region,
        "DELETE",
        &thing_uri(&resource.physical_resource_id),
        None,
    )
    .await
    .map(|_| ())
}

/// The thing's configuration as the owning service reports it, for drift detection.
///
/// `DescribeThing` answers the name and the attribute map, which map back onto the two
/// properties the type publishes. A thing the owning service no longer holds was deleted
/// ([`NOT_FOUND`]), judged on the CODE and never on a message.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let described = match read_thing(ctx, region, &resource.physical_resource_id).await {
        Ok(described) => described,
        Err(refusal) if refusal.code == NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let mut configuration = Map::new();
    configuration.insert(
        THING_NAME.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    if let Some(attributes) = described
        .get(REPORTED_ATTRIBUTES_MEMBER)
        .and_then(Value::as_object)
    {
        let mut payload = Map::new();
        payload.insert(ATTRIBUTES.to_string(), Value::Object(attributes.clone()));
        configuration.insert(ATTRIBUTE_PAYLOAD.to_string(), Value::Object(payload));
    }
    Ok(Some(Value::Object(configuration)))
}

/// The body `CreateThing` is called with: the declared attribute map where the declaration
/// carries one, and nothing else — the thing's name is the URI label, and `billingGroupName`
/// and `thingTypeName` are members of the call this type publishes no property for.
fn create_request(properties: &Value) -> Value {
    let mut request = Map::new();
    if let Some(attributes) = declared_attributes(properties) {
        request.insert(ATTRIBUTE_PAYLOAD_MEMBER.to_string(), payload(attributes));
    }
    Value::Object(request)
}

/// The body `UpdateThing` is called with: the attribute list the new declaration asks for,
/// which is the EMPTY list where it states none, so the registry holds exactly the attributes
/// the declaration carries ([`update`]).
fn update_request(declared: &Value) -> Value {
    let mut request = Map::new();
    request.insert(
        ATTRIBUTE_PAYLOAD_MEMBER.to_string(),
        payload(declared_attributes(declared).unwrap_or_default()),
    );
    Value::Object(request)
}

/// One `AttributePayload` carrying `attributes`, in the API's own member names.
fn payload(attributes: Map<String, Value>) -> Value {
    let mut payload = Map::new();
    payload.insert(ATTRIBUTES_MEMBER.to_string(), Value::Object(attributes));
    Value::Object(payload)
}

/// The attribute map a declaration states, as the API takes it, or `None` where the
/// declaration carries no `AttributePayload` with a map at all.
///
/// The map's KEYS are the caller's own, so they are carried exactly as declared. A VALUE the
/// API models as text that a template wrote as a number or a flag — which is every value a
/// `Ref` to a non-string parameter yields — is carried as the text that value spells, since
/// `Attributes` is a map of strings ("A JSON string containing name-value pairs",
/// `AttributePayload$attributes`); anything else travels as written, so the owning service's
/// own validation is what judges it.
fn declared_attributes(properties: &Value) -> Option<Map<String, Value>> {
    let declared = properties
        .get(ATTRIBUTE_PAYLOAD)?
        .get(ATTRIBUTES)?
        .as_object()?;
    Some(
        declared
            .iter()
            .map(|(key, value)| (key.clone(), attribute_value(value)))
            .collect(),
    )
}

/// One declared attribute value as the API takes it: the text a scalar spells, and anything
/// else exactly as written.
fn attribute_value(value: &Value) -> Value {
    match value {
        Value::String(_) => value.clone(),
        Value::Number(number) => Value::String(number.to_string()),
        Value::Bool(flag) => Value::String(flag.to_string()),
        other => other.clone(),
    }
}

/// The attributes the type publishes, read off whatever AWS IoT answered about the thing: the
/// ARN and the id that service minted for it.
fn published_attributes(answered: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    for (attribute, member) in [
        (ARN_ATTRIBUTE, THING_ARN_MEMBER),
        (ID_ATTRIBUTE, THING_ID_MEMBER),
    ] {
        if let Some(value) = answered
            .get(member)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert(attribute.to_string(), value.to_string());
        }
    }
    attributes
}

/// `DescribeThing` for `name`.
async fn read_thing(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Value, Refusal> {
    call_iot(ctx, region, "GET", &thing_uri(name), None).await
}

/// The URI the per-thing operations are bound to (`/things/{thingName}`, that model's own
/// `http` binding), with the name written as ONE path segment so a name outside the modelled
/// pattern is refused by that operation's validation instead of addressing a different one.
fn thing_uri(name: &str) -> String {
    format!("/things/{}", percent_encoded(name))
}

/// The declaration's properties, and the members of its `AttributePayload`, held to what this
/// arm applies.
fn refuse_declared_properties(properties: &Value, resource_type: &str) -> Result<(), Refusal> {
    refuse_unapplied_properties(properties, resource_type, THING_PROPERTIES)?;
    if let Some(declared) = properties.get(ATTRIBUTE_PAYLOAD) {
        refuse_unapplied_properties(
            declared,
            &format!("{resource_type}'s {ATTRIBUTE_PAYLOAD}"),
            ATTRIBUTE_PAYLOAD_PROPERTIES,
        )?;
    }
    Ok(())
}

/// The refusal for a declared property this module does not apply, naming which property it
/// was: refusing is what keeps a thing from being reported as created — or as updated — with
/// less than its declaration asked for.
fn refuse_unapplied_properties(
    properties: &Value,
    resource_type: &str,
    published: &[&str],
) -> Result<(), Refusal> {
    let Value::Object(properties) = properties else {
        return Ok(());
    };
    for property in properties.keys() {
        if published.contains(&property.as_str()) {
            continue;
        }
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {resource_type}"
            ),
        });
    }
    Ok(())
}

/// Call one AWS IoT operation on the REST-JSON wire its model declares.
async fn call_iot(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json(ctx, IOT_SIGNING_NAME, region, method, uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared attribute map reaches `CreateThing` under that call's own member names,
    /// with the caller's keys untouched and a scalar the API models as text carried as the text
    /// it spells.
    ///
    /// The rows span what a declaration states: no payload at all, an empty map, a map of
    /// text, and a map whose values a template wrote as a number and as a flag.
    #[test]
    fn the_declared_attributes_reach_the_api_under_its_own_member_names() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "no payload at all",
                serde_json::json!({"ThingName": "sensor-1"}),
                serde_json::json!({}),
            ),
            (
                "an empty attribute map",
                serde_json::json!({"AttributePayload": {"Attributes": {}}}),
                serde_json::json!({"attributePayload": {"attributes": {}}}),
            ),
            (
                "a map of text",
                serde_json::json!({
                    "AttributePayload": {"Attributes": {"Model": "a-1", "site.id": "EU/1"}},
                }),
                serde_json::json!({
                    "attributePayload": {"attributes": {"Model": "a-1", "site.id": "EU/1"}},
                }),
            ),
            (
                "a map carrying a number and a flag",
                serde_json::json!({
                    "AttributePayload": {"Attributes": {"Revision": 3, "Managed": true}},
                }),
                serde_json::json!({
                    "attributePayload": {"attributes": {"Revision": "3", "Managed": "true"}},
                }),
            ),
        ];
        for (case, declared, expected) in cases {
            let request = create_request(declared);
            assert_eq!(
                &request, expected,
                "case {case:?}: expected the request {expected}, got {request}"
            );
        }
    }

    /// A revision that drops the payload asks for a thing with NO attributes, so the update
    /// sends the empty list rather than nothing at all — and never asks for a merge, so the
    /// registry holds exactly what the declaration carries.
    #[test]
    fn the_update_sends_the_list_the_revision_asks_for() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "a revision that drops the payload",
                serde_json::json!({"ThingName": "sensor-1"}),
                serde_json::json!({"attributePayload": {"attributes": {}}}),
            ),
            (
                "a revision that states one attribute",
                serde_json::json!({"AttributePayload": {"Attributes": {"Model": "a-2"}}}),
                serde_json::json!({"attributePayload": {"attributes": {"Model": "a-2"}}}),
            ),
        ];
        for (case, declared, expected) in cases {
            let request = update_request(declared);
            assert_eq!(
                &request, expected,
                "case {case:?}: expected the request {expected}, got {request}"
            );
        }
    }

    /// The URI is the model's own binding with the name as ONE path segment, so a name
    /// carrying a character that is structure in a URI cannot address another operation.
    #[test]
    fn the_uri_carries_the_name_as_one_segment() {
        let cases: &[(&str, &str, &str)] = &[
            ("an ordinary name", "sensor-1", "/things/sensor-1"),
            ("a name with a colon", "a:b", "/things/a%3Ab"),
            ("a name with a slash", "a/b", "/things/a%2Fb"),
        ];
        for (case, name, expected) in cases {
            let actual = thing_uri(name);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the URI {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every property the type publishes is applied; one no call here carries is refused,
    /// naming it — inside the `AttributePayload` as well as at the top level.
    #[test]
    fn an_unapplied_property_is_refused() {
        let declared = serde_json::json!({
            "ThingName": "sensor-1",
            "AttributePayload": {"Attributes": {"Model": "a-1"}},
        });
        assert!(
            refuse_declared_properties(&declared, IOT_THING).is_ok(),
            "case \"every published property\": expected it to be applied, got a refusal"
        );
        let cases: &[(&str, Value, &str)] = &[
            (
                "a property no call carries",
                serde_json::json!({"ThingName": "sensor-1", "Unheard": 1}),
                "Unheard",
            ),
            (
                "a payload member no call carries",
                serde_json::json!({"AttributePayload": {"Merge": true}}),
                "Merge",
            ),
        ];
        for (case, declared, named) in cases {
            let refusal = refuse_declared_properties(declared, IOT_THING)
                .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {:?}",
                refusal.message
            );
        }
    }

    /// The attributes the type publishes are the ones AWS IoT minted, read off its own answer;
    /// an answer carrying neither publishes neither.
    #[test]
    fn the_published_attributes_are_the_ones_the_owning_service_minted() {
        let answered = serde_json::json!({
            "thingName": "sensor-1",
            "thingArn": "arn:aws:iot:us-west-2:000000000000:thing/sensor-1",
            "thingId": "2c7e0a1f-0e3a-4c0e-9f3a-8f6a5d4c3b2a",
        });
        let published = published_attributes(&answered);
        assert_eq!(
            published.get(ARN_ATTRIBUTE).map(String::as_str),
            Some("arn:aws:iot:us-west-2:000000000000:thing/sensor-1"),
            "case \"a full answer\": expected the minted ARN, got {published:?}"
        );
        assert_eq!(
            published.get(ID_ATTRIBUTE).map(String::as_str),
            Some("2c7e0a1f-0e3a-4c0e-9f3a-8f6a5d4c3b2a"),
            "case \"a full answer\": expected the minted id, got {published:?}"
        );
        let bare = published_attributes(&serde_json::json!({"thingName": "sensor-1"}));
        assert!(
            bare.is_empty(),
            "case \"an answer carrying neither\": expected no attribute, got {bare:?}"
        );
    }
}
