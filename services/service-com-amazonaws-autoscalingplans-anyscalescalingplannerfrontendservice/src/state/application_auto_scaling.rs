// SPDX-License-Identifier: CC0-1.0

//! The target tracking scaling policies a scaling plan INSTALLS, in the service that owns
//! them.
//!
//! `ScalingInstruction` is documented as "AWS Auto Scaling creates target tracking scaling
//! policies based on the scaling instructions"
//! (`smithy_query(query="shape",shape="ScalingInstruction")`), and a target tracking
//! scaling policy of an ECS service, a Spot Fleet request, an Aurora replica set or a
//! DynamoDB table is an Application Auto Scaling resource: that is the service whose
//! `DescribeScalingPolicies` reports it, whose `PutScalingPolicy` a caller would otherwise
//! write it with, and whose scalable target carries the capacity window the plan sets. So
//! a policy this crate REPORTS for a plan's resource exists only once that service holds
//! it, and applying a plan means writing it there — registering the scalable target the
//! instruction's capacity bounds describe, then putting one policy per target tracking
//! configuration under the name the plan derives for it
//! ([`crate::state::derived_policy_name`]).
//!
//! Withdrawing a plan is the same statement read backwards: the registration and the
//! policies a resource carries are the plan's, so a resource the plan stops covering has
//! both taken away there — the policies deleted and the scalable target deregistered —
//! and that service then reports the resource as one nothing scales. A plan is treated as
//! the OWNER of the registration of every resource it covers, which is what deleting a
//! plan is documented to do ("Deleting a scaling plan deletes the underlying
//! ScalingInstruction for all of the scalable resources that are covered by the plan"):
//! the withdrawal does not ask how the registration came to be there, so a resource a
//! caller had registered itself and then brought under a plan loses that registration
//! with the plan.
//!
//! The writes are made by CALLING Application Auto Scaling, in process over the callback
//! transport (`::smithy_server_runtime::EmbeddedEmulator`), signed the way every
//! in-process caller signs (`::service_common::in_process_sigv4`) — so the policies are
//! that service's own records, answered by its own reads, and the two services can no more
//! disagree about them than two readers of one store can. Nothing here opens a socket or
//! leaves the host: the connector dispatches the request back into this same emulator.
//!
//! An engine that does not hold Application Auto Scaling answers such a call with the
//! router's own `UnknownService`, and that service answers its own refusal for a resource
//! it cannot carry a target for (a table that does not exist, a capacity window it
//! refuses). Neither is this operation's answer to give: a plan is stored and reported on
//! its own terms whatever another service says about the resources it names, so a call
//! that was refused is left to the issue that owns the plan's own failure statuses.
//!
//! The namespace decides WHICH service owns the policy: Application Auto Scaling's own
//! `ServiceNamespace` lists `ecs`, `ec2`, `rds` and `dynamodb` among the namespaces a
//! scaling instruction can name, and does NOT list `autoscaling`
//! (`smithy_query(query="shape",shape="ServiceNamespace",model_path="services/service-com-amazonaws-applicationautoscaling-anyscalefrontendservice/smithy-model/com-amazonaws-applicationautoscaling-anyscalefrontendservice.json")`)
//! — an Auto Scaling group's target tracking policy is an Amazon EC2 Auto Scaling
//! resource, so an instruction naming that namespace installs nothing here.
//!
//! The work one write path costs is bounded by what a plan may hold: one call per
//! resource the plan covers, plus one per target tracking configuration of that resource,
//! and the write paths refuse a plan past [`crate::state::MAX_SCALING_INSTRUCTIONS`]
//! resources or past the documented
//! [`crate::state::MAX_TARGET_TRACKING_CONFIGURATIONS`] configurations per resource before
//! anything is stored or called — so the PRODUCT the fan-out and the policy diff below
//! are driven by is capped, not just each factor.

use std::collections::BTreeSet;

use ::serde_json::{json, Value};

use crate::ops::types::{ScalingInstruction, TargetTrackingConfiguration};
use crate::state::{configures_dynamic_scaling, derived_policy_name, resource_identity};

