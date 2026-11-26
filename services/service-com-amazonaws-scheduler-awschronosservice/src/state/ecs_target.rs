// SPDX-License-Identifier: CC0-1.0

//! The templated ECS target: the rule its parameters have to satisfy ACROSS members
//! when a schedule is written.
//!
//! `Target$EcsParameters` is "the templated target type for the Amazon ECS `RunTask`
//! API operation" (`smithy_query(query="shape",
//! shape="com.amazonaws.scheduler#EcsParameters")`), and the block it carries is the
//! task this service runs at each occurrence. Each of its members is bounded by the
//! model on its own — the request boundary has already applied those bounds by the
//! time a handler runs — but one requirement the model states is about TWO members
//! together, so no boundary walk can apply it:
//!
//! > "Specifies whether the task's elastic network interface receives a public IP
//! > address. You can specify `ENABLED` only when `LaunchType` in `EcsParameters` is
//! > set to `FARGATE`." (`AwsVpcConfiguration$AssignPublicIp`)
//!
//! A public address is assigned by the launch type that provisions the network
//! interface, so asking for one under any other launch type asks for something
//! `RunTask` will not do: the schedule could never deliver, which is why the write is
//! refused immediately instead of at the first occurrence
//! ([`ecs_target_violation`]).
//!
//! One concern, shared by the two operations that WRITE a schedule: both carry
//! `Target$EcsParameters`, so an update cannot move a stored schedule onto a network
//! configuration a create would have refused.

use crate::ops::types::Target;

/// The `LaunchType` that provisions the task's own elastic network interface, and so
/// the one launch type a public address may be asked for under
/// (`AwsVpcConfiguration$AssignPublicIp`). One of the three the enum admits
/// (`smithy_query(query="shape", shape="com.amazonaws.scheduler#LaunchType")`:
/// `EC2 | FARGATE | EXTERNAL`).
const LAUNCH_TYPE_FARGATE: &str = "FARGATE";

/// The `AssignPublicIp` value that ASKS for a public address
/// (`smithy_query(query="shape", shape="com.amazonaws.scheduler#AssignPublicIp")`:
/// `ENABLED | DISABLED`).
const ASSIGN_PUBLIC_IP_ENABLED: &str = "ENABLED";

/// How this service states a public address asked for under a launch type that does
/// not assign one: it names the parameter and the launch type the request stated, and
/// nothing else — the member carries one value, so there is nothing of the value to
/// report.
fn assign_public_ip_unsupported(launch_type: &str) -> String {
    format!(
        "Parameter AssignPublicIp for the target is not supported when launch type is \
         {launch_type}"
    )
}

/// Refuse an ECS target that asks for a public IP address under a launch type that
/// assigns none; `None` for every other target.
///
/// The rule is the one `AssignPublicIp` states: the value `ENABLED` may stand only
/// beside the `FARGATE` launch type, so it is refused beside any OTHER launch type
/// the request states — the refusal naming that launch type. A request stating no
/// launch type states none for the refusal to be about and asks the task's own
/// cluster for one, so it is left to `RunTask`; `DISABLED`, an absent member and a
/// target carrying no ECS parameters at all ask for no public address and are subject
/// to nothing here.
pub(crate) fn ecs_target_violation(target: &Target) -> Option<String> {
    let parameters = target.ecs_parameters.as_ref()?;
    let configuration = parameters
        .network_configuration
        .as_ref()?
        .awsvpc_configuration
        .as_ref()?;
    if configuration.assign_public_ip.as_deref() != Some(ASSIGN_PUBLIC_IP_ENABLED) {
        return None;
    }
    let launch_type = parameters.launch_type.as_deref()?;
    (launch_type != LAUNCH_TYPE_FARGATE).then(|| assign_public_ip_unsupported(launch_type))
}

#[cfg(test)]
mod tests {
    //! The cross-member rule this module owns, over a SET of ECS targets rather than
    //! the one recorded request. The same rule is driven through real requests, on
    //! both operations that write a schedule, in `tests/all/ecs_target_network.rs`.

    use super::*;
    use crate::ops::types::{AwsVpcConfiguration, EcsParameters, NetworkConfiguration};

    /// A target whose ECS parameters state `launch_type` and `assign_public_ip`, each
    /// where a row states one.
    fn ecs_target(launch_type: Option<&str>, assign_public_ip: Option<&str>) -> Target {
        Target {
            arn: "arn:aws:ecs:eu-west-2:000000000000:cluster/c".to_string(),
            role_arn: "arn:aws:iam::000000000000:role/r".to_string(),
            ecs_parameters: Some(EcsParameters {
                task_definition_arn: "arn:aws:ecs:eu-west-2:000000000000:task-definition/t:1"
                    .to_string(),
                launch_type: launch_type.map(ToString::to_string),
                network_configuration: Some(NetworkConfiguration {
                    awsvpc_configuration: Some(AwsVpcConfiguration {
                        subnets: vec!["subnet-0a1b2c3d".to_string()],
                        security_groups: None,
                        assign_public_ip: assign_public_ip.map(ToString::to_string),
                    }),
                }),
                ..EcsParameters::default()
            }),
            ..Target::default()
        }
    }

    /// A public address may be asked for only under the launch type that assigns one:
    /// the rows differ along the launch type stated (every value the enum admits, and
    /// none at all) and along what the configuration asks for.
    #[test]
    fn a_public_address_is_asked_for_only_under_the_launch_type_that_assigns_one() {
        // (case, the target, the launch type the refusal names)
        let rows: [(&str, Target, Option<&str>); 8] = [
            (
                "a public address under the launch type that assigns one",
                ecs_target(Some("FARGATE"), Some("ENABLED")),
                None,
            ),
            (
                "a public address under instances of the account's own",
                ecs_target(Some("EC2"), Some("ENABLED")),
                Some("EC2"),
            ),
            (
                "a public address under external instances",
                ecs_target(Some("EXTERNAL"), Some("ENABLED")),
                Some("EXTERNAL"),
            ),
            (
                "no public address under instances of the account's own",
                ecs_target(Some("EC2"), Some("DISABLED")),
                None,
            ),
            (
                "the member left out under instances of the account's own",
                ecs_target(Some("EC2"), None),
                None,
            ),
            (
                "a public address with no launch type stated",
                ecs_target(None, Some("ENABLED")),
                None,
            ),
            (
                "an ECS target with no network configuration",
                Target {
                    ecs_parameters: Some(EcsParameters {
                        task_definition_arn: "arn:aws:ecs:eu-west-2:000000000000:task-definition/t:1"
                            .to_string(),
                        launch_type: Some("EC2".to_string()),
                        ..EcsParameters::default()
                    }),
                    ..ecs_target(Some("EC2"), Some("ENABLED"))
                },
                None,
            ),
            (
                "a target of another kind entirely",
                Target {
                    arn: "arn:aws:sqs:eu-west-2:000000000000:q".to_string(),
                    role_arn: "arn:aws:iam::000000000000:role/r".to_string(),
                    ..Target::default()
                },
                None,
            ),
        ];

        for (case, target, launch_type) in rows {
            let violation = ecs_target_violation(&target);
            let expected = launch_type.map(assign_public_ip_unsupported);
            assert_eq!(
                violation, expected,
                "case {case:?}: expected {expected:?}, got {violation:?}"
            );
        }
    }
}
