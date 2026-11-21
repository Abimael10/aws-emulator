// SPDX-License-Identifier: CC0-1.0

//! The TAGS of an `AWS::AutoScaling::AutoScalingGroup` a stack deployed, across the stack
//! UPDATE that declares a different set of them.
//!
//! RULE under test: **a resource's tag set is carried by no configuration-update call of the
//! owning service, so a deployment that declares it differently applies it IN PLACE through the
//! pair of calls that service publishes for the tags of a resource that already exists — the
//! stack reaches `UPDATE_COMPLETE`, the resource keeps the identity the stack published for it,
//! and the owning service's own read reports exactly the tags the revision declares.** Here the
//! owning service is Amazon EC2 Auto Scaling: `UpdateAutoScalingGroup` has no `Tags` member at
//! all, while `CreateOrUpdateTags` "creates or updates tags for the specified Auto Scaling
//! group" and `DeleteTags` "deletes the specified tags" — so a revised `Tags` property reaches
//! that service as those two calls and `DescribeAutoScalingGroups` answers with the tags the
//! revision asked for, each carrying the `PropagateAtLaunch` its declaration states.
//!
//! Why it matters: tagging an Auto Scaling group is the common case for a template that
//! declares one (cost allocation, ownership, anything keying off a tag), and a tag value is
//! exactly the kind of thing a deployment revises. An update that refuses the change instead of
//! applying it rolls the whole stack back, discards the tag change, and leaves every later
//! operation starting from a rolled-back stack.
//!
//! The rows differ along what the rule generalises over — how the declared SET changes and the
//! FORM the revision states it in: a value changed beside a member the group's own update call
//! carries, a tag added, one of two removed with the other kept, a tag renamed, the propagation
//! flag flipped with the value left alone, a value emptied, every declared tag removed by an
//! empty list and by dropping the property, tags declared on a group created with none, and the
//! same set restated so the deployment owes no tagging call at all.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::auto_scaling_group_reads::described_group;
use crate::harness::stack_event_reasons;
use crate::stack_lifecycle::{
    call, create_stack, member, router_with_engine, settled_stack, xml_blocks, xml_text, REGION,
};

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The Availability Zone the groups place instances in — one of the Region these stacks deploy
/// in. The groups launch nothing (`MinSize` is zero), because what the rows are about is the tag
/// set of the group rather than its capacity.
const ZONE: &str = "us-east-1a";

/// The `MaxSize` every row's create declares, and the one the rows that also change a member of
/// the group's own update call revise it to.
const CREATED_MAX_SIZE: u32 = 1;
const REVISED_MAX_SIZE: u32 = 2;

/// The three keys CloudFormation records on every resource it provisions — the stack's name, the
/// stack's id and the resource's logical id — which no declaration states and which every row
/// expects the group to go on carrying across the update.
///
/// Source: `crate::state::provisioning`'s `STACK_NAME_TAG`, `STACK_ID_TAG` and `LOGICAL_ID_TAG`.
const PROVENANCE_KEYS: &[&str] = &[
    "aws:cloudformation:stack-name",
    "aws:cloudformation:stack-id",
    "aws:cloudformation:logical-id",
];

/// One declared tag of the group's `Tags` property: the key, the value, and the
/// `PropagateAtLaunch` the declaration states — "determines whether the tag is added to new
/// instances as they are launched in the group" (`Tag$PropagateAtLaunch`).
#[derive(Debug, Clone, Copy)]
struct Tag {
    key: &'static str,
    value: &'static str,
    propagate: bool,
}

const ENV_BLUE: Tag = Tag {
    key: "environment",
    value: "blue",
    propagate: true,
};

/// The same tag — same key, so the same tag of the group — with another value.
const ENV_GREEN: Tag = Tag {
    key: "environment",
    value: "green",
    propagate: true,
};

/// The same tag again, with the value a `TagValue` may be at its documented minimum length
/// (`length: min 0`): the empty string is a value, not the absence of a tag.
const ENV_EMPTY: Tag = Tag {
    key: "environment",
    value: "",
    propagate: true,
};

