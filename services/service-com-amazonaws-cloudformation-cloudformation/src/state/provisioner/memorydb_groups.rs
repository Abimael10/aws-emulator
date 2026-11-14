// SPDX-License-Identifier: CC0-1.0

//! The per-type provisioning arms of Amazon MemoryDB's two configuration GROUPS: the
//! parameter group an `AWS::MemoryDB::ParameterGroup` declares and the subnet group an
//! `AWS::MemoryDB::SubnetGroup` declares. One file of [`super`]'s per-type arms,
//! dispatched to by its `Provisioner`.
//!
//! "Ref returns the name" for each, and each type states that name itself
//! (`ParameterGroupName` / `SubnetGroupName`, which the conventional `<last segment>Name`
//! reading of `super::template::physical_name` finds), so the resource's physical id is
//! the name the template asked for and the group is created in Amazon MemoryDB under
//! exactly it. That is the handle every call of that service on a group takes —
//! `DescribeParameterGroups`, `DescribeSubnetGroups` and the two deletes each address one
//! by its name — so a client that reads a group's name out of the stack it deployed
//! resolves it in the service that owns the resource, and the `ARN` the stack publishes
//! for the group is the ARN that service reports in the group's own `ARN` member.
//!
//! Each type's properties are its create's own members, spelled identically on both sides
//! ([`PARAMETER_GROUP_PROPERTIES`] / [`SUBNET_GROUP_PROPERTIES`]), with two exceptions:
//! the name CloudFormation decides, which these arms push, and the tag list, which is a
//! set the declaration and the stack both contribute to ([`tags_of`]). A parameter group's
//! declared `Parameters` are a SECOND call, the same pair a client makes — a group is
//! created carrying none of its own values, and an `UpdateParameterGroup` is what puts the
//! declared ones on it ([`apply_declared_parameters`]).

use super::*;

/// Amazon MemoryDB's signing name and `X-Amz-Target` prefix, from its model (`sigv4Name:
/// memorydb`, service shape `AmazonMemoryDB`, protocol `awsJson1_1`) — the wire these
/// calls are written on, as that service's own suites drive it.
pub(crate) const MEMORYDB_SIGNING_NAME: &str = "memorydb";
pub(crate) const MEMORYDB_TARGET_PREFIX: &str = "AmazonMemoryDB";

/// The two Amazon MemoryDB group types a cluster's configuration is declared with. Their
/// names live in the return-value catalogue, which is where the ARN each publishes is
/// computed from the group's name
/// ([`super::template::MEMORYDB_PARAMETER_GROUP`] /
/// [`super::template::MEMORYDB_SUBNET_GROUP`]).
pub(crate) use super::template::{MEMORYDB_PARAMETER_GROUP, MEMORYDB_SUBNET_GROUP};

/// Which of the two groups an arm is working on, so the one pair of create/delete/read
/// arms serves both types rather than two copies of each. Which it is, is decided once by
/// [`super::provisioner_for`] and travels with the `Provisioner` variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Parameter,
    Subnet,
}

impl Group {
    /// The member every call of that service carries this kind of group's NAME in
    /// (`CreateParameterGroup$ParameterGroupName`, `CreateSubnetGroup$SubnetGroupName`),
    /// which is also the type's own name property.
    fn name_member(self) -> &'static str {
        match self {
            Self::Parameter => "ParameterGroupName",
            Self::Subnet => "SubnetGroupName",
        }
    }

    /// The operation that makes one, and the one that removes it.
    fn create_operation(self) -> &'static str {
        match self {
            Self::Parameter => "CreateParameterGroup",
            Self::Subnet => "CreateSubnetGroup",
        }
    }

    fn delete_operation(self) -> &'static str {
        match self {
            Self::Parameter => "DeleteParameterGroup",
            Self::Subnet => "DeleteSubnetGroup",
        }
    }

    /// The operation that reports one, and the member its answer carries the group under
    /// — the list `@paginated` names (`DescribeParameterGroups$ParameterGroups`,
    /// `DescribeSubnetGroups$SubnetGroups`).
    fn describe_operation(self) -> &'static str {
        match self {
            Self::Parameter => "DescribeParameterGroups",
            Self::Subnet => "DescribeSubnetGroups",
        }
    }

    fn described_list(self) -> &'static str {
        match self {
            Self::Parameter => "ParameterGroups",
            Self::Subnet => "SubnetGroups",
        }
    }

    /// The error code Amazon MemoryDB answers for a group of this kind it does not hold
    /// (each type's own `*NotFoundFault`), which is what an awsJson client branches on.
    fn not_found_code(self) -> &'static str {
        match self {
            Self::Parameter => "ParameterGroupNotFoundFault",
            Self::Subnet => "SubnetGroupNotFoundFault",
        }
    }

    /// The members the create takes beside the name and the tags, each the declared
    /// property of the same name.
    fn create_members(self) -> &'static [&'static str] {
        match self {
            Self::Parameter => &["Family", "Description"],
            Self::Subnet => &["Description", "SubnetIds"],
        }
    }

    /// Every property the type publishes, which is what a declaration may state: a
    /// property no arm here maps onto the create's request is refused rather than dropped
    /// silently.
    fn properties(self) -> &'static [&'static str] {
        match self {
            Self::Parameter => PARAMETER_GROUP_PROPERTIES,
            Self::Subnet => SUBNET_GROUP_PROPERTIES,
        }
    }
}

