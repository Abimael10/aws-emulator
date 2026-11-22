// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of AWS Systems Manager Quick Setup's own control plane: the
//! configuration manager an `AWS::SSMQuickSetup::ConfigurationManager` declares. One file of
//! [`super`]'s per-type arms, dispatched to by its `Provisioner`.
//!
//! ## Why the resource is really created
//!
//! `CreateConfigurationManager` takes the manager's name, its description, the configuration
//! definitions it deploys and its tags, takes no identifier at all, and answers the
//! `ManagerArn` Quick Setup MINTED — the value this type's `Ref` returns and the handle
//! `GetConfigurationManager`, `UpdateConfigurationManager`, `UpdateConfigurationDefinition`,
//! `DeleteConfigurationManager` and the tag plane each address a manager by. So the ARN that
//! create answered is the resource's physical id, replacing the stand-in the naming rules
//! computed. A stack that reported the resource created while nothing created it would publish
//! an ARN resolving to nothing: no manager to read, no configuration deployed from its
//! definitions, and no tag on it.
//!
//! ## The properties
//!
//! The type's properties ARE that create's own members, spelled identically on both sides —
//! `Name`, `Description`, `ConfigurationDefinitions` and `Tags`
//! (`smithy_query(query="closure",operation="CreateConfigurationManager")`) — and a declared
//! definition's members are `ConfigurationDefinitionInput`'s own the same way (`Type`,
//! `Parameters`, `TypeVersion`, `LocalDeploymentExecutionRoleName`,
//! `LocalDeploymentAdministrationRoleArn`). So the evaluated property document IS the request,
//! with the one shape that differs rewritten: the type publishes `Tags` as a MAP, which is the
//! shape that call's own `Tags` member takes, so the tags CloudFormation gives the resource
//! travel beside the declaration's own ([`tags_of`], [`tags_as_map`]) and are reconciled
//! afterwards through that service's own `TagResource`/`UntagResource`
//! ([`retag_configuration_manager`]).
//!
//! A declared property no arm carries is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]): a manager reported created with fewer definitions than its
//! declaration asked for is the shallow success this arm exists to avoid.
//!
//! ## What the resource publishes
//!
//! The ARN beside the two instants Quick Setup keeps for the manager and the summaries of the
//! work its deployment is doing, all read off that service's own report of the manager the
//! create just made ([`reported_attributes_of`]) rather than computed here — exactly as an
//! `AWS::Braket::SpendingLimit`'s `CreatedAt` is ([`super::braket_spending_limits`]).
//!
//! ## What a declaration that service refuses does
//!
//! Nothing here checks a declared value: every constraint is Quick Setup's own — a definition's
//! `Type` must name a configuration type it offers, its `Parameters` must name the deployment's
//! targets and carry what that type requires of it, and a local deployment's administration
//! role must be one the deploying service can assume — so a declaration outside one of them is
//! refused BY that service, and that refusal is what fails the resource and rolls the stack
//! back.

use super::*;

/// Quick Setup's signing name, from its model (`sigv4Name: ssm-quicksetup`, which is also its
/// `awsService.arnNamespace`), and it speaks `restJson1` — so its calls ride on
/// [`call_rest_json_named`], each on the URI its own operation's `@http` binding declares,
/// naming the operation so a refusal is reported the way that service's own SDK raises it.
pub(super) const QUICK_SETUP_SIGNING_NAME: &str = "ssm-quicksetup";

/// The collection `CreateConfigurationManager` is bound to (`POST /configurationManager`). ONE
/// manager is addressed under it by the ARN that service minted
/// (`GET`/`PUT`/`DELETE /configurationManager/{ManagerArn}`), and the tag plane addresses one
/// by the same ARN under its own collection (`/tags/{ResourceArn}`).
const CONFIGURATION_MANAGER_URI: &str = "/configurationManager";
const TAGS_URI: &str = "/tags";

/// The operations this module calls, as Quick Setup's own model names them.
const CREATE_CONFIGURATION_MANAGER: &str = "CreateConfigurationManager";
const GET_CONFIGURATION_MANAGER: &str = "GetConfigurationManager";
const UPDATE_CONFIGURATION_MANAGER: &str = "UpdateConfigurationManager";
const DELETE_CONFIGURATION_MANAGER: &str = "DeleteConfigurationManager";
const LIST_TAGS_FOR_RESOURCE: &str = "ListTagsForResource";
const TAG_RESOURCE: &str = "TagResource";
const UNTAG_RESOURCE: &str = "UntagResource";

