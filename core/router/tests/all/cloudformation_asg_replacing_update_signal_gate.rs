// SPDX-License-Identifier: CC0-1.0

//! The REPLACEMENT an `UpdatePolicy`'s `AutoScalingReplacingUpdate` asks a stack update for:
//! the new Auto Scaling group the update builds beside the one the stack has, and what decides
//! whether that replacement is accepted.
//!
//! The RULE under test: when an `UpdateStack` changes the LAUNCH CONFIGURATION of an
//! `AWS::AutoScaling::AutoScalingGroup` whose declaration carries an
//! `AutoScalingReplacingUpdate` with `WillReplace: true`, CloudFormation replaces the GROUP
//! rather than changing it: it creates a new group — so the logical resource reports a new
//! `PhysicalResourceId` — and the new group's create is governed by the resource's own
//! `CreationPolicy`, which "is invoked during resource creation" whichever operation performs
//! that create. So the replacement has to receive the success signals its `ResourceSignal`
//! asks for, and an instance of the replacement that reports its own bootstrap FAILED fails
//! that create — "a failure signal causes CloudFormation to immediately fail the stack
//! creation or update" (`SignalResource$Status`) — which rolls the update back, leaving the
//! group the stack started with in place.
//!
//! This needs CloudFormation, Amazon EC2 Auto Scaling and Amazon EC2 to agree over one engine
//! — the stack declares the launch template and the group, that service launches the instances
//! and Amazon EC2 holds the boot program each of them runs — which is why the suite lives
//! here.
//!
//! The rows span what the rule generalises over: what the replacement's instances report about
//! their own bootstrap, which is what decides whether the update is accepted or rejected.
//! Nothing here waits for a span to pass: every wait polls for the CONDITION it is about.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{
    block_of, element, elements, form_encode, members_of, physical_ids_of,
};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";

/// The image and instance type the declared launch data names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The lifecycle state a group reports an instance of its own capacity in once it is serving
/// ("`InService` — The instance is running and in service", `Instance$LifecycleState`).
const IN_SERVICE: &str = "InService";

/// The logical id the template declares the group under, which is what its instances' signals
/// name (`SignalResource$LogicalResourceId`).
const GROUP: &str = "Asg";

/// How many instances each declared group holds.
const DESIRED: usize = 2;

/// How long a wait gives a condition the emulator owes, and how often it looks. Generous — the
/// whole suite settles in seconds — and only here so a failure is a failure rather than a
/// hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(25);

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

/// One Amazon EC2 Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The boot program the declared launch template carries: the invocation of the helper script
/// AWS documents an instance signalling its stack with ("you can use the `cfn-signal` helper
/// script … to signal"), reporting the exit code `succeeds` asks for — `0` for a bootstrap
/// that came up, a non-zero one for its failure.
///
/// `generation` is what makes one revision's program differ from another's, which is what gives
/// the launch template a new version and the group a changed launch configuration.
fn boot_program(stack: &str, generation: &str, succeeds: bool) -> String {
    let exit_code = if succeeds { 0 } else { 1 };
    format!(
        "#!/bin/bash\necho generation {generation}\ncfn-signal --exit-code {exit_code} \
         --stack {stack} --resource {GROUP} --region {REGION}\n"
    )
}

/// The template one row deploys: a launch template whose boot program signals the group, and a
/// group that `AutoScalingReplacingUpdate` asks an update to REPLACE.
///
/// The group states no `AutoScalingGroupName` of its own: "CloudFormation cannot replace a
/// resource whose name the template states", so a replaceable group is one CloudFormation names
/// itself.
fn template(stack: &str, generation: &str, succeeds: bool) -> String {
    let program = boot_program(stack, generation, succeeds);
    let encoded = aws_smithy_types::base64::encode(&program);
    format!(
        r#"{{"Resources":{{
  "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.0.0.0/16"}}}},
  "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.0.0/24","AvailabilityZone":"{REGION}a"}}}},
  "Sg":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
    "GroupDescription":"the instances of the declared group","VpcId":{{"Ref":"Vpc"}}}}}},
  "Lt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{
    "LaunchTemplateName":"{stack}-lt",
    "LaunchTemplateData":{{"ImageId":"{IMAGE_ID}","InstanceType":"{INSTANCE_TYPE}",
      "SecurityGroupIds":[{{"Fn::GetAtt":["Sg","GroupId"]}}],
      "UserData":"{encoded}"}}}}}},
  "{GROUP}":{{"Type":"AWS::AutoScaling::AutoScalingGroup",
    "CreationPolicy":{{"ResourceSignal":{{"Count":{DESIRED},"Timeout":"PT10M"}}}},
    "UpdatePolicy":{{"AutoScalingReplacingUpdate":{{"WillReplace":true}}}},
    "Properties":{{
    "MinSize":"1","MaxSize":"3","DesiredCapacity":"{DESIRED}",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"Lt"}},
      "Version":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}]}}}}}}}}"#
    )
}

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack reports `wanted`, or the OTHER settled status it
/// reached instead.
async fn wait_for_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
    through: &[&str],
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", stack)]).await;
        let status = element(&body, "StackStatus");
        if status == wanted || (settled(&status) && !through.contains(&status.as_str())) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never reached {wanted}: it is {status:?}, with the \
             failures {:?}",
            failed_events(router, stack).await
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stack's FAILED resource events, each as `logical id: status: reason`.
async fn failed_events(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let body = cfn(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|event| {
            let status = elements(event, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical_id = elements(event, "LogicalResourceId").into_iter().next()?;
            let reason = elements(event, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical_id}: {status}: {reason}"))
        })
        .collect()
}

/// The physical id the stack reports for the group right now, which is the name of the group
/// the stack manages.
async fn group_name(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = cfn(router, "DescribeStackResources", &[("StackName", stack)]).await;
    physical_ids_of(&body)
        .get(GROUP)
        .unwrap_or_else(|| {
            panic!("case {case:?}: the stack reports a physical id for {GROUP}, got {body}")
        })
        .clone()
}

/// The ids of the instances the group `group` reports IN SERVICE right now, or `None` where
/// that service holds no group of that name at all.
async fn in_service(router: &MultiServiceRouter, group: &str) -> Option<BTreeSet<String>> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    if !body.contains(group) {
        return None;
    }
    let mut serving = BTreeSet::new();
    for entry in block_of(&body, "Instances")
        .map(members_of)
        .unwrap_or_default()
    {
        let state = elements(entry, "LifecycleState").into_iter().next();
        let instance_id = elements(entry, "InstanceId").into_iter().next();
        if let (Some(state), Some(instance_id)) = (state, instance_id) {
            if state == IN_SERVICE {
                serving.insert(instance_id);
            }
        }
    }
    Some(serving)
}

