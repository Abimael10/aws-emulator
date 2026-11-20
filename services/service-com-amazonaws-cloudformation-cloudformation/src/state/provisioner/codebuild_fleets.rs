// SPDX-License-Identifier: CC0-1.0

//! The AWS CodeBuild COMPUTE FLEET a stack declares: `AWS::CodeBuild::Fleet`, created in
//! CodeBuild itself through that service's own `CreateFleet`, read back through
//! `BatchGetFleets` and removed by `DeleteFleet`.
//!
//! One file of [`super`]'s per-type arms, beside the report group and build project arms the
//! same owning service has ([`super::network_and_delivery`]): a fleet's identity is the ARN
//! CodeBuild draws per fleet rather than the name any rule could compute, so its arms live
//! together here.
//!
//! ## The rule this file carries
//!
//! A stack reports a resource created only where the service that OWNS the type really holds
//! it, and the identifiers it publishes for the resource are the ones that service answered.
//! So a stack that reached `CREATE_COMPLETE` with a fleet declared is one whose client finds
//! that fleet in `ListFleets` and resolves it through `BatchGetFleets`, and a stack that
//! removed it is one whose fleet CodeBuild no longer holds.
//!
//! The fleet's handle is the ARN: "Ref returns the fleet ARN", which carries the `<name>:<id>`
//! identifier CodeBuild mints per fleet (`Fleet$arn`, `Fleet$id`), so it is this resource's
//! physical id and comes off the create's own answer
//! ([`crate::state::template::identity_is_service_assigned`] names the type). The NAME a
//! declaration states — or the `<stack>-<logical id>-<unique id>` name CloudFormation generates
//! for one that states none — is only what the fleet is CREATED under, which is why it stays
//! the type's name property.

use super::*;

/// The resource type this file provisions. Visible to the rest of this service's state
/// because the naming rules and the identity catalogue key their entries for the type by it.
pub(in crate::state) const CODEBUILD_FLEET: &str = "AWS::CodeBuild::Fleet";

/// The `AWS::CodeBuild::Fleet` properties `CreateFleet` takes under its own member name, as
/// `(property, member)` pairs: the same member under a lower-case initial.
///
/// Source: AWS's CloudFormation resource reference for the type against
/// `smithy_query(query="operation",operation="CreateFleet",
/// model_path="services/service-com-amazonaws-codebuild-codebuild-20161006/smithy-model/\
/// com-amazonaws-codebuild-codebuild-20161006.json")` — that call's members are `name`,
/// `baseCapacity`, `environmentType`, `computeType`, `computeConfiguration`,
/// `scalingConfiguration`, `overflowBehavior`, `vpcConfig`, `proxyConfiguration`, `imageId`,
/// `fleetServiceRole` and `tags`.
///
/// `Name` is not here because it is the one value CloudFormation decides
/// ([`declared_name`]); `Tags` is not because the tags CloudFormation gives the resource
/// travel beside the declared ones ([`tags_of`]); and the four STRUCTURES are not, because
/// each is spelled member by member below.
const CODEBUILD_FLEET_MEMBERS: &[(&str, &str)] = &[
    ("BaseCapacity", "baseCapacity"),
    ("EnvironmentType", "environmentType"),
    ("ComputeType", "computeType"),
    ("OverflowBehavior", "overflowBehavior"),
    ("ImageId", "imageId"),
    ("FleetServiceRole", "fleetServiceRole"),
];

/// The property carrying the compute attributes of an attribute-based or custom-instance
/// fleet, whose members the type publishes under the names `ComputeConfiguration`'s own
/// shape spells them (`vCpu`, `memory`, `disk`, `machineType`, `instanceType`) rather than
/// under an upper-cased initial — so each pair here is the same name on both sides.
const CODEBUILD_COMPUTE_CONFIGURATION_PROPERTY: &str = "ComputeConfiguration";
const CODEBUILD_COMPUTE_CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("vCpu", "vCpu"),
    ("memory", "memory"),
    ("disk", "disk"),
    ("machineType", "machineType"),
    ("instanceType", "instanceType"),
];

