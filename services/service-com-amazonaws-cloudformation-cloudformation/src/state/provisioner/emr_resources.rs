// SPDX-License-Identifier: CC0-1.0

//! The Amazon EMR arms beside the cluster's in [`data_and_storage`]: the security
//! configuration an `AWS::EMR::SecurityConfiguration` declares, the Studio an
//! `AWS::EMR::Studio` declares and the session mapping an `AWS::EMR::StudioSessionMapping`
//! puts on one. One file of [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! Each of the three is created through Amazon EMR's OWN API — the same call a client makes
//! — so what a stack reports created is what that service's own reads answer with:
//! `DescribeSecurityConfiguration` resolves the name a `Ref` publishes and
//! `ListSecurityConfigurations` reports it once, `DescribeStudio` resolves the `es-…` id the
//! create minted, and `GetStudioSessionMapping` resolves the mapping. A document, a network
//! object or a role that service refuses is the resource's own create failure, carrying
//! Amazon EMR's refusal as the resource's status reason.
//!
//! The three identities are each the one Amazon EMR itself addresses the resource by. A
//! security configuration IS its name ("Ref returns the Name of the security
//! configuration"), which every call on one takes, so the name the declaration states is the
//! physical id. A Studio's is the `es-…` `StudioId` `CreateStudio` MINTS — a template
//! declares the Studio's `Name`, never that id — so it is reported as the resource's own
//! physical id ([`Created::physical_resource_id`]), which [`super::provisioning::reconcile`]
//! adopts. A session mapping has no identifier of its own at all: that service addresses one
//! by the Studio holding it, the kind of identity it maps and that identity's name together,
//! which is this type's composite primary identifier
//! ([`super::template::composite_identity_properties`]) and what the delete and the read
//! below take the three parts off.

use super::*;

/// The type of an Amazon EMR security configuration: the EMR model carries
/// `awsService.cloudFormationName: EMR`, as the cluster type beside it records. "Ref returns
/// the Name of the security configuration", which is the name the declaration states.
pub(in crate::state) const EMR_SECURITY_CONFIGURATION: &str = "AWS::EMR::SecurityConfiguration";

/// The type of an Amazon EMR Studio. "Ref returns the Studio ID", the `es-…` identifier
/// `CreateStudio` mints, which is why [`super::template::identity_is_service_assigned`] names
/// this type.
pub(in crate::state) const EMR_STUDIO: &str = "AWS::EMR::Studio";

/// The type of one identity's mapping onto such a Studio.
pub(in crate::state) const EMR_STUDIO_SESSION_MAPPING: &str = "AWS::EMR::StudioSessionMapping";

/// The three properties a session mapping is identified by — its type's composite primary
/// identifier — which are the three members `CreateStudioSessionMapping`,
/// `GetStudioSessionMapping` and `DeleteStudioSessionMapping` each address one by.
pub(in crate::state) const EMR_STUDIO_SESSION_MAPPING_IDENTITY: &[&str] =
    &["StudioId", "IdentityType", "IdentityName"];

/// The operations this module calls, as Amazon EMR's own model names them.
const CREATE_SECURITY_CONFIGURATION: &str = "CreateSecurityConfiguration";
const DELETE_SECURITY_CONFIGURATION: &str = "DeleteSecurityConfiguration";
const DESCRIBE_SECURITY_CONFIGURATION: &str = "DescribeSecurityConfiguration";
const CREATE_STUDIO: &str = "CreateStudio";
const DELETE_STUDIO: &str = "DeleteStudio";
const DESCRIBE_STUDIO: &str = "DescribeStudio";
const CREATE_STUDIO_SESSION_MAPPING: &str = "CreateStudioSessionMapping";
const DELETE_STUDIO_SESSION_MAPPING: &str = "DeleteStudioSessionMapping";
const GET_STUDIO_SESSION_MAPPING: &str = "GetStudioSessionMapping";

/// The member every one of those calls names the resource in, and the ones the two reads
/// answer the resource under.
const NAME_MEMBER: &str = "Name";
const SECURITY_CONFIGURATION_MEMBER: &str = "SecurityConfiguration";
const STUDIO_ID_MEMBER: &str = "StudioId";
const STUDIO_MEMBER: &str = "Studio";
const SESSION_MAPPING_MEMBER: &str = "SessionMapping";
const URL_MEMBER: &str = "Url";
const TAGS_MEMBER: &str = "Tags";