/// Poll the group until it reports `wanted` instances in service.
async fn wait_for_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let serving = in_service(router, group).await;
        if serving
            .as_ref()
            .is_some_and(|serving| serving.len() >= wanted)
        {
            return serving.expect("checked just above");
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to report {wanted} {IN_SERVICE} instance(s), got \
             {serving:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// One row: what the replacement's instances report about their own bootstrap, and what that
/// leaves the stack and the two groups as.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// Whether the boot program of the REPLACEMENT's instances reports success.
    replacement_succeeds: bool,
    /// The status the update settles in.
    settles_in: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "a replacement whose instances signal success",
        stack: "replacing-update-accepted",
        replacement_succeeds: true,
        settles_in: "UPDATE_COMPLETE",
    },
    Row {
        case: "a replacement whose instances report their bootstrap failed",
        stack: "replacing-update-rejected",
        replacement_succeeds: false,
        settles_in: "UPDATE_ROLLBACK_COMPLETE",
    },
];

#[tokio::test]
async fn a_replacing_update_swaps_the_group_and_is_rejected_when_the_replacement_fails() {
    let router = router();
    for row in ROWS {
        let case = row.case;
        cfn(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", template(row.stack, "1", true).as_str()),
            ],
        )
        .await;
        let status = wait_for_status(&router, case, row.stack, "CREATE_COMPLETE", &[]).await;
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: the group's instances signal success, so the create must complete, \
             with the failures {:?}",
            failed_events(&router, row.stack).await
        );
        let before = group_name(&router, case, row.stack).await;
        let serving_before = wait_for_capacity(&router, case, &before, DESIRED).await;

        cfn(
            &router,
            "UpdateStack",
            &[
                ("StackName", row.stack),
                (
                    "TemplateBody",
                    template(row.stack, "2", row.replacement_succeeds).as_str(),
                ),
            ],
        )
        .await;
        let status = wait_for_status(
            &router,
            case,
            row.stack,
            row.settles_in,
            &["CREATE_COMPLETE"],
        )
        .await;
        assert_eq!(
            status, row.settles_in,
            "case {case:?}: expected the update to settle in {}, with the failures {:?}",
            row.settles_in,
            failed_events(&router, row.stack).await
        );
        let after = group_name(&router, case, row.stack).await;
        if row.replacement_succeeds {
            // The group was REPLACED, so the logical resource reports a new physical id, and
            // the group the stack started with is gone from the owning service.
            assert_ne!(
                after, before,
                "case {case:?}: a replacing update must give the group a new physical id"
            );
            let serving = wait_for_capacity(&router, case, &after, DESIRED).await;
            assert!(
                serving.is_disjoint(&serving_before),
                "case {case:?}: the replacement group {after} must serve instances of its own, \
                 got {serving:?} beside the original {serving_before:?}"
            );
        } else {
            // The replacement was REJECTED, so the stack went back to the group it had: that
            // group is the one it reports and it is serving the capacity it was declared with.
            assert_eq!(
                after, before,
                "case {case:?}: a rejected replacement must leave the original group as the \
                 stack's"
            );
            let serving = wait_for_capacity(&router, case, &before, DESIRED).await;
            assert_eq!(
                serving.len(),
                DESIRED,
                "case {case:?}: the group the stack went back to must serve its declared \
                 capacity, got {serving:?}"
            );
            // ...and the failure is reported on the GROUP, naming the signal that rejected the
            // replacement.
            let reported = failed_events(&router, row.stack).await;
            assert!(
                reported
                    .iter()
                    .any(|event| event.starts_with(&format!("{GROUP}:"))
                        && event.contains("FAILURE")),
                "case {case:?}: the group's own event must report the FAILURE signal that \
                 rejected the replacement, got {reported:?}"
            );
        }
    }
}
