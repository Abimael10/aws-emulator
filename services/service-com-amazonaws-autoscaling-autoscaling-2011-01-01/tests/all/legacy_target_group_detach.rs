// SPDX-License-Identifier: CC0-1.0

//! `DetachLoadBalancerTargetGroups` over the awsQuery wire.
//!
//! The rule under test: a legacy target-group detach is ONE unit of work over the
//! attachments the named Auto Scaling group holds — every target group the request
//! names must already be attached to that group, so a call that names one the group
//! does not hold detaches NONE of them and is refused `ValidationError` naming the
//! target groups that are not part of the group, while a call whose every ARN is
//! attached removes exactly those and leaves the rest attached in the order the group
//! holds them.
//!
//! The tables below span what that rule generalises over — how many target groups the
//! group holds, how many the request names, where in the request an unattached ARN
//! sits, whether it was never attached or was detached by an earlier call, whether it
//! repeats, and the documented per-call bound — and every row reads the group back
//! through `DescribeAutoScalingGroups` (the implemented reader of a group's
//! `TargetGroupARNs`), so no row can pass on an accepted-but-unstored change.

use crate::support::{borrowed, create_target_group, emulator, p, query, seed_subnets, tag_value,
                     tag_values, vpc_of, xml_unescape};

use smithy_server_runtime::MultiServiceRouter;

/// The launch source every group here is created from — a launch TEMPLATE, because no
/// account can create a launch configuration (see
/// `launch_configurations_are_unavailable.rs`).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The most target groups one call may name, from the model's own documentation for
/// `DetachLoadBalancerTargetGroupsType$TargetGroupARNs`: "You can specify up to 10
/// target groups."
const MAX_TARGET_GROUPS_PER_CALL: usize = 10;

/// How many target groups this suite names: the per-call bound plus the two more a row needs
/// to name one the group never held.
const SEEDED_TARGET_GROUPS: usize = MAX_TARGET_GROUPS_PER_CALL + 2;

/// The emulator every test here drives, with the target groups its rows name already created
/// in Elastic Load Balancing v2 itself — an association names a target group the account
/// HOLDS, which is what a create and an attach of one resolve — answered 1-based, the way the
/// rows number them.
async fn emulator_with_target_groups() -> (MultiServiceRouter, Vec<String>) {
    let (router, engine) = emulator();
    let subnets = seed_subnets(&engine, 1);
    let vpc_id = vpc_of(&router, &subnets[0]).await;
    let mut arns = Vec::with_capacity(SEEDED_TARGET_GROUPS);
    for index in 1..=SEEDED_TARGET_GROUPS {
        arns.push(create_target_group(&router, &format!("tg-{index}"), 80, "instance", &vpc_id).await);
    }
    (router, arns)
}

/// The ARN of the `index`-th target group of that set, numbered from one as the rows do.
fn target_group_arn(seeded: &[String], index: usize) -> String {
    seeded[index - 1].clone()
}

/// The first `count` target group ARNs.
fn target_group_arns(seeded: &[String], count: usize) -> Vec<String> {
    seeded[..count].to_vec()
}

/// Create a group holding `attached` target groups, with no capacity so nothing is
/// launched, and fail the test if the create itself is refused.
async fn create_group(
    router: &MultiServiceRouter,
    name: &str,
    attached: &[String],
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "0"),
        p("AvailabilityZones.member.1", "us-west-2a"),
    ];
    for (index, arn) in attached.iter().enumerate() {
        params.push(p(
            format!("TargetGroupARNs.member.{}", index + 1).as_str(),
            arn.clone(),
        ));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?} with {} target group(s) must succeed, got {status} with {body:?}",
        attached.len()
    );
}

