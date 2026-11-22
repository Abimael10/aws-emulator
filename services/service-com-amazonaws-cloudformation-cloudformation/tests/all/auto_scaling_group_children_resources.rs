// SPDX-License-Identifier: CC0-1.0

//! The three resources a stack declares ON one of its Auto Scaling groups — an
//! `AWS::AutoScaling::LifecycleHook`, an `AWS::AutoScaling::ScheduledAction` and an
//! `AWS::AutoScaling::WarmPool` — read back through the wire of the service that OWNS them.
//!
//! RULE under test: **a resource a template declares on a group is created in Amazon EC2 Auto
//! Scaling itself, through the one call that states it (`PutLifecycleHook`,
//! `PutScheduledUpdateGroupAction`, `PutWarmPool`), under the name the stack publishes as that
//! resource's `Ref` — so that service's own read of the group answers with it; a revision that
//! states it differently reaches it through that same call, keeping the name; and a revision
//! that DROPS it removes it from that service while the group it was on stays.**
//!
//! Why it matters: a client that deploys a group with CloudFormation then operates it through
//! the service APIs — completing a lifecycle action, listing the schedule, reading the warm
//! pool's instances. A child the stack reports created that `DescribeLifecycleHooks`,
//! `DescribeScheduledActions` or `DescribeWarmPool` answers nothing for tells the client the
//! deploy produced nothing, while the stack claims success.
//!
//! Nothing here waits out a span: the pools prepare no instance and the groups launch none, so
//! what each row observes is the stack's own settled status and the owning service's answer,
//! never how fast the host runs.
//!
//! The rows differ along what the rule generalises over: WHICH of the three types the resource
//! is, whether its declaration NAMES it or leaves the name to CloudFormation, which group of
//! the stack it is on, and the FORM the declaration takes — a hook stating every member and one
//! shrunk to the member it cannot be written without, an action on a recurrence and one on an
//! absolute window, a pool whose state stops its instances and one whose state keeps them
//! running under a reuse policy.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::auto_scaling_group_reads::describe_groups;
use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, member, router, xml_blocks, xml_text, REGION,
};

/// The stack every case of this suite deploys.
const STACK: &str = "asg-children";

/// How long [`settled`] waits for one stack operation to reach a terminal status, and how often
/// it looks.
///
/// What every case waits for is the CONDITION — a status that is not an operation in flight —
/// and nothing here asserts how long one took. The budget is this suite's own rather than the
/// shared waiter's because one case drives THREE operations over a stack whose resources sit
/// three dependency levels deep, each level owed its own transition span: that is the most
/// spans any one suite here waits through, so it is given the most room, far beyond what those
/// spans add up to even on a host running every suite of the workspace at once.
const SETTLE_DEADLINE: Duration = Duration::from_secs(300);
const SETTLE_POLL: Duration = Duration::from_millis(100);

