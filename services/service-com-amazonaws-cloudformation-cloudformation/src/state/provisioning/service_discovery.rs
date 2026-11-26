// SPDX-License-Identifier: CC0-1.0

//! The five `AWS::ServiceDiscovery::*` types a Cloud Map namespace's estate is declared with —
//! an HTTP, a public DNS and a private DNS namespace, a service of one, and an instance
//! registered in a service — created in AWS Cloud Map itself.
//!
//! This is the per-type half of provisioning for those types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives in
//! its own module beside [`super::appsync_event_and_merged_apis`] for the reason that module
//! records — the catalogue file is far past the length a change to this repository may touch —
//! and is reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! A namespace is the handle every other Cloud Map call about one takes: `ListNamespaces`
//! reports it, `GetNamespace` reads it, `CreateService` places a service IN it and
//! `DiscoverInstances` resolves instances THROUGH it. So a stack that reported a namespace
//! created while nothing created it publishes an id that resolves to nothing — the client that
//! deployed the template finds no namespace to register a service against, and a second stack
//! declaring the same name would be accepted where Cloud Map refuses it. A service and an
//! instance are the same case one level down, and the service is also where Cloud Map's own
//! PRECONDITIONS live: a service that declares a `DnsConfig` in a namespace that holds no
//! hosted zone is refused with `InvalidInput` ("you can't use `DnsConfig` in an HTTP
//! namespace"), so a stack declaring one must FAIL the resource rather than report a
//! DNS-discoverable service in a namespace that has no DNS.
//!
//! ## The properties
//!
//! Each namespace type's properties are its own create's members under the same names
//! (`Description`, `Properties`, `Vpc`, `Tags`) beside the `Name` the namespace is created
//! UNDER, which CloudFormation decides: the name the declaration asked for, or the one it
//! generated for a declaration stating none ([`crate::state::provisioner::declared_name`]).
//! `AWS::ServiceDiscovery::Service`'s are `CreateService`'s own members; the service's own
//! `Name` is decided the same way.
//! `AWS::ServiceDiscovery::Instance`'s are `RegisterInstance`'s: the `ServiceId` it is
//! registered in, the `InstanceAttributes` it answers with, and the `InstanceId` that
//! identifies it — which is this resource's physical id rather than a value the body carries.
//!
//! A declared property these arms do not apply is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid.
//!
//! ## The identifiers
//!
//! A namespace create answers an `OperationId` and nothing else — the `ns-…` id Cloud Map
//! minted reaches the caller through that operation's `Targets` map ("NAMESPACE: the namespace
//! ID is returned in the `ResourceId` property") — so the create reads that operation and then
//! the namespace itself, and reports the id, the ARN and the hosted zone that service holds.
//! `CreateService` answers the service it made, carrying the `srv-…` id and the ARN Cloud Map
//! assigned. Both are adopted from the create
//! ([`crate::state::template::identity_is_service_assigned`] names those four types).

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{
    call_json_rpc, key_value_tag_list, storable_tags, Created, Readiness, Refusal, Removal,
    AWS_JSON_1_1,
};
use crate::state::template::{
    SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE, SERVICEDISCOVERY_HTTP_NAMESPACE,
    SERVICEDISCOVERY_INSTANCE, SERVICEDISCOVERY_INSTANCE_ID_PROPERTY,
    SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE, SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE,
    SERVICEDISCOVERY_SERVICE, SERVICEDISCOVERY_SERVICE_ID_PROPERTY,
};
use crate::state::StoredResource;

/// Cloud Map's signing name, which is what routes the in-process call to its own handlers, and
/// the service shape an awsJson `X-Amz-Target` names an operation under (that model's
/// `sigv4Name: servicediscovery` and service shape `Route53AutoNaming_v20170314`,
/// `smithy_query(query="service_info")`). It speaks `awsJson1_1`.
const CLOUD_MAP_SIGNING_NAME: &str = "servicediscovery";
/// See [`CLOUD_MAP_SIGNING_NAME`].
const CLOUD_MAP_TARGET_PREFIX: &str = "Route53AutoNaming_v20170314";

/// The error codes Cloud Map answers for a resource it does not hold (the modelled 404s of its
/// reads and deletes), which is what says a delete has nothing left to remove and a read
/// nothing to report.
const NAMESPACE_NOT_FOUND: &str = "NamespaceNotFound";
/// See [`NAMESPACE_NOT_FOUND`].
const SERVICE_NOT_FOUND: &str = "ServiceNotFound";
/// See [`NAMESPACE_NOT_FOUND`].
const INSTANCE_NOT_FOUND: &str = "InstanceNotFound";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property, and
/// for an answer that carries none of the identifier the resource IS.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// Which of the five types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::ServiceDiscovery::HttpNamespace`.
    HttpNamespace,
    /// `AWS::ServiceDiscovery::PublicDnsNamespace`.
    PublicDnsNamespace,
    /// `AWS::ServiceDiscovery::PrivateDnsNamespace`.
    PrivateDnsNamespace,
    /// `AWS::ServiceDiscovery::Service`.
    Service,
    /// `AWS::ServiceDiscovery::Instance`.
    Instance,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        SERVICEDISCOVERY_HTTP_NAMESPACE => Some(Kind::HttpNamespace),
        SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE => Some(Kind::PublicDnsNamespace),
        SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE => Some(Kind::PrivateDnsNamespace),
        SERVICEDISCOVERY_SERVICE => Some(Kind::Service),
        SERVICEDISCOVERY_INSTANCE => Some(Kind::Instance),
        _ => None,
    }
}

/// The member every Cloud Map call carries a resource's name in, and the one its creates carry
/// the tag list in.
const NAME_MEMBER: &str = "Name";
/// See [`NAME_MEMBER`].
const TAGS_MEMBER: &str = "Tags";

/// The member a call addressing ONE namespace, service or operation names it by (`GetNamespace`
/// and `DeleteNamespace`'s `Id`, `GetService`'s, `UpdateService`'s and `DeleteService`'s `Id`).
const ID_MEMBER: &str = "Id";

/// The member an asynchronous call answers the filed operation's id under, and the one
/// `GetOperation` takes it as.
const OPERATION_ID_MEMBER: &str = "OperationId";

/// The members `GetOperation`'s answer carries: the operation itself, the targets it names and
/// the state it is in.
const OPERATION_MEMBER: &str = "Operation";
/// See [`OPERATION_MEMBER`].
const TARGETS_MEMBER: &str = "Targets";
/// See [`OPERATION_MEMBER`].
const STATUS_MEMBER: &str = "Status";
/// See [`OPERATION_MEMBER`].
const ERROR_MESSAGE_MEMBER: &str = "ErrorMessage";
/// See [`OPERATION_MEMBER`].
const ERROR_CODE_MEMBER: &str = "ErrorCode";

