// SPDX-License-Identifier: CC0-1.0

//! The `NotificationConfigurations` of an `AWS::AutoScaling::AutoScalingGroup` a stack
//! deploys, created, revised and removed through Amazon EC2 Auto Scaling's own calls.
//!
//! RULE under test: **a declared property the owning service provisions through a call of its
//! own, rather than as a member of the resource's create, is handed to that service as THAT
//! call — one per declared entry, after the resource itself exists — so the stack reaches
//! `CREATE_COMPLETE` and the owning service's own read reports exactly what the declaration
//! states; a revision is applied in place through the same pair of calls; and a call that
//! service refuses fails the resource with THAT service's sentence inside its SDK's envelope,
//! leaving behind what the calls already made.** The property here is the group's
//! `NotificationConfigurations`: Amazon EC2 Auto Scaling configures the notifications of a
//! group that exists (`PutNotificationConfiguration` "configures an Auto Scaling group to send
//! notifications when specified events take place […] this configuration overwrites any
//! existing configuration", `DeleteNotificationConfiguration` "deletes the specified
//! notification"), so each declared entry reaches that service as one such call and
//! `DescribeNotificationConfigurations` answers with one (`TopicARN`, `NotificationType`) pair
//! per declared type.
//!
//! Why it matters: notifications are how a client learns that a group it deployed launched or
//! terminated an instance, so a template that declares them is the normal way to deploy a group
//! at all. A deployment that drops the property leaves the group silent while the stack reports
//! it as created; one that fails the resource over it cannot deploy the template AWS accepts.
//! And when the owning service refuses one entry, the failed resource's own
//! `ResourceStatusReason` is the only channel that says WHICH value it refused — the channel
//! every consumer of a failed deployment parses.
//!
//! The rows differ along what the rule generalises over: how many entries one declaration
//! states and how many event types each names, which published type they name, whether a type
//! repeats, the FORM the declaration takes (a list, one entry spelled without the sequence
//! syntax, the empty list, the property absent), how a revision changes the set (a type added,
//! the set narrowed, a topic added, one of two topics removed, the topic exchanged, a topic
//! named by two entries and then no longer declared, every entry removed by an empty list and
//! by dropping the property, entries introduced on a group deployed with none), and — for the
//! refusal — WHAT the owning service refuses (an event type
//! outside the catalog it publishes, a `TopicARN` naming no topic) and whether a valid entry was
//! configured ahead of it.
//!
//! The event types the rows name are read back from `DescribeAutoScalingNotificationTypes` over
//! that service's own wire before they are used, so no row can pass on a type this emulator and
//! this suite agree on but that service never publishes; every accepted row reads the group's
//! configurations back through `DescribeNotificationConfigurations`, so none can pass on an
//! accepted-but-unprovisioned declaration.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

use crate::auto_scaling_group_reads::described_group;
use crate::harness::{
    assert_sdk_diagnostic_with_attempts, failed_resource_reason, quoted_and_class,
    stack_event_reasons,
};
use crate::stack_lifecycle::{
    autoscaling, call, create_stack, member, router, settled_stack, sns, xml_blocks, xml_text,
    xml_texts, REGION,
};

/// The AMI the launch template's data names, in Amazon EC2's own id form (`ami-` plus 17
/// hexadecimal characters).
const IMAGE_ID: &str = "ami-00000000000000001";

/// The Availability Zone the groups place instances in — one of the Region these stacks deploy
/// in. The groups launch nothing (`MinSize` is zero), because what the rows are about is a
/// property of the group rather than its capacity.
const ZONE: &str = "us-east-1a";

/// The SDK's own name for Amazon EC2 Auto Scaling, as the exception its SDK raises states it:
/// that model's `awsService.cloudFormationName`.
const AUTO_SCALING_SERVICE: &str = "AutoScaling";

/// The status that service answers a call it will not carry out with, and the
/// `HandlerErrorCode` class a refusal of the REQUEST falls in — one of the model's own
/// `HandlerErrorCode` values.
const REFUSED_STATUS: u16 = 400;
const INVALID_REQUEST: &str = "InvalidRequest";

