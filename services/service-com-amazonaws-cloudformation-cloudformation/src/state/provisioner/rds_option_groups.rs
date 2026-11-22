// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arm of an Amazon RDS OPTION GROUP: the group an
//! `AWS::RDS::OptionGroup` declares. One file of [`super`]'s per-type arms, dispatched to by
//! its `Provisioner`.
//!
//! The type's properties are `CreateOptionGroup`'s own members
//! ([`CREATE_OPTION_GROUP_MEMBERS`]) — `EngineName`, `MajorEngineVersion` and
//! `OptionGroupDescription` — under the name CloudFormation decides, which is this resource's
//! physical id and what the type's `Ref` returns.
//!
//! The group is created in the service that OWNS it, which is the whole point of provisioning
//! it: a DB instance's `OptionGroupName` is resolved against Amazon RDS's own option groups,
//! and a client's `DescribeOptionGroups` after the stack reports `CREATE_COMPLETE` has to
//! report the group the template declared.
//!
//! The declared `OptionConfigurations` are a SECOND call ([`include_declared_options`]), the
//! same pair a client makes: `CreateOptionGroup` takes no option at all — "the configuration
//! of options to include in a group" is `ModifyOptionGroup`'s `OptionsToInclude` — so a group
//! is created empty and the options a template declares are added to it, exactly as a
//! parameter group's declared `Parameters` are written by the modify beside its create
//! ([`super::modify_parameters`]).

use super::*;

/// `AWS::RDS::OptionGroup`: "Ref returns the name of the option group", which is the name
/// `DescribeOptionGroups`, `ModifyOptionGroup` and `DeleteOptionGroup` each address one by —
/// so this resource's physical id needs no replacing by what the create answered.
pub(crate) const RDS_OPTION_GROUP: &str = "AWS::RDS::OptionGroup";

/// The error code Amazon RDS answers for an option group it does not hold:
/// `OptionGroupNotFoundFault`'s own `awsQueryError` code (`smithy_query(query="operation",
/// operation="DeleteOptionGroup")`), which is what an awsQuery client branches on.
const OPTION_GROUP_NOT_FOUND: &str = "OptionGroupNotFoundFault";

/// The members `CreateOptionGroup` takes, from Amazon RDS's own model
/// (`smithy_query(query="operation", operation="CreateOptionGroup")`), with the tag list's
/// element name from its list shape's member `@xmlName`
/// (`smithy_query(query="shape", shape="TagList")`).
///
/// This is the catalogue the declaration of an `AWS::RDS::OptionGroup` is walked against: the
/// type's properties ARE these members, so a property whose name is one of them is passed
/// through under it and one the catalogue does not carry is refused rather than dropped
/// ([`query_members`]).
pub(crate) const CREATE_OPTION_GROUP_MEMBERS: &[(&str, QueryMember)] = &[
    ("EngineName", QueryMember::Scalar),
    ("MajorEngineVersion", QueryMember::Scalar),
    ("OptionGroupDescription", QueryMember::Scalar),
    ("OptionGroupName", QueryMember::Scalar),
    ("Tags", QueryMember::List("Tag")),
];

/// The property an `AWS::RDS::OptionGroup` declares its options under, which is no member of
/// the create at all: it reaches Amazon RDS as the modify's own `OptionsToInclude`.
const OPTION_CONFIGURATIONS_PROPERTY: &str = "OptionConfigurations";

/// The element one entry of that member is written under: `OptionConfigurationList`'s member
/// `@xmlName` (`smithy_query(query="shape", shape="OptionConfigurationList")`).
const OPTION_CONFIGURATION_ELEMENT: &str = "OptionConfiguration";

/// The properties of `AWS::RDS::OptionGroup` the walk over the declaration does NOT turn into
/// a `CreateOptionGroup` member: the name CloudFormation decides, the tag list it merges, and
/// the options the modify below carries.
const OPTION_GROUP_PROPERTIES_NOT_SENT: &[&str] = &[
    "OptionGroupName",
    "Tags",
    OPTION_CONFIGURATIONS_PROPERTY,
];

