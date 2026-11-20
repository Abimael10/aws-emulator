// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of Amazon CloudWatch Application Insights: the APPLICATION an
//! `AWS::ApplicationInsights::Application` declares, created in that service itself through its
//! own `CreateApplication`, read back through `DescribeApplication` and removed by
//! `DeleteApplication`. One file of [`super`]'s per-type arms, dispatched to by its
//! `Provisioner`.
//!
//! ## Why it is really created
//!
//! An application is read back through Application Insights' own API — a client that deployed
//! the stack describes it, lists its components and configures their monitoring — and the
//! application is created OVER a resource group, which that service resolves in Resource Groups
//! before it holds anything. A stack that reported the resource created while nothing created it
//! publishes an ARN that resolves to nothing, and a template naming a group that does not exist
//! deploys silently instead of failing on that service's own not-found refusal.
//!
//! ## What the declaration is
//!
//! The type's properties are `CreateApplication`'s own members, named identically on both sides
//! but for one rename: the template spells `AutoConfigurationEnabled` where that create takes
//! `AutoConfigEnabled` ([`APPLICATION_MEMBERS`]). `Tags` is the `[{Key, Value}]` list most types
//! publish, which is the shape its own `Tags` member takes, so the declared tags travel beside
//! the ones CloudFormation gives the resource ([`tags_of`]). `CustomComponents` is no member of
//! the create at all — a custom component groups resources of an application that exists, which
//! `CreateComponent` makes — so the arm makes that second call per declared component
//! ([`group_custom_components`]).
//!
//! A property outside that list is REFUSED rather than dropped
//! ([`refuse_unmapped_properties`]), so an application is never reported created with less than
//! its declaration asked for; the `ApplicationARN` the type publishes is read-only and is
//! dropped before the declaration gets here ([`super::template::read_only_properties`]).
//!
//! The application's identity is the ARN its own `Ref` returns, which is a total function of the
//! Region, the account and the group the application monitors — so the naming rules compute it
//! ([`super::template::return_values`]) and this arm reports no physical id of its own.

use super::*;

/// Application Insights' signing name and `X-Amz-Target` prefix, from its model
/// (`sigv4Name: applicationinsights`, service shape `EC2WindowsBarleyService`), which speaks
/// `awsJson1_1` — so its calls ride on [`call_json_rpc`].
const APPLICATION_INSIGHTS_SIGNING_NAME: &str = "applicationinsights";
const APPLICATION_INSIGHTS_TARGET_PREFIX: &str = "EC2WindowsBarleyService";

/// The operations this module calls, as Application Insights' own model names them.
const CREATE_APPLICATION: &str = "CreateApplication";
const DESCRIBE_APPLICATION: &str = "DescribeApplication";
const DELETE_APPLICATION: &str = "DeleteApplication";
const CREATE_COMPONENT: &str = "CreateComponent";

/// The error code Application Insights answers for a resource it does not hold
/// (`ResourceNotFoundException`, the modelled 404 of its reads, its deletes and of a create
/// whose resource group is not the account's).
const APPLICATION_INSIGHTS_NOT_FOUND: &str = "ResourceNotFoundException";

/// The member every call on one application addresses it by: the resource group it monitors,
/// which is how that service keys an application (`DescribeApplication$ResourceGroupName`,
/// `@required`).
const RESOURCE_GROUP_NAME_MEMBER: &str = "ResourceGroupName";

/// The member `CreateApplication` and `DescribeApplication` answer the application's record in.
const APPLICATION_INFO_MEMBER: &str = "ApplicationInfo";

/// The member of that record that carries where the application is in its onboarding
/// (`ApplicationInfo$LifeCycle`), and the one value a still-onboarding application carries:
/// Application Insights reports an application it is still onboarding the group's resources for
/// as `CREATING`, in which its components are not there to be configured yet.
const LIFE_CYCLE_MEMBER: &str = "LifeCycle";
const LIFE_CYCLE_CREATING: &str = "CREATING";

/// The members `CreateComponent` takes a custom component as, which are the members the
/// `CustomComponent` property type publishes under the same names.
const COMPONENT_NAME_MEMBER: &str = "ComponentName";
const RESOURCE_LIST_MEMBER: &str = "ResourceList";

/// The property a declaration states its custom components in, which is no member of the create.
const CUSTOM_COMPONENTS_PROPERTY: &str = "CustomComponents";

/// The `Tags` member `CreateApplication` takes, as the `[{Key, Value}]` list its own `TagList`
/// is.
const TAGS_MEMBER: &str = "Tags";

/// The properties this arm maps onto the application's create, and so the ones a declaration may
/// carry: every property of the type whose value this arm really applies.
///
/// Source: the type's own entry in the AWS resource and property types reference read beside
/// `CreateApplication`'s own members (`smithy_query(query="operation",
/// operation="CreateApplication")`). `ComponentMonitoringSettings` and `LogPatternSets` are NOT
/// here: each is applied by a call of that service this emulator does not serve yet
/// (`UpdateComponentConfiguration` over a recommendation nothing computes, and
/// `CreateLogPattern`), so a declaration stating one is refused rather than reported as applied.
const APPLICATION_PROPERTIES: &[&str] = &[
    "AttachMissingPermission",
    "AutoConfigurationEnabled",
    "CWEMonitorEnabled",
    CUSTOM_COMPONENTS_PROPERTY,
    "GroupingType",
    "OpsCenterEnabled",
    "OpsItemSNSTopicArn",
    RESOURCE_GROUP_NAME_MEMBER,
    "SNSNotificationArn",
    TAGS_MEMBER,
];