/// Four event types a group can be configured to notify about, from the catalog
/// `DescribeAutoScalingNotificationTypes` publishes and `NotificationConfiguration$NotificationType`
/// documents
/// (`smithy_query(query="doc",shape="NotificationConfiguration",member="NotificationType",
/// model_path="services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/smithy-model/\
/// com-amazonaws-autoscaling-autoscaling-2011-01-01.json")`). Spelled out here so a wrong
/// constant in the implementation cannot make a row pass, and each one is asserted to be in the
/// catalog that service publishes before any row uses it ([`published_types`]).
const TEST_NOTIFICATION: &str = "autoscaling:TEST_NOTIFICATION";
const INSTANCE_LAUNCH: &str = "autoscaling:EC2_INSTANCE_LAUNCH";
const INSTANCE_LAUNCH_ERROR: &str = "autoscaling:EC2_INSTANCE_LAUNCH_ERROR";
const INSTANCE_TERMINATE: &str = "autoscaling:EC2_INSTANCE_TERMINATE";

/// An event type outside that catalog, in the spelling of one that is in it: the value whose
/// refusal the failing rows are about.
const UNPUBLISHED_TYPE: &str = "autoscaling:NOT_A_REAL_TYPE";

/// A `TopicARN` that names no Amazon SNS topic at all: a topic is a REGIONAL resource named by
/// a resource ARN of that service's namespace, so a bare name names nothing this service could
/// ever notify.
const MALFORMED_TOPIC_ARN: &str = "not-a-topic-arn";

/// The `MaxSize` every row's create declares, and the one the row that also revises a member of
/// the group's own update call states.
const CREATED_MAX_SIZE: u32 = 2;
const REVISED_MAX_SIZE: u32 = 3;

/// One declared `NotificationConfigurations` entry: the topic it names — by the NAME this suite
/// created in Amazon SNS, resolved to that topic's ARN when the template is written — and the
/// event types that topic is to be told about.
#[derive(Debug, Clone, Copy)]
struct Entry {
    topic: &'static str,
    types: &'static [&'static str],
}

/// The topics the rows name, by the name they were created under: the ARN Amazon SNS minted for
/// each, which is the value a template declares and the value that service's read reports.
type Topics = BTreeMap<&'static str, String>;

/// Create one topic through Amazon SNS's own wire and answer the ARN that service minted for
/// it: a `TopicARN` names a topic of that service, so the rows name real ones rather than ARNs
/// spelled here.
async fn create_topic(case: &str, router: &MultiServiceRouter, name: &str) -> String {
    let (status_code, body) = sns(router, "CreateTopic", &[("Name", name)]).await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: CreateTopic({name}) must answer 200, got {status_code} {body}"
    );
    let arn = xml_text(&body, "TopicArn")
        .unwrap_or_else(|| panic!("case {case:?}: expected a TopicArn, got {body}"));
    assert!(
        arn.ends_with(&format!(":{name}")),
        "case {case:?}: expected the ARN of the topic named {name}, got {arn:?}"
    );
    arn
}

