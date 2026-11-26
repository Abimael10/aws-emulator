// SPDX-License-Identifier: CC0-1.0

//! The whole sentence a launch that could not be REGISTERED in one of the group's attachments
//! records, and the state it leaves behind.
//!
//! RULE under test: registering a launched instance with each target group the group is
//! attached to is part of bringing that instance into service, and Elastic Load Balancing v2
//! takes no target its target group's own VPC does not hold (`InvalidTargetException`: "the
//! specified target does not exist, is not in the same VPC as the target group, or has an
//! unsupported instance type"). So a launch an attachment would refuse reaches no service:
//! each unit of capacity it could not place records a terminal `Cancelled` activity — "the
//! StatusCode value is Failed or Cancelled and the StatusMessage element in the response
//! indicates the cause of the failure" (`DescribeScalingActivities`) — whose `StatusMessage` is
//! Amazon EC2 Auto Scaling's own `Failed to update target group ` followed by the target group
//! it could not update, then that service's refusal for the targets it would not take, then the
//! launch that was not made; the attempt Amazon EC2 made is taken back, so the group holds no
//! member for it, that target group holds no target, and Amazon EC2 reports the instance on its
//! way out.
//!
//! The rows span what the rule generalises over: how many units the capacity change asks for at
//! once (one, two — the message names every target the registration was refused for), and how
//! the client stated the attachment (the group's own `TargetGroupARNs`, an `elbv2` traffic
//! source, and an `AttachLoadBalancerTargetGroups` made after the group was created, which are
//! the three spellings of one association).
//!
//! Which VPC relationships cancel a launch and which do not — and that the capacity lands once
//! the client repairs the attachment — is spanned over the whole assembled emulator by
//! `core/router/tests/all/asg_launch_into_a_target_group_of_another_vpc_is_cancelled.rs`; what
//! is pinned HERE is the sentence, element by element, against the ids the owning services
//! minted.

use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::support::{
    borrowed, create_target_group, ec2_query, elbv2_query, emulator, list_members, p, query,
    seed_subnets, tag_value, tag_values, vpc_of, xml_unescape,
};

/// The tag Amazon EC2 Auto Scaling puts the group's name on every instance it launches in, so
/// the account's own Amazon EC2 view can be asked which instances belong to one group.
///
/// SOURCE: the reserved tag AWS documents on the instances of an Auto Scaling group,
/// `aws:autoscaling:groupName`; written out here rather than taken from the implementation,
/// which a case re-using the service's own literal would assert nothing about.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// The terminal `ScalingActivityStatusCode` a launch Amazon EC2 Auto Scaling did not bring into
/// service ends in, from the model's own `ScalingActivityStatusCode`.
const CANCELLED: &str = "Cancelled";

/// The `Progress` a terminal activity reports: the step it describes is over.
const COMPLETE: &str = "100";

/// The `InstanceStateName` values an instance Amazon EC2 has been asked to terminate is in —
/// it is `shutting-down` and reaches `terminated` on that service's own clock, so a read
/// straight after the launch may catch either.
const LEAVING_STATES: &[&str] = &["shutting-down", "terminated"];

/// How long a case keeps looking for the outcome it waits on before it gives up — generous on
/// purpose, so a loaded host is never the reason a case fails.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// How the client stated the association between the group and the target group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// The group's own `TargetGroupARNs`, on the create.
    TargetGroupArns,
    /// An `elbv2` traffic source, on the create.
    TrafficSource,
    /// `AttachLoadBalancerTargetGroups`, after the group was created.
    AttachAfterCreate,
}

/// One row: the group, how its attachment was stated, and how many units the capacity change
/// asks for at once.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    group: &'static str,
    spelling: Spelling,
    capacity: usize,
}

/// What one scaling activity reported.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Outcome {
    status_code: String,
    status_message: String,
    description: String,
    progress: String,
}

