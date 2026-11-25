// SPDX-License-Identifier: CC0-1.0

//! `AWS::CloudFront::VpcOrigin`: the VPC origin a stack declares so a distribution can read a
//! private load balancer or instance inside a VPC, created in Amazon CloudFront itself.
//!
//! ## Why it is really created
//!
//! A VPC origin's identity is the `vo_…` id CloudFront MINTS for it: `GetVpcOrigin`,
//! `UpdateVpcOrigin`, `DeleteVpcOrigin` and an origin's own `VpcOriginConfig$VpcOriginId` all
//! address one by that id, and the ARN built around it is what the resource-policy surface
//! shares. So a stack that reported a VPC origin created while nothing created it publishes an
//! id and an ARN that resolve to nothing — the first `GetVpcOrigin` on a stack output answers
//! `EntityNotFound`, blaming the client for an id the deployment itself produced, and the
//! distribution that was meant to read through the origin can never be wired to it. Calling
//! that service's own create is what makes the identity the stack publishes one CloudFront
//! really holds.
//!
//! ## The declaration, and the one configuration
//!
//! The type publishes two properties — the `VpcOriginEndpointConfig` that IS the create's
//! configuration and the `Tags` its create carries — so the declaration is rendered as the
//! members of that configuration ([`ENDPOINT_CONFIG_MEMBERS`]) inside the operation's own
//! request shape, through the same rendering the catalogue in [`super`] uses. Two members the
//! API declares `@required` are optional in a template and carry a DOCUMENTED default, which
//! is what this module supplies for a declaration that states neither
//! ([`DOCUMENTED_PORTS`]); the protocol policy has no such documented value, so a declaration
//! that omits it is refused naming the member rather than sent at a value this emulator chose.
//!
//! ## Deleting one
//!
//! `DeleteVpcOrigin` takes the version CloudFront reports the origin under, so the delete reads
//! that version first, and the service refuses the delete of an origin whose endpoint has not
//! reached the edge locations yet ([`ILLEGAL_DELETE`]) — which is not a failure of the
//! deletion but the deletion not being possible yet, so it is reported as a removal still in
//! progress and asked for again ([`remove`]), the pattern
//! [`super::super::provisioner::Provisioner::removal`] documents.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{Created, Refusal, Removal, StoredResource};

/// The resource type this module provisions.
pub(crate) const CLOUDFRONT_VPC_ORIGIN: &str = crate::state::template::CLOUDFRONT_VPC_ORIGIN;

/// Every property the type publishes: the endpoint configuration its create takes as the
/// request's configuration, and the tags that create carries. A declared property this list
/// does not carry is one no call here applies.
const PROPERTIES: &[&str] = &[ENDPOINT_CONFIG_PROPERTY, "Tags"];

/// The property the endpoint configuration is declared under, which is also the member
/// `CreateVpcOrigin` takes it as (`smithy_query(query="operation",operation="CreateVpcOrigin")`).
const ENDPOINT_CONFIG_PROPERTY: &str = "VpcOriginEndpointConfig";

/// The members `VpcOriginEndpointConfig` publishes, in the order that shape declares them
/// (`smithy_query(query="shape",shape="VpcOriginEndpointConfig")`).
///
/// This is both the ORDER the request document writes them in and the set of members a
/// declaration may state. The template spells each one as the API does, except the list of SSL
/// protocols, whose initialism the CloudFormation resource reference writes `OriginSSLProtocols`
/// — the spelling an `AWS::CloudFront::Distribution`'s own `CustomOriginConfig` uses, resolved to
/// the API's member by [`super::RENAMED_MEMBERS`].
const ENDPOINT_CONFIG_MEMBERS: &[&str] = &[
    "Name",
    "Arn",
    "HTTPPort",
    "HTTPSPort",
    "OriginProtocolPolicy",
    "OriginSSLProtocols",
    "IpAddressType",
];

/// The two ports an endpoint is reached on when its declaration names neither: the defaults
/// CloudFront's own model documents for them — `VpcOriginEndpointConfig$HTTPPort` "The default
/// value is `80`" and `$HTTPSPort` "The default value is `443`"
/// (`smithy_query(query="shape",shape="VpcOriginEndpointConfig")`) — while both members are
/// `@required` on the wire, so the handler is what supplies them for a template that states
/// only the endpoint's ARN and name.
const DOCUMENTED_PORTS: &[(&str, &str)] = &[("HTTPPort", "80"), ("HTTPSPort", "443")];

