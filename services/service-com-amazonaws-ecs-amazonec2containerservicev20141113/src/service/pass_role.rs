// SPDX-License-Identifier: CC0-1.0

//! Whether the caller may HAND Amazon ECS the IAM roles its request names — the decision
//! taken beside this service's own action gate, before a handler runs.
//!
//! # The rule
//!
//! A request that names a role for Amazon ECS to act under hands that role over:
//!
//! * the task EXECUTION role, which the container agent acts as ("the task execution role
//!   that grants the Amazon ECS container agent permission to make Amazon Web Services API
//!   calls on your behalf", `RegisterTaskDefinition$executionRoleArn`);
//! * the TASK role its containers assume ("the role that containers in this task can assume",
//!   `TaskOverride$taskRoleArn`);
//! * the role a service lets Amazon ECS call a load balancer with ("the name or full Amazon
//!   Resource Name (ARN) of the IAM role that allows Amazon ECS to make calls to your load
//!   balancer on your behalf", `CreateService$role`);
//! * and every INFRASTRUCTURE role Amazon ECS manages a resource of the account with — "the
//!   Amazon ECS infrastructure IAM role that is used to manage your Amazon Web Services
//!   infrastructure": a Managed Instances provider's capacity
//!   (`CreateManagedInstancesProviderConfiguration$infrastructureRoleArn`), a gateway's load
//!   balancers (`CreateGatewayRequest$infrastructureRoleArn`), the Amazon EBS volume a launch
//!   configures (`ServiceManagedEBSVolumeConfiguration$roleArn`,
//!   `TaskManagedEBSVolumeConfiguration$roleArn`) and the VPC Lattice target group a service
//!   binds (`VpcLatticeConfiguration$roleArn`).
//!
//! Handing a role to a service is its own IAM action, `iam:PassRole`, granted on the role
//! being handed over — so a caller whose policies allow this service's action but not that one
//! is refused, and what it asked for is not created.
//!
//! The refusal is the same `AccessDeniedException` this service answers an action no policy
//! allows with, worded by the same shared sentence
//! ([`super::authorization::authorize_action`]): the principal, `iam:PassRole`, the role the
//! request named and which of IAM's rules withheld the grant. The model declares that error
//! on each of these operations (`@httpError 403`,
//! `smithy_query(query="operation",operation="RegisterTaskDefinition")`), which is the status
//! it is answered with.
//!
//! # Which roles, and whose decision
//!
//! The roles come from the REQUEST ([`passed_roles`]) rather than from a stored record, for
//! the reason the action gate beside it reads its resource from the request: a refused call
//! must not tell an unauthorized caller whether what it named exists. Each reference is
//! resolved to the role's ARN through this crate's ONE reading of a role reference
//! ([`crate::state::requested_role_arn`]), which is the resource a policy statement about that
//! role names. They are judged in the order Amazon ECS acts under them — the role the resource
//! itself is managed as, then the execution role, then the task role — so a request naming
//! several roles its caller may pass none of is refused on the first of them, and at most
//! [`MAX_PASSED_ROLES`] distinct roles are judged for one request.
//!
//! The decision itself is IAM's, taken through the one evaluation this service takes every
//! authorization decision through ([`super::authorization`]), so a caller the action gate does
//! not bound (the account's own published credential, a principal carrying a grant this
//! emulator cannot read) is not bounded here either.

use ::aws_smithy_runtime_api::http::Request as RtRequest;
use ::smithy_server_runtime::ServerHttpProtocol;

use crate::ops::types::*;
use crate::service::read_input;

/// The IAM action that hands one of the caller's roles to a service, as the IAM Service
/// Authorization Reference names it — granted on the role being handed over.
pub const PASS_ROLE_ACTION: &str = "iam:PassRole";