/// The error code Quick Setup answers for a manager it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its read, its two updates, its delete and
/// its tag plane).
const QUICK_SETUP_RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// Every property of the type this arm carries onto the owning API, so a declaration stating
/// one outside the set is REFUSED rather than reported as created without it.
const MAPPED_PROPERTIES: &[&str] = &[
    "ConfigurationDefinitions",
    "Description",
    "Name",
    "Tags",
];

/// The properties that are members of `CreateConfigurationManager` under the very same name:
/// the manager's name, its description and the definitions it deploys. `Tags` is not among
/// them — the declaration's own travel beside the propagated ones.
const CREATE_MEMBERS: &[(&str, &str)] = &[
    ("ConfigurationDefinitions", "ConfigurationDefinitions"),
    ("Description", "Description"),
    ("Name", "Name"),
];

/// The two properties `UpdateConfigurationManager` takes beside the ARN it addresses a manager
/// by, each the member of the same name: everything that call can change on a manager that
/// exists.
const UPDATE_MEMBERS: &[(&str, &str)] = &[("Description", "Description"), ("Name", "Name")];

/// The member both the create's answer and every read of a manager carry its ARN in, and the
/// member the tag calls address a resource by.
const MANAGER_ARN_MEMBER: &str = "ManagerArn";

/// The member the create and `TagResource` take their tag map in, which is also the member
/// `ListTagsForResource` answers the manager's tags in — as a LIST of `{Key, Value}` pairs,
/// which is the shape [`declared_tags`] reads.
const TAGS_MEMBER: &str = "Tags";

/// The members a read of a manager reports the instants Quick Setup keeps for it in, under the
/// attributes the type publishes them as — both the service's own clock, never this one's.
const REPORTED_INSTANTS: &[(&str, &str)] = &[
    ("CreatedAt", "CreatedAt"),
    ("LastModifiedAt", "LastModifiedAt"),
];

/// The member a read of a manager reports the summaries of its deployment in, and the attribute
/// the type publishes them as.
///
/// The attribute is a LIST whose elements are STRUCTURES, so each summary is carried under its
/// own indexed key ([`list_attribute_element`], the one carrier a string-valued attribute map
/// has for a list) as the document that service reported for it ([`attribute_text`]) — which is
/// how `Fn::GetAtt` resolves the list and a dotted path one element of it.
const STATUS_SUMMARIES_MEMBER: &str = "StatusSummaries";
const STATUS_SUMMARIES_ATTRIBUTE: &str = "StatusSummaries";

/// The members of one reported status summary this module reads: which kind of work it is about
/// and the phase that work is in.
const STATUS_TYPE_MEMBER: &str = "StatusType";
const STATUS_MEMBER: &str = "Status";

/// The kind of work a manager's CREATION is finished on: the deployment of its configuration
/// definitions, whose own summary is the one `StatusType` a readiness read looks at
/// (`smithy_query(query="shape",shape="StatusType")` publishes `Deployment` beside
/// `AsyncExecutions`, which is the work that deployment then runs).
const DEPLOYMENT_STATUS_TYPE: &str = "Deployment";

/// The phase that deployment reaches when it is done, and the phases it is still working
/// through — `Status`'s own values (`smithy_query(query="shape",shape="Status")`). A manager
/// reported in any other phase is one no wait brings further, which is a failed create.
const DEPLOYMENT_SUCCEEDED: &str = "SUCCEEDED";
const DEPLOYMENT_IN_PROGRESS: &[&str] = &["INITIALIZING", "DEPLOYING"];

