// SPDX-License-Identifier: CC0-1.0

//! Which Elastic Load Balancing v2 TARGET GROUPS an Auto Scaling group may be attached to,
//! over the awsQuery wire.
//!
//! The rule under test: an Auto Scaling group registers its own Amazon EC2 instances, by
//! instance id, and only a target group whose `TargetType` is `instance` takes a target
//! stated that way — so an attach naming a target group of ANY other target type is refused
//! whole (HTTP 400, `Sender`/`ValidationError`, the sentence that names the offending target
//! group) and attaches nothing, while one naming an `instance` target group is served and
//! reported. Both ways of stating the attachment are held to it: the target-group ARNs of
//! `AttachLoadBalancerTargetGroups` and the `elbv2` traffic sources of
//! `AttachTrafficSources`, whether or not the request states the traffic source's type.
//!
//! Each table spans what the rule generalises over — every target type the other service's
//! own catalog holds, both attach operations, and where in the list the offending entry sits
//! — rather than one recorded row; every expected sentence is written out here rather than
//! taken from the crate, and every assertion names its case and the value actually reported.

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_target_group, elbv2_query, emulator, list_members, p, query,
    seed_launch_templates, seed_subnets, tag_value, vpc_of, xml_unescape, SeededSubnet,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// Every OTHER value of that catalog, which an Auto Scaling group's instances cannot be
/// registered under.
///
/// SOURCE: Elastic Load Balancing v2's own `TargetTypeEnum`
/// (`smithy_query(query="shape",shape="TargetTypeEnum")` of that service's model) holds
/// `instance`, `ip`, `lambda` and `alb`; these are its members apart from the one above.
const OTHER_TARGET_TYPES: [&str; 3] = ["ip", "lambda", "alb"];

/// The protocol a target group of `target_type` is created with: an Application Load Balancer
/// target "terminates the application protocol itself", so the target group in front of it
/// carries TCP, and every other target type here takes an HTTP target group.
fn protocol_for(target_type: &str) -> &'static str {
    if target_type == "alb" {
        "TCP"
    } else {
        "HTTP"
    }
}

/// The port every target group of this suite listens on.
const TARGET_GROUP_PORT: i32 = 80;

/// The launch template every group here launches from.
const LAUNCH_TEMPLATE: &str = "lt-target-type";

/// The wire error code and error type a refused attach carries.
const VALIDATION_ERROR: &str = "ValidationError";
const SENDER: &str = "Sender";

/// The whole message an attach naming a target group of another target type is refused with:
/// fixed text around one variable part, the offending target group as the request wrote it.
///
/// Written out here rather than taken from the crate, so a wrong sentence in the handler
/// cannot make a row pass.
fn invalid_target_type_message(target_group: &str) -> String {
    format!(
        "Provided Target Group '{target_group}' has invalid target type. Please ensure all \
         provided Target Groups have target type of instance."
    )
}

/// Create one target group of `target_type` over Elastic Load Balancing v2's own wire and
/// answer the ARN that service minted for it.
async fn target_group_of(
    router: &MultiServiceRouter,
    name: &str,
    target_type: &str,
    vpc_id: &str,
) -> String {
    if protocol_for(target_type) == "HTTP" {
        return create_target_group(router, name, TARGET_GROUP_PORT, target_type, vpc_id).await;
    }
    let (status, body) = elbv2_query(
        router,
        "CreateTargetGroup",
        &[
            p("Name", name),
            p("Protocol", protocol_for(target_type)),
            p("Port", TARGET_GROUP_PORT.to_string()),
            p("VpcId", vpc_id),
            p("TargetType", target_type),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the {target_type:?} target group {name:?} must succeed, got {status} with \
         {body:?}"
    );
    let arn = tag_value(&body, "TargetGroupArn");
    assert!(
        !arn.is_empty(),
        "creating the {target_type:?} target group {name:?} must report its ARN, got {body}"
    );
    arn
}

/// Create one Auto Scaling group with no capacity, placed in `subnet`.
async fn given_group(router: &MultiServiceRouter, name: &str, subnet: &SeededSubnet) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("VPCZoneIdentifier", subnet.id.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "creating {name:?} must succeed, got {status} with {body:?}"
    );
}