/// The code CloudFront answers for a VPC origin this account does not hold, which is what says
/// a delete has nothing left to remove: "The entity was not found."
const ENTITY_NOT_FOUND: &str = "EntityNotFound";

/// The code CloudFront answers a delete of a VPC origin whose endpoint has not been deployed to
/// the edge locations yet: `IllegalDelete`, the modelled refusal of a delete that is not allowed
/// for the entity — a condition the CLOCK of that service owes away, so the deletion is asked
/// for again until it does.
const ILLEGAL_DELETE: &str = "IllegalDelete";

/// Create the VPC origin an `AWS::CloudFront::VpcOrigin` declares.
///
/// The tags the declaration asks for and the ones CloudFormation gives the resource travel on
/// the create itself: `CreateVpcOrigin` takes the configuration and the tag list as one
/// request, which is the one call that makes a tagged VPC origin in a single step.
///
/// `Err` is CloudFront's own refusal of the create: an endpoint ARN that names no resource a
/// VPC origin can front, a name another origin of the account already holds, an
/// `OriginSslProtocols` whose stated quantity disagrees with its items.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let body = request_document(&resource.properties, resource, propagated)?;
    let answered = super::call_cloudfront(ctx, region, "POST", VPC_ORIGIN_COLLECTION, Some(&body), None)
        .await?;
    let attributes = attributes(&answered);
    let id = attributes.get("Id").cloned().ok_or_else(|| Refusal {
        code: "UnidentifiedFailure".to_string(),
        message: "CreateVpcOrigin answered no VpcOrigin.Id".to_string(),
    })?;
    Ok(Created {
        physical_resource_id: Some(id),
        attributes,
    })
}

/// Apply a CHANGED declaration to the VPC origin the stack already has.
///
/// `Ok(None)` — the change reported as NOT applied — because the call that would carry it,
/// `UpdateVpcOrigin`, is still a scaffold stub in the service that owns the type
/// (`services/service-com-amazonaws-cloudfront-cloudfront2020-05-31/src/ops/update_vpc_origin.rs`,
/// which has a stub issue of its own): there is no call here that could change the endpoint
/// configuration of the origin that exists, and reporting the resource updated while nothing
/// changed it would publish a configuration CloudFront does not hold.
pub(super) fn update() -> Option<Created> {
    None
}

/// Delete the VPC origin a stack created, addressed by the id CloudFront minted for it.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    remove(ctx, region, &resource.physical_resource_id)
        .await
        .map(|_| ())
}

/// Ask CloudFront to take the VPC origin away, and report whether it still holds it.
///
/// One function for the delete and for the wait that follows it, because the work is the same
/// at both: the version the origin is reported under is read, the deletion is asked for
/// conditional on it, and the refusal that says the endpoint has not reached the edge locations
/// yet ([`ILLEGAL_DELETE`]) is not a failure of the deletion — it is the deletion not being
/// possible yet, so what is reported is the state CloudFront has the origin in now.
///
/// A VPC origin CloudFront no longer holds is already in the state the delete asks for, judged
/// on the CODE and never on a message.
pub(super) async fn remove(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    id: &str,
) -> Result<Removal, Refusal> {
    let path = vpc_origin_path(id);
    let (reported, version) = match super::read_version(ctx, region, &path).await {
        Ok(read) => read,
        Err(refusal) if refusal.code == ENTITY_NOT_FOUND => return Ok(Removal::Gone),
        Err(refusal) => return Err(refusal),
    };
    match super::call_cloudfront(ctx, region, "DELETE", &path, None, version.as_deref()).await {
        Ok(_) => Ok(Removal::Gone),
        Err(refusal) if refusal.code == ENTITY_NOT_FOUND => Ok(Removal::Gone),
        Err(refusal) if refusal.code == ILLEGAL_DELETE => Ok(Removal::Pending(reported_status(
            &reported,
        ))),
        Err(refusal) => Err(refusal),
    }
}