/// The `OperationStatus` value that says the operation did not go through ("FAIL: the operation
/// failed. For the failure reason, see `ErrorMessage`").
const OPERATION_FAILED: &str = "FAIL";

/// The `OperationStatus` value that says the operation was carried out ("SUCCESS: the operation
/// succeeded"), which is the only state a resource the operation CREATES exists in.
const OPERATION_SUCCEEDED: &str = "SUCCESS";

/// The `OperationType` a namespace create files, whichever of the three namespace calls made it
/// (`smithy_query(query="shape",shape="OperationType")`): each of `CreateHttpNamespace`,
/// `CreatePublicDnsNamespace` and `CreatePrivateDnsNamespace` answers an `OperationId` and
/// reports the creation's outcome through that one operation.
const CREATE_NAMESPACE_OPERATION: &str = "CREATE_NAMESPACE";

/// The `ListOperations` members a filtered listing carries: the filter list, the page bound and
/// the page cursor.
const FILTERS_MEMBER: &str = "Filters";
/// See [`FILTERS_MEMBER`].
const VALUES_MEMBER: &str = "Values";
/// See [`FILTERS_MEMBER`].
const MAX_RESULTS_MEMBER: &str = "MaxResults";
/// See [`FILTERS_MEMBER`].
const NEXT_TOKEN_MEMBER: &str = "NextToken";
/// The member `ListOperations` answers the page of operation summaries under.
const OPERATIONS_MEMBER: &str = "Operations";

/// The `OperationFilterName` values that pick out ONE namespace's operations and ONE kind of
/// operation ("NAMESPACE_ID: gets operations related to specified namespaces", "TYPE: gets
/// specified types of operation"), both of which `EQ` — the default condition — supports.
const NAMESPACE_ID_FILTER: &str = "NAMESPACE_ID";
/// See [`NAMESPACE_ID_FILTER`].
const TYPE_FILTER: &str = "TYPE";

/// How many operations one page of the listing asks for: the top of `ListOperations`'
/// `MaxResults` range (1..100), so the account's operations are read in as few round trips as
/// they can be.
const OPERATION_PAGE_SIZE: i64 = 100;

/// How many pages one readiness read walks before it gives up looking.
///
/// `ListOperations` "gets MaxResults operations and then filters them based on the specified
/// criteria", so a page may answer empty while a later one carries the match — and the listing
/// has to be walked rather than read once. This is the backstop that keeps one readiness read
/// finite whatever the account holds.
const MAX_OPERATION_PAGES: usize = 64;

/// The key of `Operation$Targets` naming the NAMESPACE an operation acted on, which is where
/// the id a namespace create minted reaches the caller.
const NAMESPACE_TARGET: &str = "NAMESPACE";

/// The member an answer about a namespace carries the namespace in, and the one an answer about
/// a service carries the service in.
const NAMESPACE_MEMBER: &str = "Namespace";
/// See [`NAMESPACE_MEMBER`].
const SERVICE_MEMBER: &str = "Service";

/// The members a reported namespace carries its type-specific records in, inside which a DNS
/// namespace reports the hosted zone Cloud Map created for it.
const PROPERTIES_MEMBER: &str = "Properties";
/// See [`PROPERTIES_MEMBER`].
const DNS_PROPERTIES_MEMBER: &str = "DnsProperties";

/// The member every answer carries the ARN Cloud Map assigned the resource under.
const ARN_MEMBER: &str = "Arn";

/// The member `RegisterInstance` takes the instance's identifier as, and the one it takes the
/// attribute map as.
const INSTANCE_ID_MEMBER: &str = "InstanceId";
/// See [`INSTANCE_ID_MEMBER`].
const ATTRIBUTES_MEMBER: &str = "Attributes";

/// The member a service's declaration states the instance attributes in, which is the map
/// `RegisterInstance` takes as `Attributes`.
const INSTANCE_ATTRIBUTES_PROPERTY: &str = "InstanceAttributes";

/// The property a service's declaration states the DNS records in, and the member
/// `DnsConfigChange` is the one changeable part of it.
const DNS_CONFIG_PROPERTY: &str = "DnsConfig";
/// See [`DNS_CONFIG_PROPERTY`].
const DNS_RECORDS_MEMBER: &str = "DnsRecords";

/// The property a private DNS namespace names the VPC its hosted zone is associated with,
/// which `CreatePrivateDnsNamespace` declares required.
const VPC_PROPERTY: &str = "Vpc";

/// The property every one of these types states a description in, which is also the one member
/// each namespace's update carries.
const DESCRIPTION_PROPERTY: &str = "Description";

/// Every property `AWS::ServiceDiscovery::HttpNamespace` publishes.
///
/// Source: that type's entry in the "AWS resource and property types reference" of the
/// CloudFormation User Guide, whose properties are `CreateHttpNamespace`'s own members
/// (`smithy_query(query="operation",operation="CreateHttpNamespace")`).
const HTTP_NAMESPACE_PROPERTIES: &[&str] = &["Description", "Name", "Tags"];

/// Every property `AWS::ServiceDiscovery::PublicDnsNamespace` publishes: the HTTP namespace's,
/// plus the `Properties` document carrying the hosted zone's SOA record.
const PUBLIC_DNS_NAMESPACE_PROPERTIES: &[&str] = &["Description", "Name", "Properties", "Tags"];

/// Every property `AWS::ServiceDiscovery::PrivateDnsNamespace` publishes: the public DNS
/// namespace's, plus the `Vpc` its private hosted zone is associated with.
const PRIVATE_DNS_NAMESPACE_PROPERTIES: &[&str] =
    &["Description", "Name", "Properties", "Tags", "Vpc"];

/// Every property `AWS::ServiceDiscovery::Service` publishes, which are `CreateService`'s own
/// members.
const SERVICE_PROPERTIES: &[&str] = &[
    "Description",
    "DnsConfig",
    "HealthCheckConfig",
    "HealthCheckCustomConfig",
    "Name",
    "NamespaceId",
    "Tags",
    "Type",
];

/// Every property `AWS::ServiceDiscovery::Instance` publishes, which are `RegisterInstance`'s
/// own members.
const INSTANCE_PROPERTIES: &[&str] = &["InstanceAttributes", "InstanceId", "ServiceId"];

/// The members of each namespace create a declaration states under the same names, beside the
/// `Name` CloudFormation decides and the tags it propagates.
const HTTP_NAMESPACE_MEMBERS: &[&str] = &["Description"];
/// See [`HTTP_NAMESPACE_MEMBERS`].
const DNS_NAMESPACE_MEMBERS: &[&str] = &["Description", "Properties"];
/// See [`HTTP_NAMESPACE_MEMBERS`]; a private DNS namespace also names its VPC.
const PRIVATE_DNS_NAMESPACE_MEMBERS: &[&str] = &["Description", "Properties", "Vpc"];