/// The property carrying the fleet's auto-scaling configuration, and the members
/// `ScalingConfigurationInput` takes it as — each the property with a lower-cased initial.
const CODEBUILD_SCALING_CONFIGURATION_PROPERTY: &str = "ScalingConfiguration";
const CODEBUILD_SCALING_CONFIGURATION_MEMBERS: &[(&str, &str)] = &[
    ("ScalingType", "scalingType"),
    ("MaxCapacity", "maxCapacity"),
];

/// The property of that configuration carrying the target-tracking policies, and the members
/// one `TargetTrackingScalingConfiguration` takes.
const CODEBUILD_TARGET_TRACKING_PROPERTY: &str = "TargetTrackingScalingConfigs";
const CODEBUILD_TARGET_TRACKING_MEMBERS: &[(&str, &str)] =
    &[("MetricType", "metricType"), ("TargetValue", "targetValue")];

/// The property carrying the VPC the fleet's instances run in, and the members `VpcConfig`
/// takes it as.
const CODEBUILD_VPC_CONFIG_PROPERTY: &str = "VpcConfig";
const CODEBUILD_VPC_CONFIG_MEMBERS: &[(&str, &str)] = &[
    ("VpcId", "vpcId"),
    ("Subnets", "subnets"),
    ("SecurityGroupIds", "securityGroupIds"),
];

/// The property carrying the network access control of the fleet's instances, and the members
/// `ProxyConfiguration` takes it as.
const CODEBUILD_PROXY_CONFIGURATION_PROPERTY: &str = "ProxyConfiguration";
const CODEBUILD_PROXY_CONFIGURATION_MEMBERS: &[(&str, &str)] =
    &[("DefaultBehavior", "defaultBehavior")];

/// The property of that configuration carrying the ordered rules, and the members one
/// `FleetProxyRule` takes.
const CODEBUILD_PROXY_RULES_PROPERTY: &str = "OrderedProxyRules";
const CODEBUILD_PROXY_RULE_MEMBERS: &[(&str, &str)] = &[
    ("Type", "type"),
    ("Effect", "effect"),
    ("Entities", "entities"),
];

/// Every property the type publishes, which is what a declaration may state: a property no
/// arm here maps onto `CreateFleet`'s request is refused rather than dropped silently, so a
/// fleet the stack reports created is never one configured with less than its declaration
/// asked for.
const CODEBUILD_FLEET_PROPERTIES: &[&str] = &[
    "Name",
    "BaseCapacity",
    "EnvironmentType",
    "ComputeType",
    "OverflowBehavior",
    "ImageId",
    "FleetServiceRole",
    CODEBUILD_COMPUTE_CONFIGURATION_PROPERTY,
    CODEBUILD_SCALING_CONFIGURATION_PROPERTY,
    CODEBUILD_VPC_CONFIG_PROPERTY,
    CODEBUILD_PROXY_CONFIGURATION_PROPERTY,
    "Tags",
];

/// The `Fleet` members CodeBuild reports under this type's own property names, as
/// `(property, member)` pairs, for drift detection.
///
/// The nested structures are not among them, for the reason the report group's catalogue
/// gives: a property the read reports differently is not evidence of anything
/// ([`crate::state::drift`]). `Tags` is left out because the tags a resource carries change
/// independently of its declaration.
const CODEBUILD_FLEET_REPORTED_PROPERTIES: &[(&str, &str)] = &[
    ("Name", "name"),
    ("BaseCapacity", "baseCapacity"),
    ("EnvironmentType", "environmentType"),
    ("ComputeType", "computeType"),
    ("OverflowBehavior", "overflowBehavior"),
    ("ImageId", "imageId"),
    ("FleetServiceRole", "fleetServiceRole"),
];

/// The member a `CreateFleet` answer carries the fleet in, and the members of the fleet that
/// carry the ARN CodeBuild minted for it and the name it was created under.
const CODEBUILD_FLEET_MEMBER: &str = "fleet";
const CODEBUILD_FLEET_ARN_MEMBER: &str = "arn";
const CODEBUILD_FLEET_NAME_MEMBER: &str = "name";

/// The attribute name this type publishes the fleet's ARN under, which is the one value its
/// "Return values" section names beside `Ref` — and the handle every call here addresses the
/// fleet by once the create's answer is what the record carries.
const ARN_ATTRIBUTE: &str = "Arn";

