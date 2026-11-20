// SPDX-License-Identifier: CC0-1.0

//! The LIFECYCLE HOOKS of an `AWS::AutoScaling::AutoScalingGroup` a stack deployed, across the
//! stack UPDATE that states a different set of them.
//!
//! RULE under test: **a property of a deployed resource that the owning service changes
//! through a call of its own is applied IN PLACE by the update, through that call — so the
//! stack reaches `UPDATE_COMPLETE`, the resource keeps the identity the stack published for it,
//! and the owning service's own read reports exactly what the revision declares.** The property
//! here is the group's `LifecycleHookSpecificationList`: Amazon EC2 Auto Scaling writes and
//! removes ONE hook of a group that exists (`PutLifecycleHook` "creates or updates a lifecycle
//! hook for the specified Auto Scaling group", `DeleteLifecycleHook` "deletes the specified
//! lifecycle hook"), so a revision that states a different list reaches that service as those
//! calls and `DescribeLifecycleHooks` answers with the hooks the revision asked for.
//!
//! Why it matters: lifecycle hooks are how a client wires its own work into a group's scale-out
//! and scale-in (an instance is held in a wait state until the hook is completed), so they are
//! evolved over the life of the deployment. An update that rolls back instead of applying them
//! leaves the client unable to change the hooks of a group it already deployed at all — and the
//! rollback reports the group as unchanged while the stack's own revision says otherwise.
//!
//! The rows differ along what the rule generalises over — how the declared SET changes and the
//! FORM the revision states it in: a hook added beside the one the group carries, one removed
//! with another kept, the members of the one hook changed (beside a change the group's own
//! update call carries, so both paths run in one deployment), the one hook's specification
//! SHRUNK to the member it cannot be written without, a hook renamed, every hook removed by an
//! empty list and by dropping the property, and hooks declared on a group created with none.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::auto_scaling_group_reads::described_group;
use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, member, router_with_engine, settled_stack, xml_blocks,
    xml_text, REGION,
};

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The Availability Zone the groups place instances in — one of the Region these stacks deploy
/// in. The groups launch nothing (`MinSize` is zero), because what the rows are about is a
/// property of the group rather than its capacity.
const ZONE: &str = "us-east-1a";

/// The two transitions a hook can act on, from the member's own documentation: "to create a
/// lifecycle hook for scale-out events, specify `autoscaling:EC2_INSTANCE_LAUNCHING`... for
/// scale-in events, specify `autoscaling:EC2_INSTANCE_TERMINATING`"
/// (`LifecycleHookSpecification$LifecycleTransition`).
const LAUNCHING: &str = "autoscaling:EC2_INSTANCE_LAUNCHING";
const TERMINATING: &str = "autoscaling:EC2_INSTANCE_TERMINATING";

/// One declared lifecycle hook. A member stated as `None` is one the specification does NOT
/// carry, which is a declaration of its own: the hook then carries the value a hook written
/// without that member carries, so a revision that DROPS a member is as observable as one that
/// changes it.
#[derive(Debug, Clone, Copy)]
struct Hook {
    /// `LifecycleHookName`, whose pattern admits letters, digits, `-`, `_` and `/`.
    name: &'static str,
    transition: &'static str,
    /// `HeartbeatTimeout`: "the range is from 30 to 7200 seconds. The default value is 3600
    /// seconds (1 hour)" (`LifecycleHookSpecification$HeartbeatTimeout`), so the rows below use
    /// both published bounds, a value between them, and none at all.
    heartbeat: Option<u32>,
    /// `DefaultResult`: "valid values: `CONTINUE` | `ABANDON`"; "the default value is
    /// `ABANDON`".
    result: Option<&'static str>,
    metadata: Option<&'static str>,
}

/// What a hook carries for a member its specification does not state, from those members' own
/// documentation quoted above: a hook written without a heartbeat times out after an hour, and
/// one written without a default result abandons the instance.
const DEFAULT_HEARTBEAT: u32 = 3600;
const DEFAULT_RESULT: &str = "ABANDON";

/// The hook a group is created with in most rows.
const AT_LAUNCH: Hook = Hook {
    name: "at-launch",
    transition: LAUNCHING,
    heartbeat: Some(300),
    result: Some("CONTINUE"),
    metadata: Some("declared at create"),
};

/// A hook on the OTHER transition, at the documented minimum heartbeat.
const AT_TERMINATE: Hook = Hook {
    name: "at-terminate",
    transition: TERMINATING,
    heartbeat: Some(30),
    result: Some("ABANDON"),
    metadata: Some("the second hook"),
};

/// The same hook as [`AT_LAUNCH`] — same name, so the same hook of the group — with every other
/// member changed, the heartbeat at the documented maximum.
const AT_LAUNCH_CHANGED: Hook = Hook {
    name: "at-launch",
    transition: TERMINATING,
    heartbeat: Some(7200),
    result: Some("ABANDON"),
    metadata: Some("changed by the revision"),
};