/// The members of `CreateService` a declaration states under the same names.
const SERVICE_MEMBERS: &[&str] = &[
    "Description",
    "DnsConfig",
    "HealthCheckConfig",
    "HealthCheckCustomConfig",
    "NamespaceId",
    "Type",
];

/// The members these types declare that the owning API models as NUMBERS.
///
/// Source: the member lists of the structures these types carry
/// (`smithy_query(query="shape",shape="SOA")` and its siblings): a record's and an SOA's time
/// to live (`RecordTTL`, a long) and a health check's failure threshold (`FailureThreshold`, an
/// integer).
const NUMERIC_MEMBERS: &[&str] = &["FailureThreshold", "TTL"];

/// Create the resource one of these five types declares, in Cloud Map itself — so a client that
/// deployed the stack can pass `!Ref <namespace>` straight to `GetNamespace` or `CreateService`,
/// find the namespace in `ListNamespaces`, and resolve the instances of its services.
///
/// `Err` is that service's own refusal of the create: a namespace name the account already
/// holds, a `NamespaceId` that resolves to nothing, a `DnsConfig` in a namespace that has no
/// hosted zone.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => {
            create_namespace(ctx, region, kind, resource, propagated).await
        }
        Kind::Service => create_service(ctx, region, resource, propagated).await,
        // An instance is not taggable: Cloud Map's tagging calls address a namespace or a
        // service by ARN and an instance has none, and the type publishes no `Tags` property at
        // all, so CloudFormation propagates no tag to one.
        Kind::Instance => register_instance(ctx, region, resource).await,
    }
}

/// Create the namespace one of the three namespace types declares, and report the id, the ARN
/// and the hosted zone Cloud Map holds for it.
async fn create_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        published_properties(kind),
    )?;
    let mut request = declared_members(&resource.properties, create_members(kind));
    // `CreatePrivateDnsNamespace$Vpc` is required by that call and by the type: a declaration
    // naming no VPC describes no private zone that could be made, so it is refused here rather
    // than sent as a request the owning service would reject.
    if kind == Kind::PrivateDnsNamespace && !request.contains_key(VPC_PROPERTY) {
        return Err(missing_property(&resource.resource_type, VPC_PROPERTY));
    }
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(crate::state::provisioner::declared_name(resource)),
    );
    carry_tags(&mut request, &resource.properties, propagated);
    let answered = call_cloud_map(ctx, region, create_operation(kind), &Value::Object(request))
        .await?;
    let operation_id = required_member(&answered, OPERATION_ID_MEMBER, &resource.resource_type)?;
    let namespace_id = operation_target(ctx, region, &operation_id, NAMESPACE_TARGET).await?;
    let attributes = read_namespace_attributes(ctx, region, kind, &namespace_id).await?;
    Ok(Created {
        physical_resource_id: Some(namespace_id),
        attributes,
    })
}

/// Create the service an `AWS::ServiceDiscovery::Service` declares, in the namespace its
/// declaration names, and report the id and the ARN `CreateService` answered with.
async fn create_service(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        SERVICE_PROPERTIES,
    )?;
    let mut request = declared_members(&resource.properties, SERVICE_MEMBERS);
    request.insert(
        NAME_MEMBER.to_string(),
        Value::String(crate::state::provisioner::declared_name(resource)),
    );
    carry_tags(&mut request, &resource.properties, propagated);
    let answered = call_cloud_map(
        ctx,
        region,
        create_operation(Kind::Service),
        &Value::Object(request),
    )
    .await?;
    let service = answered.get(SERVICE_MEMBER).cloned().unwrap_or(Value::Null);
    let service_id = required_member(&service, ID_MEMBER, &resource.resource_type)?;
    let mut attributes = reported_identity_attributes(&resource.resource_type, &service);
    if let Some(name) = text_member(&service, NAME_MEMBER) {
        attributes.insert(NAME_MEMBER.to_string(), name);
    }
    Ok(Created {
        physical_resource_id: Some(service_id),
        attributes,
    })
}

/// Register the instance an `AWS::ServiceDiscovery::Instance` declares in the service its
/// `ServiceId` names, through `RegisterInstance` — the one call that makes an instance.
///
/// The instance keeps the identifier the declaration asked for: `RegisterInstance$InstanceId` is
/// "an identifier that you want to associate with the instance", which is this resource's
/// physical id, so the create adopts nothing. The type publishes no `Fn::GetAtt` attribute.
async fn register_instance(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(
        &resource.properties,
        &resource.resource_type,
        INSTANCE_PROPERTIES,
    )?;
    let request = register_request(&resource.properties, resource)?;
    call_cloud_map(ctx, region, create_operation(Kind::Instance), &request).await?;
    Ok(Created::attributes(BTreeMap::new()))
}

/// `RegisterInstance`'s request body: the service the instance belongs to, the identifier
/// CloudFormation decided for it, and the attribute map it answers discovery with.
///
/// The service and the attributes are required by that call and by the type, so a declaration
/// stating neither describes no instance that could be registered and is refused here.
fn register_request(declared: &Value, resource: &StoredResource) -> Result<Value, Refusal> {
    let service_id = declared
        .get(SERVICEDISCOVERY_SERVICE_ID_PROPERTY)
        .filter(|value| !value.is_null())
        .cloned()
        .ok_or_else(|| {
            missing_property(&resource.resource_type, SERVICEDISCOVERY_SERVICE_ID_PROPERTY)
        })?;
    let attributes = declared
        .get(INSTANCE_ATTRIBUTES_PROPERTY)
        .filter(|value| value.is_object())
        .cloned()
        .ok_or_else(|| {
            missing_property(&resource.resource_type, INSTANCE_ATTRIBUTES_PROPERTY)
        })?;
    let mut request = Map::new();
    request.insert(SERVICEDISCOVERY_SERVICE_ID_PROPERTY.to_string(), service_id);
    request.insert(
        INSTANCE_ID_MEMBER.to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    request.insert(ATTRIBUTES_MEMBER.to_string(), attributes);
    Ok(Value::Object(request))
}

/// Whether Cloud Map reports the created resource ready.
///
/// A NAMESPACE is created ASYNCHRONOUSLY: each of the three namespace calls answers an
/// `OperationId` and nothing else, and that operation is the only channel the creation's outcome
/// reaches a caller on ("SUBMITTED: this is the initial state immediately after you submit a
/// request", "SUCCESS: the operation succeeded", "FAIL: the operation failed"). So the resource
/// is held open until that operation is TERMINAL: a namespace whose create has not settled is
/// not one a client can use, and one whose create lands `FAIL` is a namespace that does not
/// exist — Cloud Map rolls the staged record back — so the resource fails with that operation's
/// own reason rather than being reported created.
///
/// A SERVICE and an INSTANCE wait for nothing: `CreateService` answers the service itself, and
/// `RegisterInstance`'s operation records a registration that service already holds. Neither
/// `Service` nor `Instance` publishes a status member at all
/// (`smithy_query(query="shape",shape="Namespace,Service")`), so there is no non-terminal state
/// for one of them to settle out of.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => {
            namespace_readiness(ctx, region, physical_resource_id).await
        }
        Kind::Service | Kind::Instance => Ok(Readiness::Ready),
    }
}