/// The attribute a created Studio publishes beside the identity and the ARN: "the unique
/// access URL of the Amazon EMR Studio", which only that service can produce, so it is read
/// off the create's own answer.
const URL_ATTRIBUTE: &str = "Url";

/// The member `DescribeStudio` reports the Studio's own ARN in, which is the ARN Amazon EMR
/// built around the identifier it minted — so it is read off that service rather than computed
/// from the declared name, and is what every one of the names a type publishes an ARN under
/// answers ([`template::arn_attributes`]).
const STUDIO_ARN_MEMBER: &str = "StudioArn";

/// Call one Amazon EMR operation on the awsJson1_1 wire its model declares, addressed by the
/// signing name and `X-Amz-Target` prefix the cluster arms beside this file read from that
/// model ([`EMR_SIGNING_NAME`], [`EMR_TARGET_PREFIX`]).
async fn call_emr(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        EMR_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{EMR_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// Store the security configuration an `AWS::EMR::SecurityConfiguration` declares, so a
/// client that read the name out of the stack's outputs finds the configuration in Amazon
/// EMR itself — `DescribeSecurityConfiguration` answers with the submitted document, the
/// account's own `ListSecurityConfigurations` reports the name, and a cluster launched with
/// `SecurityConfiguration: <that name>` resolves it.
///
/// The document is the declaration's own: the type publishes the property as a JSON
/// structure where that service's member is the TEXT of one, so an object is rendered and a
/// string passes through unchanged ([`document_text`]) — which is what leaves a document
/// Amazon EMR will not store to that service's own refusal, reported as this resource's
/// create failure, rather than judged here.
///
/// The name is the resource's physical id, which is the `Name` the declaration states and the
/// generated one otherwise: Amazon EMR requires a name, and that name is the handle every
/// call on a configuration takes.
pub(super) async fn create_emr_security_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let document = declared_security_configuration(&resource.properties)?;
    let request = serde_json::json!({
        NAME_MEMBER: resource.physical_resource_id,
        SECURITY_CONFIGURATION_MEMBER: document,
    });
    call_emr(ctx, region, CREATE_SECURITY_CONFIGURATION, &request).await?;
    // The name the configuration was stored under is the one the naming rules computed, so
    // there is no identity to take over and no attribute beside the ones the convention
    // reads off that name.
    Ok(Created::default())
}

/// Delete the security configuration a stack created, by the name it was created under —
/// this resource's physical id.
///
/// A configuration Amazon EMR no longer holds is already in the state the delete asks for.
/// That service models ONE client error for every refusal of this call
/// ([`INVALID_REQUEST`]), so the code alone does not say the configuration is absent — a
/// configuration it will not delete is refused under the same code — and the message carries
/// the request's own name, which nothing here judges on. So the refusal is settled by READING
/// the resource back, which is what [`super::Provisioner::delete`] documents for a service
/// that models no not-found code of its own: a name that service reports nothing for is
/// deleted, and anything else is its refusal.
pub(super) async fn delete_emr_security_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ NAME_MEMBER: physical_resource_id });
    match call_emr(ctx, region, DELETE_SECURITY_CONFIGURATION, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == INVALID_REQUEST => {
            let held =
                describe_emr_security_configuration(ctx, region, physical_resource_id).await?;
            if held.is_none() {
                Ok(())
            } else {
                Err(refusal)
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// The configuration Amazon EMR reports for the security configuration a stack created, for
/// drift detection: its own `DescribeSecurityConfiguration` record, read by the name the
/// resource was created under. One that service no longer holds is refused with its own
/// client error, which is the resource having been deleted.
///
/// The record is reported under the type's own property names — the `Name` and the document —
/// and the document is reported as the STRUCTURE it parses as, because that is the form the
/// declaration states it in; text that is not a JSON document at all is reported as the text
/// it is.
pub(super) async fn describe_emr_security_configuration(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ NAME_MEMBER: physical_resource_id });
    let answered = match call_emr(ctx, region, DESCRIBE_SECURITY_CONFIGURATION, &request).await {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == INVALID_REQUEST => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    let mut reported = Map::new();
    if let Some(name) = reported_text(&answered, NAME_MEMBER) {
        reported.insert(NAME_MEMBER.to_string(), Value::String(name));
    }
    if let Some(document) = reported_text(&answered, SECURITY_CONFIGURATION_MEMBER) {
        let parsed = serde_json::from_str::<Value>(&document)
            .unwrap_or_else(|_| Value::String(document.clone()));
        reported.insert(SECURITY_CONFIGURATION_MEMBER.to_string(), parsed);
    }
    Ok(Some(Value::Object(reported)))
}

/// Create the Amazon EMR Studio an `AWS::EMR::Studio` declares, and report the `es-…`
/// identifier that service ASSIGNED it.
///
/// Every property of the type IS a member of `CreateStudio` under the same name — the
/// authentication mode, the VPC and subnets the Workspaces run in, the two security groups,
/// the service and user roles, the default Amazon S3 location, the identity-provider members
/// and the Identity Center ones — so they are passed through verbatim, with `Name` taken from
/// the resource's own physical name where the declaration states none (that service requires
/// a name). The declared tags travel beside the ones CloudFormation gives the resource
/// ([`tags_of`]) as the `[{Key, Value}]` `TagList` Amazon EMR's own `Tag` shape uses, which is
/// the type's own spelling too.
///
/// The network objects and the role a declaration names are resolved by Amazon EMR itself, in
/// the services that own them, so a Studio this reports created is one whose VPC, subnets,
/// security groups and service role really exist — and a declaration naming one that does not
/// is that service's refusal, reported as this resource's create failure.
pub(super) async fn create_emr_studio(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let declared = match &resource.properties {
        Value::Object(members) => members.clone(),
        _ => Map::new(),
    };
    let mut request: Map<String, Value> = declared
        .iter()
        .filter(|(member, _)| *member != TAGS_MEMBER)
        .map(|(member, value)| (member.clone(), value.clone()))
        .collect();
    let name = declared
        .get(NAME_MEMBER)
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| resource.physical_resource_id.clone());
    request.insert(NAME_MEMBER.to_string(), Value::String(name));
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }

    let answered = call_emr(ctx, region, CREATE_STUDIO, &Value::Object(request)).await?;
    let Some(studio_id) = reported_text(&answered, STUDIO_ID_MEMBER).filter(|id| !id.is_empty())
    else {
        // There would be no handle a client could call Amazon EMR with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "Amazon EMR reported no StudioId for the Studio it created".to_string(),
        });
    };
    let mut attributes = BTreeMap::new();
    if let Some(url) = reported_text(&answered, URL_MEMBER) {
        attributes.insert(URL_ATTRIBUTE.to_string(), url);
    }
    // The ARN is Amazon EMR's: it is built around the identifier that service minted, so the
    // one the naming rules computed from the declared name names no Studio at all. The create
    // does not report it, so it is read off that service's own record of the Studio it has just
    // made — the one value a client reading the stack's `Fn::GetAtt` then addresses it by.
    //
    // The read DECIDES NOTHING about the create: the Studio exists by the time it is made, so
    // a refusal of it is no reason to report the create as failed — that would leave behind a
    // Studio whose id no record of this stack carries, and nothing could then delete it. It is
    // not discarded either: a refusal is traced, with the identifier the read was about and the
    // code that service answered, so an emulator that cannot read back what it just created
    // says so rather than publishing a value nobody asked for.
    match describe_emr_studio(ctx, region, studio_id.as_str()).await {
        Ok(Some(studio)) => {
            if let Some(arn) = reported_text(&studio, STUDIO_ARN_MEMBER) {
                attributes.extend(template::arn_attributes(EMR_STUDIO, &arn));
            }
        }
        Ok(None) => {
            ::tracing::warn!(
                studio_id = %studio_id,
                "Amazon EMR reported no Studio for the one it had just created, so the \
                 resource publishes no ARN of that service's own"
            );
        }
        Err(refusal) => {
            ::tracing::warn!(
                studio_id = %studio_id,
                code = %refusal.code,
                "Amazon EMR refused the read of the Studio it had just created, so the \
                 resource publishes no ARN of that service's own"
            );
        }
    }
    Ok(Created {
        // The Studio's identity is that service's: "Ref returns the Studio ID".
        physical_resource_id: Some(studio_id),
        attributes,
    })
}