/// The topics every row of one test names, created once in Amazon SNS.
async fn topics(case: &str, router: &MultiServiceRouter, names: &[&'static str]) -> Topics {
    let mut created = Topics::new();
    for name in names {
        created.insert(name, create_topic(case, router, name).await);
    }
    created
}

/// The ARN the topic `name` was created under, which the row's declaration states.
fn arn_of(topics: &Topics, name: &str) -> String {
    topics
        .get(name)
        .unwrap_or_else(|| panic!("the suite must create the topic {name} before declaring it"))
        .clone()
}

/// The event types Amazon EC2 Auto Scaling publishes as configurable right now, as
/// `DescribeAutoScalingNotificationTypes` reports them.
async fn published_types(router: &MultiServiceRouter) -> Vec<String> {
    let (status_code, body) =
        autoscaling(router, "DescribeAutoScalingNotificationTypes", &[]).await;
    assert_eq!(
        status_code, 200,
        "DescribeAutoScalingNotificationTypes must answer 200, got {status_code} {body}"
    );
    let published = xml_texts(&body, "member");
    assert!(
        published.len() > 1,
        "the published catalog must name the types a group can notify about, got {published:?}"
    );
    published
}

/// Hold the types the rows name to the catalog that service publishes, and the refused one to
/// being outside it: a row naming a published type for the refusal, or an unpublished one for
/// the accepted cases, would be testing the opposite rule.
async fn assert_the_catalog_holds_the_declared_types(router: &MultiServiceRouter) {
    let published = published_types(router).await;
    for declared in [
        TEST_NOTIFICATION,
        INSTANCE_LAUNCH,
        INSTANCE_LAUNCH_ERROR,
        INSTANCE_TERMINATE,
    ] {
        assert!(
            published.iter().any(|known| known == declared),
            "expected {declared} to be one of the types that service publishes, got {published:?}"
        );
    }
    assert!(
        !published.iter().any(|known| known == UNPUBLISHED_TYPE),
        "expected {UNPUBLISHED_TYPE} to be outside the published catalog, got {published:?}"
    );
}

/// The declared entries as the type states them: one `NotificationConfiguration` per entry,
/// each naming its topic's ARN and the event types it asks for.
fn declared_entries(entries: &[Entry], topics: &Topics) -> Vec<Value> {
    entries
        .iter()
        .map(|entry| {
            json!({
                "TopicARN": arn_of(topics, entry.topic),
                "NotificationTypes": entry.types,
            })
        })
        .collect()
}

/// How a declaration SPELLS the property, which is a declaration of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// The sequence syntax, which is how a list property is normally written.
    List,
    /// ONE entry written without it, which is the one-entry list of itself.
    OneStructure,
    /// The property not stated at all, which asks for a group that notifies nothing.
    Absent,
}

/// The template one revision states: the launch template the group launches from, and the group
/// with the notification entries this revision declares, spelled as the row asks.
fn template(
    group: &str,
    entries: &[Entry],
    spelling: Spelling,
    topics: &Topics,
    max_size: u32,
) -> String {
    let mut declared = json!({
        "AutoScalingGroupName": group,
        "AvailabilityZones": [ZONE],
        "MinSize": "0",
        "MaxSize": max_size.to_string(),
        "DesiredCapacity": "0",
        "LaunchTemplate": {
            "LaunchTemplateId": {"Ref": "Lt"},
            "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
        },
    });
    let stated = declared_entries(entries, topics);
    match spelling {
        Spelling::List => {
            declared["NotificationConfigurations"] = Value::Array(stated);
        }
        Spelling::OneStructure => {
            let [only] = stated.as_slice() else {
                panic!("a declaration spelled as one structure states exactly one entry");
            };
            declared["NotificationConfigurations"] = only.clone();
        }
        Spelling::Absent => {}
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
                "Properties": declared,
            },
        },
    })
    .to_string()
}

/// A template whose group declares the entries `declared` verbatim — for the rows whose entry
/// is a value the owning service refuses, which no [`Entry`] can state.
fn template_with(group: &str, declared: Vec<Value>) -> String {
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
                "Properties": {
                    "AutoScalingGroupName": group,
                    "AvailabilityZones": [ZONE],
                    "MinSize": "0",
                    "MaxSize": CREATED_MAX_SIZE.to_string(),
                    "DesiredCapacity": "0",
                    "LaunchTemplate": {
                        "LaunchTemplateId": {"Ref": "Lt"},
                        "Version": {"Fn::GetAtt": ["Lt", "LatestVersionNumber"]},
                    },
                    "NotificationConfigurations": Value::Array(declared),
                },
            },
        },
    })
    .to_string()
}