/// Where the `CREATE_NAMESPACE` operation of ONE namespace has got to.
///
/// Cloud Map reports no operation for the namespace only once nothing filed one for it, which
/// no create this module makes leaves behind: that answer is read as nothing left to wait for,
/// for the reason every other cross-service pre-flight in this crate fails open — one
/// unanswered read is no evidence about the resource.
async fn namespace_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    namespace_id: &str,
) -> Result<Readiness, Refusal> {
    let Some((operation_id, status)) = namespace_create_operation(ctx, region, namespace_id).await?
    else {
        return Ok(Readiness::Ready);
    };
    match status.as_str() {
        OPERATION_SUCCEEDED => Ok(Readiness::Ready),
        // Cloud Map's own report about the creation, quoted as that service worded it: the
        // `ErrorMessage` of the operation that failed ("if the value of `Status` is `FAIL`, the
        // reason that the operation failed"), and the `ErrorCode` beside it where that is all
        // the operation carries.
        OPERATION_FAILED => Ok(Readiness::Failed(
            operation_failure(ctx, region, &operation_id).await?,
        )),
        pending => Ok(Readiness::Pending(pending.to_string())),
    }
}

/// The id and the status of the `CREATE_NAMESPACE` operation that made the namespace
/// `namespace_id`, or `None` where Cloud Map reports none.
///
/// Found through `ListOperations`, whose `NAMESPACE_ID` and `TYPE` filters name exactly that
/// operation ("if you specify more than one filter, an operation must match all filters to be
/// returned by `ListOperations`") — the id the create answered with is not what addresses the
/// namespace, so it is the namespace that addresses the operation here.
async fn namespace_create_operation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    namespace_id: &str,
) -> Result<Option<(String, String)>, Refusal> {
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_OPERATION_PAGES {
        let mut request = Map::new();
        request.insert(
            FILTERS_MEMBER.to_string(),
            serde_json::json!([
                { NAME_MEMBER: NAMESPACE_ID_FILTER, VALUES_MEMBER: [namespace_id] },
                { NAME_MEMBER: TYPE_FILTER, VALUES_MEMBER: [CREATE_NAMESPACE_OPERATION] },
            ]),
        );
        request.insert(
            MAX_RESULTS_MEMBER.to_string(),
            Value::from(OPERATION_PAGE_SIZE),
        );
        if let Some(token) = cursor {
            request.insert(NEXT_TOKEN_MEMBER.to_string(), Value::String(token));
        }
        let answered =
            call_cloud_map(ctx, region, "ListOperations", &Value::Object(request)).await?;
        let found = answered
            .get(OPERATIONS_MEMBER)
            .and_then(Value::as_array)
            .and_then(|page| {
                page.iter().find_map(|summary| {
                    Some((
                        text_member(summary, ID_MEMBER)?,
                        text_member(summary, STATUS_MEMBER)?,
                    ))
                })
            });
        if found.is_some() {
            return Ok(found);
        }
        // A page carrying a cursor has operations beyond it, which the match may be among.
        match text_member(&answered, NEXT_TOKEN_MEMBER) {
            Some(token) => cursor = Some(token),
            None => return Ok(None),
        }
    }
    Ok(None)
}

/// What Cloud Map says a FAILED operation failed for: its `ErrorMessage`, which is the sentence
/// that service worded about the creation, and its `ErrorCode` where it carries no message.
async fn operation_failure(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation_id: &str,
) -> Result<String, Refusal> {
    let request = serde_json::json!({ OPERATION_ID_MEMBER: operation_id });
    let answered = call_cloud_map(ctx, region, "GetOperation", &request).await?;
    let operation = answered.get(OPERATION_MEMBER).cloned().unwrap_or(Value::Null);
    Ok(text_member(&operation, ERROR_MESSAGE_MEMBER)
        .or_else(|| text_member(&operation, ERROR_CODE_MEMBER))
        .unwrap_or_else(|| {
            format!("Cloud Map reports the operation {operation_id} as {OPERATION_FAILED}")
        }))
}

/// The values Cloud Map publishes for a resource it now reports up.
///
/// A namespace's hosted zone is the one Cloud Map created for it, which is read here as well as
/// in the create: that zone lives in Route 53, so the id is one only the owning service can
/// report. A service's id, ARN and name are the ones `CreateService` answered with, and an
/// instance publishes nothing, so neither is read again.
pub(super) async fn ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => {
            read_namespace_attributes(ctx, region, kind, physical_resource_id).await
        }
        Kind::Service | Kind::Instance => Ok(BTreeMap::new()),
    }
}

/// Whether Cloud Map has finished removing a deleted resource.
///
/// Nothing is asked for any of the five: `DeleteNamespace`, `DeleteService` and
/// `DeregisterInstance` each take the resource away as they answer — a later read of one is that
/// resource's own not-found error — and the `Operation` a namespace delete and a deregistration
/// file is the RECORD of that delete rather than a state the resource is still in.
pub(super) fn removal() -> Removal {
    Removal::Gone
}