#[tokio::test]
async fn a_launch_an_attachment_would_refuse_records_the_whole_refusal_and_is_taken_back() {
    let rows = [
        Row {
            case: "one unit, attached as TargetGroupARNs",
            group: "vpc-refusal-arns",
            spelling: Spelling::TargetGroupArns,
            capacity: 1,
        },
        Row {
            case: "two units at once, attached as TargetGroupARNs",
            group: "vpc-refusal-arns-two",
            spelling: Spelling::TargetGroupArns,
            capacity: 2,
        },
        Row {
            case: "one unit, attached as an elbv2 traffic source",
            group: "vpc-refusal-source",
            spelling: Spelling::TrafficSource,
            capacity: 1,
        },
        Row {
            case: "one unit, attached after the group was created",
            group: "vpc-refusal-attached",
            spelling: Spelling::AttachAfterCreate,
            capacity: 1,
        },
    ];

    let (router, engine) = emulator();
    // The group's own network, and the one its instances are NOT in: each seeding call puts its
    // subnet in a VPC of its own, and the VPC a subnet is in is read back from the service that
    // owns the subnet.
    let own = seed_subnets(&engine, 1);
    let elsewhere = seed_subnets(&engine, 1);
    let own_vpc = vpc_of(&router, &own[0]).await;
    let other_vpc = vpc_of(&router, &elsewhere[0]).await;
    assert_ne!(
        own_vpc, other_vpc,
        "the two seeded subnets must be in different VPCs for any of these cases to mean \
         anything"
    );

    for row in rows {
        let case = row.case;
        let arn = create_target_group(&router, row.group, 80, "instance", &other_vpc).await;
        create_group(&router, &row, &own[0].id, &arn).await;
        if row.spelling == Spelling::AttachAfterCreate {
            let (status, body) = query(
                &router,
                "AttachLoadBalancerTargetGroups",
                &[
                    ("AutoScalingGroupName", row.group),
                    ("TargetGroupARNs.member.1", arn.as_str()),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the attach of a target group that exists must be accepted, got \
                 {status}: {body}"
            );
        }
        set_desired_capacity(&router, case, row.group, row.capacity).await;

        // Every unit the launch could not place records its own terminal activity.
        let recorded = settled_activities(&router, case, row.group, row.capacity).await;
        let cancelled: Vec<&Outcome> = recorded
            .iter()
            .filter(|outcome| outcome.status_code == CANCELLED)
            .collect();
        assert_eq!(
            cancelled.len(),
            row.capacity,
            "case {case:?}: expected one {CANCELLED} activity per unit of capacity, got \
             {recorded:?}"
        );

        // The attempts Amazon EC2 made for the launch, as the service that owns the instances
        // reports them: each one was made, and each one is on its way out.
        let attempts = ec2_instances_of(&router, case, row.group).await;
        assert!(
            attempts.len() >= row.capacity,
            "case {case:?}: expected Amazon EC2 to hold the {} attempt(s) the launch made, got \
             {attempts:?}",
            row.capacity
        );
        for instance_id in &attempts {
            let state = ec2_instance_state(&router, case, instance_id).await;
            assert!(
                LEAVING_STATES.contains(&state.as_str()),
                "case {case:?}: expected the attempt {instance_id} to be taken back, got \
                 {state:?}"
            );
        }

        // The sentence itself: fixed text around the target group the launch could not update,
        // the VPC that target group is in and the attempts it would not take — every variable
        // part read back from the service that owns it.
        let prefix = format!(
            "Failed to update target group {arn}: The following targets are not in the target \
             group VPC '{other_vpc}': "
        );
        let suffix = " (Service: ElasticLoadBalancingV2, Status Code: 400, Error Code: \
                       InvalidTarget). Launching EC2 instance failed.";
        let quoted: Vec<String> = attempts
            .iter()
            .map(|instance_id| format!("'{instance_id}'"))
            .collect();
        for outcome in &cancelled {
            let message = outcome.status_message.as_str();
            let listed = message
                .strip_prefix(&prefix)
                .and_then(|rest| rest.strip_suffix(suffix));
            let Some(listed) = listed else {
                panic!(
                    "case {case:?}: expected a message reading {prefix}<targets>{suffix}, got \
                     {message:?}"
                );
            };
            // The targets the refusal names, as a SET: the order the launch made the attempts
            // in is Amazon EC2's, not something the sentence promises.
            let mut named: Vec<&str> = listed.split(", ").collect();
            named.sort_unstable();
            let mut expected: Vec<&str> = quoted.iter().map(String::as_str).collect();
            expected.sort_unstable();
            assert_eq!(
                named, expected,
                "case {case:?}: expected the refusal to name every attempt, got {message:?}"
            );
            assert_eq!(
                outcome.progress, COMPLETE,
                "case {case:?}: a terminal activity must report its step complete, got \
                 {outcome:?}"
            );
            assert!(
                attempts
                    .iter()
                    .any(|instance_id| outcome.description.contains(instance_id.as_str())),
                "case {case:?}: the activity must describe the launch it made, got {outcome:?}"
            );
        }

        // And nothing came of the launch: no member of the group, no target in the group it
        // could not be registered in, while the capacity the client asked for stands.
        let members = group_instances(&router, case, row.group).await;
        assert!(
            members.is_empty(),
            "case {case:?}: expected the group to hold no member for a cancelled launch, got \
             {members:?}"
        );
        let targets = registered_targets(&router, case, &arn).await;
        assert!(
            targets.is_empty(),
            "case {case:?}: expected no target in the target group of another VPC, got \
             {targets:?}"
        );
    }
}

/// A target group of another VPC that takes a different KIND of target than an Amazon EC2
/// instance cancels nothing: an Auto Scaling group's instances are registered by instance id,
/// and only an `instance` target group takes a target stated that way ("`instance` - Targets
/// are specified by instance ID", `TargetGroup$TargetType`), so no registration of this
/// group's instances is owed there for the network to refuse — what is wrong with such an
/// attachment is its target type, not its VPC.
#[tokio::test]
async fn a_target_group_of_another_target_type_cancels_nothing() {
    let case = "an ip target group of another VPC";
    let (router, engine) = emulator();
    let own = seed_subnets(&engine, 1);
    let elsewhere = seed_subnets(&engine, 1);
    let other_vpc = vpc_of(&router, &elsewhere[0]).await;
    let group = "vpc-refusal-ip-target-type";
    let arn = create_target_group(&router, group, 80, "ip", &other_vpc).await;
    let (status, body) = query(
        &router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group),
            ("MinSize", "0"),
            ("MaxSize", "1"),
            ("DesiredCapacity", "0"),
            ("LaunchTemplate.LaunchTemplateName", "lt-1"),
            ("VPCZoneIdentifier", own[0].id.as_str()),
            ("TargetGroupARNs.member.1", arn.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup expected 200, got {status}: {body}"
    );
    set_desired_capacity(&router, case, group, 1).await;

    // The launch is MADE: the instance comes into service, and no activity is cancelled.
    let deadline = Instant::now() + DEADLINE;
    let (members, recorded) = loop {
        let members = group_instances(&router, case, group).await;
        let recorded = activities(&router, case, group).await;
        if !members.is_empty() || Instant::now() >= deadline {
            break (members, recorded);
        }
        tokio::time::sleep(POLL_PAUSE).await;
    };
    assert_eq!(
        members.len(),
        1,
        "case {case:?}: expected the launch to bring the group's instance into service, got \
         {members:?} with {recorded:?}"
    );
    assert!(
        !recorded
            .iter()
            .any(|outcome| outcome.status_code == CANCELLED),
        "case {case:?}: expected no cancelled launch, got {recorded:?}"
    );
}

/// Create the group `row` describes: it places its instances in `subnet`, and — unless the row
/// attaches afterwards — is created already attached to `arn` in the row's spelling.
async fn create_group(router: &MultiServiceRouter, row: &Row, subnet: &str, arn: &str) {
    let case = row.case;
    let mut params = vec![
        p("AutoScalingGroupName", row.group),
        p("MinSize", "0"),
        p("MaxSize", row.capacity.to_string()),
        p("DesiredCapacity", "0"),
        p("LaunchTemplate.LaunchTemplateName", "lt-1"),
        p("VPCZoneIdentifier", subnet),
    ];
    match row.spelling {
        Spelling::TargetGroupArns => params.push(p("TargetGroupARNs.member.1", arn)),
        Spelling::TrafficSource => {
            params.push(p("TrafficSources.member.1.Identifier", arn));
            params.push(p("TrafficSources.member.1.Type", "elbv2"));
        }
        Spelling::AttachAfterCreate => {}
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup for {:?} expected 200, got {status}: {body}",
        row.group
    );
}

/// Ask the group for `capacity` units of capacity, which is the call that makes the launch.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    capacity: usize,
) {
    let (status, body) = query(
        router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", capacity.to_string().as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity({capacity}) must be served, got {status}: {body}"
    );
}

