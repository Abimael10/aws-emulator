// SPDX-License-Identifier: CC0-1.0

//! The `AWS::AppSync::*` types a stack declares BEYOND a single GraphQL API's own estate:
//! an EVENT API, the channel namespaces published events travel over on it, and the LINK of
//! a source API into a Merged API — created in AWS AppSync itself.
//!
//! This is the per-type half of provisioning for those three types, the half
//! [`super::super::provisioner`] holds for every other type this emulator creates; it lives
//! in its own module beside [`super::appsync_resources`] for the reason that module records —
//! the catalogue file is far past the length a change to this repository may touch — and is
//! reached through the same seam ([`super::Owner`]).
//!
//! ## Why the resources are really created
//!
//! Each of these three is a handle the owning service ASSIGNS and every later call takes.
//! `CreateApi` takes a name and no identifier and answers the `apiId` it minted, the ARN
//! built on it and the `dns` records of the two endpoints the Event API is SERVED at — a
//! publisher POSTs an event to the first and a subscriber dials the second — so a stack whose
//! `Api` exists nowhere publishes endpoints nothing serves and an id no `GetApi` resolves. A
//! channel namespace exists ON that API and is what admits a publish or a subscribe on a
//! channel, and an association is what makes a Merged API serve a source API's schema at all:
//! `AssociateSourceGraphqlApi` mints the association's own `associationId`, and the merge it
//! schedules is the only thing that puts the source API's types into the merged one. So all
//! three are provisioned here rather than left declared-only, and what each create answers is
//! the resource's identity and attributes
//! ([`crate::state::template::identity_is_service_assigned`] names all three).
//!
//! ## The calls
//!
//! Each type is the call AppSync publishes for it, on the URI that operation's own `@http`
//! binding declares (that model speaks `restJson1`, so every call rides [`call_rest_json`]):
//!
//! * `AWS::AppSync::Api` is `CreateApi` (`POST /v2/apis`), changed by `UpdateApi`
//!   (`POST /v2/apis/{apiId}`) and removed by `DeleteApi`;
//! * `AWS::AppSync::ChannelNamespace` is `CreateChannelNamespace`
//!   (`POST /v2/apis/{apiId}/channelNamespaces`), changed and removed by the calls on that
//!   same namespace (`UpdateChannelNamespace`, `DeleteChannelNamespace`);
//! * `AWS::AppSync::SourceApiAssociation` is `AssociateSourceGraphqlApi`
//!   (`POST /v1/mergedApis/{mergedApiIdentifier}/sourceApiAssociations`), which SCHEDULES the
//!   merge of the source API's schema into the Merged API — it answers the association in the
//!   non-terminal `MERGE_SCHEDULED` state — so this resource's creation is held open until
//!   `GetSourceApiAssociation` reports the merge finished ([`readiness`]), and its removal
//!   until that read no longer finds the association ([`removal`]), `DisassociateSourceGraphqlApi`
//!   scheduling the deletion rather than performing it.
//!
//! ## The properties
//!
//! Every property these types publish is a member of the owning call under the SAME name with
//! its initial letter lower-cased, nested members included (`EventConfig.AuthProviders`
//! against `eventConfig.authProviders`, `HandlerConfigs.OnPublish.Integration.DataSourceName`
//! against `handlerConfigs.onPublish.integration.dataSourceName`), which is the walk
//! [`lower_camel_members`] is. Three are not carried that way:
//!
//! * `ApiId` ADDRESSES the call rather than travelling in it: it is the `@httpLabel` of every
//!   call on a channel namespace, as `MergedApiIdentifier` is of every call on an association;
//! * `Tags` is the `[{Key, Value}]` list a template writes against the `TagMap` these creates
//!   take, so it is rendered as that map;
//! * `CodeHandlers` is the handler CODE the namespace runs, carried verbatim.
//!
//! A declared property no call here carries is REFUSED rather than dropped
//! ([`refuse_unapplied_properties`]): a resource reported created with less than its
//! declaration asked for is the shallow success these arms exist to avoid. `CodeS3Location`
//! holds the text of `CodeHandlers` in an Amazon S3 object, which is READ into that property
//! before the create is made ([`super::appsync_s3_documents`]), so a namespace is created with
//! the handlers it named either way.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use super::{applicable_tags, call_rest_json, percent_encoded, Created, Readiness, Refusal, Removal};
use crate::state::provisioner::{attribute_text, lower_camel_members, tags_as_map, tags_of};
use crate::state::template::{
    APPSYNC_API, APPSYNC_API_ID_PROPERTY, APPSYNC_CHANNEL_NAMESPACE, DNS_HTTP_ATTRIBUTE,
    DNS_REALTIME_ATTRIBUTE, APPSYNC_MERGED_API_IDENTIFIER_PROPERTY, APPSYNC_SOURCE_API_ASSOCIATION,
    APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY,
};
use crate::state::StoredResource;

