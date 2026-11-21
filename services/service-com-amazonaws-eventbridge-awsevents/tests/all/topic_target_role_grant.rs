// SPDX-License-Identifier: CC0-1.0

//! A topic target under the target's own `RoleArn`: the role grants the publish the topic's
//! own policy withholds, and an explicit deny in that policy still refuses it — end to end
//! over the wire through the real `MultiServiceRouter` with Amazon SNS, Amazon SQS and IAM
//! mounted beside EventBridge on one engine.
//!
//! The RULE asserted here is the one `role_granted_function_target_runs_its_handler` and the
//! router's `events_target_role_grants_the_delivery` state for the other kinds AWS governs by
//! the target's own document ("for Lambda functions, Amazon SNS topics and Amazon SQS queues,
//! EventBridge relies on resource-based policies", `PutTargets`), read here for the SNS branch:
//! a topic target naming no role is admitted by the topic's own access policy alone, because
//! the publish is made as a service principal that holds no identity-based policies; one that
//! names a role is reached AS that role, and within one account "the resource-based policy and
//! the identity-based policy are evaluated together, and access is granted if either allows
//! it", so the permissions in force on that role admit the publish — while an explicit `Deny`
//! in the topic's own policy refuses it whatever the role allows, and a role EventBridge cannot
//! assume for the invoking rule fails the handover before the publish is reached.
//!
//! The rows differ along what the rule generalises over: whether the target names a role, what
//! that role trusts and what it is allowed, and what the topic's own policy states (nothing
//! beyond the one Amazon SNS generates for its owner, or an explicit deny). The publish is
//! observed where a client observes one — on the queue subscribed to the topic.
//!
//! The harness, the topic and its subscriber are `target_dispatch`'s and
//! `topic_target_policy_authorization`'s, so the two readings of one wire cannot drift.

use serde_json::json;

use crate::target_dispatch::{
    drain, emulator, events, given_queue_with_policy, given_rule, iam, publish, trust_policy,
    xml_text,
};
use crate::topic_target_policy_authorization::{granting_sns, given_topic, PolicyFor};

/// The rule every row publishes through, and the id of its one target.
const RULE: &str = "topic-role-rule";
const TARGET_ID: &str = "topic";

/// The service principal a target's role must trust for EventBridge to act as it, and one that
/// is not this service — the literals AWS publishes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The action a delivery to a topic target performs, and one that is not it.
const PUBLISH_ACTION: &str = "sns:Publish";
const ANOTHER_ACTION: &str = "sqs:SendMessage";

/// The role the row's target names, when it names one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Role {
    trusts: &'static str,
    allows: &'static str,
}

/// One row: the role the target names, whether the topic's own policy denies the publish, and
/// whether the subscriber is notified.
struct Row {
    case: &'static str,
    role: Option<Role>,
    topic_denies: bool,
    published: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting this service and allowed the publish carries the delivery",
            role: Some(Role {
                trusts: EVENTS_PRINCIPAL,
                allows: PUBLISH_ACTION,
            }),
            topic_denies: false,
            published: true,
        },
        Row {
            case: "a role that does not trust this service publishes nothing",
            role: Some(Role {
                trusts: OTHER_PRINCIPAL,
                allows: PUBLISH_ACTION,
            }),
            topic_denies: false,
            published: false,
        },
        Row {
            case: "an assumable role allowed another service's call publishes nothing",
            role: Some(Role {
                trusts: EVENTS_PRINCIPAL,
                allows: ANOTHER_ACTION,
            }),
            topic_denies: false,
            published: false,
        },
        Row {
            case: "a topic whose policy DENIES the publish refuses it whatever the role allows",
            role: Some(Role {
                trusts: EVENTS_PRINCIPAL,
                allows: PUBLISH_ACTION,
            }),
            topic_denies: true,
            published: false,
        },
        Row {
            case: "a target naming no role is decided by the topic's own policy alone",
            role: None,
            topic_denies: false,
            published: false,
        },
    ]
}

/// Every row: what the role the target names allows, and what the topic's own policy states,
/// decide the publish together.
#[tokio::test]
async fn a_topic_targets_role_grants_the_publish_its_own_policy_withholds() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        // The queue subscribed to the topic, which is where a publish is observed.
        let (subscriber_url, subscriber_arn) = given_queue_with_policy(
            &emulator,
            &format!("topic-role-subscriber-{index}"),
            Some(&granting_sns),
        )
        .await;
        // The topic's own policy: either nothing beyond the one Amazon SNS generates for its
        // owner, or an explicit deny of the publish for this service's principal.
        let denial: PolicyFor = Box::new(|topic_arn: &str| {
            json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "DenyEventBridgePublish",
                    "Effect": "Deny",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUBLISH_ACTION,
                    "Resource": topic_arn,
                }],
            })
            .to_string()
        });
        let topic_arn = given_topic(
            &emulator,
            case,
            &format!("role-topic-{index}"),
            &subscriber_arn,
            row.topic_denies.then_some(&denial),
        )
        .await;

        let role_arn = match row.role {
            None => None,
            Some(role) => {
                let name = format!("topic-role-{index}");
                let trust = trust_policy(role.trusts);
                let (status, created) = iam(
                    &emulator,
                    "CreateRole",
                    &[("RoleName", &name), ("AssumeRolePolicyDocument", &trust)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: CreateRole answered {status} {created}"
                );
                let arn = xml_text(&created, "Arn")
                    .unwrap_or_else(|| panic!("case {case:?}: expected an Arn in {created}"));
                let document = json!({
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Action": role.allows,
                        "Resource": &topic_arn,
                    }],
                })
                .to_string();
                let (status, written) = iam(
                    &emulator,
                    "PutRolePolicy",
                    &[
                        ("RoleName", &name),
                        ("PolicyName", "delivery"),
                        ("PolicyDocument", &document),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: PutRolePolicy answered {status} {written}"
                );
                Some(arn)
            }
        };

        let mut target = json!({ "Id": TARGET_ID, "Arn": &topic_arn });
        if let Some(role_arn) = &role_arn {
            target["RoleArn"] = json!(role_arn);
        }
        given_rule(&emulator, RULE, json!([target])).await;
        publish(&emulator).await;

        let notified = drain(&emulator, &subscriber_url).await;
        assert_eq!(
            notified.len(),
            usize::from(row.published),
            "case {case:?}: expected {} notification(s) on the subscriber, got {notified:?}",
            usize::from(row.published)
        );
        // Nothing of the rule changed between the rows but the role and the topic's policy, so
        // a row that published and a row that did not are the same wiring under two documents.
        let (status, listed) = events(&emulator, "ListTargetsByRule", &json!({ "Rule": RULE })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTargetsByRule answered {status} {listed}"
        );
        assert_eq!(
            listed["Targets"][0]["Arn"].as_str(),
            Some(topic_arn.as_str()),
            "case {case:?}: expected the rule to hold the topic target, got {listed}"
        );
    }
}