/// The same key and value, with the propagation decision the other way round: the one member of
/// an Auto Scaling group's tag that no other service's tag carries.
const ENV_BLUE_UNPROPAGATED: Tag = Tag {
    key: "environment",
    value: "blue",
    propagate: false,
};

/// A tag under ANOTHER key, which is a tag of its own.
const TEAM: Tag = Tag {
    key: "team",
    value: "platform",
    propagate: false,
};

/// The first tag under a DIFFERENT key: a rename is a tag removed and a tag added.
const ENV_RENAMED: Tag = Tag {
    key: "stage",
    value: "blue",
    propagate: true,
};

/// One row: the stack it deploys, the group it declares, the tag list the create and the
/// revision state, and the `MaxSize` the revision declares. `None` is the property not stated at
/// all, which is a different declaration from the empty list.
struct Row {
    case: &'static str,
    stack: &'static str,
    group: &'static str,
    created: Option<&'static [Tag]>,
    revised: Option<&'static [Tag]>,
    revised_max_size: u32,
}

const ROWS: &[Row] = &[
    Row {
        case: "a tag value changed beside the MaxSize the group's own update call carries",
        stack: "asg-tags-revalued",
        group: "asg-tags-revalued-group",
        created: Some(&[ENV_BLUE]),
        revised: Some(&[ENV_GREEN]),
        revised_max_size: REVISED_MAX_SIZE,
    },
    Row {
        case: "a tag added beside the one the group was created with",
        stack: "asg-tags-added",
        group: "asg-tags-added-group",
        created: Some(&[ENV_BLUE]),
        revised: Some(&[ENV_BLUE, TEAM]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "one of two tags removed, the other kept",
        stack: "asg-tags-removed-one",
        group: "asg-tags-removed-one-group",
        created: Some(&[ENV_BLUE, TEAM]),
        revised: Some(&[TEAM]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the one tag renamed, which is one tag removed and another added",
        stack: "asg-tags-renamed",
        group: "asg-tags-renamed-group",
        created: Some(&[ENV_BLUE]),
        revised: Some(&[ENV_RENAMED]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the one tag's PropagateAtLaunch flipped, its value left alone",
        stack: "asg-tags-repropagated",
        group: "asg-tags-repropagated-group",
        created: Some(&[ENV_BLUE]),
        revised: Some(&[ENV_BLUE_UNPROPAGATED]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the one tag's value emptied, which is a value and not the absence of a tag",
        stack: "asg-tags-emptied-value",
        group: "asg-tags-emptied-value-group",
        created: Some(&[ENV_BLUE]),
        revised: Some(&[ENV_EMPTY]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "every declared tag removed, the property stated as an empty list",
        stack: "asg-tags-emptied",
        group: "asg-tags-emptied-group",
        created: Some(&[ENV_BLUE, TEAM]),
        revised: Some(&[]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "every declared tag removed, the property no longer stated at all",
        stack: "asg-tags-dropped",
        group: "asg-tags-dropped-group",
        created: Some(&[ENV_BLUE]),
        revised: None,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "tags declared on a group that was created with none",
        stack: "asg-tags-introduced",
        group: "asg-tags-introduced-group",
        created: None,
        revised: Some(&[ENV_BLUE, TEAM]),
        revised_max_size: CREATED_MAX_SIZE,
    },
    Row {
        case: "the same tags restated, only the MaxSize revised",
        stack: "asg-tags-restated",
        group: "asg-tags-restated-group",
        created: Some(&[ENV_BLUE, TEAM]),
        revised: Some(&[ENV_BLUE, TEAM]),
        revised_max_size: REVISED_MAX_SIZE,
    },
];

/// The row's template at one revision: the launch template the group launches from, and the
/// group with the tag list and the `MaxSize` this revision declares.
fn template(row: &Row, tags: Option<&[Tag]>, max_size: u32) -> String {
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
    if let Some(tags) = tags {
        let declared: Vec<Value> = tags
            .iter()
            .map(|tag| {
                json!({
                    "Key": tag.key,
                    "Value": tag.value,
                    "PropagateAtLaunch": tag.propagate,
                })
            })
            .collect();
        group["Tags"] = Value::Array(declared);
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

/// One tag as Amazon EC2 Auto Scaling reports it, in the text its answer carries each member as.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct ReportedTag {
    key: String,
    value: String,
    propagate: String,
}

/// The tags that service reports for the group right now, LESS the three CloudFormation records
/// itself — the read a client takes with the group name a completed deployment handed it.
///
/// `DescribeAutoScalingGroups` carries "the tags for the Auto Scaling group" as that group's own
/// `Tags` list, so the list is taken as a BLOCK first and its `<member>` entries read inside it:
/// the answer carries other `<member>` lists (the group's Availability Zones among them) that a
/// flat scan would sweep up. A member the answer leaves empty is that member's empty value.
async fn reported_tags(case: &str, router: &MultiServiceRouter, group: &str) -> Vec<ReportedTag> {
    let body = described_group(case, router, group).await;
    let list = xml_blocks(&body, "Tags").into_iter().next().unwrap_or_default();
    let mut reported: Vec<ReportedTag> = xml_blocks(&list, "member")
        .iter()
        .map(|entry| {
            // Every tag of a group reports the group it is on, so a tag answered for another
            // group could not pass for one of these.
            assert_eq!(
                xml_text(entry, "ResourceId").as_deref(),
                Some(group),
                "case {case:?}: expected every reported tag to be on {group:?}, got {entry}"
            );
            assert_eq!(
                xml_text(entry, "ResourceType").as_deref(),
                Some("auto-scaling-group"),
                "case {case:?}: expected every reported tag to name the one resource type this \
                 service tags, got {entry}"
            );
            ReportedTag {
                key: xml_text(entry, "Key").unwrap_or_default(),
                value: xml_text(entry, "Value").unwrap_or_default(),
                propagate: xml_text(entry, "PropagateAtLaunch").unwrap_or_default(),
            }
        })
        .collect();
    let held: BTreeSet<&str> = reported.iter().map(|tag| tag.key.as_str()).collect();
    for key in PROVENANCE_KEYS {
        assert!(
            held.contains(key),
            "case {case:?}: expected the group to go on carrying CloudFormation's own {key:?} \
             record, got {reported:?}"
        );
    }
    reported.retain(|tag| !PROVENANCE_KEYS.contains(&tag.key.as_str()));
    reported.sort();
    reported
}

/// The tags a declaration asks the group to end up carrying, in the order the read above answers
/// in: each key with the value and the propagation decision its declaration states.
fn wanted_tags(tags: Option<&[Tag]>) -> Vec<ReportedTag> {
    let mut wanted: Vec<ReportedTag> = tags
        .unwrap_or(&[])
        .iter()
        .map(|tag| ReportedTag {
            key: tag.key.to_string(),
            value: tag.value.to_string(),
            propagate: tag.propagate.to_string(),
        })
        .collect();
    wanted.sort();
    wanted
}

#[tokio::test]
async fn an_update_applies_the_declared_tags_to_the_group_it_deployed() {
    let (router, _engine) = router_with_engine();

    // Every row's stack is created first and settled afterwards: the groups live in ONE
    // emulator, so a read for one group's tags has other groups' tags to confuse it with.
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
        // The tags the CREATE asked for are the group's, which is what makes the update's
        // reconciliation observable at all.
        assert_eq!(
            reported_tags(case, &router, row.group).await,
            wanted_tags(row.created),
            "case {case:?}: expected the group to carry the tags the create declared"
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

        // ...and the tags are exactly the ones the revision declares, each carrying the value
        // and the propagation decision it states.
        assert_eq!(
            reported_tags(case, &router, row.group).await,
            wanted_tags(row.revised),
            "case {case:?}: expected the group to carry the tags the revision declares"
        );
    }
}