/// The SigV4 signing name of the service that owns these types, which is what routes the
/// in-process call to AWS AppSync's own handlers (that model's `sigv4Name`/`endpointPrefix`).
const APPSYNC_SIGNING_NAME: &str = "appsync";

/// The error code AppSync answers for a resource it does not hold (`NotFoundException`, the
/// modelled refusal of its reads and deletes), which is what says a delete has nothing left to
/// remove and a read has nothing to report.
const NOT_FOUND: &str = "NotFoundException";

/// The code a refusal of this module's own carries for a declaration it cannot act on: the
/// same one the catalogue's arms use for a property this emulator does not apply.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The code a refusal carries for a declaration that states none of a required property.
const MISSING_PROPERTY: &str = "InvalidRequest";

/// The code a create that answered 2xx without the identifier AppSync assigns is reported
/// under.
const UNIDENTIFIED_FAILURE: &str = "InternalFailure";

/// How deeply a declared property document is already nested where [`lower_camel_members`] is
/// given it: the STARTING depth of that walk, which is zero for a property document handed to
/// it whole.
const PROPERTY_NESTING: usize = 0;

/// Which of the three types one resource is.
///
/// Visible to the whole of this service's state module, because it travels with
/// [`super::Owner`]: the algorithm resolves a type's owner once and passes it back here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state) enum Kind {
    /// `AWS::AppSync::Api`.
    EventApi,
    /// `AWS::AppSync::ChannelNamespace`.
    ChannelNamespace,
    /// `AWS::AppSync::SourceApiAssociation`.
    SourceApiAssociation,
}

/// The kind `resource_type` names, or `None` for a type this module does not own.
pub(super) fn kind_of(resource_type: &str) -> Option<Kind> {
    match resource_type {
        APPSYNC_API => Some(Kind::EventApi),
        APPSYNC_CHANNEL_NAMESPACE => Some(Kind::ChannelNamespace),
        APPSYNC_SOURCE_API_ASSOCIATION => Some(Kind::SourceApiAssociation),
        _ => None,
    }
}

/// The property a resource is created under, where its type carries one.
const NAME: &str = "Name";

/// The property carrying a resource's tags, which reaches a create as a map.
const TAGS: &str = "Tags";

/// The member that tag map travels as.
const TAGS_MEMBER: &str = "tags";

/// Every property `AWS::AppSync::Api` publishes. A declared property this list does not carry
/// is refused rather than dropped.
const API_PROPERTIES: &[&str] = &[EVENT_CONFIG, NAME, "OwnerContact", TAGS];

/// The property carrying the Event API's authorization and logging configuration, which
/// `CreateApi` and `UpdateApi` both declare required.
const EVENT_CONFIG: &str = "EventConfig";

/// The properties of an Event API that travel as the create's own members, each the property
/// with its initial letter lower-cased.
const API_MEMBERS: &[&str] = &[EVENT_CONFIG, "OwnerContact"];

/// Every property `AWS::AppSync::ChannelNamespace` publishes.
const CHANNEL_NAMESPACE_PROPERTIES: &[&str] = &[
    APPSYNC_API_ID_PROPERTY,
    "CodeHandlers",
    CODE_S3_LOCATION,
    "HandlerConfigs",
    NAME,
    "PublishAuthModes",
    "SubscribeAuthModes",
    TAGS,
];

/// The property naming an Amazon S3 object the handler code is held in, which is read into
/// `CodeHandlers` before the create is made ([`super::appsync_s3_documents`]).
const CODE_S3_LOCATION: &str = "CodeS3Location";

/// The properties of a channel namespace that travel as the create's own members.
const CHANNEL_NAMESPACE_MEMBERS: &[&str] = &[
    "CodeHandlers",
    "HandlerConfigs",
    "PublishAuthModes",
    "SubscribeAuthModes",
];

/// Every property `AWS::AppSync::SourceApiAssociation` publishes.
const ASSOCIATION_PROPERTIES: &[&str] = &[
    "Description",
    APPSYNC_MERGED_API_IDENTIFIER_PROPERTY,
    ASSOCIATION_CONFIG,
    APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY,
];

