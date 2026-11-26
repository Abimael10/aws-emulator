// SPDX-License-Identifier: CC0-1.0

//! `AWS::CloudFront::RealtimeLogConfig`: the real-time log configuration a stack declares so a
//! cache behaviour can send its log records to an Amazon Kinesis data stream, created in Amazon
//! CloudFront itself.
//!
//! ## Why it is really created
//!
//! A real-time log configuration's identity is the ARN CloudFront mints for it:
//! `GetRealtimeLogConfig`, `UpdateRealtimeLogConfig`, `DeleteRealtimeLogConfig`,
//! `ListDistributionsByRealtimeLogConfig` and a cache behaviour's own
//! `RealtimeLogConfigArn` all address one by that ARN. So a stack that reported a
//! configuration created while nothing created it publishes an ARN that resolves to nothing —
//! the first `GetRealtimeLogConfig` on a stack output answers `NoSuchRealtimeLogConfig`,
//! blaming the client for a value the deployment itself produced, and a distribution that
//! attaches the configuration is refused `NoSuchRealtimeLogConfig` in turn, so the real-time
//! logging the template asked for never happens. Calling that service's own create is what
//! makes the ARN the stack publishes one CloudFront really holds.
//!
//! ## The declaration
//!
//! The type publishes exactly the four members `CreateRealtimeLogConfig` takes
//! ([`PROPERTIES`]), each spelled as that operation spells it, so the declaration is rendered
//! as the operation's own request document through the rendering the catalogue in [`super`]
//! uses — the two lists under the element names the model's own list members carry
//! ([`super::WRAPPED_LISTS`]), and a Kinesis endpoint's two ARNs under the initialisms the API
//! writes them with ([`super::RENAMED_MEMBERS`]). All four members are `@required` by the API
//! and required by the type, so a declaration that states none of one is refused naming it
//! rather than sent without it.
//!
//! The bounds the TYPE'S SCHEMA publishes for a property — above all the sampling rate, "a
//! whole number between 1 and 100 (inclusive)" — are judged before this module calls anybody,
//! by the provider's own model validation ([`super::super::model_validation`]), which fails the
//! RESOURCE with the schema validator's sentence and asks the owning service nothing. Every
//! other bound of the declaration is that service's to enforce: the create is made, and the
//! refusal it answers is what fails the resource — so either way a client sees a
//! resource-handler outcome rather than a template-syntax error.
//!
//! ## Deleting one
//!
//! `DeleteRealtimeLogConfig` takes the name or the ARN of the configuration to remove, and
//! this provider holds the ARN as the resource's physical id, so the delete addresses exactly
//! the configuration the stack created. A configuration CloudFront no longer holds is already
//! in the state the delete asks for, judged on the CODE and never on a message.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{Created, Refusal, Removal, StoredResource};

/// The resource type this module provisions.
pub(crate) const CLOUDFRONT_REALTIME_LOG_CONFIG: &str =
    crate::state::template::CLOUDFRONT_REALTIME_LOG_CONFIG;

/// Every property the type publishes, in the order `CreateRealtimeLogConfigRequest` declares
/// its members (`smithy_query(query="shape",shape="CreateRealtimeLogConfigRequest")`).
///
/// This is both the ORDER the request document writes them in and the set of members a
/// declaration may state; a declared property this list does not carry is one no call here
/// applies.
const PROPERTIES: &[&str] = &["EndPoints", "Fields", "Name", "SamplingRate"];

/// The attribute the type publishes: the ARN of the configuration, which is also what its
/// `Ref` returns.
pub(crate) const ARN_ATTRIBUTE: &str = "Arn";

/// The member CloudFront reports that ARN under (`RealtimeLogConfig$ARN`).
const REPORTED_ARN: &str = "ARN";

/// The code CloudFront answers for a real-time log configuration this account does not hold,
/// which is what says a delete has nothing left to remove.
const NO_SUCH_REALTIME_LOG_CONFIG: &str = "NoSuchRealtimeLogConfig";