/// How many DISTINCT roles ONE request has judged.
///
/// A bound on the work one call costs rather than an Amazon ECS quota: a request hands over a
/// handful of roles — the two a task runs under, the one Amazon ECS manages the resource with,
/// the one a volume is created with, and one per VPC Lattice target group a service binds — so
/// the first this many DISTINCT roles a request names are judged and no more. The figure is
/// this service's rather than one a request chose, which is what keeps the decisions one call
/// takes independent of the length of a list a caller sent, exactly as the registrations'
/// `MAX_GUARDING_GROUPS` holds the groups a placement declares. A role named twice is one
/// decision, and a refusal among the roles judged refuses the call.
pub const MAX_PASSED_ROLES: usize = 16;

/// The roles the request `operation` hands Amazon ECS, as the ARNs IAM judges `iam:PassRole`
/// against and in the order Amazon ECS acts under them — empty for an operation whose request
/// names none, and for a body that cannot be read at all (which is the framework's own
/// refusal, not this layer's question).
///
/// Every value is read out of the request through the SAME protocol and generated input schema
/// the dispatch sequence will hand the handler ([`read_input`]), so this layer cannot judge a
/// different role from the one the operation would act under.
pub fn passed_roles(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Vec<String> {
    resolved(references_of(operation, protocol, req))
}

/// The role REFERENCES the request `operation` carries, in the order Amazon ECS acts under
/// them, exactly as the request spelled them.
fn references_of(
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Vec<Option<String>> {
    // The roles a LAUNCH hands over: the two its `overrides` may name — the execution role
    // first, because Amazon ECS acts as it before the containers are given the task role —
    // and the one the Amazon EBS volume it configures at launch is created with.
    let launched = |overrides: Option<TaskOverride>,
                    volumes: Option<Vec<TaskVolumeConfiguration>>| {
        let (execution, task) = overrides.map_or((None, None), |overrides| {
            (overrides.execution_role_arn, overrides.task_role_arn)
        });
        let mut references = vec![execution, task];
        references.extend(
            configured_volumes(volumes)
                .filter_map(|volume| volume.managed_ebs_volume)
                .map(|volume| Some(volume.role_arn)),
        );
        references
    };
    match operation {
        // The revision being registered declares the roles every task of it runs under, so
        // the registration is where they are handed over — for a task definition of the
        // account's own and for a daemon's alike.
        "RegisterTaskDefinition" => read_input::<RegisterTaskDefinitionInput>(
            protocol,
            REGISTER_TASK_DEFINITION_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![input.execution_role_arn, input.task_role_arn]
        }),
        "RegisterDaemonTaskDefinition" => read_input::<RegisterDaemonTaskDefinitionInput>(
            protocol,
            REGISTER_DAEMON_TASK_DEFINITION_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![input.execution_role_arn, input.task_role_arn]
        }),
        // A launch hands over the roles its own `overrides` name and the one its volume
        // configuration names; the ones the stored revision declares were handed over by the
        // registration that wrote them.
        "RunTask" => read_input::<RunTaskInput>(protocol, RUN_TASK_INPUT_SCHEMA, req)
            .map_or_else(Vec::new, |input| {
                launched(input.overrides, input.volume_configurations)
            }),
        "StartTask" => read_input::<StartTaskInput>(protocol, START_TASK_INPUT_SCHEMA, req)
            .map_or_else(Vec::new, |input| {
                launched(input.overrides, input.volume_configurations)
            }),
        // A service hands over the role Amazon ECS reaches its load balancer with, and the
        // infrastructure roles its volume and VPC Lattice configurations name.
        "CreateService" => {
            read_input::<CreateServiceInput>(protocol, CREATE_SERVICE_INPUT_SCHEMA, req)
                .map_or_else(Vec::new, |input| {
                    let mut references = vec![input.role];
                    references.extend(service_infrastructure_roles(
                        input.volume_configurations,
                        input.vpc_lattice_configurations,
                    ));
                    references
                })
        }
        // An update hands over the same infrastructure roles for the configurations it
        // replaces; it names no load-balancer role of its own.
        "UpdateService" => {
            read_input::<UpdateServiceInput>(protocol, UPDATE_SERVICE_INPUT_SCHEMA, req)
                .map_or_else(Vec::new, |input| {
                    service_infrastructure_roles(
                        input.volume_configurations,
                        input.vpc_lattice_configurations,
                    )
                })
        }
        // The role Amazon ECS manages a Managed Instances provider's capacity as.
        "CreateCapacityProvider" => read_input::<CreateCapacityProviderInput>(
            protocol,
            CREATE_CAPACITY_PROVIDER_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![input
                .managed_instances_provider
                .map(|provider| provider.infrastructure_role_arn)]
        }),
        "UpdateCapacityProvider" => read_input::<UpdateCapacityProviderInput>(
            protocol,
            UPDATE_CAPACITY_PROVIDER_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![input
                .managed_instances_provider
                .map(|provider| provider.infrastructure_role_arn)]
        }),
        // The role Amazon ECS manages a gateway's infrastructure as.
        "CreateGateway" => {
            read_input::<CreateGatewayInput>(protocol, CREATE_GATEWAY_INPUT_SCHEMA, req)
                .map_or_else(Vec::new, |input| vec![Some(input.infrastructure_role_arn)])
        }
        "UpdateGateway" => {
            read_input::<UpdateGatewayInput>(protocol, UPDATE_GATEWAY_INPUT_SCHEMA, req)
                .map_or_else(Vec::new, |input| vec![input.infrastructure_role_arn])
        }
        // A gateway service hands over all three: the infrastructure role Amazon ECS manages
        // it as, and the execution and task roles its own task runs under.
        "CreateExpressGatewayService" => read_input::<CreateExpressGatewayServiceInput>(
            protocol,
            CREATE_EXPRESS_GATEWAY_SERVICE_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![
                Some(input.infrastructure_role_arn),
                input.execution_role_arn,
                input.task_role_arn,
            ]
        }),
        "UpdateExpressGatewayService" => read_input::<UpdateExpressGatewayServiceInput>(
            protocol,
            UPDATE_EXPRESS_GATEWAY_SERVICE_INPUT_SCHEMA,
            req,
        )
        .map_or_else(Vec::new, |input| {
            vec![input.execution_role_arn, input.task_role_arn]
        }),
        // Every other operation of this service names no role for Amazon ECS to act under.
        _ => Vec::new(),
    }
}