/// Create the configuration manager an `AWS::SSMQuickSetup::ConfigurationManager` declares, in
/// Quick Setup itself — so a client that deployed the stack reads the manager back through that
/// service's own `GetConfigurationManager` and finds it in `ListConfigurationManagers` for the
/// ARN the stack published, with one configuration deployed per account and Region its
/// definitions target.
///
/// The declaration IS the request ([`CREATE_MEMBERS`]), with the declared tags beside the ones
/// CloudFormation gives the resource as the tag MAP that call takes.
///
/// The identity is Quick Setup's: the `ManagerArn` it minted becomes this resource's physical id
/// — the type's primary identifier, which its `Ref` returns — and the values the type publishes
/// are read off that service's own report of the manager it has just stored.
pub(super) async fn create_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, MAPPED_PROPERTIES)?;
    let properties = resource
        .properties
        .as_object()
        .cloned()
        .unwrap_or_else(Map::new);
    let mut request = Map::new();
    copy_members(&properties, CREATE_MEMBERS, &mut request);
    let tags = tags_as_map(&tags_of(&resource.properties, propagated));
    if !tags.is_empty() {
        request.insert(TAGS_MEMBER.to_string(), Value::Object(tags));
    }
    let answered = call_rest_json_named(
        ctx,
        QUICK_SETUP_SIGNING_NAME,
        region,
        CREATE_CONFIGURATION_MANAGER,
        "POST",
        CONFIGURATION_MANAGER_URI,
        Some(&Value::Object(request)),
    )
    .await?;
    let arn = reported_text(&answered, MANAGER_ARN_MEMBER).unwrap_or_default();
    if arn.is_empty() {
        // There would be no handle a client could call Quick Setup with, and nothing for the
        // stack to publish, so this is a failed create rather than a created resource.
        return Err(Refusal {
            code: "InternalFailure".to_string(),
            message: "the Quick Setup control plane answered a created configuration manager \
                      without an ARN"
                .to_string(),
        });
    }
    let reported = read_configuration_manager(ctx, region, &arn).await?;
    let mut attributes = reported_attributes_of(reported.as_ref());
    attributes.insert(
        template::SSM_QUICKSETUP_CONFIGURATION_MANAGER_ARN_ATTRIBUTE.to_string(),
        arn.clone(),
    );
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Apply a changed declaration to the manager the stack created, through
/// `UpdateConfigurationManager` — the one call that changes a manager that exists.
///
/// It takes the manager's NAME and DESCRIPTION addressed by the ARN Quick Setup minted, so a
/// changed declaration of either reaches that manager rather than replacing it. The definitions
/// it deploys are not members of it at all, which is why a declaration restating them replaces
/// the resource ([`template::replacement_properties`]) instead of reaching here; the tags
/// travel separately ([`retag_configuration_manager`]).
pub(super) async fn update_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Created, Refusal> {
    // The same check the create makes, against the NEW declaration: a property no arm carries
    // onto the owning API would otherwise be reported as applied while being dropped.
    if let Some(property) = declared
        .as_object()
        .and_then(|properties| unmapped_property(properties, MAPPED_PROPERTIES))
    {
        return Err(Refusal {
            code: NOT_IMPLEMENTED.to_string(),
            message: format!(
                "this emulator does not apply the {property} property of {}",
                resource.resource_type
            ),
        });
    }
    let mut request = Map::new();
    if let Some(properties) = declared.as_object() {
        copy_members(properties, UPDATE_MEMBERS, &mut request);
    }
    call_rest_json_named(
        ctx,
        QUICK_SETUP_SIGNING_NAME,
        region,
        UPDATE_CONFIGURATION_MANAGER,
        "PUT",
        &configuration_manager_uri(&resource.physical_resource_id),
        Some(&Value::Object(request)),
    )
    .await?;
    // The update answers no document, so what the resource now reports is read off that
    // service's own report of the manager — the one reading the create uses.
    let reported =
        read_configuration_manager(ctx, region, &resource.physical_resource_id).await?;
    let mut attributes = reported_attributes_of(reported.as_ref());
    attributes.insert(
        template::SSM_QUICKSETUP_CONFIGURATION_MANAGER_ARN_ATTRIBUTE.to_string(),
        resource.physical_resource_id.clone(),
    );
    Ok(Created::attributes(attributes))
}

