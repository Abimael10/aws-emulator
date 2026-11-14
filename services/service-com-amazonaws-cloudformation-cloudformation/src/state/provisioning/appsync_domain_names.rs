// SPDX-License-Identifier: CC0-1.0

//! `AWS::AppSync::DomainName` and `AWS::AppSync::DomainNameApiAssociation`: the custom
//! domain a stack declares for an AppSync API, and the mapping of an API onto it, created
//! in AppSync itself.
//!
//! This is the per-type half of provisioning for two resource types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::iot_authorizer`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! A custom domain is a handle every other AppSync call about one takes: `GetDomainName`,
//! `UpdateDomainName`, `AssociateApi`, `GetApiAssociation`, `DisassociateApi` and
//! `DeleteDomainName` each address a domain by the NAME the template stated, and each
//! answers `NotFoundException` for a name the account holds no domain under. So a stack that
//! reported a domain created while nothing created it publishes a name that resolves to
//! nothing — no GraphQL request could be served on it, the mapping the next template
//! declares on it cannot be made, and a second stack declaring the same name would be
//! accepted where AppSync refuses it. The mapping is the same case one level down: it exists
//! ON the domain that holds it, and it is what makes the declared API answer on that domain,
//! so a stack that reported one created while nothing associated it leaves a domain
//! `GetApiAssociation` reports nothing for.
//!
//! ## The properties
//!
//! `AWS::AppSync::DomainName`'s properties are `CreateDomainName`'s own members with an
//! upper-case initial — `CertificateArn`, `Description` and `Tags` — beside the `DomainName`
//! the domain is created UNDER, which is this resource's physical id and what its `Ref`
//! resolves to. `AWS::AppSync::DomainNameApiAssociation`'s are `AssociateApi`'s: the `ApiId`
//! the call carries, beside the `DomainName` that call addresses, which is the only
//! identifier AppSync knows an association by — every operation on one binds that domain
//! name as its `@httpLabel` and the service publishes no identifier of its own — so it is
//! this resource's physical id and the `ApiAssociationIdentifier` the type publishes.
//!
//! A declared property these arms do not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    applicable_tags, call_rest_json, percent_encoded, Created, Readiness, Refusal, Removal,
};
use crate::state::provisioner::{tags_as_map, tags_of};
use crate::state::StoredResource;

/// The resource types this module provisions.
pub(crate) const APPSYNC_DOMAIN_NAME: &str = "AWS::AppSync::DomainName";
pub(crate) const APPSYNC_DOMAIN_NAME_API_ASSOCIATION: &str =
    "AWS::AppSync::DomainNameApiAssociation";

/// The SigV4 signing name of the service that owns them, which is what routes the in-process
/// call to AppSync's own handlers (that model's `sigv4Name`/`endpointPrefix`,
/// `smithy_query(query="service_info")`). It speaks `restJson1`, so each call is a method
/// and a URI rather than an `X-Amz-Target`.
const APPSYNC_SIGNING_NAME: &str = "appsync";

/// The collection `CreateDomainName` is bound to (`POST /v1/domainnames`); one domain is
/// addressed under it by the name it was created with
/// (`GET|POST|DELETE /v1/domainnames/{domainName}`), and its mapping onto an API by the
/// segment below (`POST|GET|DELETE /v1/domainnames/{domainName}/apiassociation`).
const DOMAIN_NAMES_URI: &str = "/v1/domainnames";
const API_ASSOCIATION_SEGMENT: &str = "apiassociation";

/// The error code AppSync answers for a domain — or a mapping — it does not hold
/// (`NotFoundException`, the modelled 404 of its reads, its associate and its deletes),
/// which is what says a delete has nothing left to remove and a read nothing to report.
const APPSYNC_NOT_FOUND: &str = "NotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// Which of the two types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::AppSync::DomainName`.
    DomainName,
    /// `AWS::AppSync::DomainNameApiAssociation`.
    ApiAssociation,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        APPSYNC_DOMAIN_NAME => Some(Kind::DomainName),
        APPSYNC_DOMAIN_NAME_API_ASSOCIATION => Some(Kind::ApiAssociation),
        _ => None,
    }
}

/// The property naming the domain, which is the name a domain is created under and the
/// domain a mapping is made on — in both cases the resource's physical id rather than a
/// member of a request body.
const DOMAIN_NAME_PROPERTY: &str = "DomainName";