/// Apply a CHANGED declaration to the resource the stack already has, through Cloud Map's own
/// update call.
///
/// Each namespace takes its DESCRIPTION, and a DNS namespace its SOA record beside it: that is
/// what `UpdateHttpNamespace`, `UpdatePublicDnsNamespace` and `UpdatePrivateDnsNamespace` carry,
/// the name and the VPC being fixed when the namespace is made. A SERVICE takes its description,
/// the DNS records its `DnsConfig` declares and its health check, which is what `UpdateService`
/// carries. An INSTANCE is re-registered: "if you specify an existing `InstanceId` … the call
/// updates the existing [instance]", which is the one call that changes an instance's attributes.
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, &resource.resource_type, published_properties(kind))?;
    match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => {
            let mut change = Map::new();
            // `HttpNamespaceChange$Description` is required, so a declaration that DROPS the
            // description clears it rather than leaving the one the namespace was made with.
            change.insert(
                DESCRIPTION_PROPERTY.to_string(),
                Value::String(
                    text_member(declared, DESCRIPTION_PROPERTY).unwrap_or_default(),
                ),
            );
            if kind != Kind::HttpNamespace {
                if let Some(properties) = declared
                    .get(PROPERTIES_MEMBER)
                    .filter(|value| !value.is_null())
                {
                    change.insert(
                        PROPERTIES_MEMBER.to_string(),
                        modelled(PROPERTIES_MEMBER, properties),
                    );
                }
            }
            let request = serde_json::json!({
                ID_MEMBER: resource.physical_resource_id,
                NAMESPACE_MEMBER: Value::Object(change),
            });
            call_cloud_map(ctx, region, update_operation(kind), &request).await?;
            Ok(Some(Created::attributes(BTreeMap::new())))
        }
        Kind::Service => {
            let mut change = declared_members(declared, &["Description", "HealthCheckConfig"]);
            // `ServiceChange$DnsConfig` is a `DnsConfigChange`, which carries the records
            // alone: the namespace a service lives in and its routing policy are fixed when
            // Cloud Map makes it.
            if let Some(records) = declared
                .get(DNS_CONFIG_PROPERTY)
                .and_then(|config| config.get(DNS_RECORDS_MEMBER))
                .filter(|value| !value.is_null())
            {
                change.insert(
                    DNS_CONFIG_PROPERTY.to_string(),
                    serde_json::json!({
                        DNS_RECORDS_MEMBER: modelled(DNS_RECORDS_MEMBER, records),
                    }),
                );
            }
            let request = serde_json::json!({
                ID_MEMBER: resource.physical_resource_id,
                SERVICE_MEMBER: Value::Object(change),
            });
            call_cloud_map(ctx, region, update_operation(kind), &request).await?;
            Ok(Some(Created::attributes(BTreeMap::new())))
        }
        Kind::Instance => {
            let request = register_request(declared, resource)?;
            call_cloud_map(ctx, region, update_operation(kind), &request).await?;
            Ok(Some(Created::attributes(BTreeMap::new())))
        }
    }
}

/// Delete the resource a stack created: a namespace with `DeleteNamespace`, a service with
/// `DeleteService`, an instance with `DeregisterInstance` — each addressed by the identifier
/// Cloud Map knows it by, which is this resource's physical id.
///
/// One Cloud Map no longer holds is already in the state the delete asks for, judged on the CODE
/// and never on a message, which carries the request's own name.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let (operation, request) = match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => (
            "DeleteNamespace",
            serde_json::json!({ ID_MEMBER: resource.physical_resource_id }),
        ),
        Kind::Service => (
            "DeleteService",
            serde_json::json!({ ID_MEMBER: resource.physical_resource_id }),
        ),
        Kind::Instance => {
            let service_id = resource
                .properties
                .get(SERVICEDISCOVERY_SERVICE_ID_PROPERTY)
                .filter(|value| !value.is_null())
                .cloned()
                .ok_or_else(|| {
                    missing_property(
                        &resource.resource_type,
                        SERVICEDISCOVERY_SERVICE_ID_PROPERTY,
                    )
                })?;
            (
                "DeregisterInstance",
                serde_json::json!({
                    SERVICEDISCOVERY_SERVICE_ID_PROPERTY: service_id,
                    INSTANCE_ID_MEMBER: resource.physical_resource_id,
                }),
            )
        }
    };
    match call_cloud_map(ctx, region, operation, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if is_not_found(&refusal.code) => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as Cloud Map reports it, for drift detection: `GetNamespace`'s
/// own record of a namespace, `GetService`'s of a service and `GetInstance`'s of an instance,
/// under this type's property names. One that service no longer holds was deleted, which is what
/// `None` reports.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    match kind {
        Kind::HttpNamespace | Kind::PublicDnsNamespace | Kind::PrivateDnsNamespace => {
            let Some(reported) =
                read_namespace(ctx, region, &resource.physical_resource_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(reported_as(
                &reported,
                &["Description", "Name", "Properties"],
            )))
        }
        Kind::Service => {
            let Some(reported) = read_service(ctx, region, &resource.physical_resource_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(reported_as(
                &reported,
                &[
                    "Description",
                    "DnsConfig",
                    "HealthCheckConfig",
                    "HealthCheckCustomConfig",
                    "Name",
                    "NamespaceId",
                    "Type",
                ],
            )))
        }
        // An instance is read under the service its declaration names: `GetInstance` addresses
        // one by that pair and by nothing else, so a declaration naming no service is no
        // question Cloud Map could be asked.
        Kind::Instance => {
            let Some(service_id) = text_member(
                &resource.properties,
                SERVICEDISCOVERY_SERVICE_ID_PROPERTY,
            ) else {
                return Ok(None);
            };
            let request = serde_json::json!({
                SERVICEDISCOVERY_SERVICE_ID_PROPERTY: service_id,
                INSTANCE_ID_MEMBER: resource.physical_resource_id,
            });
            let reported = match call_cloud_map(ctx, region, "GetInstance", &request).await {
                Ok(answered) => answered.get("Instance").cloned().unwrap_or(Value::Null),
                Err(refusal) if is_not_found(&refusal.code) => return Ok(None),
                Err(refusal) => return Err(refusal),
            };
            let mut configuration = Map::new();
            if let Some(id) = text_member(&reported, "Id") {
                configuration.insert(
                    SERVICEDISCOVERY_INSTANCE_ID_PROPERTY.to_string(),
                    Value::String(id),
                );
            }
            if let Some(attributes) = reported.get(ATTRIBUTES_MEMBER) {
                configuration.insert(
                    INSTANCE_ATTRIBUTES_PROPERTY.to_string(),
                    attributes.clone(),
                );
            }
            Ok(Some(Value::Object(configuration)))
        }
    }
}

/// The namespace Cloud Map reports under the id the stack created it with, or `None` where it
/// holds none.
async fn read_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    namespace_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ ID_MEMBER: namespace_id });
    match call_cloud_map(ctx, region, "GetNamespace", &request).await {
        Ok(answered) => Ok(answered.get(NAMESPACE_MEMBER).cloned()),
        Err(refusal) if is_not_found(&refusal.code) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The service Cloud Map reports under the id its create minted, or `None` where it holds none.
async fn read_service(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    service_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ ID_MEMBER: service_id });
    match call_cloud_map(ctx, region, "GetService", &request).await {
        Ok(answered) => Ok(answered.get(SERVICE_MEMBER).cloned()),
        Err(refusal) if is_not_found(&refusal.code) => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The attributes the stack publishes for a namespace, read off Cloud Map's own record of it:
/// the id and the ARN that service assigned, and — for a DNS namespace — the Route 53 hosted
/// zone it created with the namespace.
///
/// A record that service does not report leaves the attributes it would have filled alone: one
/// unanswered read is no evidence about the resource.
async fn read_namespace_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    namespace_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    let Some(reported) = read_namespace(ctx, region, namespace_id).await? else {
        return Ok(BTreeMap::new());
    };
    let resource_type = type_of(kind);
    let mut attributes = reported_identity_attributes(resource_type, &reported);
    if kind != Kind::HttpNamespace {
        if let Some(hosted_zone_id) = reported
            .get(PROPERTIES_MEMBER)
            .and_then(|properties| properties.get(DNS_PROPERTIES_MEMBER))
            .and_then(|dns| text_member(dns, SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE))
        {
            attributes.insert(
                SERVICEDISCOVERY_HOSTED_ZONE_ID_ATTRIBUTE.to_string(),
                hosted_zone_id,
            );
        }
    }
    Ok(attributes)
}