/// Create the option group an `AWS::RDS::OptionGroup` declares.
pub(crate) async fn create_option_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    let mut parameters = query_members(
        &resource.properties,
        CREATE_OPTION_GROUP_MEMBERS,
        &[],
        OPTION_GROUP_PROPERTIES_NOT_SENT,
        RDS_OPTION_GROUP,
    )?;
    parameters.push((
        "OptionGroupName".to_string(),
        resource.physical_resource_id.clone(),
    ));
    push_tag_list(
        &mut parameters,
        "Tags",
        "Tag",
        &tags_of(&resource.properties, propagated),
    );
    let answered = call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "CreateOptionGroup",
        &parameters,
    )
    .await?;
    include_declared_options(
        ctx,
        region,
        &resource.physical_resource_id,
        &resource.properties,
    )
    .await?;

    let mut attributes = BTreeMap::new();
    if let Some(arn) = xml_element(&answered, "OptionGroupArn").filter(|arn| !arn.is_empty()) {
        // The group's own ARN, which is the resource's: every name the type publishes its ARN
        // under is re-pointed at the one Amazon RDS reports
        // ([`super::template::arn_attributes`]).
        attributes.extend(super::template::arn_attributes(RDS_OPTION_GROUP, &arn));
    }
    Ok(Created::attributes(attributes))
}

/// Add the options a declaration carries to the group named `physical_resource_id`, through
/// the modify a client adds them with.
///
/// `OptionConfigurations` is the `OptionConfiguration` STRUCTURE list `ModifyOptionGroup`'s
/// `OptionsToInclude` takes, so each declared entry is written as one element of that list
/// and walked to its leaves ([`push_list`]) — the options a template declares are what
/// `DescribeOptionGroups` then reports the group carrying. A declaration carrying none makes
/// no call at all.
async fn include_declared_options(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
    properties: &Value,
) -> Result<(), Refusal> {
    let Some(declared) = properties.get(OPTION_CONFIGURATIONS_PROPERTY) else {
        return Ok(());
    };
    let mut parameters = vec![(
        "OptionGroupName".to_string(),
        physical_resource_id.to_string(),
    )];
    push_list(
        &mut parameters,
        "OptionsToInclude",
        OPTION_CONFIGURATION_ELEMENT,
        declared,
        0,
    )?;
    // A declaration whose list is empty names no option, so there is nothing to include.
    if parameters.len() == 1 {
        return Ok(());
    }
    // "Specifies whether to apply the change immediately or during the next maintenance
    // window": a stack reports a resource created when its options are in place rather than
    // when the next window comes round, which is how every other RDS arm here applies a
    // change.
    parameters.push(("ApplyImmediately".to_string(), "true".to_string()));
    call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "ModifyOptionGroup",
        &parameters,
    )
    .await
    .map(|_| ())
}

/// The option group's configuration as Amazon RDS reports it, for drift detection. A group it
/// no longer holds (`OptionGroupNotFoundFault`) was deleted.
///
/// An option group is reported as a STRUCTURE whose scalar members are named exactly as this
/// type's properties are (`EngineName`, `MajorEngineVersion`, `OptionGroupDescription`), so
/// those members are the actual configuration the declared one is compared against. Its
/// composite member is not: `Options` is reported as the option structures the group HOLDS,
/// where the property is the configuration list the modify took, so comparing the two would
/// need a mapping this module would have to invent ([`super::drift`]).
pub(crate) async fn read_option_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let parameters = vec![(
        "OptionGroupName".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DescribeOptionGroups",
        &parameters,
    )
    .await
    {
        Ok(answered) => Ok(Some(Value::Object(xml_scalar_children(
            &answered,
            "OptionGroup",
        )))),
        Err(refusal) if refusal.code == OPTION_GROUP_NOT_FOUND => Ok(None),
        Err(refusal) => Err(refusal),
    }
}

/// Delete the option group a stack created, addressed by its name — this resource's physical
/// id. A group Amazon RDS no longer holds is already in the state the delete asks for.
pub(crate) async fn delete_option_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let parameters = vec![(
        "OptionGroupName".to_string(),
        physical_resource_id.to_string(),
    )];
    match call_query(
        ctx,
        RDS_SIGNING_NAME,
        region,
        RDS_API_VERSION,
        "DeleteOptionGroup",
        &parameters,
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == OPTION_GROUP_NOT_FOUND => Ok(()),
        Err(refusal) => Err(refusal),
    }
}
