// SPDX-License-Identifier: CC0-1.0

//! Which target group ARNs an Auto Scaling group may be associated with.
//!
//! The rule under test: a target group is identified within ONE Region and ONE account, and
//! the Auto Scaling group it serves is the one in the Region the request is addressed to, so
//! the only value `TargetGroupARNs` may carry is an Elastic Load Balancing target group ARN
//! (`arn:<partition>:elasticloadbalancing:<region>:<account>:targetgroup/<name>/<id>`) of
//! THAT Region and account. Anything else names a target group this endpoint could never
//! register an instance in, so the call is refused whole — HTTP 400, error code
//! `ValidationError`, and nothing attached — while an ARN of the request's own Region is
//! accepted and the group reports it.
//!
//! The rows below span what the rule generalises over: WHICH field of the ARN is out of
//! scope (the Region, the account, the service namespace, the resource type), WHICH other
//! Region it names, whether the field is empty or merely different, how many entries the
//! request carries and where in the list the offending one sits, and the operation the
//! member travels on — `AttachLoadBalancerTargetGroups` and `CreateAutoScalingGroup` both
//! carry it. Every row reads the group back through `DescribeAutoScalingGroups`, the
//! implemented reader of its `TargetGroupARNs`, so no row can pass on a refusal that still
//! wrote, nor on an accepted-but-unstored attachment.
//!
//! The same attachments are read back through `DescribeLoadBalancerTargetGroups` as well,
//! the reader whose whole subject they are, so an accepted attachment is one both readers of
//! it report; that reader's own rule has its own suite
//! (`tests/all/describe_load_balancer_target_groups.rs`).

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{
    borrowed, create_launch_template, create_target_group, emulator, p, query, seed_subnets,
    tag_value, tag_values, vpc_of, xml_unescape, REGION,
};

/// The `TargetTypeEnum` value of a target group whose targets are Amazon EC2 instances
/// (`TargetGroup$TargetType`: "`instance` - Targets are specified by instance ID").
const TARGET_TYPE_INSTANCE: &str = "instance";

/// The port the target groups of this suite listen on.
const TARGET_GROUP_PORT: i32 = 80;

/// How many other Regions the rows walk: enough that no row depends on one particular
/// neighbour, few enough to keep the table small.
const OTHER_REGIONS_IN_ROWS: usize = 3;

/// Regions OTHER than the one every request here is signed for, taken from this emulator's
/// own Region catalog for the partition (`ec2::regions_in_partition_of`, the catalog
/// `DescribeRegions` answers from) rather than written out here.
fn other_regions() -> Vec<&'static str> {
    let regions: Vec<&'static str> = ec2::regions_in_partition_of(REGION)
        .into_iter()
        .filter(|region| *region != REGION)
        .take(OTHER_REGIONS_IN_ROWS)
        .collect();
    assert_eq!(
        regions.len(),
        OTHER_REGIONS_IN_ROWS,
        "the Region catalog must offer {OTHER_REGIONS_IN_ROWS} Regions beside {REGION}, got \
         {regions:?}"
    );
    regions
}

/// An account id that is not this emulator's: the same 12-digit form
/// (`service_common::arn::is_account_id`), so only the value differs.
const OTHER_ACCOUNT_ID: &str = "210987654321";

/// The ARN a target group of `region`, held by `account`, is written as: Elastic Load
/// Balancing's own form, with the resource path `CreateTargetGroup` mints.
fn target_group_arn(region: &str, account: &str, name: &str) -> String {
    format!("arn:aws:elasticloadbalancing:{region}:{account}:targetgroup/{name}/0123456789abcdef")
}

/// Create one Auto Scaling group holding `desired` instances and, optionally, the target
/// groups it is created attached to. Answers the call's status, wire error code and message.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    template: &str,
    subnet_id: &str,
    desired: usize,
    attached: &[String],
) -> (u16, String, String) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", template),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", desired.to_string()),
        p("VPCZoneIdentifier", subnet_id),
    ];
    for (index, arn) in attached.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Post one attach and answer its status, wire error code and message.
async fn attach(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
    arns: &[String],
) -> (u16, String, String) {
    let mut params = vec![p("AutoScalingGroupName", name)];
    for (index, arn) in arns.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "AttachLoadBalancerTargetGroups", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The target groups the group reports under its own `TargetGroupARNs`, in the order it holds
/// them.
async fn attached_target_groups(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {name:?} must succeed, got {status} with {body:?}"
    );
    let Some((_, rest)) = body.split_once("<TargetGroupARNs>") else {
        return Vec::new();
    };
    let inner = rest
        .split_once("</TargetGroupARNs>")
        .map_or(rest, |(inner, _)| inner);
    tag_values(inner, "member")
}

/// The message this service refuses an unresolvable target group ARN with, read from the
/// crate so the rows cannot drift from the handlers.
fn refusal() -> &'static str {
    autoscaling::state::elbv2_targets::UNATTACHABLE_TARGET_GROUPS_MESSAGE
}