/// Application Auto Scaling's signing name, which is what routes an in-process call to its
/// own handlers, and the service shape an `awsJson1_1` `X-Amz-Target` names an operation
/// under (that model's `sigv4Name` and service shape, `smithy_query(query="service_info")`).
const SIGNING_NAME: &str = "application-autoscaling";
/// See [`SIGNING_NAME`].
const TARGET_PREFIX: &str = "AnyScaleFrontendService";
/// The media type an `awsJson1_1` request and its answer carry.
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// The region an in-process call is made in when the request carries no credential scope,
/// which is the region that service stamps its own resources with.
const DEFAULT_REGION: &str = "us-east-1";

/// The only `PolicyType` a scaling plan installs, as Application Auto Scaling names it
/// (that model's `PolicyType` value list).
const TARGET_TRACKING_SCALING: &str = "TargetTrackingScaling";

/// The service namespaces of this service's scalable dimensions whose scaling policies
/// Application Auto Scaling owns — see this module's own documentation for where the list
/// comes from and why `autoscaling` is not in it.
const OWNED_NAMESPACES: &[&str] = &["dynamodb", "ecs", "ec2", "rds"];

/// The region to make this request's in-process calls in: the one in the caller's SigV4
/// credential scope, so a plan's policies are installed in the region the plan was written
/// in.
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map(|scope| scope.region)
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// What identifies one installed policy: the resource it is on, and its name.
type InstalledPolicy = (ResourceTriple, String);

/// The `(ServiceNamespace, ResourceId, ScalableDimension)` of one scalable resource, owned
/// so it outlives the instruction it was read from.
type ResourceTriple = (String, String, String);

/// Make Application Auto Scaling hold exactly the registrations and policies `current`
/// asks for, for the plan named `plan_name`, having held the ones `previous` asked for.
///
/// This is the one entry every write path of this service uses: a create has nothing
/// `previous`, an update has the instruction set it replaces, and a delete has nothing
/// `current`. Each resource `current` names gets its scalable target registered with the
/// capacity window its instruction states, and one policy per target tracking
/// configuration; a policy `previous` installed that `current` no longer asks for is
/// deleted, which is what makes a dropped configuration, a re-pointed metric and a dropped
/// resource stop reporting a policy that nothing is tracking any more.
///
/// The REGISTRATION is withdrawn the same way: a resource `previous` registered a scalable
/// target for that `current` no longer covers is DEREGISTERED, after its policies are
/// deleted — the plan is what registered it, so when the plan stops covering the resource
/// (an instruction dropped by an update, every instruction by a delete) nothing is left
/// managing its capacity, and Application Auto Scaling reports neither a target nor a
/// policy for it.
///
/// Answers nothing: a plan is created, updated and deleted on its own terms, and what
/// another service says about the resources it names is not this operation's answer to
/// give.
pub async fn install(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    plan_name: &str,
    previous: &[ScalingInstruction],
    current: &[ScalingInstruction],
) {
    // The policies asked for are INDEXED before the diff, so deciding whether one the
    // plan already installed is still asked for is a lookup rather than a scan of
    // everything asked for: both sets are the caller's to size (inside the bound the
    // write paths enforce), so their product must not drive the work.
    let wanted: BTreeSet<InstalledPolicy> = installed_policies(plan_name, current);
    for policy in installed_policies(plan_name, previous) {
        if !wanted.contains(&policy) {
            delete_scaling_policy(callback, region, &policy).await;
        }
    }
    // The registrations are diffed the same way, and withdrawn AFTER the policies of the
    // resources they are on: a target the plan no longer covers takes its whole
    // registration with it, so the resource stops being one Application Auto Scaling
    // manages rather than keeping the dead plan's capacity window.
    let kept: BTreeSet<ResourceTriple> = registered_targets(current);
    for target in registered_targets(previous) {
        if !kept.contains(&target) {
            deregister_scalable_target(callback, region, &target).await;
        }
    }
    for instruction in current.iter().filter(|instruction| owned(instruction)) {
        register_scalable_target(callback, region, instruction).await;
        if !configures_dynamic_scaling(instruction) {
            continue;
        }
        for (position, configuration) in instruction
            .target_tracking_configurations
            .iter()
            .enumerate()
        {
            let name = derived_policy_name(plan_name, instruction, configuration, position);
            put_scaling_policy(callback, region, instruction, configuration, &name).await;
        }
    }
}

/// Whether the scaling policies of the resource `instruction` applies to are Application
/// Auto Scaling's to hold.
fn owned(instruction: &ScalingInstruction) -> bool {
    OWNED_NAMESPACES.contains(&instruction.service_namespace.as_str())
}