/// The (topic, event type) pairs Amazon EC2 Auto Scaling reports for the group right now — the
/// read a client takes with the group name a completed deployment handed it.
async fn reported_configurations(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
) -> Vec<(String, String)> {
    let (status_code, body) = autoscaling(
        router,
        "DescribeNotificationConfigurations",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status_code, 200,
        "case {case:?}: DescribeNotificationConfigurations({group}) must answer 200, got \
         {status_code} {body}"
    );
    let mut reported: Vec<(String, String)> = xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            // Every configuration reports the group it belongs to, so one answered for another
            // group could not pass for one of these.
            assert_eq!(
                xml_text(entry, "AutoScalingGroupName").as_deref(),
                Some(group),
                "case {case:?}: expected every reported configuration to belong to {group:?}, \
                 got {entry}"
            );
            (
                xml_text(entry, "TopicARN").unwrap_or_default(),
                xml_text(entry, "NotificationType").unwrap_or_default(),
            )
        })
        .collect();
    reported.sort();
    reported
}

/// The pairs a declaration asks the group to end up with, in the order the read above answers
/// in.
///
/// Two readings of the rule are in here. `PutNotificationConfiguration` "overwrites any
/// existing configuration", so an entry naming a topic an earlier entry already named REPLACES
/// that topic's event types rather than adding to them; and one call configures the SET of the
/// types it names, so a type one entry names twice configures one.
fn wanted_configurations(entries: &[Entry], topics: &Topics) -> Vec<(String, String)> {
    let mut by_topic: BTreeMap<String, Vec<&'static str>> = BTreeMap::new();
    for entry in entries {
        by_topic.insert(arn_of(topics, entry.topic), entry.types.to_vec());
    }
    let mut wanted: Vec<(String, String)> = by_topic
        .into_iter()
        .flat_map(|(arn, types)| {
            types
                .into_iter()
                .map(move |event| (arn.clone(), event.to_string()))
        })
        .collect();
    wanted.sort();
    wanted.dedup();
    wanted
}

/// One row of the CREATE table: the stack it deploys, the group it declares, and the entries the
/// declaration states, in the spelling it states them.
struct Created {
    case: &'static str,
    stack: &'static str,
    group: &'static str,
    entries: &'static [Entry],
    spelling: Spelling,
}

/// The topics the create table names.
const CREATE_TOPICS: &[&str] = &["asg-notify-first", "asg-notify-second"];

const CREATED: &[Created] = &[
    Created {
        case: "one topic told about one event",
        stack: "asg-notify-one",
        group: "asg-notify-one-group",
        entries: &[Entry {
            topic: "asg-notify-first",
            types: &[INSTANCE_LAUNCH],
        }],
        spelling: Spelling::List,
    },
    Created {
        case: "one topic told about three events",
        stack: "asg-notify-three-events",
        group: "asg-notify-three-events-group",
        entries: &[Entry {
            topic: "asg-notify-first",
            types: &[TEST_NOTIFICATION, INSTANCE_LAUNCH, INSTANCE_TERMINATE],
        }],
        spelling: Spelling::List,
    },
    Created {
        case: "two topics, each told about events of its own",
        stack: "asg-notify-two-topics",
        group: "asg-notify-two-topics-group",
        entries: &[
            Entry {
                topic: "asg-notify-first",
                types: &[INSTANCE_LAUNCH],
            },
            Entry {
                topic: "asg-notify-second",
                types: &[INSTANCE_TERMINATE, INSTANCE_LAUNCH_ERROR],
            },
        ],
        spelling: Spelling::List,
    },
    Created {
        case: "one event type named twice, which configures one",
        stack: "asg-notify-repeated",
        group: "asg-notify-repeated-group",
        entries: &[Entry {
            topic: "asg-notify-second",
            types: &[TEST_NOTIFICATION, TEST_NOTIFICATION],
        }],
        spelling: Spelling::List,
    },
    Created {
        case: "the one entry spelled without the sequence syntax",
        stack: "asg-notify-one-structure",
        group: "asg-notify-one-structure-group",
        entries: &[Entry {
            topic: "asg-notify-first",
            types: &[TEST_NOTIFICATION],
        }],
        spelling: Spelling::OneStructure,
    },
    Created {
        case: "the property stated as an empty list",
        stack: "asg-notify-empty",
        group: "asg-notify-empty-group",
        entries: &[],
        spelling: Spelling::List,
    },
    Created {
        case: "the property not stated at all",
        stack: "asg-notify-absent",
        group: "asg-notify-absent-group",
        entries: &[],
        spelling: Spelling::Absent,
    },
];