/// Poll `DescribeStacks` the way a client's waiter does and answer the body of the first read
/// that reports `STACK` settled — or the empty body this service answers once a deletion has
/// completed.
async fn settled(router: &MultiServiceRouter, case: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", STACK)]).await;
        if code != 200 {
            return String::new();
        }
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.is_empty() && !reported.ends_with("_IN_PROGRESS") {
            return body;
        }
        if seen.last().map(String::as_str) != Some(reported.as_str()) {
            seen.push(reported);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {STACK} to settle, saw {seen:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The Availability Zone the groups place instances in, one of the Region these stacks deploy
/// in.
const ZONE: &str = "us-east-1a";

/// The two transitions a hook can act on, from the member's own documentation: "to create a
/// lifecycle hook for scale-out events, specify `autoscaling:EC2_INSTANCE_LAUNCHING`... for
/// scale-in events, specify `autoscaling:EC2_INSTANCE_TERMINATING`"
/// (`PutLifecycleHookType$LifecycleTransition`).
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// What a hook written WITHOUT a member carries, from those members' own documentation:
/// `HeartbeatTimeout` is "the maximum time, in seconds… The default value is 3600 seconds (1
/// hour)" and `DefaultResult` "the default value is ABANDON".
const DEFAULT_HEARTBEAT: &str = "3600";
const DEFAULT_RESULT: &str = "ABANDON";

/// The name the declaration states for the hook and the action it names, so a case can tell a
/// declared name from one CloudFormation decided.
const DECLARED_HOOK: &str = "asg-children-at-launch";
const DECLARED_ACTION: &str = "asg-children-nightly";

/// The launch template and the two groups every revision of the stack declares, so a revision
/// differs only in the CHILDREN it states.
fn groups() -> Value {
    let launch = json!({
        "LaunchTemplateId": {"Ref": "Lt"},
        "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
    });
    json!({
        "Lt": {
            "Type": "AWS::EC2::LaunchTemplate",
            "Properties": {
                "LaunchTemplateData": {"ImageId": IMAGE_ID, "InstanceType": "t3.micro"},
            },
        },
        "AsgA": {
            "Type": "AWS::AutoScaling::AutoScalingGroup",
            "Properties": {
                "MinSize": "0",
                "MaxSize": "2",
                "DesiredCapacity": "0",
                "AvailabilityZones": [ZONE],
                "LaunchTemplate": launch,
            },
        },
        "AsgB": {
            "Type": "AWS::AutoScaling::AutoScalingGroup",
            "Properties": {
                "MinSize": "0",
                "MaxSize": "1",
                "DesiredCapacity": "0",
                "AvailabilityZones": [ZONE],
                "LaunchTemplate": launch,
            },
        },
    })
}

/// The revision the stack is CREATED from: a named hook stating every member and an unnamed one
/// shrunk to the member a hook cannot be written without, a named action on a recurrence and an
/// unnamed one on an absolute window, and a warm pool on each group — one preparing no instance,
/// one preparing an instance under a reuse policy.
fn created() -> String {
    let mut resources = groups();
    let declared = json!({
        "NamedHook": {
            "Type": "AWS::AutoScaling::LifecycleHook",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "LifecycleHookName": DECLARED_HOOK,
                "LifecycleTransition": LAUNCHING,
                "HeartbeatTimeout": 300,
                "DefaultResult": "CONTINUE",
                "NotificationMetadata": "declared at create",
            },
        },
        "UnnamedHook": {
            "Type": "AWS::AutoScaling::LifecycleHook",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "LifecycleTransition": TERMINATING,
            },
        },
        "NamedAction": {
            "Type": "AWS::AutoScaling::ScheduledAction",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "ScheduledActionName": DECLARED_ACTION,
                "Recurrence": "0 1 * * *",
                "MinSize": 0,
                "MaxSize": 2,
                "DesiredCapacity": 1,
            },
        },
        "UnnamedAction": {
            "Type": "AWS::AutoScaling::ScheduledAction",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgB"},
                "StartTime": "2031-01-01T00:00:00Z",
                "EndTime": "2031-02-01T00:00:00Z",
                "MinSize": 1,
            },
        },
        "PoolA": {
            "Type": "AWS::AutoScaling::WarmPool",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "MinSize": 0,
                "MaxGroupPreparedCapacity": 0,
                "PoolState": "Stopped",
            },
        },
        "PoolB": {
            "Type": "AWS::AutoScaling::WarmPool",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgB"},
                "MinSize": 0,
                "MaxGroupPreparedCapacity": 1,
                "PoolState": "Running",
                "InstanceReusePolicy": {"ReuseOnScaleIn": true},
            },
        },
    });
    template(&mut resources, declared)
}

/// The revision the stack is UPDATED to: every member of the named hook, the named action and
/// the pool on the first group stated DIFFERENTLY, and the three children of the second group
/// and the second hook DROPPED.
fn revised() -> String {
    let mut resources = groups();
    let declared = json!({
        "NamedHook": {
            "Type": "AWS::AutoScaling::LifecycleHook",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "LifecycleHookName": DECLARED_HOOK,
                "LifecycleTransition": TERMINATING,
                "HeartbeatTimeout": 7200,
                "DefaultResult": "ABANDON",
                "NotificationMetadata": "changed by the revision",
            },
        },
        "NamedAction": {
            "Type": "AWS::AutoScaling::ScheduledAction",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "ScheduledActionName": DECLARED_ACTION,
                "Recurrence": "30 2 * * *",
                "MinSize": 1,
                "MaxSize": 2,
                "DesiredCapacity": 2,
            },
        },
        "PoolA": {
            "Type": "AWS::AutoScaling::WarmPool",
            "Properties": {
                "AutoScalingGroupName": {"Ref": "AsgA"},
                "MinSize": 0,
                "MaxGroupPreparedCapacity": 0,
                "PoolState": "Hibernated",
            },
        },
    });
    template(&mut resources, declared)
}