/// How an attach STATES the target groups it names — the two operations that add the same
/// association, and the two ways a traffic source may be typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachedAs {
    /// `AttachLoadBalancerTargetGroups`, under `TargetGroupARNs`.
    TargetGroupArns,
    /// `AttachTrafficSources`, each identifier typed `elbv2`.
    TrafficSourcesTyped,
    /// `AttachTrafficSources`, with the type left out — "required if the identifier is the
    /// name of a Classic Load Balancer", so an ARN may leave it out.
    TrafficSourcesUntyped,
}

/// Post one attach of `arns` to `group`, stated as `form`, and answer its status, wire error
/// code, error type and message.
async fn attach(
    router: &MultiServiceRouter,
    group: &str,
    arns: &[String],
    form: AttachedAs,
) -> (u16, String, String, String) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    let action = match form {
        AttachedAs::TargetGroupArns => {
            for (index, arn) in arns.iter().enumerate() {
                params.push(p(
                    &format!("TargetGroupARNs.member.{}", index + 1),
                    arn.clone(),
                ));
            }
            "AttachLoadBalancerTargetGroups"
        }
        AttachedAs::TrafficSourcesTyped | AttachedAs::TrafficSourcesUntyped => {
            for (index, arn) in arns.iter().enumerate() {
                let prefix = format!("TrafficSources.member.{}", index + 1);
                params.push(p(&format!("{prefix}.Identifier"), arn.clone()));
                if form == AttachedAs::TrafficSourcesTyped {
                    params.push(p(&format!("{prefix}.Type"), "elbv2"));
                }
            }
            "AttachTrafficSources"
        }
    };
    let (status, body) = query(router, action, &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        tag_value(&body, "Type"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// What the group reports it is attached to: the target groups under its own
/// `TargetGroupARNs`, and the identifiers under its `TrafficSources`, each in the order it
/// holds them.
async fn reported_attachments(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> (Vec<String>, Vec<String>) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the read-back of {group:?} must succeed, got {status} with {body:?}"
    );
    let target_groups = list_members(&body, "TargetGroupARNs")
        .into_iter()
        .map(str::to_string)
        .collect();
    let identifiers = list_members(&body, "TrafficSources")
        .into_iter()
        .map(|member| tag_value(member, "Identifier"))
        .collect();
    (target_groups, identifiers)
}

/// Every form an attach may be stated in, which each table below spans.
const FORMS: [AttachedAs; 3] = [
    AttachedAs::TargetGroupArns,
    AttachedAs::TrafficSourcesTyped,
    AttachedAs::TrafficSourcesUntyped,
];

/// RULE: an attach is served for an `instance` target group and refused, whole, for a target
/// group of every other target type — through either operation that adds the association, and
/// whether or not the traffic source states its type.
#[tokio::test]
async fn an_attach_admits_an_instance_target_group_and_refuses_every_other_target_type() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);

    // Each row: the target type the attach names a target group of, and whether the attach
    // must be served. The catalog is the other service's own, with its `instance` member
    // served and every other member refused.
    let mut rows: Vec<(&str, bool)> = vec![(TARGET_TYPE_INSTANCE, true)];
    rows.extend(OTHER_TARGET_TYPES.map(|target_type| (target_type, false)));

    for (row, (target_type, served)) in rows.into_iter().enumerate() {
        let arn = target_group_of(&router, &format!("tg-type-{row}"), target_type, &vpc_id).await;
        for (column, form) in FORMS.into_iter().enumerate() {
            let case = format!("a {target_type:?} target group attached as {form:?}");
            let group = format!("asg-type-{row}-{column}");
            given_group(&router, &group, &subnets[0]).await;

            let (status, code, kind, message) =
                attach(&router, &group, std::slice::from_ref(&arn), form).await;
            let (target_groups, identifiers) = reported_attachments(&router, &case, &group).await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the attach to be served, got {status} {code:?} \
                     {message:?}"
                );
                // The association is the group's own, reported under the member the attach
                // stated it in: a served attach is one a later read can see.
                let reported = match form {
                    AttachedAs::TargetGroupArns => &target_groups,
                    _ => &identifiers,
                };
                assert_eq!(
                    reported,
                    &vec![arn.clone()],
                    "case {case:?}: expected the group to report {arn:?}, got target groups \
                     {target_groups:?} and traffic sources {identifiers:?}"
                );
                continue;
            }
            assert_eq!(
                (status, code.as_str(), kind.as_str(), message.as_str()),
                (
                    400,
                    VALIDATION_ERROR,
                    SENDER,
                    invalid_target_type_message(&arn).as_str()
                ),
                "case {case:?}: expected the attach to be refused for the target type, got \
                 {status} {code:?} {kind:?} {message:?}"
            );
            assert_eq!(
                (target_groups.as_slice(), identifiers.as_slice()),
                ([].as_slice(), [].as_slice()),
                "case {case:?}: expected a refused attach to attach nothing, got target groups \
                 {target_groups:?} and traffic sources {identifiers:?}"
            );
        }
    }
}