/// The infrastructure roles a SERVICE's own configurations name: the one the Amazon EBS volume
/// it configures at launch is created with, then the one it binds each VPC Lattice target
/// group with.
fn service_infrastructure_roles(
    volumes: Option<Vec<ServiceVolumeConfiguration>>,
    lattice: Option<Vec<VpcLatticeConfiguration>>,
) -> Vec<Option<String>> {
    configured_volumes(volumes)
        .filter_map(|volume| volume.managed_ebs_volume)
        .map(|volume| Some(volume.role_arn))
        .chain(
            lattice
                .unwrap_or_default()
                .into_iter()
                .take(MAX_PASSED_ROLES)
                .map(|configuration| Some(configuration.role_arn)),
        )
        .collect()
}

/// The volume configurations of a request the gate reads: the first
/// [`crate::state::MAX_VOLUME_CONFIGURATIONS`] of them, because "the volume configuration may
/// contain multiple volumes but only one volume configured at launch is supported" — a longer
/// list configures volumes no revision could hold and is refused by the operation itself.
fn configured_volumes<Volume>(
    volumes: Option<Vec<Volume>>,
) -> impl Iterator<Item = Volume> {
    volumes
        .unwrap_or_default()
        .into_iter()
        .take(crate::state::MAX_VOLUME_CONFIGURATIONS)
}