/// The same hook again, SHRUNK to the members a specification cannot be written without: the
/// revision no longer stating the heartbeat, the result or the metadata asks for a hook
/// carrying what a hook written without them carries, not for the values it was given.
const AT_LAUNCH_SHRUNK: Hook = Hook {
    name: "at-launch",
    transition: LAUNCHING,
    heartbeat: None,
    result: None,
    metadata: None,
};

/// A hook the revision names differently, which is a hook of its own.
const AT_LAUNCH_RENAMED: Hook = Hook {
    name: "at-launch/renamed",
    transition: LAUNCHING,
    heartbeat: Some(300),
    result: Some("CONTINUE"),
    metadata: Some("declared at create"),
};

/// The `MaxSize` every row's create declares, and the one the row that also changes a property
/// of the GROUP revises it to.
const CREATED_MAX_SIZE: u32 = 2;
const REVISED_MAX_SIZE: u32 = 3;

/// One row: the stack it deploys, the group it declares, and the hook list the create and the
/// revision state. `None` is the property not stated at all, which is a different declaration
/// from the empty list.
struct Row {
    case: &'static str,
    stack: &'static str,
    group: &'static str,
    created: Option<&'static [Hook]>,
    revised: Option<&'static [Hook]>,
    /// The `MaxSize` the revision declares, so one row changes a property the group's own
    /// update call carries beside its hooks.
    revised_max_size: u32,
}

const ROWS: &[Row] = &[
    Row {
        case: "a hook added beside the one the group was created with",
        stack: "asg-hooks-added",
        group: "asg-hooks-added-group",
        created: Some(&[AT_LAUNCH]),
        revised: Some(&[AT_LAUNCH, AT_TERMINATE]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "one of two hooks removed, the other kept",
        stack: "asg-hooks-removed-one",
        group: "asg-hooks-removed-one-group",
        created: Some(&[AT_LAUNCH, AT_TERMINATE]),
        revised: Some(&[AT_TERMINATE]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the one hook's members changed, beside a change the group's own update carries",
        stack: "asg-hooks-changed",
        group: "asg-hooks-changed-group",
        created: Some(&[AT_LAUNCH]),
        revised: Some(&[AT_LAUNCH_CHANGED]),
        revised_max_size: REVISED_MAX_SIZE,
    },
    Row {
        case: "the one hook renamed",
        stack: "asg-hooks-renamed",
        group: "asg-hooks-renamed-group",
        created: Some(&[AT_LAUNCH]),
        revised: Some(&[AT_LAUNCH_RENAMED]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "every hook removed, the property stated as an empty list",
        stack: "asg-hooks-emptied",
        group: "asg-hooks-emptied-group",
        created: Some(&[AT_LAUNCH, AT_TERMINATE]),
        revised: Some(&[]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "every hook removed, the property no longer stated at all",
        stack: "asg-hooks-dropped",
        group: "asg-hooks-dropped-group",
        created: Some(&[AT_LAUNCH]),
        revised: None,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "hooks declared on a group that was created with none",
        stack: "asg-hooks-introduced",
        group: "asg-hooks-introduced-group",
        created: None,
        revised: Some(&[AT_LAUNCH, AT_TERMINATE]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the one hook's specification shrunk to the member it cannot be written without",
        stack: "asg-hooks-shrunk",
        group: "asg-hooks-shrunk-group",
        created: Some(&[AT_LAUNCH]),
        revised: Some(&[AT_LAUNCH_SHRUNK]),
        revised_max_size: CREATED_MAX_SIZE,
    },
];

/// One hook as a `LifecycleHookSpecification` of the type's own list: the members this
/// specification STATES, and no entry at all for one it does not.
fn declared(hook: &Hook) -> Value {
    let mut specification = json!({
        "LifecycleHookName": hook.name,
        "LifecycleTransition": hook.transition,
    });
    if let Some(heartbeat) = hook.heartbeat {
        specification["HeartbeatTimeout"] = json!(heartbeat);
    }
    if let Some(result) = hook.result {
        specification["DefaultResult"] = json!(result);
    }
    if let Some(metadata) = hook.metadata {
        specification["NotificationMetadata"] = json!(metadata);
    }
    specification
}

/// The row's template at one revision: the launch template the group launches from, and the
/// group with the hook list this revision declares.
fn template(row: &Row, hooks: Option<&[Hook]>, max_size: u32) -> String {
    let mut group = json!({
        "AutoScalingGroupName": row.group,
        "AvailabilityZones": [ZONE],
        "MinSize": "0",
        "MaxSize": max_size.to_string(),
        "DesiredCapacity": "0",
        "LaunchTemplate": {
            "LaunchTemplateId": {"Ref": "Lt"},
            "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
        },
    });
    if let Some(hooks) = hooks {
        let declared: Vec<Value> = hooks.iter().map(declared).collect();
        group["LifecycleHookSpecificationList"] = Value::Array(declared);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Lt": {
                "Type": "AWS::EC2::LaunchTemplate",
                "Properties": {
                    "LaunchTemplateData": {"ImageId": IMAGE_ID, "InstanceType": "t3.micro"},
                },
            },
            "Asg": {
                "Type": "AWS::AutoScaling::AutoScalingGroup",
                "Properties": group,
            },
        },
    })
    .to_string()
}

/// One hook as Amazon EC2 Auto Scaling reports it: the members a specification states, in the
/// text that service's answer carries them as.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReportedHook {
    name: String,
    transition: String,
    heartbeat: String,
    result: String,
    metadata: String,
}

/// The hooks that service reports for the group right now, by name — the read a client takes
/// with the group name a completed deployment handed it.
async fn reported_hooks(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<ReportedHook> {
    let (status_code, body) = autoscaling(
        router,
        "DescribeLifecycleHooks",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeLifecycleHooks({group}) must answer 200, got {status_code} {body}"
    );
    let mut reported: Vec<ReportedHook> = xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            // Every hook of a group reports the group it belongs to, so a hook answered for
            // another group could not pass for one of these.
            assert_eq!(
                xml_text(entry, "AutoScalingGroupName").as_deref(),
                Some(group),
                "case {case:?}: expected every reported hook to belong to {group:?}, got {entry}"
            );
            ReportedHook {
                name: xml_text(entry, "LifecycleHookName").unwrap_or_default(),
                transition: xml_text(entry, "LifecycleTransition").unwrap_or_default(),
                heartbeat: xml_text(entry, "HeartbeatTimeout").unwrap_or_default(),
                result: xml_text(entry, "DefaultResult").unwrap_or_default(),
                metadata: xml_text(entry, "NotificationMetadata").unwrap_or_default(),
            }
        })
        .collect();
    reported.sort();
    reported
}