/// The property carrying which merging option the link is made under
/// (`SourceApiAssociationConfig.MergeType`: `AUTO_MERGE` or the default `MANUAL_MERGE`).
const ASSOCIATION_CONFIG: &str = "SourceApiAssociationConfig";

/// The properties of an association that travel as the create's own members beside the source
/// API it links.
const ASSOCIATION_MEMBERS: &[&str] = &["Description", ASSOCIATION_CONFIG];

/// The member `CreateApi` answers the created Event API in, and the one
/// `CreateChannelNamespace` answers its namespace in.
const API_MEMBER: &str = "api";
const CHANNEL_NAMESPACE_MEMBER: &str = "channelNamespace";

/// The member every answer about an association carries it in.
const ASSOCIATION_MEMBER: &str = "sourceApiAssociation";

/// The member an `Api` carries its endpoints' DNS records in, and the keys of the two records
/// it holds: "`Api$dns` … will include an HTTP and a real-time endpoint".
const DNS_MEMBER: &str = "dns";
const HTTP_ENDPOINT: &str = "HTTP";
const REALTIME_ENDPOINT: &str = "REALTIME";

/// The attributes an association publishes, as `(attribute, the member AppSync answers it in)`
/// — each read off that service's own record of the link, so the ids, the ARNs of both sides
/// and the state the link is in are the ones AppSync reports.
const ASSOCIATION_ATTRIBUTES: &[(&str, &str)] = &[
    ("AssociationArn", "associationArn"),
    ("AssociationId", "associationId"),
    ("MergedApiArn", "mergedApiArn"),
    ("MergedApiId", "mergedApiId"),
    ("SourceApiArn", "sourceApiArn"),
    ("SourceApiAssociationStatus", "sourceApiAssociationStatus"),
    ("SourceApiId", "sourceApiId"),
];

/// The member a reported association carries its state in, and the member carrying "the
/// detailed message related to the current state of the source API association".
const ASSOCIATION_STATUS_MEMBER: &str = "sourceApiAssociationStatus";
const ASSOCIATION_STATUS_DETAIL_MEMBER: &str = "sourceApiAssociationStatusDetail";

/// The `SourceApiAssociationStatus` of a link whose merge has been carried out, which is the
/// state the created association settles in.
const MERGE_SUCCESS: &str = "MERGE_SUCCESS";

/// The two `SourceApiAssociationStatus` values no wait brings a created association out of:
/// the merge failed, or the automatic merge could not even be scheduled.
const MERGE_FAILED: &str = "MERGE_FAILED";
const AUTO_MERGE_SCHEDULE_FAILED: &str = "AUTO_MERGE_SCHEDULE_FAILED";

/// Create the resource one of these types declares, in AWS AppSync itself — so a client that
/// deployed the stack reads every one of them back through that service's own reads, publishes
/// an event to the endpoint the stack published, and queries a Merged API that really serves
/// its source API's schema.
pub(super) async fn create(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unapplied_properties(&resource.properties, resource, properties_of(kind))?;
    let fetched = super::appsync_s3_documents::fetched(
        ctx,
        region,
        &resource.properties,
        super::appsync_s3_documents::CHANNEL_NAMESPACE_DOCUMENTS,
    )
    .await?
    .map(|properties| StoredResource {
        properties,
        ..resource.clone()
    });
    let resource = fetched.as_ref().unwrap_or(resource);
    match kind {
        Kind::EventApi => create_api(ctx, region, resource, propagated).await,
        Kind::ChannelNamespace => create_channel_namespace(ctx, region, resource, propagated).await,
        Kind::SourceApiAssociation => associate_source_api(ctx, region, resource).await,
    }
}

/// Create the Event API an `AWS::AppSync::Api` declares, under the name the declaration asked
/// for.
///
/// AppSync assigns the API's identity: the call takes no identifier and answers the `apiId` it
/// minted — which is what this type's `Ref` returns — the ARN built on it and the DNS records
/// of the two endpoints the API is now SERVED at, which no naming rule could compute. An
/// answer carrying no endpoint publishes an address nothing serves, so the resource is reported
/// as failed rather than created without them ([`minted`]).
async fn create_api(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let mut request = Map::new();
    request.insert(
        "name".to_string(),
        Value::String(required(resource, properties, NAME)?),
    );
    carry(&mut request, properties, API_MEMBERS)?;
    carry_tags(&mut request, properties, propagated);
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        EVENT_APIS_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let api = answered.get(API_MEMBER).cloned().unwrap_or(Value::Null);
    Ok(Created {
        physical_resource_id: Some(minted(&api, "apiId", CREATE_API)?),
        attributes: api_attributes(&api)?,
    })
}