/// Every property `AWS::AppSync::DomainName` publishes: the two members
/// `CreateDomainName` carries beside the name, the name itself, and the tags the domain
/// carries.
const DOMAIN_NAME_PROPERTIES: &[&str] = &["CertificateArn", "Description", "DomainName", "Tags"];

/// Every property `AWS::AppSync::DomainNameApiAssociation` publishes: the API the mapping
/// points at, and the domain it is made on.
const API_ASSOCIATION_PROPERTIES: &[&str] = &["ApiId", "DomainName"];

/// The properties of this type that are MEMBERS of `CreateDomainName`, paired with the
/// member each is carried as — the property with a lower-cased initial.
const DOMAIN_NAME_MEMBERS: &[(&str, &str)] = &[
    ("CertificateArn", "certificateArn"),
    ("Description", "description"),
];

/// The property and the member naming the API a mapping points at, which `AssociateApi`
/// declares required.
const API_ID_PROPERTY: &str = "ApiId";
const API_ID_MEMBER: &str = "apiId";

/// The member `CreateDomainName` takes the domain's name as, which is the name the resource
/// is created under rather than a value the declaration adds to the body.
const DOMAIN_NAME_MEMBER: &str = "domainName";

/// The member every answer about a domain carries its configuration in
/// (`CreateDomainName`/`GetDomainName`/`UpdateDomainName`'s `domainNameConfig`), and the
/// one every answer about a mapping carries the mapping in (`AssociateApi`'s and
/// `GetApiAssociation`'s `apiAssociation`).
const DOMAIN_NAME_CONFIG_MEMBER: &str = "domainNameConfig";
const API_ASSOCIATION_MEMBER: &str = "apiAssociation";

/// The attributes `AWS::AppSync::DomainName` publishes through `Fn::GetAtt`, as
/// `(attribute, the member AppSync answers it in)`: the endpoint AppSync serves the domain
/// through, the Route 53 hosted zone that endpoint lives in, and the ARN AppSync names the
/// domain by.
///
/// Each is read off that service's own answer, so what the stack publishes is the endpoint
/// AppSync really assigned — never a host or a zone computed here, which is a value a caller
/// could put in a real DNS alias record.
const DOMAIN_NAME_ATTRIBUTES: &[(&str, &str)] = &[
    ("AppSyncDomainName", "appsyncDomainName"),
    ("DomainNameArn", "domainNameArn"),
    ("HostedZoneId", "hostedZoneId"),
];

/// The attribute `AWS::AppSync::DomainNameApiAssociation` publishes, read off the domain
/// name AppSync reports the mapping under: the association's own identifier.
const API_ASSOCIATION_IDENTIFIER_ATTRIBUTE: &str = "ApiAssociationIdentifier";

/// The member a reported mapping carries its status in, and the two values of
/// `AssociationStatus` that are not terminal states of a created mapping: "PROCESSING: The
/// API association is being created" is the state it comes up in, and "FAILED: The API
/// association has failed" the state no wait brings it out of.
const ASSOCIATION_STATUS_MEMBER: &str = "associationStatus";
const ASSOCIATION_PROCESSING: &str = "PROCESSING";
const ASSOCIATION_SUCCESS: &str = "SUCCESS";

/// The member a reported mapping carries the detail of its last deployment in, which is what
/// a mapping that failed has to say about it.
const DEPLOYMENT_DETAIL_MEMBER: &str = "deploymentDetail";

/// Create the resource one of these two types declares, in AppSync itself — so a client that
/// deployed the stack can pass `!Ref <domain>` straight to `GetDomainName` or
/// `AssociateApi`, and read the mapping back with `GetApiAssociation`.
///
/// `Err` is that service's own refusal of the create: a certificate it will not serve the
/// name with, a domain name the account already holds, an `apiId` that resolves to no API.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::DomainName => create_domain_name(ctx, region, resource, propagated).await,
        // A mapping is not taggable: AppSync's tagging calls address a resource by ARN and an
        // association has none, and the type publishes no `Tags` property at all, so
        // CloudFormation propagates no tag to one.
        Kind::ApiAssociation => create_api_association(ctx, region, resource).await,
    }
}