/// The hooks a declaration asks the group to end up with, in the same order the read above
/// answers in: each member the specification states, and what a hook written without a member
/// carries for the ones it does not ([`DEFAULT_HEARTBEAT`], [`DEFAULT_RESULT`], and no
/// metadata at all).
fn wanted_hooks(hooks: Option<&[Hook]>) -> Vec<ReportedHook> {
    let mut wanted: Vec<ReportedHook> = hooks
        .unwrap_or(&[])
        .iter()
        .map(|hook| ReportedHook {
            name: hook.name.to_string(),
            transition: hook.transition.to_string(),
            heartbeat: hook.heartbeat.unwrap_or(DEFAULT_HEARTBEAT).to_string(),
            result: hook.result.unwrap_or(DEFAULT_RESULT).to_string(),
            metadata: hook.metadata.unwrap_or_default().to_string(),
        })
        .collect();
    wanted.sort();
    wanted
}

#[tokio::test]
async fn an_update_applies_the_declared_lifecycle_hooks_to_the_group_it_deployed() {
    let (router, _engine) = router_with_engine();

    // Every row's stack is created first and settled afterwards: the groups live in ONE
    // emulator, so a read for one group's hooks has other groups' hooks to confuse it with.
    for row in ROWS {
        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template(row, row.created, CREATED_MAX_SIZE),
            &[],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {body}",
            row.case
        );
    }

    for row in ROWS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the stack to deploy, got {described} (reasons {:?})",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        // The hooks the CREATE asked for are the group's, which is what makes the update's
        // reconciliation observable at all.
        assert_eq!(
            reported_hooks(case, &router, row.group).await,
            wanted_hooks(row.created),
            "case {case:?}: expected the group to carry the hooks the create declared"
        );
    }

    for row in ROWS {
        let (status_code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(row, row.revised, row.revised_max_size),
                ),
            ],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: UpdateStack must answer 200, got {status_code} {body}",
            row.case
        );
    }

    for row in ROWS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::UPDATE_COMPLETE),
            "case {case:?}: expected the update to complete, got {described} (reasons {:?})",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        // The group kept the identity the stack published for it — the name it was created
        // under, which Amazon EC2 Auto Scaling still answers for — and the member the group's
        // own update call carries landed on it.
        let group = described_group(case, &router, row.group).await;
        assert_eq!(
            xml_text(&group, "MaxSize").as_deref(),
            Some(row.revised_max_size.to_string().as_str()),
            "case {case:?}: expected the group to run at the revised MaxSize {}, got {group}",
            row.revised_max_size
        );

        // ...and the hooks are exactly the ones the revision declares, each carrying the
        // members it states.
        assert_eq!(
            reported_hooks(case, &router, row.group).await,
            wanted_hooks(row.revised),
            "case {case:?}: expected the group to carry the hooks the revision declares"
        );
    }
}