/// The URI `CreateRealtimeLogConfig` (`POST`) and `UpdateRealtimeLogConfig` (`PUT`) are bound
/// to, under this service's API-version prefix.
const REALTIME_LOG_CONFIG_COLLECTION: &str = "/realtime-log-config";

/// The URI `DeleteRealtimeLogConfig` is bound to: a `POST` of its own, the configuration to
/// remove named in the request's body rather than in the path.
const DELETE_REALTIME_LOG_CONFIG: &str = "/delete-realtime-log-config";

/// Create the real-time log configuration an `AWS::CloudFront::RealtimeLogConfig` declares.
///
/// `Err` is CloudFront's own refusal of the create: a sampling rate outside the documented
/// percentage range, a list that names nothing, an endpoint that names no Kinesis data stream,
/// a name another configuration of the account already holds.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let body = request_document(CREATE_REQUEST, &resource.properties, resource)?;
    let answered = super::call_cloudfront(
        ctx,
        region,
        "POST",
        REALTIME_LOG_CONFIG_COLLECTION,
        Some(&body),
        None,
    )
    .await?;
    let arn = reported_arn(&answered, "CreateRealtimeLogConfig")?;
    Ok(Created {
        physical_resource_id: Some(arn.clone()),
        attributes: attributes(&arn),
    })
}

/// Apply a CHANGED declaration to the configuration the stack already has, through
/// `UpdateRealtimeLogConfig` — the call that takes the configuration's name together with the
/// parameters to change, so it keeps its identity and nothing is replaced.
///
/// A changed `Name` never reaches here: it is the property the resource takes its identity
/// from, which recreates the resource
/// ([`crate::state::template::name_is_create_only`]).
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    let body = request_document(UPDATE_REQUEST, declared, resource)?;
    let answered = super::call_cloudfront(
        ctx,
        region,
        "PUT",
        REALTIME_LOG_CONFIG_COLLECTION,
        Some(&body),
        None,
    )
    .await?;
    let arn = reported_arn(&answered, "UpdateRealtimeLogConfig")?;
    Ok(Some(Created {
        physical_resource_id: None,
        attributes: attributes(&arn),
    }))
}

/// Delete the configuration a stack created, addressed by the ARN CloudFront minted for it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    remove(ctx, region, &resource.physical_resource_id)
        .await
        .map(|_| ())
}

/// Ask CloudFront to take the configuration away, and report whether it still holds it.
///
/// A configuration CloudFront no longer holds is already in the state the delete asks for,
/// judged on the CODE and never on a message. Every other refusal is that service's own and
/// travels as it is — a configuration a distribution still attaches is `RealtimeLogConfigInUse`,
/// which is a delete that cannot be made rather than one that is still being made.
pub(super) async fn remove(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    arn: &str,
) -> Result<Removal, Refusal> {
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    let body = format!(
        "<{DELETE_REQUEST} xmlns=\"{namespace}\">{}</{DELETE_REQUEST}>",
        super::xml_leaf(REPORTED_ARN, arn)
    );
    match super::call_cloudfront(
        ctx,
        region,
        "POST",
        DELETE_REALTIME_LOG_CONFIG,
        Some(&body),
        None,
    )
    .await
    {
        Ok(_) => Ok(Removal::Gone),
        Err(refusal) if refusal.code == NO_SUCH_REALTIME_LOG_CONFIG => Ok(Removal::Gone),
        Err(refusal) => Err(refusal),
    }
}

/// The root element of each of the three request documents, as the API reference names them:
/// none of these operations binds a member to the URI or to a header, so every member travels
/// in the body under the root element the operation's own request shape is.
const CREATE_REQUEST: &str = "CreateRealtimeLogConfigRequest";
const UPDATE_REQUEST: &str = "UpdateRealtimeLogConfigRequest";
const DELETE_REQUEST: &str = "DeleteRealtimeLogConfigRequest";