/// The distinct role ARNs `references` name, in the order they were named and at most
/// [`MAX_PASSED_ROLES`] of them.
///
/// Each reference is resolved through this crate's one reading of a role reference
/// ([`crate::state::requested_role_arn`]), which answers the ARN a value already spelling one
/// names and the ARN of the role of that name in this account otherwise, and nothing for a
/// member that is absent or spells nothing but whitespace — so a member naming no role hands
/// nothing over.
fn resolved(references: Vec<Option<String>>) -> Vec<String> {
    let mut roles: Vec<String> = Vec::new();
    for reference in references {
        if roles.len() >= MAX_PASSED_ROLES {
            break;
        }
        let Some(role) = crate::state::requested_role_arn(reference.as_deref()) else {
            continue;
        };
        if !roles.iter().any(|held| held == &role) {
            roles.push(role);
        }
    }
    roles
}

/// The refusal this request earns for a role its caller may not hand to Amazon ECS, or
/// `None` when IAM permits every role it names (and for every operation and caller the
/// decision does not bound — see the module docs).
///
/// Taken before anything the request names is resolved or written, so a refused call leaves
/// the account exactly as it was.
pub fn pass_role_refusal<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    operation: &str,
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Option<AccessDeniedException> {
    passed_roles(operation, protocol, req)
        .into_iter()
        .find_map(|role| {
            super::authorization::authorize_action(engine, ctx, PASS_ROLE_ACTION, &role).err()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the resource `iam:PassRole` is judged against is the ARN of the role the request
    /// handed over — the ARN itself when the member carries one, and the ARN of the role of
    /// that name in this account when it carries a name — and a member naming no role hands
    /// nothing over.
    #[test]
    fn a_passed_role_is_judged_on_the_roles_own_arn() {
        let account = ::engine::emulator_account_id();
        // (case, what the request carried, the resource IAM is asked about)
        let rows: &[(&str, Option<&str>, Option<String>)] = &[
            (
                "a full role ARN",
                Some("arn:aws:iam::000000000000:role/execution"),
                Some(format!("arn:aws:iam::{account}:role/execution")),
            ),
            (
                "a role ARN carrying a path",
                Some("arn:aws:iam::000000000000:role/team/execution"),
                Some(format!("arn:aws:iam::{account}:role/team/execution")),
            ),
            (
                "a bare role name",
                Some("ecsLoadBalancerRole"),
                Some(format!("arn:aws:iam::{account}:role/ecsLoadBalancerRole")),
            ),
            (
                "a role name prefixed with its path",
                Some("/foo/bar"),
                Some(format!("arn:aws:iam::{account}:role/foo/bar")),
            ),
            ("a member the request left out", None, None),
            ("an empty value", Some(""), None),
            ("a value spelling nothing but whitespace", Some("   "), None),
        ];
        for (case, carried, expected) in rows {
            let judged = resolved(vec![carried.map(ToString::to_string)]);
            let expected: Vec<String> = expected.clone().into_iter().collect();
            assert_eq!(
                judged, expected,
                "case {case:?}: expected {expected:?}, got {judged:?}"
            );
        }
    }

    /// RULE: one request has at most [`MAX_PASSED_ROLES`] DISTINCT roles judged, and a role
    /// named twice is one decision — so the work one call costs is this service's figure
    /// rather than the length of a list a caller sent.
    #[test]
    fn one_request_has_its_distinct_roles_judged_once_and_bounded() {
        let repeated = vec![Some("one".to_string()), Some("one".to_string())];
        let judged = resolved(repeated);
        assert_eq!(
            judged.len(),
            1,
            "case \"a role named twice\": expected one decision, got {judged:?}"
        );
        let many: Vec<Option<String>> = (0..MAX_PASSED_ROLES * 3)
            .map(|index| Some(format!("role{index}")))
            .collect();
        let judged = resolved(many);
        assert_eq!(
            judged.len(),
            MAX_PASSED_ROLES,
            "case \"a list longer than the bound\": expected {MAX_PASSED_ROLES} roles, got \
             {judged:?}"
        );
    }
}