/// The `CreateVpcOriginRequest` document a create sends: the declared endpoint configuration and
/// the tag list, under the root element the operation's request shape is.
fn request_document(
    properties: &Value,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<String, Refusal> {
    super::refuse_unapplied_properties(properties, &resource.resource_type, PROPERTIES)?;
    super::refuse_unrenderable_members(properties, &resource.resource_type, 0)?;
    let config = endpoint_config_element(properties, resource)?;
    let tags = tags_element(properties, propagated);
    let namespace = super::CLOUDFRONT_XML_NAMESPACE;
    Ok(format!(
        "<CreateVpcOriginRequest xmlns=\"{namespace}\">{config}{tags}</CreateVpcOriginRequest>"
    ))
}

/// The `VpcOriginEndpointConfig` element a create sends: the members the declaration states, in
/// the order the shape declares them, plus the documented default of a `@required` member the
/// declaration left out ([`DOCUMENTED_PORTS`]).
fn endpoint_config_element(
    properties: &Value,
    resource: &StoredResource,
) -> Result<String, Refusal> {
    let declared = properties
        .get(ENDPOINT_CONFIG_PROPERTY)
        .and_then(Value::as_object)
        .ok_or_else(|| missing(ENDPOINT_CONFIG_PROPERTY, resource))?;
    super::refuse_unapplied_members(declared, ENDPOINT_CONFIG_PROPERTY, ENDPOINT_CONFIG_MEMBERS)?;
    // `OriginProtocolPolicy` is `@required` by the API and published as a property this
    // emulator has no documented value for, so the create's request cannot be built without
    // it: the declaration is refused naming the member rather than sent at a value nothing
    // states.
    for member in REQUIRED_CONFIG_MEMBERS {
        if declared
            .get(*member)
            .filter(|value| !value.is_null())
            .is_none()
        {
            return Err(missing(
                &format!("{ENDPOINT_CONFIG_PROPERTY}.{member}"),
                resource,
            ));
        }
    }
    let mut rendered = String::new();
    for member in ENDPOINT_CONFIG_MEMBERS {
        match declared.get(*member).filter(|value| !value.is_null()) {
            Some(value) => rendered.push_str(&super::rendered_member(
                member,
                value,
                declared,
                ENDPOINT_CONFIG_PROPERTY,
            )),
            None => {
                if let Some((_, default)) =
                    DOCUMENTED_PORTS.iter().find(|(port, _)| port == member)
                {
                    rendered.push_str(&super::xml_leaf(member, default));
                }
            }
        }
    }
    Ok(format!(
        "<{ENDPOINT_CONFIG_PROPERTY}>{rendered}</{ENDPOINT_CONFIG_PROPERTY}>"
    ))
}

/// The members of the configuration this module cannot build the create's request without: the
/// ones the API declares `@required` that neither the declaration's own defaults nor a
/// documented one supplies.
const REQUIRED_CONFIG_MEMBERS: &[&str] = &["Name", "Arn", "OriginProtocolPolicy"];

/// The `Tags` element a create carries: the tags the declaration asks for and the ones
/// CloudFormation propagates to the resource, in the container the model wraps a tag list in.
fn tags_element(properties: &Value, propagated: &[(String, String)]) -> String {
    let tags = super::tags_of(properties, propagated);
    if tags.is_empty() {
        return String::new();
    }
    let items: String = tags
        .iter()
        .map(|(key, value)| {
            format!(
                "<Tag>{}{}</Tag>",
                super::xml_leaf("Key", key),
                super::xml_leaf("Value", value)
            )
        })
        .collect();
    format!("<Tags><Items>{items}</Items></Tags>")
}

/// The attributes a `VpcOrigin` answer reports for the resource: the ARN and the id CloudFront
/// minted, the status it reports the origin in and the two instants of its own clock — which
/// are the attributes `AWS::CloudFront::VpcOrigin` publishes.
fn attributes(answered: &str) -> BTreeMap<String, String> {
    let reported = super::xml_scalar_children(answered, "VpcOrigin");
    let mut attributes = BTreeMap::new();
    for attribute in ["Arn", "CreatedTime", "Id", "LastModifiedTime", "Status"] {
        if let Some(value) = reported
            .get(attribute)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            attributes.insert(
                attribute.to_string(),
                super::published_attribute(attribute, value),
            );
        }
    }
    attributes
}

/// The status CloudFront reports the origin in, read off its own answer, or the empty string
/// for an answer that states none: what a removal still in progress is reported as.
fn reported_status(reported: &str) -> String {
    super::xml_scalar_children(reported, "VpcOrigin")
        .get("Status")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The URI the per-origin operations are bound to (`/2020-05-31/vpc-origin/{Id}`), with the id
/// written as ONE path segment so an id outside the modelled form is refused by that
/// operation's validation instead of addressing a different origin.
fn vpc_origin_path(id: &str) -> String {
    format!("{VPC_ORIGIN_COLLECTION}/{}", super::percent_encoded(id))
}

/// The URI `CreateVpcOrigin` is bound to, under this service's API-version prefix.
const VPC_ORIGIN_COLLECTION: &str = "/vpc-origin";

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