/// Bring the tags of the manager a resource record names to the ones this deployment gives it,
/// through Quick Setup's own `TagResource` and `UntagResource`.
///
/// The set is the declaration's own `Tags` beside the ones CloudFormation propagates
/// ([`tags_of`]) — the same set the CREATE applied — so a key outside it is one this deployment
/// no longer gives the resource and is removed, and one in it with another value is re-valued.
/// Where the two already agree nothing is called at all, which is what makes this safe to ask
/// on every deployment; a manager that service no longer holds has no tags to reconcile.
pub(super) async fn retag_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
    propagated: &[(String, String)],
) -> Result<(), Refusal> {
    let uri = tags_uri(&resource.physical_resource_id);
    let answered = match call_rest_json_named(
        ctx,
        QUICK_SETUP_SIGNING_NAME,
        region,
        LIST_TAGS_FOR_RESOURCE,
        "GET",
        &uri,
        None,
    )
    .await
    {
        Ok(answered) => answered,
        Err(refusal) if refusal.code == QUICK_SETUP_RESOURCE_NOT_FOUND => return Ok(()),
        Err(refusal) => return Err(refusal),
    };
    // That read answers the tags as a LIST of `{Key, Value}` pairs, which is the shape a
    // declaration's own `Tags` property states them in too.
    let held = declared_tags(&answered);
    let desired = tags_of(declared, propagated);
    let (added, removed) = tag_delta(&held, &desired);
    if !added.is_empty() {
        call_rest_json_named(
            ctx,
            QUICK_SETUP_SIGNING_NAME,
            region,
            TAG_RESOURCE,
            "PUT",
            &uri,
            Some(&Value::Object(
                [(TAGS_MEMBER.to_string(), Value::Object(tags_as_map(&added)))]
                    .into_iter()
                    .collect(),
            )),
        )
        .await?;
    }
    if !removed.is_empty() {
        // `tagKeys` is an `@httpQuery` LIST, which travels as the parameter repeated once per
        // key rather than as one joined value.
        let keys = removed
            .iter()
            .map(|key| format!("tagKeys={}", percent_encoded(key)))
            .collect::<Vec<String>>()
            .join("&");
        call_rest_json_named(
            ctx,
            QUICK_SETUP_SIGNING_NAME,
            region,
            UNTAG_RESOURCE,
            "DELETE",
            &format!("{uri}?{keys}"),
            None,
        )
        .await?;
    }
    Ok(())
}