/// The `Id` and the ARN one reported record carries, under every name the type publishes them
/// as: `Id`/`<Type>Id` is the uniform convention's reading of the physical id, and the ARN is
/// reported under each of the four spellings a type's return values may use
/// ([`crate::state::template::arn_attributes`], the one place that set is built).
fn reported_identity_attributes(
    resource_type: &str,
    reported: &Value,
) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    if let Some(id) = text_member(reported, ID_MEMBER) {
        attributes.insert(ID_MEMBER.to_string(), id);
    }
    if let Some(arn) = text_member(reported, ARN_MEMBER) {
        attributes.extend(crate::state::template::arn_attributes(resource_type, &arn));
    }
    attributes
}

/// The resource `Targets[<target>]` of the operation `operation_id` names.
///
/// A namespace create answers an `OperationId` and nothing else, and the id Cloud Map minted is
/// in that operation's targets, so this is how the created namespace is identified at all. An
/// operation that service reports FAILED is one nothing is going to bring up, so its own
/// `ErrorCode`/`ErrorMessage` is the refusal of the create.
async fn operation_target(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation_id: &str,
    target: &str,
) -> Result<String, Refusal> {
    let request = serde_json::json!({ OPERATION_ID_MEMBER: operation_id });
    let answered = call_cloud_map(ctx, region, "GetOperation", &request).await?;
    let operation = answered.get(OPERATION_MEMBER).cloned().unwrap_or(Value::Null);
    if text_member(&operation, STATUS_MEMBER).as_deref() == Some(OPERATION_FAILED) {
        return Err(Refusal {
            code: text_member(&operation, ERROR_CODE_MEMBER)
                .unwrap_or_else(|| MISSING_PROPERTY.to_string()),
            message: text_member(&operation, ERROR_MESSAGE_MEMBER).unwrap_or_else(|| {
                format!("Cloud Map reports the operation {operation_id} as {OPERATION_FAILED}")
            }),
        });
    }
    operation
        .get(TARGETS_MEMBER)
        .and_then(|targets| text_member(targets, target))
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!(
                "Cloud Map reported no {target} target for the operation {operation_id}, which is \
                 what a reference to this resource resolves to"
            ),
        })
}

/// Whether an error code is Cloud Map answering that it holds no such resource.
fn is_not_found(code: &str) -> bool {
    matches!(
        code,
        NAMESPACE_NOT_FOUND | SERVICE_NOT_FOUND | INSTANCE_NOT_FOUND
    )
}

/// The resource type one kind is, for the arms that word a message or build an ARN attribute
/// set from it.
fn type_of(kind: Kind) -> &'static str {
    match kind {
        Kind::HttpNamespace => SERVICEDISCOVERY_HTTP_NAMESPACE,
        Kind::PublicDnsNamespace => SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE,
        Kind::PrivateDnsNamespace => SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE,
        Kind::Service => SERVICEDISCOVERY_SERVICE,
        Kind::Instance => SERVICEDISCOVERY_INSTANCE,
    }
}

/// Every property the type one kind is publishes.
fn published_properties(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::HttpNamespace => HTTP_NAMESPACE_PROPERTIES,
        Kind::PublicDnsNamespace => PUBLIC_DNS_NAMESPACE_PROPERTIES,
        Kind::PrivateDnsNamespace => PRIVATE_DNS_NAMESPACE_PROPERTIES,
        Kind::Service => SERVICE_PROPERTIES,
        Kind::Instance => INSTANCE_PROPERTIES,
    }
}

/// The members of one kind's create a declaration states under the same names.
///
/// An INSTANCE has none: every one of its properties is a member of `RegisterInstance`, two
/// under that call's own names, so its request is built by [`register_request`] rather than
/// from a member list ([`register_request`]'s own test is where that invariant is stated).
fn create_members(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::HttpNamespace => HTTP_NAMESPACE_MEMBERS,
        Kind::PublicDnsNamespace => DNS_NAMESPACE_MEMBERS,
        Kind::PrivateDnsNamespace => PRIVATE_DNS_NAMESPACE_MEMBERS,
        Kind::Service => SERVICE_MEMBERS,
        Kind::Instance => &[],
    }
}

/// The Cloud Map operation that CREATES a resource of one kind.
fn create_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::HttpNamespace => "CreateHttpNamespace",
        Kind::PublicDnsNamespace => "CreatePublicDnsNamespace",
        Kind::PrivateDnsNamespace => "CreatePrivateDnsNamespace",
        Kind::Service => "CreateService",
        // An instance is REGISTERED in a service: that is the one call that makes one.
        Kind::Instance => "RegisterInstance",
    }
}

/// The Cloud Map operation that CHANGES a resource of one kind.
fn update_operation(kind: Kind) -> &'static str {
    match kind {
        Kind::HttpNamespace => "UpdateHttpNamespace",
        Kind::PublicDnsNamespace => "UpdatePublicDnsNamespace",
        Kind::PrivateDnsNamespace => "UpdatePrivateDnsNamespace",
        Kind::Service => "UpdateService",
        // "If you specify an existing `InstanceId` … the call updates the existing
        // [instance]", so re-registering is the one call that changes an instance.
        Kind::Instance => "RegisterInstance",
    }
}

/// Put the tags the resource must carry on a create request: the ones its declaration asks for
/// and the ones CloudFormation propagates, as the `[{Key, Value}]` `TagList` every Cloud Map
/// create takes.
fn carry_tags(
    request: &mut Map<String, Value>,
    properties: &Value,
    propagated: &[(String, String)],
) {
    let tags = storable_tags(properties, propagated);
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), key_value_tag_list(&tags));
    }
}