/// The request document a create or an update sends: the four declared members, in the order
/// the request shape declares them, under the root element `root` names.
fn request_document(
    root: &str,
    properties: &Value,
    resource: &StoredResource,
) -> Result<String, Refusal> {
    super::refuse_unapplied_properties(properties, &resource.resource_type, PROPERTIES)?;
    super::refuse_unrenderable_members(properties, &resource.resource_type, 0)?;
    let empty = Map::new();
    let declared = properties.as_object().unwrap_or(&empty);
    let mut rendered = String::new();
    for member in PROPERTIES {
        // Every member of this request is `@required`, and none of them has a documented
        // value the handler could supply for a declaration that states it nowhere: such a
        // declaration is refused naming the member rather than sent without it.
        let Some(value) = declared.get(*member).filter(|value| !value.is_null()) else {
            return Err(missing(member, resource));
        };
        rendered.push_str(&super::rendered_member(
            member,
            value,
            declared,
            &resource.resource_type,
        ));
    }
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    Ok(format!(
        "<{root} xmlns=\"{namespace}\">{rendered}</{root}>"
    ))
}

/// The ARN a `RealtimeLogConfig` answer reports for the configuration, or the refusal for an
/// answer that identifies nothing: a resource whose identity nothing reported is not one a
/// stack can publish.
fn reported_arn(answered: &str, operation: &str) -> Result<String, Refusal> {
    super::xml_scalar_children(answered, "RealtimeLogConfig")
        .get(REPORTED_ARN)
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .map(str::to_string)
        .ok_or_else(|| Refusal {
            code: "UnidentifiedFailure".to_string(),
            message: format!("{operation} answered no RealtimeLogConfig.{REPORTED_ARN}"),
        })
}

/// The one attribute the type publishes, from the ARN CloudFront reported.
fn attributes(arn: &str) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.to_string());
    attributes
}