/// The properties of `AWS::MemoryDB::ParameterGroup`: the name CloudFormation decides, the
/// engine family the group's values belong to, its description, the parameter values the
/// group overrides, and its tags.
const PARAMETER_GROUP_PROPERTIES: &[&str] = &[
    "ParameterGroupName",
    "Family",
    "Description",
    "Parameters",
    "Tags",
];

/// The properties of `AWS::MemoryDB::SubnetGroup`: the name CloudFormation decides, its
/// description, the VPC subnets a cluster in the group is placed in, and its tags.
const SUBNET_GROUP_PROPERTIES: &[&str] = &[
    "SubnetGroupName",
    "Description",
    "SubnetIds",
    "Tags",
];

/// Create the group a declaration of either MemoryDB group type asks for, under the name
/// the naming rules settled — the declared name, or the generated one where the template
/// states none, which is this resource's physical id and what the type's `Ref` returns.
///
/// What the create ANSWERS is the group's own ARN, which is the resource's: the attributes
/// the type publishes it under are re-pointed at the one Amazon MemoryDB reported
/// ([`super::template::arn_attributes`]), so the ARN a stack's output carries for the
/// group is the ARN that service's own describe reports for it.
pub(crate) async fn create_memorydb_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group: Group,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, group.properties())?;
    let mut request = Map::new();
    if let Value::Object(properties) = &resource.properties {
        for member in group.create_members() {
            if let Some(value) = properties.get(*member) {
                request.insert((*member).to_string(), value.clone());
            }
        }
    }
    request.insert(
        group.name_member().to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("Tags".to_string(), tags_as_list(&tags, UPPER_CASED_TAG));
    }
    let reported = call_memorydb(
        ctx,
        region,
        group.create_operation(),
        &Value::Object(request),
    )
    .await?;
    // The declared parameter values are the group's own, which the create does not take:
    // they reach MemoryDB through the update that puts values on a group, against a group
    // that holds none yet.
    if group == Group::Parameter {
        apply_parameter_values(
            ctx,
            region,
            &resource.physical_resource_id,
            &Value::Null,
            &resource.properties,
        )
        .await?;
    }
    let mut attributes = BTreeMap::new();
    if let Some(arn) = reported_arn(&reported, group) {
        attributes.extend(super::template::arn_attributes(
            &resource.resource_type,
            &arn,
        ));
    }
    Ok(Created::attributes(attributes))
}

/// Apply a CHANGED declaration of either MemoryDB group type to the group the stack
/// already has.
///
/// What each type's own service can change is what reaches it:
///
/// * a parameter group's VALUES, which `UpdateParameterGroup` ("Updates the parameters of a
///   parameter group") takes as the name/value pairs the type's `Parameters` map carries.
///   A value the declaration no longer states is taken back off the group through
///   `ResetParameterGroup` ("Modifies the parameters of a parameter group to the engine or
///   system default value"), so the group's values are the ones the declaration now states
///   rather than those plus whatever an earlier revision set. Its `Family` and
///   `Description` are create-only — that service publishes no call that changes either —
///   which is why a declaration restating one REPLACES the group
///   ([`super::template::replacement_properties`]) rather than reaching this arm;
/// * a subnet group's `Description` and `SubnetIds`, which are `UpdateSubnetGroup`'s own
///   members ("Updates a subnet group"), sent as the declaration now states them.
///
/// A declaration whose only change is its tags reaches this with nothing to do: the tag set
/// travels on its own ([`Provisioner::retag`]).
pub(crate) async fn update_memorydb_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group: Group,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    match group {
        Group::Parameter => {
            apply_parameter_values(
                ctx,
                region,
                &resource.physical_resource_id,
                &resource.properties,
                declared,
            )
            .await?;
        }
        Group::Subnet => {
            let mut request = Map::new();
            for member in ["Description", "SubnetIds"] {
                if let Some(value) = declared.get(member) {
                    if resource.properties.get(member) != Some(value) {
                        request.insert(member.to_string(), value.clone());
                    }
                }
            }
            if request.is_empty() {
                return Ok(Some(Created::attributes(BTreeMap::new())));
            }
            request.insert(
                group.name_member().to_string(),
                Value::String(resource.physical_resource_id.clone()),
            );
            call_memorydb(ctx, region, "UpdateSubnetGroup", &Value::Object(request)).await?;
        }
    }
    Ok(Some(Created::attributes(BTreeMap::new())))
}