/// One revision's template: the shared groups, the children it declares, and the outputs that
/// publish every resource's `Ref` — which is the handle a client reads each child back by.
fn template(resources: &mut Value, declared: Value) -> String {
    let mut outputs = json!({});
    for (logical_id, resource) in declared
        .as_object()
        .expect("the declared children are an object")
    {
        resources[logical_id] = resource.clone();
    }
    for logical_id in resources
        .as_object()
        .expect("the resources are an object")
        .keys()
        .filter(|logical_id| *logical_id != "Lt")
    {
        outputs[logical_id] = json!({"Value": {"Ref": logical_id}});
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs,
    })
    .to_string()
}

/// Every output the settled stack published, as a map from `OutputKey` to `OutputValue`.
///
/// An entry with a key and no value is of no use to a client at all, so a value is asserted
/// PRESENT for every output the stack reports.
fn published(case: &str, described: &str) -> Vec<(String, String)> {
    let outputs = xml_blocks(described, "Outputs")
        .into_iter()
        .next()
        .unwrap_or_default();
    xml_blocks(&outputs, "member")
        .into_iter()
        .map(|entry| {
            let key = xml_text(&entry, "OutputKey").unwrap_or_else(|| {
                panic!("case {case:?}: every output must report its key, got {entry}")
            });
            let value = xml_text(&entry, "OutputValue").unwrap_or_else(|| {
                panic!("case {case:?}: output {key:?} must report a value, got {entry}")
            });
            (key, value)
        })
        .collect()
}

/// One output's value, by key.
fn output(case: &str, described: &str, key: &str) -> String {
    published(case, described)
        .into_iter()
        .find(|(reported, _)| reported == key)
        .map(|(_, value)| value)
        .unwrap_or_else(|| panic!("case {case:?}: no output named {key:?} in {described}"))
}