/// Every policy a plan installs for `instructions`, as the resource it is on and the name
/// it carries.
///
/// A `BTreeSet`, so the diff in [`install`] is a lookup per policy and two instruction
/// sets naming one policy render it once.
fn installed_policies(
    plan_name: &str,
    instructions: &[ScalingInstruction],
) -> BTreeSet<InstalledPolicy> {
    instructions
        .iter()
        .filter(|instruction| owned(instruction) && configures_dynamic_scaling(instruction))
        .flat_map(|instruction| {
            let (namespace, resource_id, dimension) = resource_identity(instruction);
            let triple: ResourceTriple = (
                namespace.to_string(),
                resource_id.to_string(),
                dimension.to_string(),
            );
            instruction
                .target_tracking_configurations
                .iter()
                .enumerate()
                .map(move |(position, configuration)| {
                    (
                        triple.clone(),
                        derived_policy_name(plan_name, instruction, configuration, position),
                    )
                })
                .collect::<Vec<InstalledPolicy>>()
        })
        .collect()
}

/// Every scalable target a plan registers for `instructions`, as the resource it is on.
///
/// One per resource an instruction of an owned namespace covers, whether or not that
/// instruction configures dynamic scaling — which is exactly what [`install`] registers,
/// so the diff withdraws precisely what a previous instruction set put there.
fn registered_targets(instructions: &[ScalingInstruction]) -> BTreeSet<ResourceTriple> {
    instructions
        .iter()
        .filter(|instruction| owned(instruction))
        .map(|instruction| {
            let (namespace, resource_id, dimension) = resource_identity(instruction);
            (
                namespace.to_string(),
                resource_id.to_string(),
                dimension.to_string(),
            )
        })
        .collect()
}

/// Register the scalable target an instruction's capacity bounds describe.
///
/// A target tracking policy moves the capacity of a SCALABLE TARGET, so the window the
/// instruction states — its `MinCapacity` and `MaxCapacity`, "the minimum / maximum
/// capacity" of the resource the plan scales — is what the plan registers before it puts a
/// policy on that resource.
async fn register_scalable_target(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    instruction: &ScalingInstruction,
) {
    let (namespace, resource_id, dimension) = resource_identity(instruction);
    let request = json!({
        "ServiceNamespace": namespace,
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
        "MinCapacity": instruction.min_capacity,
        "MaxCapacity": instruction.max_capacity,
    });
    call(callback, region, "RegisterScalableTarget", &request).await;
}

/// Put the target tracking scaling policy one configuration of an instruction describes.
///
/// The configuration is carried over member for member: both services word a target
/// tracking configuration with the same members (the target value, the predefined or
/// customized metric that is tracked, the two cooldowns and whether scale-in is disabled),
/// and the one member this service's configuration carries that Application Auto Scaling's
/// does not — `EstimatedInstanceWarmup`, an Amazon EC2 Auto Scaling member — is not sent.
async fn put_scaling_policy(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    instruction: &ScalingInstruction,
    configuration: &TargetTrackingConfiguration,
    policy_name: &str,
) {
    let (namespace, resource_id, dimension) = resource_identity(instruction);
    let request = json!({
        "PolicyName": policy_name,
        "ServiceNamespace": namespace,
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
        "PolicyType": TARGET_TRACKING_SCALING,
        "TargetTrackingScalingPolicyConfiguration": tracking_configuration(configuration),
    });
    call(callback, region, "PutScalingPolicy", &request).await;
}