/// Create the custom domain an `AWS::AppSync::DomainName` declares.
///
/// The declared properties ARE `CreateDomainName`'s members once their initials are
/// lower-cased, with the name the domain is created under supplied by CloudFormation: it is
/// this resource's physical id, which the naming rules read off the declared `DomainName`.
/// The certificate is required by that call and by the type, so a declaration naming none is
/// refused here rather than sent as a request the owning service would reject.
async fn create_domain_name(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        DOMAIN_NAME_PROPERTIES,
    )?;
    let mut request = declared_members(&resource.properties, DOMAIN_NAME_MEMBERS);
    if !request.contains_key(DOMAIN_NAME_MEMBERS[0].1) {
        return Err(missing_property(
            &resource.resource_type,
            DOMAIN_NAME_MEMBERS[0].0,
        ));
    }
    request.insert(
        DOMAIN_NAME_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    // The domain carries the tags its declaration asks for plus the ones CloudFormation gives
    // the resource, as the `TagMap` `CreateDomainName`'s own `tags` member takes — less the
    // keys in AWS's reserved namespace, which `TagKey`'s own pattern refuses (`^(?!aws:)`), so
    // the three records CloudFormation keeps of its own accord are not sent to be refused.
    let tags = tags_as_map(&applicable_tags(&tags_of(&resource.properties, propagated)));
    if !tags.is_empty() {
        request.insert("tags".to_string(), Value::Object(tags));
    }
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        DOMAIN_NAMES_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    // The domain was created under the name CloudFormation decided, so the computed physical
    // id is the one it answers to; what the create adds is the endpoint, the zone and the ARN
    // AppSync assigned it.
    Ok(Created::attributes(domain_name_attributes(&answered)))
}

/// Map the API an `AWS::AppSync::DomainNameApiAssociation` declares onto the domain it names,
/// through `AssociateApi` — the one call that makes a mapping.
///
/// The API is required by that call and by the type; the domain is the call's own path label,
/// which is this resource's physical id ([`DOMAIN_NAME_PROPERTY`]), so a declaration naming
/// no domain describes no mapping that could be made and is refused here.
async fn create_api_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        API_ASSOCIATION_PROPERTIES,
    )?;
    if resource.properties.get(DOMAIN_NAME_PROPERTY).is_none() {
        return Err(missing_property(&resource.resource_type, DOMAIN_NAME_PROPERTY));
    }
    let request = associate_request(&resource.properties, &resource.resource_type)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &api_association_uri(&resource.physical_resource_id),
        Some(&request),
    )
    .await?;
    Ok(Created::attributes(api_association_attributes(&answered)))
}

/// Whether AppSync reports the created resource ready.
///
/// A DOMAIN exists from the moment `CreateDomainName` answers: that answer carries the
/// configuration every later read reports, and the service publishes no status a fresh domain
/// settles out of. A MAPPING does not — `AssociationStatus` documents "PROCESSING: The API
/// association is being created. You cannot modify association requests during processing" —
/// so the resource's creation is held open until AppSync reports it `SUCCESS`.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    match kind {
        Kind::DomainName => Ok(Readiness::Ready),
        Kind::ApiAssociation => api_association_readiness(ctx, region, physical_resource_id).await,
    }
}

/// Whether AppSync has finished deploying the mapping the stack asked for, read off
/// `GetApiAssociation`'s own report of it.
///
/// A mapping that service no longer holds is not one a wait brings up: the association the
/// create made is gone, so the resource's creation cannot finish.
async fn api_association_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_api_association(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AppSync holds no API association for the custom domain name \
             {physical_resource_id}"
        )));
    };
    let status = text_member(&reported, ASSOCIATION_STATUS_MEMBER).unwrap_or_default();
    if status == ASSOCIATION_SUCCESS {
        return Ok(Readiness::Ready);
    }
    if status == ASSOCIATION_PROCESSING {
        return Ok(Readiness::Pending(status));
    }
    // `deploymentDetail` is "details about the last deployment status", which is what that
    // service has to say about the state it reports; the status speaks for itself otherwise.
    Ok(Readiness::Failed(
        match text_member(&reported, DEPLOYMENT_DETAIL_MEMBER) {
            Some(detail) => format!(
                "AppSync reports the API association of the custom domain name \
                 {physical_resource_id} as {status}: {detail}"
            ),
            None => format!(
                "AppSync reports the API association of the custom domain name \
                 {physical_resource_id} as {status}"
            ),
        },
    ))
}

