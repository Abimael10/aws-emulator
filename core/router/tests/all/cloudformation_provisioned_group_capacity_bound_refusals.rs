// SPDX-License-Identifier: CC0-1.0

//! A capacity change refused against the bounds of an Auto Scaling group a CLOUDFORMATION
//! stack provisioned is refused with the same sentence as one against any other group.
//!
//! RULE: `SetDesiredCapacity` asks for a capacity the group's own stored bounds admit, so a
//! capacity outside them is refused HTTP 400 `ValidationError` with the fixed sentence
//! `New SetDesiredCapacity value <capacity> is above max value <MaxSize> for the
//! AutoScalingGroup.` (or `below min value <MinSize>`), whose only variable parts are the
//! capacity the request named and the bound of the group it crossed as the group holds it NOW
//! — the sentence ends at that period, and names neither the group nor how the group came to
//! exist. A capacity that is not a count at all is refused for its SIGN instead, `New
//! SetDesiredCapacity value <capacity> is negative.`, naming no bound. Smithy models
//! `SetDesiredCapacity` with `ScalingActivityInProgressFault` and `ResourceContentionFault`
//! only (`smithy_query(query="operation",operation="SetDesiredCapacity",
//! model_path="services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/\
//! smithy-model/com-amazonaws-autoscaling-autoscaling-2011-01-01.json")`), so a bound refusal
//! is the service-common `ValidationError` and the sentence is the one machine-visible
//! statement of WHICH rule the request missed.
//!
//! The suite lives here because the group under test is one CloudFormation created, through
//! Amazon EC2 Auto Scaling, into a network Amazon EC2 holds: all three services over one
//! engine. The rows span what the rule generalises over — which bound the capacity crossed,
//! how far past it the capacity is, which of the stack's two groups (so two generated,
//! per-run names) it is asked of, and whether the bound is the one the stack declared or the
//! one an in-place stack update left the group with — plus the refused call leaving the group
//! untouched, an in-bounds capacity still being accepted afterwards, and the group's name
//! appearing in none of the refusals.
//!
//! The last leg is the same group read after its stack is gone: a name no group answers to is
//! refused with that operation's own no-detail sentence, which carries no part of the request
//! either.
//!
//! What the OWNING crate's own suites pin, and this one therefore does not repeat: the
//! sentences themselves over a group that crate creates directly
//! (`services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01`:
//! `tests/all/group_size_and_health_check_refusal_wording.rs`,
//! `tests/all/set_desired_capacity_on_subnet_placed_groups.rs`,
//! `tests/all/capacity_sign_and_headroom_bounds.rs`). What is read here is the same rule where
//! the group's name and its bounds come from a stack rather than from the caller.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the two services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";

/// The stack under test.
const STACK: &str = "asg-capacity-bounds-stack";

/// How long a leg waits for a state the clock owes (the stack's status moves) and how often it
/// looks. Generous, and only here so a failure is a failure rather than a hang: every wait
/// below waits for its CONDITION, never for a time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// Call one query operation of `service`, answered with its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation operation that must succeed.
async fn cfn(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "cloudformation", CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "CloudFormation {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// `SetDesiredCapacity` on `group`, answered with the status, the error code and the error
/// message — whatever the answer is.
async fn set_desired_capacity(
    router: &MultiServiceRouter,
    group: &str,
    capacity: i64,
) -> (u16, String, String) {
    let (status, body) = query(
        router,
        "autoscaling",
        ASG_VERSION,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", group),
            ("DesiredCapacity", &capacity.to_string()),
        ],
    )
    .await;
    if status == 200 {
        return (status, String::new(), String::new());
    }
    (status, element(&body, "Code"), element(&body, "Message"))
}

/// The `DesiredCapacity` the group reports for itself, read through the operation whose
/// subject it is. The read must report one, so a before/after comparison cannot degenerate
/// into comparing two absences.
async fn reported_capacity(router: &MultiServiceRouter, group: &str) -> String {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    let reported = elements(&body, "DesiredCapacity").into_iter().next();
    reported
        .filter(|capacity| !capacity.is_empty())
        .unwrap_or_else(|| {
            panic!("expected the group {group:?} to report a DesiredCapacity, got {body}")
        })
}