/// Delete the Studio a stack created, by the id Amazon EMR minted for it — this resource's
/// physical id. A Studio that service no longer holds is already in the state the delete asks
/// for, settled by reading the Studio back for the reason
/// [`delete_emr_security_configuration`] records: one client error covers every refusal of
/// the call.
pub(super) async fn delete_emr_studio(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ STUDIO_ID_MEMBER: physical_resource_id });
    match call_emr(ctx, region, DELETE_STUDIO, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == INVALID_REQUEST => {
            let held = describe_emr_studio(ctx, region, physical_resource_id).await?;
            if held.is_none() {
                Ok(())
            } else {
                Err(refusal)
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// The Studio's configuration as Amazon EMR reports it, for drift detection: its own
/// `DescribeStudio` record, read by the id that service minted. Its members are EMR's own —
/// which for this type are the properties' own names — so a declared property takes part in
/// the comparison exactly as [`super::drift`] documents for every arm here.
pub(super) async fn describe_emr_studio(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ STUDIO_ID_MEMBER: physical_resource_id });
    match call_emr(ctx, region, DESCRIBE_STUDIO, &request).await {
        Ok(answered) => Ok(answered.get(STUDIO_MEMBER).cloned()),
        Err(refusal) if refusal.code == INVALID_REQUEST => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Map the identity an `AWS::EMR::StudioSessionMapping` declares onto the Studio it names, so
/// that identity really reaches the Studio the stack created and Amazon EMR's own
/// `GetStudioSessionMapping` and `ListStudioSessionMappings` report the mapping.
///
/// Every property of the type is a member of `CreateStudioSessionMapping` under the same
/// name: the Studio, the kind of identity, the identity's own name and the session policy it
/// is mapped with. They are passed through verbatim, so a Studio, an identity type or a
/// policy that service refuses is its own refusal rather than a judgement made here.
pub(super) async fn create_emr_studio_session_mapping(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Created, Refusal> {
    let request = match &resource.properties {
        Value::Object(members) => Value::Object(members.clone()),
        _ => Value::Object(Map::new()),
    };
    call_emr(ctx, region, CREATE_STUDIO_SESSION_MAPPING, &request).await?;
    // The mapping's identity is the triple its own declaration states, which the deployment
    // has already given the resource ([`super::template::composite_identity`]), and that
    // create answers no document at all.
    Ok(Created::default())
}

/// Remove the mapping a stack created, addressed by the triple Amazon EMR keys one by, which
/// only the resource's own declaration carries. A mapping that service no longer reports is
/// already in the state the delete asks for, settled by reading the mapping back for the
/// reason [`delete_emr_security_configuration`] records.
pub(super) async fn delete_emr_studio_session_mapping(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let request = session_mapping_identity(resource);
    match call_emr(ctx, region, DELETE_STUDIO_SESSION_MAPPING, &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == INVALID_REQUEST => {
            let held = describe_emr_studio_session_mapping(ctx, region, resource).await?;
            if held.is_none() {
                Ok(())
            } else {
                Err(refusal)
            }
        }
        Err(refusal) => Err(refusal),
    }
}

/// The mapping's configuration as Amazon EMR reports it, for drift detection: its own
/// `GetStudioSessionMapping` record, read by the same triple the delete addresses it by. Its
/// members carry the type's own property names, so a declared property takes part in the
/// comparison.
pub(super) async fn describe_emr_studio_session_mapping(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    let request = session_mapping_identity(resource);
    match call_emr(ctx, region, GET_STUDIO_SESSION_MAPPING, &request).await {
        Ok(answered) => Ok(answered.get(SESSION_MAPPING_MEMBER).cloned()),
        Err(refusal) if refusal.code == INVALID_REQUEST => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The three members that address ONE session mapping, read off the resource's own
/// declaration ([`EMR_STUDIO_SESSION_MAPPING_IDENTITY`]).
///
/// A part the declaration does not state is left out rather than sent empty: the request then
/// names no mapping, and Amazon EMR's own validation of the member is what refuses it.
fn session_mapping_identity(resource: &StoredResource) -> Value {
    let mut request = Map::new();
    for member in EMR_STUDIO_SESSION_MAPPING_IDENTITY {
        if let Some(value) = declared_text(&resource.properties, member) {
            request.insert((*member).to_string(), Value::String(value));
        }
    }
    Value::Object(request)
}

/// The security-configuration document a declaration states, as the TEXT Amazon EMR's member
/// takes ([`document_text`]), or the refusal a declaration that states none earns: the
/// document IS the resource, so there is nothing to create without it.
fn declared_security_configuration(properties: &Value) -> Result<String, Refusal> {
    properties
        .get(SECURITY_CONFIGURATION_MEMBER)
        .and_then(document_text)
        .ok_or_else(|| Refusal {
            code: MISSING_PROPERTY_CODE.to_string(),
            message: format!(
                "an {EMR_SECURITY_CONFIGURATION} declares the {SECURITY_CONFIGURATION_MEMBER} \
                 document it is."
            ),
        })
}