/// The attributes an Event API publishes, read off what its own create — or its own update —
/// answered: the ARN AppSync built, the id it minted, and the DNS record of each of the two
/// endpoints the API is served at.
fn api_attributes(api: &Value) -> Result<BTreeMap<String, String>, Refusal> {
    let dns = api.get(DNS_MEMBER).cloned().unwrap_or(Value::Null);
    let mut attributes = BTreeMap::new();
    attributes.insert("ApiArn".to_string(), minted(api, "apiArn", CREATE_API)?);
    attributes.insert("ApiId".to_string(), minted(api, "apiId", CREATE_API)?);
    attributes.insert(
        DNS_HTTP_ATTRIBUTE.to_string(),
        minted(&dns, HTTP_ENDPOINT, CREATE_API)?,
    );
    attributes.insert(
        DNS_REALTIME_ATTRIBUTE.to_string(),
        minted(&dns, REALTIME_ENDPOINT, CREATE_API)?,
    );
    Ok(attributes)
}

/// Create the channel namespace an `AWS::AppSync::ChannelNamespace` declares, on the Event API
/// its `ApiId` names — so a publish and a subscribe on that namespace are really admitted by
/// that API.
///
/// `Ref` returns the namespace's ARN, which `CreateChannelNamespace` answers under
/// `channelNamespace.channelNamespaceArn`: it carries the API's minted id, so only that service
/// can report it.
async fn create_channel_namespace(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let api_id = required(resource, properties, APPSYNC_API_ID_PROPERTY)?;
    let name = required(resource, properties, NAME)?;
    let mut request = Map::new();
    request.insert("name".to_string(), Value::String(name));
    carry(&mut request, properties, CHANNEL_NAMESPACE_MEMBERS)?;
    carry_tags(&mut request, properties, propagated);
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &channel_namespaces_uri(&api_id),
        Some(&Value::Object(request)),
    )
    .await?;
    let namespace = answered
        .get(CHANNEL_NAMESPACE_MEMBER)
        .cloned()
        .unwrap_or(Value::Null);
    let arn = minted(
        &namespace,
        "channelNamespaceArn",
        "CreateChannelNamespace",
    )?;
    let mut attributes = BTreeMap::new();
    attributes.insert("ChannelNamespaceArn".to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Link the source API an `AWS::AppSync::SourceApiAssociation` declares into the Merged API it
/// names, through `AssociateSourceGraphqlApi` — the call that makes the Merged API serve that
/// source API's schema.
///
/// AppSync assigns the association's identity: the call takes no identifier and answers the
/// `associationId` it generated and the `associationArn` built on it under the Merged API,
/// which is what this type's `Ref` returns and what every later call on the link takes.
async fn associate_source_api(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let properties = &resource.properties;
    let merged = required(resource, properties, APPSYNC_MERGED_API_IDENTIFIER_PROPERTY)?;
    let source = required(resource, properties, APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY)?;
    let mut request = Map::new();
    request.insert("sourceApiIdentifier".to_string(), Value::String(source));
    carry(&mut request, properties, ASSOCIATION_MEMBERS)?;
    let answered = call_appsync(
        ctx,
        region,
        "POST",
        &source_api_associations_uri(&merged),
        Some(&Value::Object(request)),
    )
    .await?;
    let association = answered
        .get(ASSOCIATION_MEMBER)
        .cloned()
        .unwrap_or(Value::Null);
    let arn = minted(&association, "associationArn", ASSOCIATE_SOURCE_API)?;
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes: association_attributes(&association),
    })
}

/// The attributes an association publishes, read off that service's own record of the link
/// ([`ASSOCIATION_ATTRIBUTES`]). A member the answer does not carry contributes nothing rather
/// than an empty attribute.
fn association_attributes(association: &Value) -> BTreeMap<String, String> {
    ASSOCIATION_ATTRIBUTES
        .iter()
        .filter_map(|(attribute, member)| {
            text_member(association, member).map(|value| ((*attribute).to_string(), value))
        })
        .collect()
}