#[tokio::test]
async fn a_stack_provisions_the_notification_configurations_its_group_declares() {
    let router = router();
    assert_the_catalog_holds_the_declared_types(&router).await;
    let topics = topics("the created topics", &router, CREATE_TOPICS).await;

    // Every row's stack is created first and settled afterwards: the groups live in ONE
    // emulator, so a read for one group's configurations has other groups' to confuse it with.
    for row in CREATED {
        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template(
                row.group,
                row.entries,
                row.spelling,
                &topics,
                CREATED_MAX_SIZE,
            ),
            &[],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {body}",
            row.case
        );
    }

    for row in CREATED {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the stack to deploy, got {described} (reasons {:?})",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        let expected = wanted_configurations(row.entries, &topics);
        assert_eq!(
            reported_configurations(case, &router, row.group).await,
            expected,
            "case {case:?}: expected the group to be configured with {expected:?}"
        );
    }
}

/// One row of the UPDATE table: what the create declares, and what the revision declares.
struct Revised {
    case: &'static str,
    stack: &'static str,
    group: &'static str,
    created: &'static [Entry],
    revised: &'static [Entry],
    /// How the REVISION spells the property, so one row removes every entry by dropping it.
    spelling: Spelling,
    /// The `MaxSize` the revision declares, so one row revises a member the group's own update
    /// call carries beside its notifications.
    revised_max_size: u32,
}

/// The topics the update table names.
const UPDATE_TOPICS: &[&str] = &["asg-revise-first", "asg-revise-second"];

const FIRST_AT_LAUNCH: Entry = Entry {
    topic: "asg-revise-first",
    types: &[INSTANCE_LAUNCH],
};

const FIRST_AT_LAUNCH_AND_TERMINATE: Entry = Entry {
    topic: "asg-revise-first",
    types: &[INSTANCE_LAUNCH, INSTANCE_TERMINATE],
};

const FIRST_AT_TEST: Entry = Entry {
    topic: "asg-revise-first",
    types: &[TEST_NOTIFICATION],
};

const SECOND_AT_TERMINATE: Entry = Entry {
    topic: "asg-revise-second",
    types: &[INSTANCE_TERMINATE],
};

/// The SAME topic as [`FIRST_AT_LAUNCH`] with other event types: a second entry naming a topic
/// an earlier one already named, which configures that one topic with the types written last.
const FIRST_AGAIN_AT_LAUNCH_ERROR: Entry = Entry {
    topic: "asg-revise-first",
    types: &[INSTANCE_LAUNCH_ERROR],
};