/// One out-of-scope value: the case, and the ARN (or non-ARN) the request carries.
fn out_of_scope_values() -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    // The Region segment names another Region: the defect this suite was filed for, over
    // three different Regions so no row depends on one.
    for region in other_regions() {
        rows.push((
            format!("a target group ARN of the Region {region}"),
            target_group_arn(region, engine::emulator_account_id(), "cross-region-tg"),
        ));
    }
    rows.extend([
        (
            "a target group ARN with an empty Region segment".to_string(),
            target_group_arn("", engine::emulator_account_id(), "regionless-tg"),
        ),
        (
            "the Region segment in upper case, which is no Region label".to_string(),
            target_group_arn(
                &REGION.to_uppercase(),
                engine::emulator_account_id(),
                "shouting-tg",
            ),
        ),
        (
            "a target group ARN of another account in this Region".to_string(),
            target_group_arn(REGION, OTHER_ACCOUNT_ID, "other-account-tg"),
        ),
        (
            "a target group ARN with an empty account segment".to_string(),
            target_group_arn(REGION, "", "accountless-tg"),
        ),
        (
            "a load balancer ARN of this Region, which names no target group".to_string(),
            format!(
                "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/app/lb/0123456789abcdef",
                engine::emulator_account_id()
            ),
        ),
        (
            "an ARN of another service's namespace in this Region".to_string(),
            format!(
                "arn:aws:ecs:{REGION}:{}:targetgroup/not-elb-tg/0123456789abcdef",
                engine::emulator_account_id()
            ),
        ),
        (
            "a value that is no ARN at all".to_string(),
            "my-target-group".to_string(),
        ),
        (
            "an ARN missing its resource field".to_string(),
            format!(
                "arn:aws:elasticloadbalancing:{REGION}:{}",
                engine::emulator_account_id()
            ),
        ),
    ]);
    rows
}

/// Every out-of-scope value, named ALONE on an attach: HTTP 400, `ValidationError`, the
/// service's own refusal sentence, and the group still holds exactly the attachment it had.
#[tokio::test]
async fn an_attach_naming_a_target_group_outside_this_region_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-scope").await;

    // The control: a target group this Region's Elastic Load Balancing v2 really holds, so
    // the only thing the rows below differ in is the ARN's scope.
    let in_region = create_target_group(
        &router,
        "tg-in-region",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;
    let (status, code, message) = create_group(
        &router,
        "asg-scope",
        "lt-scope",
        &subnets[0].id,
        0,
        std::slice::from_ref(&in_region),
    )
    .await;
    assert_eq!(
        status, 200,
        "the group with a same-Region attachment must be created, got {status} with {code:?} \
         {message:?}"
    );

    for (case, value) in out_of_scope_values() {
        let requested = vec![value.clone()];
        let (status, code, message) = attach(&router, "asg-scope", &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({value:?}): expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?} ({value:?}): expected the ValidationError code, got {code:?} \
             {message:?}"
        );
        assert_eq!(
            message,
            refusal(),
            "case {case:?} ({value:?}): expected the message {:?}, got {message:?}",
            refusal()
        );
        let attached = attached_target_groups(&router, "asg-scope").await;
        assert_eq!(
            attached,
            vec![in_region.clone()],
            "case {case:?} ({value:?}): the refusal must attach nothing, got {attached:?}"
        );
    }
}

/// The refusal is for the request as a WHOLE: one out-of-scope ARN beside resolvable ones
/// attaches NONE of them, wherever in the list it sits and however many entries travel with
/// it.
#[tokio::test]
async fn one_out_of_region_entry_refuses_the_whole_attach() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-mixed").await;

    // Three target groups of this Region, created in Elastic Load Balancing v2 itself.
    let mut in_region = Vec::new();
    for index in 0..3 {
        in_region.push(
            create_target_group(
                &router,
                &format!("tg-mixed-{index}"),
                TARGET_GROUP_PORT,
                TARGET_TYPE_INSTANCE,
                &vpc_id,
            )
            .await,
        );
    }
    let elsewhere =
        target_group_arn(other_regions()[0], engine::emulator_account_id(), "tg-away");

    let (status, code, message) =
        create_group(&router, "asg-mixed", "lt-mixed", &subnets[0].id, 0, &[]).await;
    assert_eq!(
        status, 200,
        "the group must be created, got {status} with {code:?} {message:?}"
    );

    // (case, the ARNs the attach names, in request order)
    let rows: Vec<(&str, Vec<String>)> = vec![
        (
            "the out-of-Region ARN first, before one resolvable",
            vec![elsewhere.clone(), in_region[0].clone()],
        ),
        (
            "the out-of-Region ARN last, after one resolvable",
            vec![in_region[0].clone(), elsewhere.clone()],
        ),
        (
            "the out-of-Region ARN between two resolvable ones",
            vec![
                in_region[0].clone(),
                elsewhere.clone(),
                in_region[1].clone(),
            ],
        ),
        (
            "the same out-of-Region ARN named twice beside three resolvable ones",
            vec![
                in_region[0].clone(),
                elsewhere.clone(),
                in_region[1].clone(),
                elsewhere.clone(),
                in_region[2].clone(),
            ],
        ),
    ];

    for (case, requested) in rows {
        let (status, code, message) = attach(&router, "asg-mixed", &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        assert_eq!(
            message,
            refusal(),
            "case {case:?}: expected the message {:?}, got {message:?}",
            refusal()
        );
        let attached = attached_target_groups(&router, "asg-mixed").await;
        assert!(
            attached.is_empty(),
            "case {case:?}: the refusal must attach nothing, got {attached:?}"
        );
    }

    // The control, from the very same group: the identical call without the out-of-Region
    // entry is served and attaches exactly what it names.
    let (status, code, message) = attach(&router, "asg-mixed", &in_region).await;
    assert_eq!(
        status, 200,
        "the same attach without the out-of-Region ARN must be served, got {status} with \
         {code:?} {message:?}"
    );
    let attached = attached_target_groups(&router, "asg-mixed").await;
    assert_eq!(
        attached, in_region,
        "expected the group to hold the three same-Region target groups, got {attached:?}"
    );
}