/// Post one detach and answer its status, wire error code and message.
async fn detach(
    router: &MultiServiceRouter,
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
    let (status, body) = query(router, "DetachLoadBalancerTargetGroups", &borrowed(&params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// The target groups the group reports through the implemented reader of its
/// `TargetGroupARNs`, in the order it holds them.
async fn attached_target_groups(
    router: &MultiServiceRouter,
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
    // The member list lives inside the group's own `TargetGroupARNs` element; the
    // response carries other `<member>` lists (the zones, the termination policies),
    // so the slice is narrowed to that element first.
    let opened = body.split_once("<TargetGroupARNs>");
    let Some((_, rest)) = opened else {
        return Vec::new();
    };
    let inner = rest
        .split_once("</TargetGroupARNs>")
        .map_or(rest, |(inner, _)| inner);
    tag_values(inner, "member")
}

/// The message Amazon EC2 Auto Scaling refuses a detach naming target groups the group
/// does not hold with, as the recorded refusal words it: fixed text plus the ARNs from
/// the request that are not attached.
fn unattached_message(arns: &[String]) -> String {
    format!(
        "Trying to remove Target Groups that are not part of the group: {}",
        arns.join(", ")
    )
}

/// One accepted-detach row: the case, how many target groups the group holds, and the
/// 1-based positions of the ones the request detaches, in request order.
type DetachRow = (&'static str, usize, Vec<usize>);

fn accepted_rows() -> Vec<DetachRow> {
    vec![
        ("the only target group of the group", 1, vec![1]),
        ("the first of three", 3, vec![1]),
        ("the last of three", 3, vec![3]),
        ("the middle of three", 3, vec![2]),
        ("two of three, in reverse attachment order", 3, vec![3, 1]),
        ("all three at once", 3, vec![1, 2, 3]),
        (
            "every target group of a group holding the documented maximum",
            MAX_TARGET_GROUPS_PER_CALL,
            (1..=MAX_TARGET_GROUPS_PER_CALL).collect(),
        ),
        (
            "one of a group holding the documented maximum",
            MAX_TARGET_GROUPS_PER_CALL,
            vec![MAX_TARGET_GROUPS_PER_CALL],
        ),
    ]
}

/// Every row: the named target groups are removed and only they are, and the group
/// reports what is left through its own reader.
#[tokio::test]
async fn a_detach_removes_exactly_the_attached_target_groups_it_names() {
    let (router, seeded) = emulator_with_target_groups().await;

    for (row, (case, held, detached)) in accepted_rows().into_iter().enumerate() {
        let name = format!("asg-accepted-{row}");
        let attached = target_group_arns(&seeded, held);
        create_group(&router, &name, &attached).await;

        let requested: Vec<String> = detached
            .iter()
            .map(|position| target_group_arn(&seeded, *position))
            .collect();
        let (status, code, message) = detach(&router, &name, &requested).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with {code:?} {message:?}"
        );

        let expected: Vec<String> = attached
            .iter()
            .filter(|arn| !requested.contains(arn))
            .cloned()
            .collect();
        let actual = attached_target_groups(&router, &name).await;
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the group to hold {expected:?}, got {actual:?}"
        );
    }
}

/// One refused-detach row: the case, how many target groups the group holds, the
/// positions (1-based, past `held` for an ARN the group never held) the request names,
/// and the positions the refusal must name.
type RefusalRow = (&'static str, usize, Vec<usize>, Vec<usize>);

fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        (
            "a target group the group never held, alone",
            1,
            vec![9],
            vec![9],
        ),
        (
            "an attached target group followed by one never attached",
            1,
            vec![1, 9],
            vec![9],
        ),
        (
            "a never-attached target group followed by an attached one",
            1,
            vec![9, 1],
            vec![9],
        ),
        (
            "two never-attached target groups mixed between two attached ones",
            2,
            vec![1, 8, 2, 9],
            vec![8, 9],
        ),
        (
            "the same never-attached target group named twice",
            2,
            vec![9, 9],
            vec![9, 9],
        ),
        (
            "every target group of a group that holds none",
            0,
            vec![1, 2],
            vec![1, 2],
        ),
    ]
}

/// Every row: HTTP 400, `ValidationError`, the refusal naming the target groups that are
/// not part of the group, and NOTHING detached — the group still holds every attachment
/// it held before the call, which is what makes the refusal whole.
#[tokio::test]
async fn a_detach_naming_a_target_group_the_group_does_not_hold_is_refused_whole() {
    let (router, seeded) = emulator_with_target_groups().await;

    for (row, (case, held, requested_positions, unattached_positions)) in
        refusal_rows().into_iter().enumerate()
    {
        let name = format!("asg-refused-{row}");
        let attached = target_group_arns(&seeded, held);
        create_group(&router, &name, &attached).await;

        let requested: Vec<String> = requested_positions
            .iter()
            .map(|position| target_group_arn(&seeded, *position))
            .collect();
        let unattached: Vec<String> = unattached_positions
            .iter()
            .map(|position| target_group_arn(&seeded, *position))
            .collect();

        let (status, code, message) = detach(&router, &name, &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {case:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        let expected = unattached_message(&unattached);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );

        let actual = attached_target_groups(&router, &name).await;
        assert_eq!(
            actual, attached,
            "case {case:?}: expected the group to still hold {attached:?}, got {actual:?}"
        );
    }
}

/// A target group a first detach removed is no longer part of the group, so detaching it
/// again is refused exactly as one that was never attached is — and the attachment the
/// first call left in place is still there.
#[tokio::test]
async fn detaching_an_already_detached_target_group_is_refused() {
    let (router, seeded) = emulator_with_target_groups().await;

    let attached = target_group_arns(&seeded, 2);
    create_group(&router, "asg-twice", &attached).await;

    let first = vec![attached[0].clone()];
    let (status, code, message) = detach(&router, "asg-twice", &first).await;
    assert_eq!(
        status, 200,
        "the first detach must succeed, got {status} with {code:?} {message:?}"
    );

    for (case, requested) in [
        ("the detached target group alone", first.clone()),
        (
            "the detached target group beside the one still attached",
            vec![attached[1].clone(), attached[0].clone()],
        ),
    ] {
        let (status, code, message) = detach(&router, "asg-twice", &requested).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        let expected = unattached_message(&first);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the message {expected:?}, got {message:?}"
        );
        let actual = attached_target_groups(&router, "asg-twice").await;
        assert_eq!(
            actual,
            vec![attached[1].clone()],
            "case {case:?}: expected the group to still hold its second target group, got \
             {actual:?}"
        );
    }
}

/// The documented per-call bound: a call naming the maximum is served, and one naming a
/// single target group more is refused without detaching anything.
#[tokio::test]
async fn a_detach_past_the_documented_per_call_maximum_is_refused() {
    let (router, seeded) = emulator_with_target_groups().await;

    let attached = target_group_arns(&seeded, MAX_TARGET_GROUPS_PER_CALL + 1);
    create_group(&router, "asg-bound", &attached).await;

    // One past the bound: refused, and every attachment survives.
    let (status, code, message) = detach(&router, "asg-bound", &attached).await;
    assert_eq!(
        status, 400,
        "a detach of {} target groups must be refused, got {status} with {message:?}",
        attached.len()
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code past the bound, got {code:?} {message:?}"
    );
    let expected = format!(
        "Trying to update too many Target Groups at once. The limit is \
         {MAX_TARGET_GROUPS_PER_CALL}"
    );
    assert_eq!(
        message, expected,
        "expected the message {expected:?} past the bound, got {message:?}"
    );
    let actual = attached_target_groups(&router, "asg-bound").await;
    assert_eq!(
        actual, attached,
        "expected every attachment to survive the refusal, got {actual:?}"
    );

    // AT the bound: served.
    let at_bound: Vec<String> = attached[..MAX_TARGET_GROUPS_PER_CALL].to_vec();
    let (status, code, message) = detach(&router, "asg-bound", &at_bound).await;
    assert_eq!(
        status, 200,
        "a detach of {MAX_TARGET_GROUPS_PER_CALL} target groups must succeed, got {status} with \
         {code:?} {message:?}"
    );
    let actual = attached_target_groups(&router, "asg-bound").await;
    assert_eq!(
        actual,
        vec![attached[MAX_TARGET_GROUPS_PER_CALL].clone()],
        "expected only the target group the call did not name to be left, got {actual:?}"
    );
}

/// A detach naming a group the account does not hold is refused, in the absent-group
/// wording THIS operation answers with.
#[tokio::test]
async fn a_detach_of_an_absent_group_is_refused() {
    let (router, seeded) = emulator_with_target_groups().await;

    create_group(&router, "asg-present", &target_group_arns(&seeded, 1)).await;

    for name in ["asg-absent", "ASG-Absent", "asg-present-but-longer"] {
        let (status, code, message) = detach(&router, name, &target_group_arns(&seeded, 1)).await;
        assert_eq!(
            status, 400,
            "case {name:?}: expected HTTP 400, got {status} with {code:?} {message:?}"
        );
        assert_eq!(
            code, "ValidationError",
            "case {name:?}: expected the ValidationError code, got {code:?} {message:?}"
        );
        // The absent-group refusal this operation answers with: the fixed sentence whose
        // detail field is left unfilled, so no part of the request appears in it
        // (`state::ABSENT_GROUP_MESSAGE_WITHOUT_DETAIL`).
        let expected = "AutoScalingGroup name not found - null";
        assert_eq!(
            message, expected,
            "case {name:?}: expected the message {expected:?}, got {message:?}"
        );
    }

    // The group that DOES exist still holds its attachment: the refusals above touched
    // nothing.
    let actual = attached_target_groups(&router, "asg-present").await;
    assert_eq!(
        actual,
        target_group_arns(&seeded, 1),
        "expected the stored group to keep its target group, got {actual:?}"
    );
}