/// The properties `CreateApplication` takes as members of its own, as (property, member).
///
/// Every one of them is spelled identically on both sides but `AutoConfigurationEnabled`, which
/// that create takes as `AutoConfigEnabled` — the one rename between the type and its API. The
/// tags are not here, being a set the declaration and the stack both contribute to, and the
/// custom components are not either, being no member of the create at all.
const APPLICATION_MEMBERS: &[(&str, &str)] = &[
    (RESOURCE_GROUP_NAME_MEMBER, RESOURCE_GROUP_NAME_MEMBER),
    ("AttachMissingPermission", "AttachMissingPermission"),
    ("AutoConfigurationEnabled", "AutoConfigEnabled"),
    ("CWEMonitorEnabled", "CWEMonitorEnabled"),
    ("GroupingType", "GroupingType"),
    ("OpsCenterEnabled", "OpsCenterEnabled"),
    ("OpsItemSNSTopicArn", "OpsItemSNSTopicArn"),
    ("SNSNotificationArn", "SNSNotificationArn"),
];

/// The members `DescribeApplication` reports an application's configuration under, as
/// (property, member): the properties of the type that read answers, including the one the two
/// sides spell differently.
const REPORTED_APPLICATION_MEMBERS: &[(&str, &str)] = &[
    (RESOURCE_GROUP_NAME_MEMBER, RESOURCE_GROUP_NAME_MEMBER),
    ("AttachMissingPermission", "AttachMissingPermission"),
    ("AutoConfigurationEnabled", "AutoConfigEnabled"),
    ("CWEMonitorEnabled", "CWEMonitorEnabled"),
    ("OpsCenterEnabled", "OpsCenterEnabled"),
    ("OpsItemSNSTopicArn", "OpsItemSNSTopicArn"),
    ("SNSNotificationArn", "SNSNotificationArn"),
];

/// Call one Application Insights operation on the awsJson1_1 wire its model declares.
async fn call_application_insights(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        APPLICATION_INSIGHTS_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{APPLICATION_INSIGHTS_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}

/// The resource group the application a resource record names monitors: the
/// `ResourceGroupName` its declaration states, which is the identifier every Application
/// Insights call on one takes.
///
/// A record whose declaration no longer carries it falls back to the group inside the ARN that
/// is this resource's physical id (`…:application/resource-group/<group>`), so a delete still
/// addresses the application the create made. `None` is a record that names NO group either way
/// — one whose physical id is still the stand-in name the naming rules computed, the create
/// never having been made — and there is then no application to address at all, rather than one
/// addressed by a name guessed from that stand-in.
fn application_group(resource: &StoredResource) -> Option<String> {
    declared_group(Some(&resource.properties), &resource.physical_resource_id)
}

/// [`application_group`] for a caller that holds the declaration and the physical id rather
/// than the whole record ([`Provisioner::readiness`] is handed exactly those two).
fn declared_group(declared: Option<&Value>, physical_resource_id: &str) -> Option<String> {
    let stated = declared
        .and_then(|properties| properties.get(RESOURCE_GROUP_NAME_MEMBER))
        .map(attribute_text)
        .unwrap_or_default();
    if !stated.is_empty() {
        return Some(stated);
    }
    physical_resource_id
        .split_once(&format!("{APPLICATION_INSIGHTS_APPLICATION_PATH}/"))
        .map(|(_, group)| group.to_string())
        .filter(|group| !group.is_empty())
}

/// Create the application an `AWS::ApplicationInsights::Application` declares, so a client that
/// deployed the stack finds it in Application Insights itself — `DescribeApplication` answers
/// with it and `ListApplications` reports it — rather than that service's
/// `ResourceNotFoundException` for an application nothing created.
///
/// The application is created over the resource group the declaration names, which Application
/// Insights resolves in Resource Groups: a declaration naming a group that service holds none of
/// is that service's own not-found refusal, which fails the resource instead of publishing an
/// application bound to no group.
pub(super) async fn create_application(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, APPLICATION_PROPERTIES)?;
    let mut request = Map::new();
    if let Some(declared) = resource.properties.as_object() {
        copy_members(declared, APPLICATION_MEMBERS, &mut request);
    }
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert(
            TAGS_MEMBER.to_string(),
            tags_as_list(&tags, UPPER_CASED_TAG),
        );
    }
    call_application_insights(
        ctx,
        region,
        CREATE_APPLICATION,
        &Value::Object(request),
    )
    .await?;
    group_custom_components(ctx, region, resource).await?;
    // The application's ARN is a total function of the Region, the account and the group it
    // monitors, which the naming rules already computed as this resource's physical id and its
    // one published attribute — so there is nothing of that service's own to report back.
    Ok(Created::default())
}