/// One reported record under this type's PROPERTY names, which is the vocabulary drift is judged
/// in: a member the answer does not carry contributes nothing, a property the read does not
/// report being no evidence of anything.
fn reported_as(reported: &Value, members: &[&str]) -> Value {
    let mut configuration = Map::new();
    for member in members {
        if let Some(value) = reported.get(*member).filter(|value| !value.is_null()) {
            configuration.insert((*member).to_string(), value.clone());
        }
    }
    Value::Object(configuration)
}

/// The members of `members` a declaration states, as the request carries them: the type and the
/// API spell each one identically, so every value goes through as the template wrote it — read
/// as the TYPE the owning API models it as ([`modelled`]).
fn declared_members(properties: &Value, members: &[&str]) -> Map<String, Value> {
    let mut request = Map::new();
    for member in members {
        if let Some(value) = properties.get(*member).filter(|value| !value.is_null()) {
            request.insert((*member).to_string(), modelled(member, value));
        }
    }
    request
}

/// One declared value as the TYPE the owning API models that member as, through the one walk
/// every provisioning arm in this position shares ([`super::modelled_declaration`]): the
/// members this module's types declare as NUMBERS are [`NUMERIC_MEMBERS`], and every other
/// member travels as the template wrote it.
fn modelled(member: &str, value: &Value) -> Value {
    super::modelled_declaration(member, value, &|member| {
        if NUMERIC_MEMBERS.contains(&member) {
            super::ModelledScalar::Number
        } else {
            super::ModelledScalar::AsWritten
        }
    })
}

/// One text member of a reported record, absent where the answer carries none or carries it
/// empty — an empty identifier is no identifier a caller could address.
fn text_member(reported: &Value, member: &str) -> Option<String> {
    reported
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// One text member an answer MUST carry for the resource to have an identity at all, or the
/// refusal that says it did not.
fn required_member(
    answered: &Value,
    member: &str,
    resource_type: &str,
) -> Result<String, Refusal> {
    text_member(answered, member).ok_or_else(|| Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!(
            "Cloud Map reported no {member} for the {resource_type} it created, which is what a \
             reference to this resource resolves to"
        ),
    })
}

/// The refusal for a declaration that states none of a property the owning call requires,
/// naming the property: refusing is what keeps a resource from being reported as created from a
/// declaration that describes none.
fn missing_property(resource_type: &str, property: &str) -> Refusal {
    Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!("{resource_type} declares no {property}"),
    }
}

/// The refusal for a declared property these arms do not apply, naming which property it was:
/// refusing is what keeps a resource from being reported as created — or as updated — with less
/// than its declaration asked for. `properties` is the document the deployment DECLARES, which
/// on an update is not the one the record carries.
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