/// RULE, continued: the refusal is for the request as a WHOLE — an attach that names a target
/// group of another target type beside ones it may hold attaches NEITHER, wherever in the
/// list the offending entry sits, and leaves the attachments the group already held exactly
/// as they were. The sentence names that entry, not the ones beside it.
#[tokio::test]
async fn one_target_group_of_another_target_type_refuses_the_whole_attach() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    seed_launch_templates(&engine, &[LAUNCH_TEMPLATE]);

    // Three target groups the group may hold, and one of each other target type that it may
    // not.
    let mut admitted: Vec<String> = Vec::new();
    for index in 0..3 {
        admitted.push(
            target_group_of(
                &router,
                &format!("tg-mixed-ok-{index}"),
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }

    for (row, target_type) in OTHER_TARGET_TYPES.into_iter().enumerate() {
        let refused =
            target_group_of(&router, &format!("tg-mixed-{row}"), target_type, &vpc_id).await;
        // Each row: where the offending entry sits among entries the group may hold.
        let orders: Vec<(&str, Vec<String>)> = vec![
            ("alone", vec![refused.clone()]),
            ("last", vec![admitted[1].clone(), refused.clone()]),
            ("first", vec![refused.clone(), admitted[1].clone()]),
            (
                "between two the group may hold",
                vec![
                    admitted[1].clone(),
                    refused.clone(),
                    admitted[2].clone(),
                ],
            ),
            (
                "named twice among two the group may hold",
                vec![
                    refused.clone(),
                    admitted[1].clone(),
                    refused.clone(),
                    admitted[2].clone(),
                ],
            ),
        ];
        for (column, (order, requested)) in orders.into_iter().enumerate() {
            for (depth, form) in FORMS.into_iter().enumerate() {
                let case =
                    format!("a {target_type:?} target group {order} in an attach of {form:?}");
                let group = format!("asg-mixed-{row}-{column}-{depth}");
                given_group(&router, &group, &subnets[0]).await;
                // One attachment the group already holds, so what the refused attach left
                // behind is visible as "exactly what it held", not as "nothing at all".
                let (status, code, _, message) =
                    attach(&router, &group, std::slice::from_ref(&admitted[0]), form).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the group's first attachment must be served, got {status} \
                     {code:?} {message:?}"
                );

                let (status, code, kind, message) =
                    attach(&router, &group, &requested, form).await;
                assert_eq!(
                    (status, code.as_str(), kind.as_str(), message.as_str()),
                    (
                        400,
                        VALIDATION_ERROR,
                        SENDER,
                        invalid_target_type_message(&refused).as_str()
                    ),
                    "case {case:?}: expected the whole attach to be refused and to name \
                     {refused:?}, got {status} {code:?} {kind:?} {message:?}"
                );
                let (target_groups, identifiers) =
                    reported_attachments(&router, &case, &group).await;
                let held = match form {
                    AttachedAs::TargetGroupArns => &target_groups,
                    _ => &identifiers,
                };
                assert_eq!(
                    held,
                    &vec![admitted[0].clone()],
                    "case {case:?}: expected the group to hold only what it held before the \
                     refused attach, got target groups {target_groups:?} and traffic sources \
                     {identifiers:?}"
                );
            }
        }
    }
}