/// Make the custom components a declaration's `CustomComponents` list asks for, through the
/// `CreateComponent` a client makes next: such a component groups resources of an application
/// that already exists, so that call is the one that records it. A declaration stating none asks
/// nothing.
///
/// How many calls that is is bounded by the declared list, which is bounded by the template body
/// the stack was created from, and Application Insights' own bound on how many components one
/// application holds refuses the one past it — so neither this loop nor the application it writes
/// grows with anything a caller can choose freely. A component that service refuses fails the
/// resource, rather than the application being reported created with less than its declaration
/// asked for.
async fn group_custom_components(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    let declared = resource
        .properties
        .get(CUSTOM_COMPONENTS_PROPERTY)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if declared.is_empty() {
        return Ok(());
    }
    // The create this follows was made over the group the declaration states, so the record
    // names one; a record that names none has no application for a component to belong to.
    let Some(group) = application_group(resource) else {
        return Ok(());
    };
    for component in declared {
        let mut request = Map::new();
        request.insert(
            RESOURCE_GROUP_NAME_MEMBER.to_string(),
            Value::String(group.clone()),
        );
        if let Some(name) = component.get(COMPONENT_NAME_MEMBER) {
            request.insert(
                COMPONENT_NAME_MEMBER.to_string(),
                Value::String(attribute_text(name)),
            );
        }
        if let Some(resources) = component.get(RESOURCE_LIST_MEMBER) {
            request.insert(RESOURCE_LIST_MEMBER.to_string(), resources.clone());
        }
        call_application_insights(ctx, region, CREATE_COMPONENT, &Value::Object(request)).await?;
    }
    Ok(())
}

/// Whether Application Insights reports the created application ONBOARDED.
///
/// `CreateApplication` answers the application while that service is still onboarding the
/// resources of its resource group — the `CREATING` lifecycle, in which the components the
/// declaration's monitoring is about are not there yet — so the resource's creation is finished
/// once that service reports it out of it. An application it holds nothing for is one the create
/// did not leave behind.
pub(super) async fn application_readiness(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    declared: Option<&Value>,
) -> Result<Readiness, Refusal> {
    let Some(group) = declared_group(declared, physical_resource_id) else {
        return Ok(Readiness::Failed(
            "this resource names no resource group to read an application over".to_string(),
        ));
    };
    let Some(reported) = read_application(ctx, region, &group).await? else {
        return Ok(Readiness::Failed(format!(
            "Application Insights holds no application for resource group {group}"
        )));
    };
    match reported_text(&reported, LIFE_CYCLE_MEMBER)
        .unwrap_or_default()
        .as_str()
    {
        LIFE_CYCLE_CREATING => Ok(Readiness::Pending(LIFE_CYCLE_CREATING.to_string())),
        // Every other lifecycle is one the application is onboarded in, a read that reports
        // none included: what the wait is for is the onboarding the create started.
        _ => Ok(Readiness::Ready),
    }
}

/// The application's configuration as Application Insights reports it, for drift detection,
/// under the property names the type declares — so a declared member is compared against the
/// member that service answers for it.
///
/// An application that read no longer holds was deleted, which for a READ is what
/// [`Provisioner::actual_configuration`] reports as `DELETED`. `Tags` and `CustomComponents` are
/// not reported: neither is a member of that read, and a property the actual document does not
/// carry is not evidence of anything ([`super::drift`]).
pub(super) async fn describe_application(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    // A record that names no group names no application either, which is the same answer as a
    // group that service holds no application over.
    let Some(group) = application_group(resource) else {
        return Ok(None);
    };
    Ok(read_application(ctx, region, &group)
        .await?
        .map(|reported| reported_as_properties(&reported, REPORTED_APPLICATION_MEMBERS)))
}

/// `DescribeApplication`'s own record of the application over the group of `name`, or `None` for
/// one Application Insights does not hold (`ResourceNotFoundException`, judged on the CODE and
/// never on a message that carries the request's own name).
async fn read_application(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> Result<Option<Value>, Refusal> {
    match call_application_insights(
        ctx,
        region,
        DESCRIBE_APPLICATION,
        &serde_json::json!({ RESOURCE_GROUP_NAME_MEMBER: name }),
    )
    .await
    {
        Ok(answer) => Ok(answer.get(APPLICATION_INFO_MEMBER).cloned()),
        Err(refusal) if refusal.code == APPLICATION_INSIGHTS_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Delete the application a stack created, addressed by the resource group it monitors. An
/// application Application Insights no longer holds (`ResourceNotFoundException`) is already in
/// the state the delete asks for.
pub(super) async fn delete_application(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    // A record that names no group names no application, so there is nothing to delete — and no
    // name to address a delete to that could be another resource group's application.
    let Some(group) = application_group(resource) else {
        return Ok(());
    };
    match call_application_insights(
        ctx,
        region,
        DELETE_APPLICATION,
        &serde_json::json!({ RESOURCE_GROUP_NAME_MEMBER: group }),
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == APPLICATION_INSIGHTS_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