/// Whether AppSync has finished removing a deleted resource.
///
/// Nothing is asked for either type: `DeleteDomainName` takes the domain away as it answers —
/// a later `GetDomainName` of it is a `NotFoundException` — and `DisassociateApi` leaves the
/// domain carrying no mapping at all.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has.
///
/// A DOMAIN takes its description: `UpdateDomainName` carries that member and no other, the
/// certificate being fixed when the domain is made
/// ([`crate::state::template::replacement_properties`]), so this applies the one change that
/// call can make. A MAPPING takes its API: `AssociateApi` on a domain that already carries
/// one replaces the mapping, which is how a declaration naming another API is applied —
/// and the mapping goes back to `PROCESSING`, which the resource's own wait is held on.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    match kind {
        Kind::DomainName => {
            refuse_unapplied_properties(declared, &resource.resource_type, DOMAIN_NAME_PROPERTIES)?;
            let request = declared_members(declared, &DOMAIN_NAME_MEMBERS[1..]);
            let answered = call_appsync(
                ctx,
                region,
                "POST",
                &domain_name_uri(&resource.physical_resource_id),
                Some(&Value::Object(request)),
            )
            .await?;
            Ok(Some(Created::attributes(domain_name_attributes(&answered))))
        }
        Kind::ApiAssociation => {
            refuse_unapplied_properties(
                declared,
                &resource.resource_type,
                API_ASSOCIATION_PROPERTIES,
            )?;
            let request = associate_request(declared, &resource.resource_type)?;
            let answered = call_appsync(
                ctx,
                region,
                "POST",
                &api_association_uri(&resource.physical_resource_id),
                Some(&request),
            )
            .await?;
            Ok(Some(Created::attributes(api_association_attributes(
                &answered,
            ))))
        }
    }
}

/// Delete the resource a stack created: the mapping is removed with `DisassociateApi`, the
/// domain with `DeleteDomainName`, each addressed by the domain name it was created under —
/// this resource's physical id.
///
/// One AppSync no longer holds is already in the state the delete asks for, judged on the
/// CODE and never on a message, which carries the request's own name.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = match kind {
        Kind::DomainName => domain_name_uri(&resource.physical_resource_id),
        Kind::ApiAssociation => api_association_uri(&resource.physical_resource_id),
    };
    match call_appsync(ctx, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPSYNC_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as AppSync reports it, for drift detection: `GetDomainName`'s
/// own record of the domain, and `GetApiAssociation`'s of the mapping, under this type's
/// property names. One that service no longer holds was deleted, which is what `None`
/// reports.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    match kind {
        Kind::DomainName => {
            let Some(reported) = read_domain_name(ctx, region, &resource.physical_resource_id)
                .await?
            else {
                return Ok(None);
            };
            Ok(Some(reported_as(
                &reported,
                &[
                    DOMAIN_NAME_MEMBERS[0],
                    DOMAIN_NAME_MEMBERS[1],
                    (DOMAIN_NAME_PROPERTY, DOMAIN_NAME_MEMBER),
                ],
            )))
        }
        Kind::ApiAssociation => {
            let Some(reported) =
                read_api_association(ctx, region, &resource.physical_resource_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(reported_as(
                &reported,
                &[
                    (API_ID_PROPERTY, API_ID_MEMBER),
                    (DOMAIN_NAME_PROPERTY, DOMAIN_NAME_MEMBER),
                ],
            )))
        }
    }
}