const REVISIONS: &[Revised] = &[
    Revised {
        case: "an event type added to the topic the group already notifies",
        stack: "asg-revise-grown",
        group: "asg-revise-grown-group",
        created: &[FIRST_AT_LAUNCH],
        revised: &[FIRST_AT_LAUNCH_AND_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "the topic's event types narrowed to another one, beside a change the group's own \
               update carries",
        stack: "asg-revise-narrowed",
        group: "asg-revise-narrowed-group",
        created: &[FIRST_AT_LAUNCH_AND_TERMINATE],
        revised: &[FIRST_AT_TEST],
        spelling: Spelling::List,
        revised_max_size: REVISED_MAX_SIZE,
    },
    Revised {
        case: "a second topic added beside the one the group was created with",
        stack: "asg-revise-added",
        group: "asg-revise-added-group",
        created: &[FIRST_AT_LAUNCH],
        revised: &[FIRST_AT_LAUNCH, SECOND_AT_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "one of two topics removed, the other kept",
        stack: "asg-revise-removed-one",
        group: "asg-revise-removed-one-group",
        created: &[FIRST_AT_LAUNCH, SECOND_AT_TERMINATE],
        revised: &[SECOND_AT_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "the one topic exchanged for another",
        stack: "asg-revise-exchanged",
        group: "asg-revise-exchanged-group",
        created: &[FIRST_AT_LAUNCH],
        revised: &[SECOND_AT_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "every configuration removed, the property stated as an empty list",
        stack: "asg-revise-emptied",
        group: "asg-revise-emptied-group",
        created: &[FIRST_AT_LAUNCH, SECOND_AT_TERMINATE],
        revised: &[],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "every configuration removed, the property no longer stated at all",
        stack: "asg-revise-dropped",
        group: "asg-revise-dropped-group",
        created: &[FIRST_AT_LAUNCH],
        revised: &[],
        spelling: Spelling::Absent,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "a topic named by two entries, then no longer declared at all",
        stack: "asg-revise-repeated",
        group: "asg-revise-repeated-group",
        created: &[FIRST_AT_LAUNCH, FIRST_AGAIN_AT_LAUNCH_ERROR],
        revised: &[SECOND_AT_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
    Revised {
        case: "configurations declared on a group deployed with none",
        stack: "asg-revise-introduced",
        group: "asg-revise-introduced-group",
        created: &[],
        revised: &[FIRST_AT_LAUNCH, SECOND_AT_TERMINATE],
        spelling: Spelling::List,
        revised_max_size: CREATED_MAX_SIZE,
    },
];

#[tokio::test]
async fn an_update_applies_the_notification_configurations_the_revision_declares() {
    let router = router();
    let topics = topics("the revised topics", &router, UPDATE_TOPICS).await;

    for row in REVISIONS {
        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template(
                row.group,
                row.created,
                Spelling::List,
                &topics,
                CREATED_MAX_SIZE,
            ),
            &[],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {body}",
            row.case
        );
    }

    for row in REVISIONS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the stack to deploy, got {described} (reasons {:?})",
            stack_event_reasons(&router, REGION, row.stack).await
        );
        // What the CREATE asked for is the group's, which is what makes the revision's
        // reconciliation observable at all.
        let expected = wanted_configurations(row.created, &topics);
        assert_eq!(
            reported_configurations(case, &router, row.group).await,
            expected,
            "case {case:?}: expected the create to configure {expected:?}"
        );
    }

    for row in REVISIONS {
        let (status_code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member(
                    "TemplateBody",
                    &template(
                        row.group,
                        row.revised,
                        row.spelling,
                        &topics,
                        row.revised_max_size,
                    ),
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

    for row in REVISIONS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::UPDATE_COMPLETE),
            "case {case:?}: expected the update to complete, got {described} (reasons {:?})",
            stack_event_reasons(&router, REGION, row.stack).await
        );

        // The group kept the identity the stack published for it, and the member the group's
        // own update call carries landed on it.
        let group = described_group(case, &router, row.group).await;
        assert_eq!(
            xml_text(&group, "MaxSize").as_deref(),
            Some(row.revised_max_size.to_string().as_str()),
            "case {case:?}: expected the group to run at the revised MaxSize {}, got {group}",
            row.revised_max_size
        );

        // ...and its configurations are exactly the ones the revision declares.
        let expected = wanted_configurations(row.revised, &topics);
        assert_eq!(
            reported_configurations(case, &router, row.group).await,
            expected,
            "case {case:?}: expected the revision to leave {expected:?}"
        );
    }
}

/// One row of the REFUSAL table: the entries it declares (the refused value among them), the
/// value the owning service's sentence must name, and the configurations the surviving group is
/// left with.
struct Refused {
    case: &'static str,
    stack: &'static str,
    group: &'static str,
    /// The entry declared ahead of the refused one, where the row declares one.
    accepted: Option<Entry>,
    /// What the refused entry carries: the topic name to resolve, or the malformed value to
    /// declare verbatim, and the event type it names.
    refused_topic: Option<&'static str>,
    refused_type: &'static str,
    /// The value the refusal must name — the only variable part of that service's sentence.
    names: &'static str,
}

/// The topics the refusal table names.
const REFUSAL_TOPICS: &[&str] = &["asg-refused-first", "asg-refused-second"];

const REFUSALS: &[Refused] = &[
    Refused {
        case: "an event type outside the catalog that service publishes",
        stack: "asg-refused-type",
        group: "asg-refused-type-group",
        accepted: None,
        refused_topic: Some("asg-refused-first"),
        refused_type: UNPUBLISHED_TYPE,
        names: UNPUBLISHED_TYPE,
    },
    Refused {
        case: "an unpublished event type declared after an entry that service accepts",
        stack: "asg-refused-after-accepted",
        group: "asg-refused-after-accepted-group",
        accepted: Some(Entry {
            topic: "asg-refused-first",
            types: &[INSTANCE_LAUNCH],
        }),
        refused_topic: Some("asg-refused-second"),
        refused_type: UNPUBLISHED_TYPE,
        names: UNPUBLISHED_TYPE,
    },
    Refused {
        case: "a TopicARN that names no Amazon SNS topic",
        stack: "asg-refused-topic",
        group: "asg-refused-topic-group",
        accepted: None,
        refused_topic: None,
        refused_type: TEST_NOTIFICATION,
        names: MALFORMED_TOPIC_ARN,
    },
];

/// The row's declared entries: the one the owning service accepts where the row states one, then
/// the one it refuses.
fn refused_entries(row: &Refused, topics: &Topics) -> Vec<Value> {
    let mut declared = Vec::new();
    if let Some(accepted) = row.accepted {
        declared.extend(declared_entries(&[accepted], topics));
    }
    let topic = row
        .refused_topic
        .map_or_else(|| MALFORMED_TOPIC_ARN.to_string(), |name| arn_of(topics, name));
    declared.push(json!({
        "TopicARN": topic,
        "NotificationTypes": [row.refused_type],
    }));
    declared
}

#[tokio::test]
async fn a_refused_notification_configuration_fails_the_resource_and_leaves_the_group() {
    let router = router();
    assert_the_catalog_holds_the_declared_types(&router).await;
    let topics = topics("the refused topics", &router, REFUSAL_TOPICS).await;

    // `DisableRollback` keeps the failed create exactly as it failed, which is what makes what
    // the calls already did readable: the group its own create made is still there.
    for row in REFUSALS {
        let (status_code, body) = create_stack(
            &router,
            row.stack,
            &template_with(row.group, refused_entries(row, &topics)),
            &[member("DisableRollback", "true")],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {:?}: CreateStack must answer 200, got {status_code} {body}",
            row.case
        );
    }

    for row in REFUSALS {
        let case = row.case;
        let described = settled_stack(&router, case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_FAILED),
            "case {case:?}: expected the refused create to rest failed, got {described}"
        );

        // The failed resource reports the exception its provider's SDK raised: the owning
        // service's own sentence, naming the value it refused, inside that SDK's envelope.
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            "Asg",
            resource_status::CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(case, &reason);
        assert!(
            quoted.contains(row.names),
            "case {case:?}: expected the owning service's sentence naming {:?}, got {quoted:?}",
            row.names
        );
        assert_eq!(
            class, INVALID_REQUEST,
            "case {case:?}: expected the {INVALID_REQUEST} class, got {class:?} in {reason:?}"
        );
        assert_sdk_diagnostic_with_attempts(case, &quoted, AUTO_SCALING_SERVICE, REFUSED_STATUS);

        // The group itself was created by a call of its own, so it is still registered in the
        // owning service — carrying the configurations the calls that came before the refused
        // one really made, and nothing else.
        let group = described_group(case, &router, row.group).await;
        assert_eq!(
            xml_text(&group, "AutoScalingGroupName").as_deref(),
            Some(row.group),
            "case {case:?}: expected the group the create made to survive the refusal, got \
             {group}"
        );
        let accepted: Vec<Entry> = row.accepted.into_iter().collect();
        let expected = wanted_configurations(&accepted, &topics);
        assert_eq!(
            reported_configurations(case, &router, row.group).await,
            expected,
            "case {case:?}: expected the surviving group to carry {expected:?}"
        );
    }
}