/// Whether AppSync reports the created resource ready for a client to use.
///
/// An Event API and a channel namespace EXIST from the moment their create answers: that
/// service reports no state either is still coming up in, and the API's endpoints are already
/// serving where the create said they are. An ASSOCIATION is the exception — creating one
/// schedules the merge of the source API's schema into the Merged API, so the call answers it
/// `MERGE_SCHEDULED` — so that arm reads the state back, and a merge AppSync reports FAILED
/// fails the resource with the detail it gave rather than waiting the window out.
pub(super) async fn readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    if kind != Kind::SourceApiAssociation {
        return Ok(Readiness::Ready);
    }
    let Some(association) = read_association(ctx, region, physical_resource_id).await? else {
        return Ok(Readiness::Failed(format!(
            "AppSync holds no source API association under {physical_resource_id}"
        )));
    };
    let status = text_member(&association, ASSOCIATION_STATUS_MEMBER).unwrap_or_default();
    if status == MERGE_SUCCESS {
        return Ok(Readiness::Ready);
    }
    if status != MERGE_FAILED && status != AUTO_MERGE_SCHEDULE_FAILED {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(
        match text_member(&association, ASSOCIATION_STATUS_DETAIL_MEMBER) {
            Some(detail) => format!(
                "AppSync reports the source API association {physical_resource_id} as \
                 {status}: {detail}"
            ),
            None => format!(
                "AppSync reports the source API association {physical_resource_id} as {status}"
            ),
        },
    ))
}

/// The attributes the owning service publishes for a resource it now reports up.
///
/// An Event API and a channel namespace publish only what their own creates answered, so
/// nothing is read for either. An ASSOCIATION publishes the STATE of the link, which the create
/// answered as the merge it had just scheduled: the state a client reads off the stack is the
/// one that merge left behind, so the link is read again once AppSync reports it settled.
pub(super) async fn ready_attributes(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<BTreeMap<String, String>, Refusal> {
    if kind != Kind::SourceApiAssociation {
        return Ok(BTreeMap::new());
    }
    match read_association(ctx, region, physical_resource_id).await? {
        Some(association) => Ok(association_attributes(&association)),
        None => Ok(BTreeMap::new()),
    }
}

/// Whether AppSync has finished removing a deleted resource.
///
/// `DeleteApi` and `DeleteChannelNamespace` each take the resource away as they answer, so
/// nothing is asked for either. `DisassociateSourceGraphqlApi` does not: it SCHEDULES the
/// deletion and answers the association in a `DELETION_…` state, so that arm asks until that
/// service no longer holds the link.
pub(super) async fn removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    if kind != Kind::SourceApiAssociation {
        return Ok(Removal::Gone);
    }
    match read_association(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(association) => Ok(Removal::Pending(
            text_member(&association, ASSOCIATION_STATUS_MEMBER).unwrap_or_default(),
        )),
    }
}