/// Call one Cloud Map operation on the awsJson1_1 wire its model declares.
async fn call_cloud_map(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        CLOUD_MAP_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{CLOUD_MAP_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A resource record of one of these types, as the naming rules leave one.
    fn declared(resource_type: &str, properties: Value) -> StoredResource {
        StoredResource {
            logical_id: "Namespace".to_string(),
            resource_type: resource_type.to_string(),
            physical_resource_id: "ns-0123456789abcdef".to_string(),
            attributes: BTreeMap::new(),
            metadata: None,
            properties,
            materialised: false,
            deletion_policy: None,
            update_replace_policy: None,
        }
    }

    /// Each of the five types resolves to its own kind, and nothing else does.
    #[test]
    fn each_type_resolves_to_its_kind() {
        let cases: &[(&str, Option<Kind>)] = &[
            (SERVICEDISCOVERY_HTTP_NAMESPACE, Some(Kind::HttpNamespace)),
            (
                SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE,
                Some(Kind::PublicDnsNamespace),
            ),
            (
                SERVICEDISCOVERY_PRIVATE_DNS_NAMESPACE,
                Some(Kind::PrivateDnsNamespace),
            ),
            (SERVICEDISCOVERY_SERVICE, Some(Kind::Service)),
            (SERVICEDISCOVERY_INSTANCE, Some(Kind::Instance)),
            ("AWS::ServiceDiscovery::NotAType", None),
            ("AWS::SQS::Queue", None),
        ];
        for (resource_type, expected) in cases {
            let actual = kind_of(resource_type);
            assert_eq!(
                actual, *expected,
                "case {resource_type:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: every member these arms send is a published property of the type they send it for,
    /// so a declaration is carried out in Cloud Map's own vocabulary and nothing is invented
    /// for it. The `Name` each create carries is CloudFormation's decision rather than a member
    /// read off the declaration, which is why it is not in the member lists.
    ///
    /// The four kinds whose request [`declared_members`] builds; an INSTANCE's is
    /// [`register_request`]'s, and the test of that function is where its own invariant stands.
    #[test]
    fn every_sent_member_is_a_published_property() {
        for kind in [
            Kind::HttpNamespace,
            Kind::PublicDnsNamespace,
            Kind::PrivateDnsNamespace,
            Kind::Service,
        ] {
            let published = published_properties(kind);
            for member in create_members(kind) {
                assert!(
                    published.contains(member),
                    "case {:?}: expected the sent member {member:?} to be a published property \
                     of {}, got none",
                    kind,
                    type_of(kind)
                );
            }
            for property in published {
                let placed = create_members(kind).contains(property)
                    || *property == NAME_MEMBER
                    || *property == TAGS_MEMBER;
                assert!(
                    placed,
                    "case {kind:?}: expected the property {property:?} to be a create member, \
                     the name CloudFormation decides or the propagated tags, got none"
                );
            }
        }
    }

    /// A time to live a template quoted reaches Cloud Map as the NUMBER its model declares, at
    /// every depth the declaration nests it, and a value that names no number is left exactly as
    /// the template wrote it.
    #[test]
    fn a_quoted_ttl_reaches_the_api_as_a_number() {
        let declared = serde_json::json!({
            "Properties": {"DnsProperties": {"SOA": {"TTL": "60"}}},
        });
        let carried = Value::Object(declared_members(&declared, DNS_NAMESPACE_MEMBERS));
        let expected = serde_json::json!({
            "Properties": {"DnsProperties": {"SOA": {"TTL": 60}}},
        });
        assert_eq!(
            carried, expected,
            "case \"a quoted SOA TTL\": expected {expected}, got {carried}"
        );
        let declared = serde_json::json!({
            "DnsConfig": {
                "DnsRecords": [{"Type": "A", "TTL": "15"}, {"Type": "SRV", "TTL": 30}],
                "RoutingPolicy": "MULTIVALUE",
            },
            "HealthCheckConfig": {"Type": "HTTP", "FailureThreshold": "2"},
        });
        let carried = Value::Object(declared_members(&declared, SERVICE_MEMBERS));
        let expected = serde_json::json!({
            "DnsConfig": {
                "DnsRecords": [{"Type": "A", "TTL": 15}, {"Type": "SRV", "TTL": 30}],
                "RoutingPolicy": "MULTIVALUE",
            },
            "HealthCheckConfig": {"Type": "HTTP", "FailureThreshold": 2},
        });
        assert_eq!(
            carried, expected,
            "case \"a service's quoted record TTLs\": expected {expected}, got {carried}"
        );
        let declared = serde_json::json!({"HealthCheckConfig": {"FailureThreshold": "many"}});
        let carried = Value::Object(declared_members(&declared, SERVICE_MEMBERS));
        assert_eq!(
            carried, declared,
            "case \"a threshold that names no number\": expected it to travel as written, got \
             {carried}"
        );
    }

    /// A property no call here applies is refused, naming it; every property each type
    /// publishes is accepted.
    #[test]
    fn an_unapplied_property_is_refused() {
        assert!(
            refuse_unapplied_properties(
                &serde_json::json!({
                    "Name": "orders.internal",
                    "Description": "the orders namespace",
                    "Properties": {"DnsProperties": {"SOA": {"TTL": 60}}},
                    "Tags": [],
                }),
                SERVICEDISCOVERY_PUBLIC_DNS_NAMESPACE,
                PUBLIC_DNS_NAMESPACE_PROPERTIES,
            )
            .is_ok(),
            "case \"every published property of a public DNS namespace\": expected it to be \
             applied, got a refusal"
        );
        let refusal = refuse_unapplied_properties(
            &serde_json::json!({"Name": "orders", "Unheard": 1}),
            SERVICEDISCOVERY_HTTP_NAMESPACE,
            HTTP_NAMESPACE_PROPERTIES,
        )
        .expect_err("case \"a property no call carries\": expected a refusal");
        assert!(
            refusal.message.contains("Unheard"),
            "case \"a property no call carries\": expected the refusal to name it, got {:?}",
            refusal.message
        );
    }

    /// An instance's request names the service it belongs to, the identifier CloudFormation
    /// decided and the attributes it answers discovery with — which is every property the type
    /// publishes, so nothing of its declaration is dropped — and a declaration naming no
    /// service or no attributes is refused naming the property.
    #[test]
    fn an_instances_request_carries_the_service_the_id_and_the_attributes() {
        let resource = declared(
            SERVICEDISCOVERY_INSTANCE,
            serde_json::json!({
                "ServiceId": "srv-abcdefghij123456",
                "InstanceAttributes": {"AWS_INSTANCE_IPV4": "10.0.0.7"},
            }),
        );
        let request = register_request(&resource.properties, &resource)
            .expect("case \"a full instance declaration\": expected the request");
        let expected = serde_json::json!({
            "ServiceId": "srv-abcdefghij123456",
            "InstanceId": "ns-0123456789abcdef",
            "Attributes": {"AWS_INSTANCE_IPV4": "10.0.0.7"},
        });
        assert_eq!(
            request, expected,
            "case \"a full instance declaration\": expected {expected}, got {request}"
        );
        // The invariant the member-list table states for every other kind: each published
        // property reaches the call, two of them under `RegisterInstance`'s own member names.
        for property in INSTANCE_PROPERTIES {
            let carried = match *property {
                SERVICEDISCOVERY_INSTANCE_ID_PROPERTY => INSTANCE_ID_MEMBER,
                INSTANCE_ATTRIBUTES_PROPERTY => ATTRIBUTES_MEMBER,
                other => other,
            };
            assert!(
                request.get(carried).is_some(),
                "case {property:?}: expected the published property to reach the call as \
                 {carried:?}, got {request}"
            );
        }
        for (case, properties, named) in [
            (
                "an instance naming no service",
                serde_json::json!({"InstanceAttributes": {}}),
                SERVICEDISCOVERY_SERVICE_ID_PROPERTY,
            ),
            (
                "an instance naming no attributes",
                serde_json::json!({"ServiceId": "srv-abcdefghij123456"}),
                INSTANCE_ATTRIBUTES_PROPERTY,
            ),
        ] {
            let refusal = register_request(&properties, &resource)
                .expect_err(&format!("case {case:?}: expected a refusal"));
            assert!(
                refusal.message.contains(named),
                "case {case:?}: expected the refusal to name {named:?}, got {:?}",
                refusal.message
            );
        }
    }

    /// The identity attributes read off an answer are the id and the ARN Cloud Map assigned,
    /// under every spelling the type publishes an ARN as, and an answer carrying neither reports
    /// no attribute rather than an empty one.
    #[test]
    fn the_assigned_id_and_arn_are_the_published_attributes() {
        let reported = serde_json::json!({
            "Id": "ns-0123456789abcdef",
            "Arn": "arn:aws:servicediscovery:us-east-1:000000000000:namespace/ns-0123456789abcdef",
        });
        let attributes =
            reported_identity_attributes(SERVICEDISCOVERY_HTTP_NAMESPACE, &reported);
        assert_eq!(
            attributes.get("Id").map(String::as_str),
            Some("ns-0123456789abcdef"),
            "case \"an answered namespace\": expected the id to be the Id attribute, got \
             {attributes:?}"
        );
        for spelling in ["Arn", "ARN", "HttpNamespaceArn", "HttpNamespaceARN"] {
            assert_eq!(
                attributes.get(spelling).map(String::as_str),
                Some(
                    "arn:aws:servicediscovery:us-east-1:000000000000:namespace/ns-0123456789abcdef"
                ),
                "case {spelling:?}: expected the answered ARN under that name, got {attributes:?}"
            );
        }
        let silent = serde_json::json!({});
        let attributes = reported_identity_attributes(SERVICEDISCOVERY_SERVICE, &silent);
        assert!(
            attributes.is_empty(),
            "case \"an answer carrying neither\": expected no attribute, got {attributes:?}"
        );
    }

    /// Each kind's create and update is the Cloud Map operation that service publishes for it.
    #[test]
    fn each_kind_names_its_own_namespace_calls() {
        let cases: &[(Kind, &str, &str)] = &[
            (
                Kind::HttpNamespace,
                "CreateHttpNamespace",
                "UpdateHttpNamespace",
            ),
            (
                Kind::PublicDnsNamespace,
                "CreatePublicDnsNamespace",
                "UpdatePublicDnsNamespace",
            ),
            (
                Kind::PrivateDnsNamespace,
                "CreatePrivateDnsNamespace",
                "UpdatePrivateDnsNamespace",
            ),
        ];
        for (kind, create, update) in cases {
            assert_eq!(
                create_operation(*kind), *create,
                "case {kind:?}: expected the create {create:?}, got {:?}",
                create_operation(*kind)
            );
            assert_eq!(
                update_operation(*kind), *update,
                "case {kind:?}: expected the update {update:?}, got {:?}",
                update_operation(*kind)
            );
        }
    }
}