/// One target tracking configuration, as Application Auto Scaling's
/// `TargetTrackingScalingPolicyConfiguration` words it.
fn tracking_configuration(configuration: &TargetTrackingConfiguration) -> Value {
    let mut document = json!({ "TargetValue": configuration.target_value });
    if let Some(predefined) = &configuration.predefined_scaling_metric_specification {
        let mut specification =
            json!({ "PredefinedMetricType": predefined.predefined_scaling_metric_type });
        if let Some(label) = &predefined.resource_label {
            specification["ResourceLabel"] = json!(label);
        }
        document["PredefinedMetricSpecification"] = specification;
    }
    if let Some(customized) = &configuration.customized_scaling_metric_specification {
        let mut specification = json!({
            "MetricName": customized.metric_name,
            "Namespace": customized.namespace,
            "Statistic": customized.statistic,
        });
        if let Some(dimensions) = &customized.dimensions {
            specification["Dimensions"] = Value::Array(
                dimensions
                    .iter()
                    .map(|dimension| json!({ "Name": dimension.name, "Value": dimension.value }))
                    .collect(),
            );
        }
        if let Some(unit) = &customized.unit {
            specification["Unit"] = json!(unit);
        }
        document["CustomizedMetricSpecification"] = specification;
    }
    for (member, value) in [
        ("ScaleOutCooldown", configuration.scale_out_cooldown),
        ("ScaleInCooldown", configuration.scale_in_cooldown),
    ] {
        if let Some(value) = value {
            document[member] = json!(value);
        }
    }
    if let Some(disabled) = configuration.disable_scale_in {
        document["DisableScaleIn"] = json!(disabled);
    }
    document
}

/// Deregister the scalable target a plan registered for one resource.
///
/// Addressed by the identity it was registered under — the only members this operation
/// takes — so the resource stops carrying a registration, and with it the scaling policies
/// that hang off one.
async fn deregister_scalable_target(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    target: &ResourceTriple,
) {
    let (namespace, resource_id, dimension) = target;
    let request = json!({
        "ServiceNamespace": namespace,
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
    });
    call(callback, region, "DeregisterScalableTarget", &request).await;
}

/// Delete one policy a plan installed, on the resource it is on.
async fn delete_scaling_policy(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    policy: &InstalledPolicy,
) {
    let ((namespace, resource_id, dimension), policy_name) = policy;
    let request = json!({
        "PolicyName": policy_name,
        "ServiceNamespace": namespace,
        "ResourceId": resource_id,
        "ScalableDimension": dimension,
    });
    call(callback, region, "DeleteScalingPolicy", &request).await;
}