/// Apply a CHANGED declaration to the resource the stack already has, through the update call
/// AppSync publishes for it.
///
/// `UpdateApi` takes the API's whole configuration — its name and its `eventConfig` are both
/// required members — so a changed declaration is applied by putting it. `UpdateChannelNamespace`
/// takes the handlers and the authorization modes, the API and the namespace's own name being
/// its path; `UpdateSourceApiAssociation` takes the description and the merge configuration, the
/// Merged API and the association id being its path. A declaration that restates one of those
/// PATH values names a different resource, which the deployment replaces rather than changing in
/// place, so this reports it as not applied (`Ok(None)`).
pub(super) async fn update(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    refuse_unapplied_properties(declared, resource, properties_of(kind))?;
    // A changed declaration states its handlers the same two ways a create's does, so a
    // reference it holds is read and applied as the property whose text it is.
    let fetched = super::appsync_s3_documents::fetched(
        ctx,
        region,
        declared,
        super::appsync_s3_documents::CHANNEL_NAMESPACE_DOCUMENTS,
    )
    .await?;
    let declared = fetched.as_ref().unwrap_or(declared);
    let held = &resource.properties;
    match kind {
        Kind::EventApi => {
            let mut request = Map::new();
            request.insert(
                "name".to_string(),
                Value::String(required(resource, declared, NAME)?),
            );
            carry(&mut request, declared, API_MEMBERS)?;
            let answered = call_appsync(
                ctx,
                region,
                "POST",
                &event_api_uri(&resource.physical_resource_id),
                Some(&Value::Object(request)),
            )
            .await?;
            let api = answered.get(API_MEMBER).cloned().unwrap_or(Value::Null);
            Ok(Some(Created::attributes(api_attributes(&api)?)))
        }
        Kind::ChannelNamespace => {
            let api_id = required(resource, held, APPSYNC_API_ID_PROPERTY)?;
            let name = required(resource, held, NAME)?;
            if stated_text(declared, APPSYNC_API_ID_PROPERTY) != api_id
                || stated_text(declared, NAME) != name
            {
                return Ok(None);
            }
            let mut request = Map::new();
            carry(&mut request, declared, CHANNEL_NAMESPACE_MEMBERS)?;
            let answered = call_appsync(
                ctx,
                region,
                "POST",
                &channel_namespace_uri(&api_id, &name),
                Some(&Value::Object(request)),
            )
            .await?;
            let namespace = answered
                .get(CHANNEL_NAMESPACE_MEMBER)
                .cloned()
                .unwrap_or(Value::Null);
            let arn = minted(
                &namespace,
                "channelNamespaceArn",
                "UpdateChannelNamespace",
            )?;
            let mut attributes = BTreeMap::new();
            attributes.insert("ChannelNamespaceArn".to_string(), arn);
            Ok(Some(Created::attributes(attributes)))
        }
        Kind::SourceApiAssociation => {
            for property in [
                APPSYNC_MERGED_API_IDENTIFIER_PROPERTY,
                APPSYNC_SOURCE_API_IDENTIFIER_PROPERTY,
            ] {
                if stated_text(declared, property) != stated_text(held, property) {
                    return Ok(None);
                }
            }
            let mut request = Map::new();
            carry(&mut request, declared, ASSOCIATION_MEMBERS)?;
            let answered = call_appsync(
                ctx,
                region,
                "POST",
                &association_uri(&resource.physical_resource_id)?,
                Some(&Value::Object(request)),
            )
            .await?;
            let association = answered
                .get(ASSOCIATION_MEMBER)
                .cloned()
                .unwrap_or(Value::Null);
            Ok(Some(Created::attributes(association_attributes(
                &association,
            ))))
        }
    }
}

/// Delete the resource a stack created, addressed exactly as its own read is.
///
/// A resource AppSync no longer holds is already in the state the delete asks for, judged on
/// the CODE and never on a message — which is also what deleting the Event API that held a
/// channel namespace leaves behind, since an API owns its namespaces and its keys.
pub(super) async fn delete(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let uri = addressed(kind, resource)?;
    match call_appsync(ctx, region, "DELETE", &uri, None).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The resource's configuration as AppSync reports it, for drift detection: that resource's own
/// read, under this type's property names.
///
/// `None` is a resource AppSync no longer holds, which is a deleted one. An association reports
/// only its DESCRIPTION: the identifiers a declaration states are accepted in either an id or
/// an ARN form while the read reports the ids AppSync resolved them to, so comparing them would
/// report a difference nobody made.
pub(super) async fn actual_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    kind: Kind,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let (uri, member, reported) = match kind {
        Kind::EventApi => (
            event_api_uri(&resource.physical_resource_id),
            API_MEMBER,
            API_REPORTED_PROPERTIES,
        ),
        Kind::ChannelNamespace => (
            channel_namespace_uri(
                &required(resource, &resource.properties, APPSYNC_API_ID_PROPERTY)?,
                &required(resource, &resource.properties, NAME)?,
            ),
            CHANNEL_NAMESPACE_MEMBER,
            CHANNEL_NAMESPACE_REPORTED_PROPERTIES,
        ),
        Kind::SourceApiAssociation => (
            association_uri(&resource.physical_resource_id)?,
            ASSOCIATION_MEMBER,
            ASSOCIATION_REPORTED_PROPERTIES,
        ),
    };
    let answered = match call_appsync(ctx, region, "GET", &uri, None).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == NOT_FOUND => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let read = answered.get(member).cloned().unwrap_or(Value::Null);
    let mut held = Map::new();
    for property in reported {
        if let Some(value) = read.get(lower_initial(property)) {
            held.insert((*property).to_string(), value.clone());
        }
    }
    Ok(Some(Value::Object(held)))
}

/// The members of each resource's own read that are COMPARED against its declaration, each
/// named identically on both sides but for the lower-cased initial AppSync spells its members
/// with. A member that is part of the resource's identity — the ids and ARNs the read answers —
/// is left out: it is not something a declaration states.
const API_REPORTED_PROPERTIES: &[&str] = &[NAME, "OwnerContact"];
const CHANNEL_NAMESPACE_REPORTED_PROPERTIES: &[&str] = &[NAME, "CodeHandlers"];
const ASSOCIATION_REPORTED_PROPERTIES: &[&str] = &["Description"];