/// The template the stack declares: two Auto Scaling groups, with different bounds, placed in
/// a subnet of their own VPC and launched from one launch template — the shape a stack that
/// provisions a group takes. Neither group states a name, so each carries the one
/// CloudFormation generates for it ("Ref returns the name of the Auto Scaling group").
fn template(narrow_max: &str, wide_min: &str) -> String {
    format!(
        r#"{{"Resources":{{
  "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.0.0.0/16"}}}},
  "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.0.0/24","AvailabilityZone":"{REGION}a"}}}},
  "Lt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{"LaunchTemplateData":{{
    "ImageId":"ami-0abcdef1234567890","InstanceType":"t3.micro"}}}}}},
  "NarrowAsg":{{"Type":"AWS::AutoScaling::AutoScalingGroup","Properties":{{
    "MinSize":"0","MaxSize":"{narrow_max}","DesiredCapacity":"0",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"Lt"}},
      "Version":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}]}}}},
  "WideAsg":{{"Type":"AWS::AutoScaling::AutoScalingGroup","Properties":{{
    "MinSize":"{wide_min}","MaxSize":"3","DesiredCapacity":"1",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"Lt"}},
      "Version":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}]}}}}}},
  "Outputs":{{
    "NarrowAsgName":{{"Value":{{"Ref":"NarrowAsg"}}}},
    "WideAsgName":{{"Value":{{"Ref":"WideAsg"}}}}}}}}"#
    )
}

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack settles, and answer the status it settled in.
async fn settle(router: &MultiServiceRouter, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", stack)]).await;
        let status = element(&body, "StackStatus");
        if settled(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "the stack never left {status:?}: last answer {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Which side of the group's bounds a row's capacity misses, and the group's own value for it.
#[derive(Debug, Clone, Copy)]
enum Crossed {
    /// The group's `MinSize`, which the capacity is below.
    Min(i64),
    /// The group's `MaxSize`, which the capacity is above.
    Max(i64),
    /// Neither: the capacity is not a count at all, so the refusal is for its sign.
    Sign,
}

/// The sentence the service refuses `capacity` with, built the way the rule words it: fixed
/// text around the capacity the request named and the bound of the group it crossed.
fn expected_message(capacity: i64, crossed: Crossed) -> String {
    let (relation, bound) = match crossed {
        Crossed::Min(min) => ("below min", min),
        Crossed::Max(max) => ("above max", max),
        Crossed::Sign => {
            return format!("New SetDesiredCapacity value {capacity} is negative.");
        }
    };
    format!(
        "New SetDesiredCapacity value {capacity} is {relation} value {bound} for the \
         AutoScalingGroup."
    )
}

/// One row of the refusal table: the capacity asked of which of the stack's groups, and the
/// bound of that group it crosses.
struct Row {
    case: &'static str,
    /// The output of the stack that names the group the row is asked of.
    output: &'static str,
    capacity: i64,
    crossed: Crossed,
}

/// Assert one row: the refusal's status, code and sentence, that the sentence carries no part
/// of the group's identity, and that the refused call left the group's capacity alone.
async fn check_row(router: &MultiServiceRouter, groups: &BTreeMap<String, String>, row: &Row) {
    let group = groups
        .get(row.output)
        .unwrap_or_else(|| panic!("the stack publishes {}", row.output));
    let before = reported_capacity(router, group).await;
    let (status, code, message) = set_desired_capacity(router, group, row.capacity).await;
    let expected = expected_message(row.capacity, row.crossed);
    assert_eq!(
        (status, code.as_str(), message.as_str()),
        (400, "ValidationError", expected.as_str()),
        "case {:?}: expected (400, \"ValidationError\", {expected:?}), got ({status}, \
         {code:?}, {message:?})",
        row.case
    );
    assert!(
        !message.contains(group.as_str()),
        "case {:?}: expected the refusal to name no group, got {message:?} for the group \
         {group:?}",
        row.case
    );
    let after = reported_capacity(router, group).await;
    assert_eq!(
        after, before,
        "case {:?}: expected the refused call to leave the group's DesiredCapacity at \
         {before:?}, got {after:?}",
        row.case
    );
}

#[tokio::test]
async fn a_stack_provisioned_groups_capacity_bounds_are_refused_in_aws_wording() {
    let router = router();

    // The stack settles, and publishes the name CloudFormation gave each of its groups.
    cfn(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template("1", "1").as_str()),
        ],
    )
    .await;
    let status = settle(&router, STACK).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case \"the stack that declares the groups\": expected the create to complete, got \
         {status:?}"
    );
    let described = cfn(&router, "DescribeStacks", &[("StackName", STACK)]).await;
    let stack_id = element(&described, "StackId");
    let groups: BTreeMap<String, String> = outputs_of(&described);
    for output in ["NarrowAsgName", "WideAsgName"] {
        assert!(
            groups.get(output).is_some_and(|name| !name.is_empty()),
            "case \"the stack that declares the groups\": expected the output {output} to name \
             a group, got {groups:?}"
        );
    }

    // LEG 1: the bounds the stack DECLARED, missed on either side and by varying distances,
    // on each of the two groups.
    let declared: [Row; 5] = [
        Row {
            case: "one past the declared maximum",
            output: "NarrowAsgName",
            capacity: 2,
            crossed: Crossed::Max(1),
        },
        Row {
            case: "far past the declared maximum",
            output: "NarrowAsgName",
            capacity: 7,
            crossed: Crossed::Max(1),
        },
        Row {
            case: "one past the other group's declared maximum",
            output: "WideAsgName",
            capacity: 4,
            crossed: Crossed::Max(3),
        },
        Row {
            case: "one below the other group's declared minimum",
            output: "WideAsgName",
            capacity: 0,
            crossed: Crossed::Min(1),
        },
        Row {
            case: "a capacity that is not a count at all",
            output: "WideAsgName",
            capacity: -1,
            crossed: Crossed::Sign,
        },
    ];
    for row in &declared {
        check_row(&router, &groups, row).await;
    }

    // LEG 2: the same rule against the bounds an IN-PLACE stack update left the groups with —
    // the sentence quotes what the group holds now, not what the template first declared.
    cfn(
        &router,
        "UpdateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template("3", "0").as_str()),
        ],
    )
    .await;
    let status = settle(&router, STACK).await;
    assert_eq!(
        status, "UPDATE_COMPLETE",
        "case \"the stack after its in-place update\": expected the update to complete, got \
         {status:?}"
    );
    let updated: [Row; 2] = [
        Row {
            case: "one past the maximum the stack update raised",
            output: "NarrowAsgName",
            capacity: 4,
            crossed: Crossed::Max(3),
        },
        Row {
            case: "a negative capacity under the minimum the stack update lowered, refused \
                   for its sign",
            output: "WideAsgName",
            capacity: -1,
            crossed: Crossed::Sign,
        },
    ];
    for row in &updated {
        check_row(&router, &groups, row).await;
    }

    // The capacity the lowered minimum now ADMITS is accepted, so the refusals above are the
    // bound speaking and not the operation refusing every capacity.
    let wide = groups
        .get("WideAsgName")
        .expect("the stack publishes its wide group")
        .clone();
    let (status, code, message) = set_desired_capacity(&router, &wide, 0).await;
    assert_eq!(
        status, 200,
        "case \"a capacity the updated bounds admit\": expected it to be accepted, got \
         {status} with ({code:?}, {message:?})"
    );
    let reported = reported_capacity(&router, &wide).await;
    assert_eq!(
        reported, "0",
        "case \"a capacity the updated bounds admit\": expected the group to report the \
         capacity it was set to, got {reported:?}"
    );

    // LEG 3: the stack's deletion takes both groups away, and a capacity change asked of a
    // name no group answers to carries no part of the request either.
    cfn(&router, "DeleteStack", &[("StackName", STACK)]).await;
    let status = settle(&router, &stack_id).await;
    assert_eq!(
        status, "DELETE_COMPLETE",
        "case \"the deleted stack\": expected the deletion to complete, got {status:?}"
    );
    for output in ["NarrowAsgName", "WideAsgName"] {
        let group = groups.get(output).expect("the stack publishes its groups");
        let (status, code, message) = set_desired_capacity(&router, group, 1).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (
                400,
                "ValidationError",
                "AutoScalingGroup name not found - null"
            ),
            "case \"the deleted stack's {output}\": expected the no-detail absent-group \
             refusal, got ({status}, {code:?}, {message:?})"
        );
    }
}