/// Call one Application Auto Scaling operation in process.
///
/// The request is the one that API takes — its operation in the `X-Amz-Target` header and
/// the input document as the body — signed as the identity the framework hands this
/// service and dispatched back into this same emulator. What it answered is not read: this
/// service reports a plan on its own terms, and a call another service refused is no part
/// of the plan's own answer.
async fn call(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    operation: &str,
    request: &Value,
) {
    let Ok(body) = ::serde_json::to_vec(request) else {
        return;
    };
    let built = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{SIGNING_NAME}.{region}.amazonaws.com"))
        .header("content-type", AWS_JSON_1_1)
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .body(body);
    let Ok(mut built) = built else {
        return;
    };
    if ::service_common::in_process_sigv4::sign_callback_request(
        &mut built,
        SIGNING_NAME,
        region,
        callback,
    )
    .await
    .is_err()
    {
        return;
    }
    let (parts, body) = built.into_parts();
    let request = ::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
    );
    let Ok(request) = ::aws_smithy_runtime_api::http::Request::try_from(request) else {
        return;
    };
    use ::aws_smithy_runtime_api::client::http::HttpConnector;
    let connector = callback.http_connector();
    let _ = HttpConnector::call(&connector, request).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target tracking configuration carrying every member both services word.
    fn configuration() -> TargetTrackingConfiguration {
        TargetTrackingConfiguration {
            predefined_scaling_metric_specification: None,
            customized_scaling_metric_specification: None,
            target_value: 50.0,
            disable_scale_in: Some(true),
            scale_out_cooldown: Some(60),
            scale_in_cooldown: Some(300),
            estimated_instance_warmup: Some(120),
        }
    }

    /// RULE: a configuration is carried over member for member, and the one member
    /// Application Auto Scaling does not word is not sent.
    #[test]
    fn a_configuration_carries_over_the_members_both_services_word() {
        let document = tracking_configuration(&configuration());
        for member in [
            "TargetValue",
            "DisableScaleIn",
            "ScaleOutCooldown",
            "ScaleInCooldown",
        ] {
            assert!(
                document.get(member).is_some(),
                "case {member:?}: expected the member to be carried over, got {document:?}"
            );
        }
        assert!(
            document.get("EstimatedInstanceWarmup").is_none(),
            "case EstimatedInstanceWarmup: the member is not one Application Auto Scaling \
             words, got {document:?}"
        );
    }

    /// One instruction over a resource of `namespace`, with dynamic scaling as `dynamic`
    /// says.
    fn instruction(namespace: &str, resource_id: &str, dynamic: bool) -> ScalingInstruction {
        ScalingInstruction {
            service_namespace: namespace.to_string(),
            resource_id: resource_id.to_string(),
            scalable_dimension: format!("{namespace}:resource:Capacity"),
            min_capacity: 1,
            max_capacity: 4,
            target_tracking_configurations: vec![configuration()],
            disable_dynamic_scaling: Some(!dynamic),
            ..ScalingInstruction::default()
        }
    }

    /// RULE: a plan registers a scalable target for every resource an instruction of a
    /// namespace Application Auto Scaling owns covers, whatever else that instruction
    /// states — so the set a previous instruction set is diffed against covers exactly
    /// those resources.
    #[test]
    fn the_registered_targets_are_the_resources_of_the_owned_namespaces() {
        // (case, instruction set, the resources a target is registered for)
        let rows: &[(&str, Vec<ScalingInstruction>, &[&str])] = &[
            ("nothing covered", vec![], &[]),
            (
                "one dynamically scaled resource",
                vec![instruction("dynamodb", "table/one", true)],
                &["table/one"],
            ),
            (
                "an instruction that configures no dynamic scaling still registers",
                vec![instruction("ecs", "service/cluster/one", false)],
                &["service/cluster/one"],
            ),
            (
                "two resources of two owned namespaces",
                vec![
                    instruction("rds", "cluster:one", true),
                    instruction("ec2", "spot-fleet-request/sfr-one", true),
                ],
                &["cluster:one", "spot-fleet-request/sfr-one"],
            ),
            (
                "a namespace whose policies this service does not own",
                vec![instruction("autoscaling", "group/one", true)],
                &[],
            ),
        ];
        for (case, instructions, expected) in rows {
            let registered = registered_targets(instructions);
            let mut resources: Vec<&str> = registered
                .iter()
                .map(|(_, resource_id, _)| resource_id.as_str())
                .collect();
            resources.sort_unstable();
            let mut wanted: Vec<&str> = expected.to_vec();
            wanted.sort_unstable();
            assert_eq!(
                resources, wanted,
                "case {case:?}: expected targets for {wanted:?}, got {registered:?}"
            );
        }
    }

    /// RULE: a resource the previous instruction set registered that the current one no
    /// longer covers is the one whose registration is withdrawn — every one of them when
    /// the current set is empty, as a delete makes it.
    #[test]
    fn a_resource_the_current_set_drops_is_the_one_withdrawn() {
        let previous = vec![
            instruction("dynamodb", "table/kept", true),
            instruction("dynamodb", "table/dropped", true),
        ];
        // (case, the set now in force, the resources whose registration is withdrawn)
        let rows: &[(&str, Vec<ScalingInstruction>, &[&str])] = &[
            (
                "every resource, as a delete leaves nothing in force",
                vec![],
                &["table/dropped", "table/kept"],
            ),
            (
                "only the resource the update dropped",
                vec![instruction("dynamodb", "table/kept", true)],
                &["table/dropped"],
            ),
            (
                "nothing, when the set in force still covers both",
                previous.clone(),
                &[],
            ),
        ];
        for (case, current, expected) in rows {
            let kept = registered_targets(current);
            let left = registered_targets(&previous);
            let mut withdrawn: Vec<&str> = left
                .iter()
                .filter(|target| !kept.contains(*target))
                .map(|(_, resource_id, _)| resource_id.as_str())
                .collect();
            withdrawn.sort_unstable();
            let mut wanted: Vec<&str> = expected.to_vec();
            wanted.sort_unstable();
            assert_eq!(
                withdrawn, wanted,
                "case {case:?}: expected the registrations of {wanted:?} to be withdrawn, \
                 got {withdrawn:?}"
            );
        }
    }

    /// RULE: a member the configuration does not carry is not sent as a value of its own.
    #[test]
    fn an_absent_member_is_not_sent() {
        let mut bare = configuration();
        bare.disable_scale_in = None;
        bare.scale_out_cooldown = None;
        bare.scale_in_cooldown = None;
        let document = tracking_configuration(&bare);
        for member in ["DisableScaleIn", "ScaleOutCooldown", "ScaleInCooldown"] {
            assert!(
                document.get(member).is_none(),
                "case {member:?}: an absent member must not be sent, got {document:?}"
            );
        }
    }
}