/// The collection every Event API of the account lives in, which `CreateApi` is bound to.
const EVENT_APIS_URI: &str = "/v2/apis";

/// The collection the Merged APIs of the account are addressed under, which every call on an
/// association binds its Merged API as a label of.
const MERGED_APIS_URI: &str = "/v1/mergedApis";

/// The name `AssociateSourceGraphqlApi` is reported under where it answered without the
/// identity it assigns, and the name `CreateApi` and `UpdateApi` are.
const ASSOCIATE_SOURCE_API: &str = "AssociateSourceGraphqlApi";
const CREATE_API: &str = "CreateApi";

/// The URI that addresses ONE Event API: the `@httpLabel` every call on one binds.
fn event_api_uri(api_id: &str) -> String {
    format!("{EVENT_APIS_URI}/{}", percent_encoded(api_id))
}

/// The URI the channel namespaces of one Event API are created under.
fn channel_namespaces_uri(api_id: &str) -> String {
    format!("{}/channelNamespaces", event_api_uri(api_id))
}

/// The URI that addresses ONE channel namespace, as `GetChannelNamespace`,
/// `UpdateChannelNamespace` and `DeleteChannelNamespace` bind it.
fn channel_namespace_uri(api_id: &str, name: &str) -> String {
    format!(
        "{}/{}",
        channel_namespaces_uri(api_id),
        percent_encoded(name)
    )
}

/// The URI the source API associations of one Merged API are created under.
fn source_api_associations_uri(merged_api_identifier: &str) -> String {
    format!(
        "{MERGED_APIS_URI}/{}/sourceApiAssociations",
        percent_encoded(merged_api_identifier)
    )
}

/// The URI that addresses ONE association, as `GetSourceApiAssociation`,
/// `UpdateSourceApiAssociation` and `DisassociateSourceGraphqlApi` bind it: the Merged API that
/// holds the link and the `associationId` AppSync generated.
///
/// Both labels are read off the association's own ARN, which is the identity that service
/// minted for the resource (`apis/<mergedApiId>/sourceApiAssociations/<associationId>`): it is
/// what the create answered and what the stack recorded as the resource's physical id, so a
/// record carrying anything else identifies no association and is reported rather than
/// addressed.
fn association_uri(association_arn: &str) -> Result<String, Refusal> {
    let (merged_api_id, association_id) = addressed_association(association_arn)?;
    Ok(format!(
        "{}/{}",
        source_api_associations_uri(&merged_api_id),
        percent_encoded(&association_id)
    ))
}

/// The Merged API and the association id one association ARN names
/// ([`association_uri`]).
fn addressed_association(association_arn: &str) -> Result<(String, String), Refusal> {
    let unaddressable = || Refusal {
        code: MISSING_PROPERTY.to_string(),
        message: format!(
            "{association_arn} is not the ARN of a source API association, so AppSync cannot \
             be asked about it"
        ),
    };
    // An ARN's resource portion is everything after the fifth colon
    // (`arn:<partition>:<service>:<region>:<account>:<resource>`).
    let resource = association_arn
        .splitn(6, ':')
        .nth(5)
        .ok_or_else(unaddressable)?;
    let mut segments = resource.split('/');
    let (Some("apis"), Some(merged_api_id), Some("sourceApiAssociations"), Some(association_id)) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return Err(unaddressable());
    };
    if segments.next().is_some() || merged_api_id.is_empty() || association_id.is_empty() {
        return Err(unaddressable());
    }
    Ok((merged_api_id.to_string(), association_id.to_string()))
}

/// The URI the DELETE of one resource of `kind` is bound to.
fn addressed(kind: Kind, resource: &StoredResource) -> Result<String, Refusal> {
    match kind {
        Kind::EventApi => Ok(event_api_uri(&resource.physical_resource_id)),
        Kind::ChannelNamespace => Ok(channel_namespace_uri(
            &required(resource, &resource.properties, APPSYNC_API_ID_PROPERTY)?,
            &required(resource, &resource.properties, NAME)?,
        )),
        Kind::SourceApiAssociation => association_uri(&resource.physical_resource_id),
    }
}