/// Create the compute fleet an `AWS::CodeBuild::Fleet` declares, and report the ARN CodeBuild
/// MINTED for it — so a client that deployed the stack and then calls `codebuild list-fleets`
/// or `batch-get-fleets` finds the fleet the template described, and a project that names the
/// fleet in its environment resolves one that service really holds.
///
/// The fleet is created under the name CloudFormation decided ([`declared_name`] — the
/// declared `Name`, or the `<stack>-<logical id>-<unique id>` name it generated, fitted to
/// `FleetName`'s own bound by the naming rules), carrying the capacity, the environment and
/// compute types, the overflow behaviour, the image, the service role and the four nested
/// configurations the declaration states, each under the member name `CreateFleet` takes it
/// as. CodeBuild's own validation is what judges those values — a capacity or a compute type
/// it refuses leaves the stack `CREATE_FAILED` rather than a fleet provisioned differently
/// from the template.
///
/// The fleet carries the tags the declaration asks for beside the ones CloudFormation gives
/// the resource ([`tags_of`]), as the `[{key, value}]` list CodeBuild's own `Tag` shape
/// spells.
///
/// What comes back is the ARN that service minted, which no naming rule could compute: it is
/// this resource's physical id and its one published `Fn::GetAtt` attribute, so both are
/// reported off the create's answer and nothing else is recorded — the type publishes no other
/// return value.
pub(super) async fn create_codebuild_fleet(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    propagated: &[(String, String)],
) -> Result<Created, Refusal> {
    refuse_unmapped_properties(resource, CODEBUILD_FLEET_PROPERTIES)?;
    let name = declared_name(resource);
    let mut request = fleet_request(&name, &resource.properties);
    let tags = tags_of(&resource.properties, propagated);
    if !tags.is_empty() {
        request.insert("tags".to_string(), tags_as_list(&tags, LOWER_CASED_TAG));
    }

    let answered = call_codebuild(ctx, region, "CreateFleet", &Value::Object(request)).await?;
    let created = reported_member(&answered, CODEBUILD_FLEET_MEMBER);
    let Some(arn) = created.get(CODEBUILD_FLEET_ARN_MEMBER).and_then(scalar_text) else {
        // The create answered 2xx without the ARN its output models. Nothing addresses a
        // fleet by an identity nothing minted, so this is reported as a failure rather than
        // recorded as a resource with a physical id this module invented.
        return Err(Refusal {
            code: UNIDENTIFIED_FAILURE.to_string(),
            message: format!("CreateFleet answered no {CODEBUILD_FLEET_ARN_MEMBER}"),
        });
    };
    let mut attributes = BTreeMap::new();
    attributes.insert(ARN_ATTRIBUTE.to_string(), arn.clone());
    Ok(Created {
        physical_resource_id: Some(arn),
        attributes,
    })
}