/// Bring the parameter group's stored values into step with the ones the declaration now
/// states: every value it states is PUT on the group, and every one an earlier revision set
/// that it no longer states is RESET to the engine default.
///
/// `held` is the property document the stack recorded for the resource and `declared` the
/// one this deployment states, so what is sent is the difference between the two rather
/// than the whole map on every deployment.
async fn apply_parameter_values(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    parameter_group_name: &str,
    held: &Value,
    declared: &Value,
) -> Result<(), Refusal> {
    let values = |document: &Value| -> Map<String, Value> {
        match document.get("Parameters") {
            Some(Value::Object(parameters)) => parameters.clone(),
            _ => Map::new(),
        }
    };
    let (held, declared) = (values(held), values(declared));
    let put: Vec<Value> = declared
        .iter()
        .filter(|(name, value)| held.get(name.as_str()) != Some(value))
        .filter_map(|(name, value)| {
            let value = scalar_text(value)?;
            Some(serde_json::json!({ "ParameterName": name, "ParameterValue": value }))
        })
        .collect();
    if !put.is_empty() {
        call_memorydb(
            ctx,
            region,
            "UpdateParameterGroup",
            &serde_json::json!({
                "ParameterGroupName": parameter_group_name,
                "ParameterNameValues": put,
            }),
        )
        .await?;
    }
    let reset: Vec<Value> = held
        .keys()
        .filter(|name| !declared.contains_key(name.as_str()))
        .map(|name| Value::String(name.clone()))
        .collect();
    if !reset.is_empty() {
        call_memorydb(
            ctx,
            region,
            "ResetParameterGroup",
            &serde_json::json!({
                "ParameterGroupName": parameter_group_name,
                "ParameterNames": reset,
            }),
        )
        .await?;
    }
    Ok(())
}

/// Delete the group a stack created, addressed by its name — this resource's physical id.
/// A group Amazon MemoryDB no longer holds (that kind's own not-found fault) is already in
/// the state the delete asks for.
pub(crate) async fn delete_memorydb_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group: Group,
    physical_resource_id: &str,
) -> Result<(), Refusal> {
    let request = serde_json::json!({ group.name_member(): physical_resource_id });
    match call_memorydb(ctx, region, group.delete_operation(), &request).await {
        Ok(_) => Ok(()),
        Err(refusal) if refusal.code == group.not_found_code() => Ok(()),
        Err(refusal) => Err(refusal),
    }
}

/// The group's configuration as Amazon MemoryDB reports it, for drift detection. A group
/// it no longer holds (that kind's own not-found fault) was deleted.
///
/// Its SCALAR members are reported, which covers the properties a declaration can drift in
/// under the same name (a parameter group's `Family` and `Description`, a subnet group's
/// `Description`); a member that is a LIST on the wire (a subnet group's `Subnets`, which
/// reports each subnet as a structure rather than as the declared identifier) is left out,
/// which is the position every arm whose property is not a scalar is in and what
/// [`super::drift`] documents.
pub(crate) async fn describe_memorydb_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group: Group,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let Some(reported) = read_memorydb_group(ctx, region, group, physical_resource_id).await?
    else {
        return Ok(None);
    };
    let Value::Object(members) = reported else {
        return Ok(None);
    };
    Ok(Some(Value::Object(
        members
            .into_iter()
            .filter(|(_, value)| scalar_text(value).is_some())
            .collect(),
    )))
}

/// The group Amazon MemoryDB holds under this name, or `None` for one it does not hold.
///
/// A describe of either kind answers a LIST (`ParameterGroups` / `SubnetGroups`) carrying
/// the one group the request named, so the group is the first entry of that list.
async fn read_memorydb_group(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    group: Group,
    physical_resource_id: &str,
) -> Result<Option<Value>, Refusal> {
    let request = serde_json::json!({ group.name_member(): physical_resource_id });
    let reported = match call_memorydb(ctx, region, group.describe_operation(), &request).await {
        Ok(reported) => reported,
        Err(refusal) if refusal.code == group.not_found_code() => return Ok(None),
        Err(refusal) => return Err(refusal),
    };
    Ok(reported
        .get(group.described_list())
        .and_then(Value::as_array)
        .and_then(|groups| groups.first())
        .cloned())
}

/// The ARN the create's answer carries for the group: `ARN` of the one member that answer
/// holds the group under (`CreateParameterGroup$ParameterGroup`,
/// `CreateSubnetGroup$SubnetGroup`), which is the member Amazon MemoryDB publishes a
/// group's Amazon Resource Name as.
fn reported_arn(reported: &Value, group: Group) -> Option<String> {
    let member = match group {
        Group::Parameter => "ParameterGroup",
        Group::Subnet => "SubnetGroup",
    };
    reported
        .get(member)
        .and_then(|group| group.get(super::template::UPPER_CASE_ARN_ATTRIBUTE))
        .and_then(Value::as_str)
        .filter(|arn| !arn.is_empty())
        .map(str::to_string)
}

/// Call one Amazon MemoryDB operation in process, on the awsJson wire that service
/// publishes.
pub(crate) async fn call_memorydb(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    operation: &str,
    request: &Value,
) -> Result<Value, Refusal> {
    call_json_rpc(
        ctx,
        MEMORYDB_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{MEMORYDB_TARGET_PREFIX}.{operation}"),
        request,
    )
    .await
}