/// The domain AppSync reports under the name the stack created it with, or `None` where it
/// holds none.
async fn read_domain_name(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_appsync(ctx, region, "GET", &domain_name_uri(physical_resource_id), None).await {
        Ok(answered) => Ok(answered.get(DOMAIN_NAME_CONFIG_MEMBER).cloned()),
        Err(refusal) if refusal.code == APPSYNC_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The mapping AppSync reports on that domain, or `None` where the domain — or the mapping —
/// is not there: `GetApiAssociation` answers the same `NotFoundException` for both.
async fn read_api_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    match call_appsync(
        ctx,
        region,
        "GET",
        &api_association_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(answered) => Ok(answered.get(API_ASSOCIATION_MEMBER).cloned()),
        Err(refusal) if refusal.code == APPSYNC_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// `AssociateApi`'s request body: the `apiId` the declaration names, which that call declares
/// required — a declaration naming none describes no mapping that could be made.
fn associate_request(properties: &Value, resource_type: &str) -> Result<Value, Refusal> {
    let Some(api_id) = properties
        .get(API_ID_PROPERTY)
        .filter(|value| !value.is_null())
    else {
        return Err(missing_property(resource_type, API_ID_PROPERTY));
    };
    let mut request = Map::new();
    request.insert(API_ID_MEMBER.to_string(), api_id.clone());
    Ok(Value::Object(request))
}

/// The attributes an answer about a domain reports for the resource: the three values AppSync
/// assigned it ([`DOMAIN_NAME_ATTRIBUTES`]). An answer carrying none of them reports no
/// attribute rather than an empty one.
fn domain_name_attributes(answered: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(config) = answered.get(DOMAIN_NAME_CONFIG_MEMBER) else {
        return attributes;
    };
    for (attribute, member) in DOMAIN_NAME_ATTRIBUTES {
        if let Some(value) = text_member(config, member) {
            attributes.insert((*attribute).to_string(), value);
        }
    }
    attributes
}

/// The attribute an answer about a mapping reports for the resource: the domain name AppSync
/// reports it under, which is the identifier the type publishes.
fn api_association_attributes(answered: &Value) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(association) = answered.get(API_ASSOCIATION_MEMBER) else {
        return attributes;
    };
    if let Some(domain_name) = text_member(association, DOMAIN_NAME_MEMBER) {
        attributes.insert(
            API_ASSOCIATION_IDENTIFIER_ATTRIBUTE.to_string(),
            domain_name,
        );
    }
    attributes
}

/// One reported record under this type's PROPERTY names, which is the vocabulary drift is
/// judged in: a member the answer does not carry contributes nothing, a property the read
/// does not report being no evidence of anything.
fn reported_as(reported: &Value, members: &[(&str, &str)]) -> Value {
    let mut configuration = Map::new();
    for (property, member) in members {
        if let Some(value) = reported.get(*member).filter(|value| !value.is_null()) {
            configuration.insert((*property).to_string(), value.clone());
        }
    }
    Value::Object(configuration)
}

/// The members of `members` a declaration states, as the request carries them: the property
/// and the member are the same name up to the initial's case, so every value goes through as
/// the template wrote it and that operation's own validation is what judges it.
fn declared_members(properties: &Value, members: &[(&str, &str)]) -> Map<String, Value> {
    let mut request = Map::new();
    for (property, member) in members {
        if let Some(value) = properties.get(*property).filter(|value| !value.is_null()) {
            request.insert((*member).to_string(), value.clone());
        }
    }
    request
}

/// One text member of a reported record, absent where the answer carries none or carries it
/// empty — an empty endpoint is no endpoint a caller could reach.
fn text_member(reported: &Value, member: &str) -> Option<String> {
    reported
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// The URI that addresses ONE custom domain: the collection, and the `domainName`
/// `@httpLabel` every read, update and delete of one binds, written as a single path segment
/// ([`percent_encoded`]) so a name outside the modelled pattern is refused by that
/// operation's validation instead of addressing a different one.
fn domain_name_uri(domain_name: &str) -> String {
    format!("{DOMAIN_NAMES_URI}/{}", percent_encoded(domain_name))
}

/// The URI that addresses the API ASSOCIATION of one custom domain, which is that same label
/// plus the segment the three operations on a mapping bind.
fn api_association_uri(domain_name: &str) -> String {
    format!(
        "{}/{API_ASSOCIATION_SEGMENT}",
        domain_name_uri(domain_name)
    )
}

/// The refusal for a declaration that states none of a property the owning call requires,
/// naming the property: refusing is what keeps a resource from being reported as created
/// from a declaration that describes none.
fn missing_property(resource_type: &str, property: &str) -> Refusal {
    Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!("{resource_type} declares no {property}"),
    }
}

/// The refusal for a declared property these arms do not apply, naming which property it
/// was: refusing is what keeps a domain — or a mapping — from being reported as created with
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

/// Call one AppSync operation on the REST-JSON wire its model declares.
async fn call_appsync(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    method: &'static str,
    uri: &str,
    body: Option<&Value>,
) -> Result<Value, Refusal> {
    call_rest_json(ctx, APPSYNC_SIGNING_NAME, region, method, uri, body).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared properties reach `CreateDomainName` under that call's own member names,
    /// and the name the domain is created under is not one of them: it is the request's own
    /// `domainName`, supplied from the resource's physical id.
    #[test]
    fn the_declared_properties_are_carried_under_the_api_member_names() {
        let declared = serde_json::json!({
            "DomainName": "api.example.test",
            "CertificateArn":
                "arn:aws:acm:us-east-1:000000000000:certificate/11111111-2222-3333-4444-555555555555",
            "Description": "the custom domain",
            "Tags": [{"Key": "Owner", "Value": "platform"}]
        });
        let carried = Value::Object(declared_members(&declared, DOMAIN_NAME_MEMBERS));
        let expected = serde_json::json!({
            "certificateArn":
                "arn:aws:acm:us-east-1:000000000000:certificate/11111111-2222-3333-4444-555555555555",
            "description": "the custom domain"
        });
        assert_eq!(
            carried, expected,
            "case \"a full declaration\": expected {expected}, got {carried}"
        );
    }

    /// The update carries only what `UpdateDomainName` takes, which is the description: the
    /// certificate is fixed when the domain is made.
    #[test]
    fn the_update_carries_only_what_update_domain_name_takes() {
        let declared = serde_json::json!({
            "CertificateArn":
                "arn:aws:acm:us-east-1:000000000000:certificate/11111111-2222-3333-4444-555555555555",
            "Description": "another description"
        });
        let carried = Value::Object(declared_members(&declared, &DOMAIN_NAME_MEMBERS[1..]));
        let expected = serde_json::json!({"description": "another description"});
        assert_eq!(
            carried, expected,
            "case \"a changed declaration\": expected {expected}, got {carried}"
        );
    }

    /// Each URI is the model's own binding with the domain name as ONE path segment, so a
    /// name carrying a character that is structure in a URI cannot address another operation.
    #[test]
    fn the_uris_carry_the_domain_name_as_one_segment() {
        let cases: &[(&str, &str, &str, &str)] = &[
            (
                "an ordinary name",
                "api.example.test",
                "/v1/domainnames/api.example.test",
                "/v1/domainnames/api.example.test/apiassociation",
            ),
            (
                "a wildcard name",
                "*.example.test",
                "/v1/domainnames/%2A.example.test",
                "/v1/domainnames/%2A.example.test/apiassociation",
            ),
            (
                "a name with a slash",
                "a/b",
                "/v1/domainnames/a%2Fb",
                "/v1/domainnames/a%2Fb/apiassociation",
            ),
        ];
        for (case, domain_name, domain_uri, association_uri) in cases {
            let actual = domain_name_uri(domain_name);
            assert_eq!(
                actual, *domain_uri,
                "case {case:?}: expected the URI {domain_uri:?}, got {actual:?}"
            );
            let actual = api_association_uri(domain_name);
            assert_eq!(
                actual, *association_uri,
                "case {case:?}: expected the URI {association_uri:?}, got {actual:?}"
            );
        }
    }

    /// The values AppSync assigned the domain are the attributes the type publishes, read off
    /// the configuration its answer carries — and an answer carrying none reports no
    /// attribute rather than an empty one.
    #[test]
    fn the_assigned_endpoint_zone_and_arn_are_the_published_attributes() {
        let answered = serde_json::json!({
            "domainNameConfig": {
                "domainName": "api.example.test",
                "appsyncDomainName": "abcdefghij.cloudfront.net",
                "hostedZoneId": "Z2FDTNDATAQYW2",
                "domainNameArn":
                    "arn:aws:appsync:us-east-1:000000000000:domainnames/api.example.test",
                "certificateArn": ""
            }
        });
        let attributes = domain_name_attributes(&answered);
        assert_eq!(
            attributes.get("AppSyncDomainName").map(String::as_str),
            Some("abcdefghij.cloudfront.net"),
            "case \"an answered endpoint\": expected it to be the AppSyncDomainName \
             attribute, got {attributes:?}"
        );
        assert_eq!(
            attributes.get("HostedZoneId").map(String::as_str),
            Some("Z2FDTNDATAQYW2"),
            "case \"an answered zone\": expected it to be the HostedZoneId attribute, got \
             {attributes:?}"
        );
        assert_eq!(
            attributes.get("DomainNameArn").map(String::as_str),
            Some("arn:aws:appsync:us-east-1:000000000000:domainnames/api.example.test"),
            "case \"an answered ARN\": expected it to be the DomainNameArn attribute, got \
             {attributes:?}"
        );
        let silent = serde_json::json!({"domainNameConfig": {}});
        let attributes = domain_name_attributes(&silent);
        assert!(
            attributes.is_empty(),
            "case \"an answer carrying none of them\": expected no attribute, got \
             {attributes:?}"
        );
    }

    /// A mapping's published identifier is the domain name AppSync reports it under.
    #[test]
    fn the_mappings_identifier_is_the_domain_it_is_made_on() {
        let answered = serde_json::json!({
            "apiAssociation": {
                "domainName": "api.example.test",
                "apiId": "abcdefghijklmnopqrstuvwxyz",
                "associationStatus": "PROCESSING"
            }
        });
        let attributes = api_association_attributes(&answered);
        assert_eq!(
            attributes
                .get(API_ASSOCIATION_IDENTIFIER_ATTRIBUTE)
                .map(String::as_str),
            Some("api.example.test"),
            "case \"an answered mapping\": expected the domain name to be the identifier, \
             got {attributes:?}"
        );
    }

    /// A property no call here applies is refused, naming it; every property either type
    /// publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        assert!(
            refuse_unapplied_properties(
                &serde_json::json!({
                    "DomainName": "api.example.test",
                    "CertificateArn": "arn:aws:acm:us-east-1:000000000000:certificate/a",
                    "Description": "d",
                    "Tags": []
                }),
                APPSYNC_DOMAIN_NAME,
                DOMAIN_NAME_PROPERTIES
            )
            .is_ok(),
            "case \"every published property of a domain\": expected it to be applied, got a \
             refusal"
        );
        assert!(
            refuse_unapplied_properties(
                &serde_json::json!({"DomainName": "api.example.test", "ApiId": "an-api"}),
                APPSYNC_DOMAIN_NAME_API_ASSOCIATION,
                API_ASSOCIATION_PROPERTIES
            )
            .is_ok(),
            "case \"every published property of a mapping\": expected it to be applied, got \
             a refusal"
        );
        let refusal = refuse_unapplied_properties(
            &serde_json::json!({"DomainName": "api.example.test", "Unheard": 1}),
            APPSYNC_DOMAIN_NAME,
            DOMAIN_NAME_PROPERTIES,
        )
        .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("Unheard"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }

    /// A declaration naming no API describes no mapping that could be made, so the request is
    /// refused naming that property rather than sent without it.
    #[test]
    fn a_mapping_declaring_no_api_is_refused() {
        let refusal = associate_request(
            &serde_json::json!({"DomainName": "api.example.test"}),
            APPSYNC_DOMAIN_NAME_API_ASSOCIATION,
        )
        .expect_err("case \"a mapping naming no API\": expected a refusal");
        assert!(
            refusal.message.contains(API_ID_PROPERTY),
            "case \"a mapping naming no API\": expected the refusal to name the property, got \
             {:?}",
            refusal.message
        );
        let request = associate_request(
            &serde_json::json!({"DomainName": "api.example.test", "ApiId": "an-api"}),
            APPSYNC_DOMAIN_NAME_API_ASSOCIATION,
        )
        .expect("case \"a mapping naming an API\": expected the request");
        assert_eq!(
            request,
            serde_json::json!({"apiId": "an-api"}),
            "case \"a mapping naming an API\": expected the api to be the one member, got \
             {request}"
        );
    }

    /// The tag map carries the declaration's own tags and the ones CloudFormation propagates,
    /// and never a key in the reserved namespace: `TagKey`'s own pattern refuses one, so a
    /// deployment must not fail over the records CloudFormation keeps of its own accord.
    #[test]
    fn the_tag_map_leaves_the_reserved_namespace_alone() {
        let declared = serde_json::json!({"Tags": [{"Key": "tier", "Value": "edge"}]});
        let propagated = vec![
            ("Stack".to_string(), "appsync-domains".to_string()),
            (
                "aws:cloudformation:logical-id".to_string(),
                "Domain".to_string(),
            ),
        ];
        let tags = tags_as_map(&applicable_tags(&tags_of(&declared, &propagated)));
        let keys: Vec<&str> = tags.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            vec!["tier", "Stack"],
            "case \"a declaration beside the propagated tags\": expected the declared and \
             stack keys alone, in declaration order, got {keys:?}"
        );
    }
}