/// `CreateAutoScalingGroup` carries the same member, so it is held to the same rule: a
/// create naming an out-of-scope ARN is refused and creates NO group at all, while the same
/// create with the Region's own target group succeeds and reports it.
#[tokio::test]
async fn a_create_naming_a_target_group_outside_this_region_is_refused() {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    create_launch_template(&router, "lt-create").await;

    let in_region = create_target_group(
        &router,
        "tg-create",
        TARGET_GROUP_PORT,
        TARGET_TYPE_INSTANCE,
        &vpc_id,
    )
    .await;

    for (row, (case, value)) in out_of_scope_values().into_iter().enumerate() {
        let name = format!("asg-create-{row}");
        // The out-of-scope ARN beside the Region's own, so the create is wrong in exactly
        // one way.
        let requested = vec![in_region.clone(), value.clone()];
        let (status, code, message) =
            create_group(&router, &name, "lt-create", &subnets[0].id, 0, &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?} ({value:?}): expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?} ({value:?}): expected the ValidationError code, got {code:?} \
             {message:?}"
        );
        assert_eq!(
            message,
            refusal(),
            "case {case:?} ({value:?}): expected the message {:?}, got {message:?}",
            refusal()
        );
        // The refused create left no group behind: the reader reports none under that name.
        let attached = attached_target_groups(&router, &name).await;
        assert!(
            attached.is_empty(),
            "case {case:?} ({value:?}): the refusal must create no group, got {attached:?}"
        );
    }

    // The control: the identical create carrying only the Region's own target group.
    let (status, code, message) = create_group(
        &router,
        "asg-create-ok",
        "lt-create",
        &subnets[0].id,
        0,
        std::slice::from_ref(&in_region),
    )
    .await;
    assert_eq!(
        status, 200,
        "the create with a same-Region attachment must succeed, got {status} with {code:?} \
         {message:?}"
    );
    let attached = attached_target_groups(&router, "asg-create-ok").await;
    assert_eq!(
        attached,
        vec![in_region.clone()],
        "expected the created group to report its same-Region attachment, got {attached:?}"
    );
    // The reader whose subject these attachments are reports the same one, in the state an
    // attachment of a group with no instance to register is in.
    let added = autoscaling::state::elbv2_targets::ATTACHMENT_ADDED;
    let reported = described_attachments(&router, "asg-create-ok").await;
    assert_eq!(
        reported,
        vec![(in_region, added.to_string())],
        "expected DescribeLoadBalancerTargetGroups to report the attachment as {added:?}, got \
         {reported:?}"
    );
}

/// The attachments the group reports through `DescribeLoadBalancerTargetGroups`, as
/// `(ARN, State)` in the order reported.
///
/// Only what the ROUND TRIP of this suite's subject needs: that reader's own rule — the state
/// each attachment is in, its paging and its refusals — has its own suite
/// (`tests/all/describe_load_balancer_target_groups.rs`).
async fn described_attachments(
    router: &smithy_server_runtime::MultiServiceRouter,
    name: &str,
) -> Vec<(String, String)> {
    let (status, body) = query(
        router,
        "DescribeLoadBalancerTargetGroups",
        &[("AutoScalingGroupName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeLoadBalancerTargetGroups for {name:?} must succeed, got {status} with {body:?}"
    );
    let arns = tag_values(&body, "LoadBalancerTargetGroupARN");
    let states = tag_values(&body, "State");
    assert_eq!(
        arns.len(),
        states.len(),
        "every reported attachment must carry both its ARN and its State, got {body:?}"
    );
    arns.into_iter().zip(states).collect()
}