/// The lifecycle hooks Amazon EC2 Auto Scaling reports for `group` right now, each as
/// `(name, transition, heartbeat, result, metadata)`, sorted by name.
async fn reported_hooks(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<(String, String, String, String, String)> {
    let (status, body) = autoscaling(
        router,
        "DescribeLifecycleHooks",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeLifecycleHooks({group}) must answer 200, got {status} {body}"
    );
    let mut reported: Vec<(String, String, String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            assert_eq!(
                xml_text(entry, "AutoScalingGroupName").as_deref(),
                Some(group),
                "case {case:?}: every reported hook must belong to {group:?}, got {entry}"
            );
            (
                xml_text(entry, "LifecycleHookName").unwrap_or_default(),
                xml_text(entry, "LifecycleTransition").unwrap_or_default(),
                xml_text(entry, "HeartbeatTimeout").unwrap_or_default(),
                xml_text(entry, "DefaultResult").unwrap_or_default(),
                xml_text(entry, "NotificationMetadata").unwrap_or_default(),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// The scheduled actions that service reports for `group` right now, each as
/// `(name, recurrence, min, max, desired)`, sorted by name.
async fn reported_actions(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<(String, String, String, String, String)> {
    let (status, body) = autoscaling(
        router,
        "DescribeScheduledActions",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeScheduledActions({group}) must answer 200, got {status} {body}"
    );
    let mut reported: Vec<(String, String, String, String, String)> = xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            assert_eq!(
                xml_text(entry, "AutoScalingGroupName").as_deref(),
                Some(group),
                "case {case:?}: every reported action must belong to {group:?}, got {entry}"
            );
            (
                xml_text(entry, "ScheduledActionName").unwrap_or_default(),
                xml_text(entry, "Recurrence").unwrap_or_default(),
                xml_text(entry, "MinSize").unwrap_or_default(),
                xml_text(entry, "MaxSize").unwrap_or_default(),
                xml_text(entry, "DesiredCapacity").unwrap_or_default(),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// The warm pool that service reports for `group` right now — the body of its
/// `WarmPoolConfiguration`, or `None` where it reports no pool at all.
async fn reported_pool(case: &str, router: &MultiServiceRouter, group: &str) -> Option<String> {
    let (status, body) = autoscaling(
        router,
        "DescribeWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeWarmPool({group}) must answer 200, got {status} {body}"
    );
    xml_blocks(&body, "WarmPoolConfiguration").into_iter().next()
}

/// One member of a reported warm pool.
fn pool_member(case: &str, pool: &str, tag: &str) -> String {
    xml_text(pool, tag)
        .unwrap_or_else(|| panic!("case {case:?}: the pool must report its {tag}, got {pool}"))
}

#[tokio::test]
async fn every_child_a_stack_declares_on_a_group_is_the_one_that_service_reports() {
    let router = router();

    // The CREATE: every child the template declares is created in the owning service, under the
    // name the stack publishes for it.
    let case = "the create";
    let (status, body) = create_stack(&router, STACK, &created(), &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} {body}"
    );
    let described = settled(&router, case).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::CREATE_COMPLETE),
        "case {case:?}: expected the create to complete, got {described} with reasons {:?}",
        stack_event_reasons(&router, REGION, STACK).await
    );

    let group_a = output(case, &described, "AsgA");
    let group_b = output(case, &described, "AsgB");
    let named_hook = output(case, &described, "NamedHook");
    let unnamed_hook = output(case, &described, "UnnamedHook");
    let named_action = output(case, &described, "NamedAction");
    let unnamed_action = output(case, &described, "UnnamedAction");

    // A child whose declaration NAMES it is published under that name; one that leaves the name
    // to CloudFormation is published under a name it decided, which is not the logical id — and
    // WHICH name that is, is pinned by the owning service answering for that exact value just
    // below.
    assert_eq!(
        (named_hook.as_str(), named_action.as_str()),
        (DECLARED_HOOK, DECLARED_ACTION),
        "case {case:?}: expected the declared names, got {named_hook:?} and {named_action:?}"
    );
    for (logical_id, generated) in [("UnnamedHook", &unnamed_hook), ("UnnamedAction", &unnamed_action)]
    {
        assert!(
            !generated.is_empty() && generated != logical_id,
            "case {case:?}: expected a name CloudFormation generated for {logical_id}, got \
             {generated:?}"
        );
    }

    assert_eq!(
        reported_hooks(case, &router, &group_a).await,
        {
            let mut wanted = vec![
                (
                    named_hook.clone(),
                    LAUNCHING.to_string(),
                    "300".to_string(),
                    "CONTINUE".to_string(),
                    "declared at create".to_string(),
                ),
                // The hook shrunk to the one member it cannot be written without carries what a
                // hook written without the others carries, not values invented for it.
                (
                    unnamed_hook.clone(),
                    TERMINATING.to_string(),
                    DEFAULT_HEARTBEAT.to_string(),
                    DEFAULT_RESULT.to_string(),
                    String::new(),
                ),
            ];
            wanted.sort();
            wanted
        },
        "case {case:?}: expected the group to carry both declared hooks"
    );
    assert_eq!(
        reported_actions(case, &router, &group_a).await,
        vec![(
            named_action.clone(),
            "0 1 * * *".to_string(),
            "0".to_string(),
            "2".to_string(),
            "1".to_string(),
        )],
        "case {case:?}: expected the group to carry the declared recurring action"
    );
    // The action on the OTHER group is that group's alone, and an action on an absolute window
    // states no recurrence at all.
    assert_eq!(
        reported_actions(case, &router, &group_b).await,
        vec![(
            unnamed_action.clone(),
            String::new(),
            "1".to_string(),
            String::new(),
            String::new(),
        )],
        "case {case:?}: expected the second group to carry the declared windowed action"
    );

    let pool_a = reported_pool(case, &router, &group_a)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a warm pool on {group_a}"));
    assert_eq!(
        (
            pool_member(case, &pool_a, "PoolState"),
            pool_member(case, &pool_a, "MinSize"),
        ),
        ("Stopped".to_string(), "0".to_string()),
        "case {case:?}: expected the declared pool, got {pool_a}"
    );
    let pool_b = reported_pool(case, &router, &group_b)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected a warm pool on {group_b}"));
    assert_eq!(
        (
            pool_member(case, &pool_b, "PoolState"),
            pool_member(case, &pool_b, "MinSize"),
            pool_member(case, &pool_b, "MaxGroupPreparedCapacity"),
            pool_member(case, &pool_b, "ReuseOnScaleIn"),
        ),
        (
            "Running".to_string(),
            "0".to_string(),
            "1".to_string(),
            "true".to_string()
        ),
        "case {case:?}: expected the declared pool with its reuse policy, got {pool_b}"
    );

    // The UPDATE: a revision that states a child differently reaches it through the same call
    // that created it — so the child keeps its name — and a revision that DROPS one removes it
    // from the owning service while the group it was on stays.
    let case = "the update";
    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", STACK),
            member("TemplateBody", &revised()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: UpdateStack must answer 200, got {status} {body}"
    );
    let described = settled(&router, case).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(status::UPDATE_COMPLETE),
        "case {case:?}: expected the update to complete, got {described} with reasons {:?}",
        stack_event_reasons(&router, REGION, STACK).await
    );
    assert_eq!(
        (
            output(case, &described, "NamedHook"),
            output(case, &described, "NamedAction"),
            output(case, &described, "AsgA"),
        ),
        (named_hook.clone(), named_action.clone(), group_a.clone()),
        "case {case:?}: expected every kept resource to keep the identity the stack published"
    );

    assert_eq!(
        reported_hooks(case, &router, &group_a).await,
        vec![(
            named_hook.clone(),
            TERMINATING.to_string(),
            "7200".to_string(),
            "ABANDON".to_string(),
            "changed by the revision".to_string(),
        )],
        "case {case:?}: expected the one kept hook, carrying what the revision declares"
    );
    assert_eq!(
        reported_actions(case, &router, &group_a).await,
        vec![(
            named_action.clone(),
            "30 2 * * *".to_string(),
            "1".to_string(),
            "2".to_string(),
            "2".to_string(),
        )],
        "case {case:?}: expected the kept action to carry what the revision declares"
    );
    assert!(
        reported_actions(case, &router, &group_b).await.is_empty(),
        "case {case:?}: expected the dropped action to be gone from {group_b}"
    );

    let pool_a = reported_pool(case, &router, &group_a)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: expected the kept pool on {group_a}"));
    assert_eq!(
        pool_member(case, &pool_a, "PoolState"),
        "Hibernated".to_string(),
        "case {case:?}: expected the kept pool to carry the revised state, got {pool_a}"
    );
    // The pool the revision dropped is gone from the group it was beside, and that group — which
    // the revision keeps — still answers the read.
    assert_eq!(
        reported_pool(case, &router, &group_b).await,
        None,
        "case {case:?}: expected the dropped pool to be gone from {group_b}"
    );

    // The DELETE: the stack takes its whole estate down, children included.
    let case = "the delete";
    let (status, body) = call(&router, "DeleteStack", &[member("StackName", STACK)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteStack must answer 200, got {status} {body}"
    );
    let settled = settled(&router, case).await;
    assert!(
        settled.is_empty()
            || xml_text(&settled, "StackStatus").as_deref() == Some(status::DELETE_COMPLETE),
        "case {case:?}: expected the delete to complete, got {settled} with reasons {:?}",
        stack_event_reasons(&router, REGION, STACK).await
    );
    assert!(
        reported_hooks(case, &router, &group_a).await.is_empty(),
        "case {case:?}: expected no hook to survive the stack's deletion"
    );
    // ...and neither group is there any more, so nothing of the estate the children hung off
    // outlived the stack that declared it.
    let (body, names) = describe_groups(case, &router, &[&group_a, &group_b]).await;
    assert!(
        names.is_empty(),
        "case {case:?}: expected both groups to be gone, got {names:?} in {body}"
    );
}