/// The refusal for a declaration that states none of a property no call here can be built
/// without, naming which one it was.
fn missing(property: &str, resource: &StoredResource) -> Refusal {
    Refusal {
        code: super::MISSING_PROPERTY.to_string(),
        message: format!(
            "Property {property} cannot be empty for resource of type {}",
            resource.resource_type
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The record a declaration is rendered against: only its type and its properties are
    /// read off here.
    fn resource(properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "TheConfig".to_string(),
            resource_type: CLOUDFRONT_REALTIME_LOG_CONFIG.to_string(),
            physical_resource_id: String::new(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// One well-formed declaration, as a template writes it: the name, the sampling rate, the
    /// fields and one Kinesis endpoint naming a stream and the role CloudFront writes to it
    /// with (the property spellings the resource reference publishes, `RoleArn` and
    /// `StreamArn`).
    fn declared() -> Value {
        ::serde_json::json!({
            "Name": "edge-logs",
            "SamplingRate": 100,
            "Fields": ["timestamp", "c-ip"],
            "EndPoints": [{
                "StreamType": "Kinesis",
                "KinesisStreamConfig": {
                    "RoleArn": "arn:aws:iam::000000000000:role/cf-logs",
                    "StreamArn": "arn:aws:kinesis:us-east-1:000000000000:stream/edge",
                },
            }],
        })
    }

    /// The document a create sends is the operation's own request: the four members in the
    /// order the request shape declares them, the two lists under the element names that
    /// model's own list members carry, and the endpoint's two ARNs under the API's
    /// initialisms.
    ///
    /// The members of one declared STRUCTURE are rendered in the order the loaded document
    /// holds them, which depends on whether anything in the build enables `serde_json`'s
    /// `preserve_order` feature — so what is asserted of the endpoint is the fragments it is
    /// built from rather than one of the two orders they can come in.
    #[test]
    fn the_create_renders_the_operations_own_request_document() {
        let rendered = request_document(CREATE_REQUEST, &declared(), &resource(declared()))
            .expect("case {a declaration of every member}: expected a request document");
        let namespace = super::super::CLOUDFRONT_XML_NAMESPACE;
        assert!(
            rendered.starts_with(&format!("<{CREATE_REQUEST} xmlns=\"{namespace}\">"))
                && rendered.ends_with(&format!("</{CREATE_REQUEST}>")),
            "case {{a declaration of every member}}: expected the operation's own request \
             element, got {rendered}"
        );
        for fragment in [
            // The two lists, each under the element name its own model member carries.
            "<EndPoints><member>",
            "</member></EndPoints>",
            "<Fields><Field>timestamp</Field><Field>c-ip</Field></Fields>",
            // The endpoint, with the two ARNs renamed to the initialisms the API takes.
            "<StreamType>Kinesis</StreamType>",
            "<KinesisStreamConfig>",
            "<RoleARN>arn:aws:iam::000000000000:role/cf-logs</RoleARN>",
            "<StreamARN>arn:aws:kinesis:us-east-1:000000000000:stream/edge</StreamARN>",
            "<Name>edge-logs</Name>",
            "<SamplingRate>100</SamplingRate>",
        ] {
            assert!(
                rendered.contains(fragment),
                "case {fragment:?}: expected the request document to carry it, got {rendered}"
            );
        }
        // The four members travel in the order the request shape declares them.
        let positions: Vec<usize> = PROPERTIES
            .iter()
            .map(|member| {
                rendered
                    .find(&format!("<{member}>"))
                    .unwrap_or_else(|| panic!("case {member:?}: expected it in {rendered}"))
            })
            .collect();
        assert!(
            positions.windows(2).all(|pair| pair[0] < pair[1]),
            "case {{the member order}}: expected {PROPERTIES:?} in that order, got {rendered}"
        );
    }

    /// A declaration that states none of a `@required` member is refused naming that member,
    /// rather than sent to CloudFront without it.
    #[test]
    fn a_declaration_missing_a_required_member_is_refused_naming_it() {
        for member in PROPERTIES {
            let mut properties = declared();
            properties
                .as_object_mut()
                .expect("the declaration above is an object")
                .remove(*member);
            let refused = request_document(CREATE_REQUEST, &properties, &resource(properties.clone()))
                .expect_err("expected the refusal of a declaration missing a required member");
            assert_eq!(
                refused.message,
                format!(
                    "Property {member} cannot be empty for resource of type \
                     {CLOUDFRONT_REALTIME_LOG_CONFIG}"
                ),
                "case {member:?}: expected the refusal to name the missing property, got \
                 {refused:?}"
            );
        }
    }

    /// A declared property the type does not publish is refused rather than dropped: a
    /// configuration reported created with less than its declaration asked for is the shallow
    /// success this module exists to avoid.
    #[test]
    fn a_property_the_type_does_not_publish_is_refused() {
        let mut properties = declared();
        properties
            .as_object_mut()
            .expect("the declaration above is an object")
            .insert("Tags".to_string(), ::serde_json::json!([]));
        let refused = request_document(CREATE_REQUEST, &properties, &resource(properties.clone()))
            .expect_err("expected the refusal of a property the type does not publish");
        assert!(
            refused.message.contains("Tags"),
            "case {{a property the type does not publish}}: expected the refusal to name it, \
             got {refused:?}"
        );
    }

    /// The ARN is read off the answer's own `RealtimeLogConfig`, and an answer that identifies
    /// nothing is a refusal rather than an empty identity.
    #[test]
    fn the_identity_is_the_arn_the_answer_reports() {
        let expected = "arn:aws:cloudfront::000000000000:realtime-log-config/edge-logs";
        let answered = format!(
            "<CreateRealtimeLogConfigResult><RealtimeLogConfig><ARN>{expected}</ARN>\
             <Name>edge-logs</Name></RealtimeLogConfig></CreateRealtimeLogConfigResult>"
        );
        let read = reported_arn(&answered, "CreateRealtimeLogConfig")
            .expect("case {an answer reporting the configuration}: expected its ARN");
        assert_eq!(
            read, expected,
            "case {{an answer reporting the configuration}}: expected {expected}, got {read}"
        );
        let silent = "<CreateRealtimeLogConfigResult><RealtimeLogConfig>\
             <Name>edge-logs</Name></RealtimeLogConfig></CreateRealtimeLogConfigResult>";
        assert!(
            reported_arn(silent, "CreateRealtimeLogConfig").is_err(),
            "case {{an answer identifying nothing}}: expected a refusal, got an identity"
        );
    }
}