/// Delete the configuration manager a stack created, by the ARN it was created under — this
/// resource's physical id. One Quick Setup no longer holds (`ResourceNotFoundException`, judged
/// on the CODE and never on a message that carries the request's own values) is already in the
/// state the delete asks for.
pub(super) async fn delete_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    match call_rest_json_named(
        ctx,
        QUICK_SETUP_SIGNING_NAME,
        region,
        DELETE_CONFIGURATION_MANAGER,
        "DELETE",
        &configuration_manager_uri(physical_resource_id),
        None,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == QUICK_SETUP_RESOURCE_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// Whether the manager the create answered for has finished DEPLOYING: Quick Setup answers the
/// create with the manager's deployment still in its first phase — a configuration manager is
/// the deployment of its definitions, and that work walks `INITIALIZING` and `DEPLOYING` before
/// it has `SUCCEEDED` — so the resource's creation is held open until that service reports the
/// deployment summary done.
///
/// Read off that service's own report of the manager, never decided here. A manager it no longer
/// holds, and one whose report carries no deployment summary at all, leave nothing to wait for.
pub(super) async fn configuration_manager_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Readiness, Refusal> {
    let Some(reported) = read_configuration_manager(ctx, region, physical_resource_id).await?
    else {
        return Ok(Readiness::Ready);
    };
    let Some(status) = deployment_status(&reported) else {
        return Ok(Readiness::Ready);
    };
    if status == DEPLOYMENT_SUCCEEDED {
        return Ok(Readiness::Ready);
    }
    if DEPLOYMENT_IN_PROGRESS.contains(&status.as_str()) {
        return Ok(Readiness::Pending(status));
    }
    Ok(Readiness::Failed(format!(
        "Quick Setup reports the configuration manager's deployment as {status}"
    )))
}

/// What one read of a manager a stack DELETED says about the teardown: Quick Setup answers the
/// delete with the manager still reported while its configurations are torn down, so the
/// deletion is finished once that service no longer holds it.
pub(super) async fn configuration_manager_removal(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Removal, Refusal> {
    match read_configuration_manager(ctx, region, physical_resource_id).await? {
        None => Ok(Removal::Gone),
        Some(reported) => Ok(Removal::Pending(
            deployment_status(&reported).unwrap_or_else(|| DEPLOYMENT_SUCCEEDED.to_string()),
        )),
    }
}

/// The manager's configuration as Quick Setup reports it, for drift detection:
/// `GetConfigurationManager`'s own record, read by the ARN the stack published, projected onto
/// the property names the type declares those members as. One it no longer holds was deleted,
/// which is what `None` reports.
///
/// The definitions are NOT among the projected members: that read reports each one with the `Id`
/// Quick Setup assigned it, which a declaration states nowhere, so comparing the two documents
/// would report a difference the template cannot express — and a property the actual document
/// does not carry is not evidence of anything ([`super::drift`]).
pub(super) async fn describe_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    Ok(read_configuration_manager(ctx, region, physical_resource_id)
        .await?
        .map(|manager| {
            reported_as_properties(&manager, &[("Description", "Description"), ("Name", "Name")])
        }))
}

/// The manager record Quick Setup holds under `manager_arn`, or `None` when it holds none
/// (`ResourceNotFoundException`, judged on the CODE and never on a message).
async fn read_configuration_manager(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    manager_arn: &str,
) -> Result<Option<Value>, Refusal> {
    match call_rest_json_named(
        ctx,
        QUICK_SETUP_SIGNING_NAME,
        region,
        GET_CONFIGURATION_MANAGER,
        "GET",
        &configuration_manager_uri(manager_arn),
        None,
    )
    .await
    {
        Ok(answered) => Ok(Some(answered)),
        Err(refusal) if refusal.code == QUICK_SETUP_RESOURCE_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// The phase Quick Setup reports the manager's DEPLOYMENT in, out of the status summaries its
/// read carries one per kind of work; `None` where the report carries no deployment summary.
fn deployment_status(reported: &Value) -> Option<String> {
    let Some(Value::Array(summaries)) = reported.get(STATUS_SUMMARIES_MEMBER) else {
        return None;
    };
    summaries
        .iter()
        .find(|summary| {
            reported_text(summary, STATUS_TYPE_MEMBER).as_deref() == Some(DEPLOYMENT_STATUS_TYPE)
        })
        .and_then(|summary| reported_text(summary, STATUS_MEMBER))
}

/// The attributes one reported manager gives the resource: the two instants Quick Setup keeps
/// for it, each rendered the way CloudFormation publishes an instant ([`published_instant`]),
/// and the summaries of its work, one element per indexed key. A manager that service reports
/// nothing about gives none, so the resource keeps what the naming rules computed rather than
/// taking an empty value.
fn reported_attributes_of(reported: Option<&Value>) -> BTreeMap<String, String> {
    let mut attributes = BTreeMap::new();
    let Some(reported) = reported else {
        return attributes;
    };
    for (attribute, member) in REPORTED_INSTANTS {
        if let Some(instant) = reported.get(*member).and_then(instant_text) {
            attributes.insert((*attribute).to_string(), published_instant(&instant));
        }
    }
    if let Some(Value::Array(summaries)) = reported.get(STATUS_SUMMARIES_MEMBER) {
        for (index, summary) in summaries.iter().enumerate() {
            attributes.insert(
                list_attribute_element(STATUS_SUMMARIES_ATTRIBUTE, index),
                attribute_text(summary),
            );
        }
    }
    attributes
}

/// The URI ONE configuration manager is addressed at: the collection, then the `ManagerArn`
/// `@httpLabel` escaped as a single path segment ([`percent_encoded`]) so an ARN's own `:` and
/// `/` address one manager rather than another path.
fn configuration_manager_uri(manager_arn: &str) -> String {
    format!(
        "{CONFIGURATION_MANAGER_URI}/{}",
        percent_encoded(manager_arn)
    )
}

/// The URI the tags of ONE resource of that service are addressed at: its own collection and
/// the `ResourceArn` label escaped the same way.
fn tags_uri(resource_arn: &str) -> String {
    format!("{TAGS_URI}/{}", percent_encoded(resource_arn))
}