/// Every scaling activity the group has recorded, read through `DescribeScalingActivities` the
/// way a client polls for the outcome of the capacity change it just asked for.
async fn activities(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<Outcome> {
    let (status, body) = query(
        router,
        "DescribeScalingActivities",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScalingActivities expected 200, got {status}: {body}"
    );
    list_members(&body, "Activities")
        .into_iter()
        .map(|member| Outcome {
            status_code: tag_value(member, "StatusCode"),
            status_message: xml_unescape(&tag_value(member, "StatusMessage")),
            description: xml_unescape(&tag_value(member, "Description")),
            progress: tag_value(member, "Progress"),
        })
        .collect()
}

/// The group's activities once there are at least `wanted` of them and every one carries the
/// terminal code a cancelled launch ends in.
///
/// Looked for again until that holds or [`DEADLINE`] passes: this waits for the OUTCOME rather
/// than for any length of time.
async fn settled_activities(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<Outcome> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let settled = activities(router, case, group).await;
        let cancelled = settled
            .iter()
            .filter(|outcome| outcome.status_code == CANCELLED)
            .count();
        if cancelled >= wanted {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change must record {wanted} cancelled activity(ies), \
             got {settled:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The instance ids the group reports holding, as its own read reports them.
async fn group_instances(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAutoScalingGroups expected 200, got {status}: {body}"
    );
    tag_values(&body, "InstanceId")
}

/// The instance ids the account's own Amazon EC2 view holds FOR THIS GROUP, asked of the
/// service that owns the instances through the tag Auto Scaling marks them with, sorted and
/// deduplicated so a case compares a set rather than an order EC2 never promised.
async fn ec2_instances_of(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = ec2_query(
        router,
        "DescribeInstances",
        &[
            p("Filter.1.Name", format!("tag:{GROUP_NAME_TAG}")),
            p("Filter.1.Value.1", group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances expected 200, got {status}: {body}"
    );
    let mut ids = tag_values(&body, "instanceId");
    ids.sort();
    ids.dedup();
    ids
}

/// What Amazon EC2 reports one instance's state as, read from a `DescribeInstances` that named
/// exactly that instance — so the state belongs to it and to no other member.
async fn ec2_instance_state(router: &MultiServiceRouter, case: &str, instance_id: &str) -> String {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id:?}) expected 200, got {status}: {body}"
    );
    tag_value(&tag_value(&body, "instanceState"), "name")
}

/// The target ids Elastic Load Balancing v2 holds in the target group `arn`, asked of the
/// service that owns the registrations.
async fn registered_targets(router: &MultiServiceRouter, case: &str, arn: &str) -> Vec<String> {
    let (status, body) =
        elbv2_query(router, "DescribeTargetHealth", &[p("TargetGroupArn", arn)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeTargetHealth expected 200, got {status}: {body}"
    );
    tag_values(&body, "Id")
}