/// Delete the fleet a stack created, addressed by the ARN CodeBuild keyed it by — the handle
/// `DeleteFleet` takes ("the ARN of the compute fleet"), which is this resource's physical id.
///
/// `DeleteFleet` models no does-not-exist error: a fleet CodeBuild no longer holds is already
/// in the state the delete asks for, and that call says so itself. A record carrying NO
/// identity is a resource whose create never answered one
/// ([`crate::state::provisioning`] forgets the computed name for such a type), so there is no
/// fleet to address and nothing is asked of that service — sending the NAME instead would
/// address no fleet, since that call takes an ARN alone.
pub(super) async fn delete_codebuild_fleet(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<(), Refusal> {
    if resource.physical_resource_id.is_empty() {
        return Ok(());
    }
    call_codebuild(
        ctx,
        region,
        "DeleteFleet",
        &serde_json::json!({ "arn": resource.physical_resource_id }),
    )
    .await?;
    Ok(())
}

/// The fleet's configuration as CodeBuild reports it, for drift detection, under the property
/// names the type declares — so a declared member is compared against the member of the same
/// name that service answers with.
///
/// `BatchGetFleets` reports a handle it holds no fleet for in `fleetsNotFound` rather than
/// refusing the read, so an answer carrying no fleet is that service saying it no longer holds
/// the resource — the `DELETED` drift status.
pub(super) async fn describe_codebuild_fleet(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
) -> Result<Option<Value>, Refusal> {
    // "The names or ARNs of the compute fleets" (`BatchGetFleets$names`), so the handle the
    // record carries addresses the fleet whichever of the two it is.
    let request = serde_json::json!({ "names": [codebuild_fleet_handle(resource)] });
    let answered = call_codebuild(ctx, region, "BatchGetFleets", &request).await?;
    let Some(fleet) = answered
        .get("fleets")
        .and_then(Value::as_array)
        .and_then(|fleets| fleets.first())
    else {
        return Ok(None);
    };
    Ok(Some(reported_as_properties(
        fleet,
        CODEBUILD_FLEET_REPORTED_PROPERTIES,
    )))
}

/// One `CreateFleet` request: the `name` CloudFormation decided, the scalar members
/// `properties` declares ([`CODEBUILD_FLEET_MEMBERS`]) and each nested configuration it
/// states, mapped member by member.
fn fleet_request(name: &str, properties: &Value) -> Map<String, Value> {
    let mut request = Map::new();
    request.insert(
        CODEBUILD_FLEET_NAME_MEMBER.to_string(),
        Value::String(name.to_string()),
    );
    if let Some(declared) = properties.as_object() {
        copy_members(declared, CODEBUILD_FLEET_MEMBERS, &mut request);
    }
    if let Some(declared) = properties.get(CODEBUILD_COMPUTE_CONFIGURATION_PROPERTY) {
        request.insert(
            "computeConfiguration".to_string(),
            mapped_members(declared, CODEBUILD_COMPUTE_CONFIGURATION_MEMBERS),
        );
    }
    if let Some(declared) = properties.get(CODEBUILD_SCALING_CONFIGURATION_PROPERTY) {
        request.insert(
            "scalingConfiguration".to_string(),
            scaling_configuration(declared),
        );
    }
    if let Some(declared) = properties.get(CODEBUILD_VPC_CONFIG_PROPERTY) {
        request.insert(
            "vpcConfig".to_string(),
            mapped_members(declared, CODEBUILD_VPC_CONFIG_MEMBERS),
        );
    }
    if let Some(declared) = properties.get(CODEBUILD_PROXY_CONFIGURATION_PROPERTY) {
        request.insert(
            "proxyConfiguration".to_string(),
            proxy_configuration(declared),
        );
    }
    request
}

/// The declared `ScalingConfiguration` as `CreateFleet` takes it: the configuration's own
/// members plus each target-tracking policy it lists, mapped member by member.
fn scaling_configuration(declared: &Value) -> Value {
    let mut mapped = mapped_members(declared, CODEBUILD_SCALING_CONFIGURATION_MEMBERS);
    let Value::Object(members) = &mut mapped else {
        return mapped;
    };
    if let Some(policies) = declared
        .get(CODEBUILD_TARGET_TRACKING_PROPERTY)
        .and_then(Value::as_array)
    {
        members.insert(
            "targetTrackingScalingConfigs".to_string(),
            Value::Array(
                policies
                    .iter()
                    .map(|policy| mapped_members(policy, CODEBUILD_TARGET_TRACKING_MEMBERS))
                    .collect(),
            ),
        );
    }
    mapped
}

/// The declared `ProxyConfiguration` as that call takes it: the default behaviour plus each
/// rule the declaration orders, mapped member by member.
fn proxy_configuration(declared: &Value) -> Value {
    let mut mapped = mapped_members(declared, CODEBUILD_PROXY_CONFIGURATION_MEMBERS);
    let Value::Object(members) = &mut mapped else {
        return mapped;
    };
    if let Some(rules) = declared
        .get(CODEBUILD_PROXY_RULES_PROPERTY)
        .and_then(Value::as_array)
    {
        members.insert(
            "orderedProxyRules".to_string(),
            Value::Array(
                rules
                    .iter()
                    .map(|rule| mapped_members(rule, CODEBUILD_PROXY_RULE_MEMBERS))
                    .collect(),
            ),
        );
    }
    mapped
}

/// The handle the drift read addresses this resource's fleet by: the ARN CodeBuild minted,
/// which is this type's physical id once its create went through, and otherwise the name the
/// fleet would be created under — `BatchGetFleets` takes either ("the names or ARNs of the
/// compute fleets"), so a record whose create never answered an identity is still read by the
/// name it was asked for rather than not read at all.
fn codebuild_fleet_handle(resource: &StoredResource) -> String {
    if resource.physical_resource_id.is_empty() {
        return declared_name(resource);
    }
    resource.physical_resource_id.clone()
}