/// The association AppSync reports under the ARN the stack recorded, or `None` where it holds
/// none — which is what its deletion having been carried out leaves behind.
async fn read_association(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    association_arn: &str,
) -> Result<Option<Value>, Refusal> {
    match call_appsync(ctx, region, "GET", &association_uri(association_arn)?, None).await {
        Ok(answered) => Ok(answered.get(ASSOCIATION_MEMBER).cloned()),
        Err(refusal) if refusal.code == NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Every property the type of `kind` publishes, for the one check that refuses a declared
/// property no call this module makes carries.
fn properties_of(kind: Kind) -> &'static [&'static str] {
    match kind {
        Kind::EventApi => API_PROPERTIES,
        Kind::ChannelNamespace => CHANNEL_NAMESPACE_PROPERTIES,
        Kind::SourceApiAssociation => ASSOCIATION_PROPERTIES,
    }
}

/// Copy the declared properties `carried` onto the request under the member names the owning
/// call spells them with — the property with its initial letter lower-cased, inside and out
/// ([`lower_camel_members`]). A property the declaration does not state is left out rather than
/// sent empty, so AppSync's own validation judges the request it would judge for a client's own
/// call.
fn carry(
    request: &mut Map<String, Value>,
    properties: &Value,
    carried: &[&str],
) -> Result<(), Refusal> {
    for property in carried {
        if let Some(value) = stated(properties, property) {
            request.insert(
                lower_initial(property),
                lower_camel_members(&value, PROPERTY_NESTING)?,
            );
        }
    }
    Ok(())
}

/// Put the tags the resource must carry on the request, as the `TagMap` these creates take:
/// the ones the declaration asks for and the ones CloudFormation propagates, less the keys in
/// AWS's reserved namespace, which `TagKey`'s own pattern refuses — so the records
/// CloudFormation keeps of its own accord are not sent to be refused. An empty set is left off
/// rather than sent as an empty map, which that member's own `@length(min: 1)` would refuse.
fn carry_tags(
    request: &mut Map<String, Value>,
    properties: &Value,
    propagated: &[(String, String)],
) {
    let tags = tags_as_map(&tags_of(properties, &applicable_tags(propagated)));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
}

/// One property as the declaration states it, or `None` where it states none.
fn stated(properties: &Value, property: &str) -> Option<Value> {
    properties
        .get(property)
        .filter(|value| !value.is_null())
        .cloned()
}

/// One property as the TEXT it carries, or the empty text where the declaration states none.
fn stated_text(properties: &Value, property: &str) -> String {
    stated(properties, property)
        .as_ref()
        .map(attribute_text)
        .unwrap_or_default()
}

/// One property a call cannot be made without, as the text it carries, or the refusal for a
/// declaration that states none.
fn required(
    resource: &StoredResource,
    properties: &Value,
    property: &str,
) -> Result<String, Refusal> {
    match stated(properties, property).as_ref().map(attribute_text) {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(Refusal {
            code: MISSING_PROPERTY.to_string(),
            message: format!("{} requires the {property} property", resource.resource_type),
        }),
    }
}

/// The member name a property travels under: the property with its initial letter lower-cased,
/// which is how AppSync's model spells every member these types publish as a property.
fn lower_initial(property: &str) -> String {
    let mut characters = property.chars();
    match characters.next() {
        Some(initial) => initial.to_lowercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// One text member of a reported record, absent where the answer carries none or carries it
/// empty — an empty identifier is no identifier.
fn text_member(reported: &Value, member: &str) -> Option<String> {
    reported
        .get(member)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

/// One identifier AppSync MINTED, read off the record its call answered.
///
/// A call that answered 2xx WITHOUT it identifies no resource, so the resource is reported as
/// failed rather than recorded under an identifier nothing answers to.
fn minted(record: &Value, member: &str, operation: &str) -> Result<String, Refusal> {
    text_member(record, member).ok_or_else(|| Refusal {
        code: UNIDENTIFIED_FAILURE.to_string(),
        message: format!("AppSync answered {operation} without a {member}"),
    })
}

/// The refusal for a declared property no call this module makes carries, naming which property
/// it was: refusing is what keeps a resource from being reported as created — or as updated —
/// with less than its declaration asked for. `properties` is the document the deployment
/// DECLARES, which on an update is not the one the record carries.
fn refuse_unapplied_properties(
    properties: &Value,
    resource: &StoredResource,
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
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    Ok(())
}

/// Call one AWS AppSync operation on the `restJson1` wire its model declares, on the URI that
/// operation's own `@http` binding states.
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
mod tests;
