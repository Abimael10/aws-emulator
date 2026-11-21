// SPDX-License-Identifier: CC0-1.0

//! A log-group target under the target's own `RoleArn`: which of the TWO calls the write is
//! made of that role grants, and what an explicit deny in the group's own documents still
//! refuses — end to end over the wire through the real `MultiServiceRouter` with CloudWatch
//! Logs and IAM mounted beside EventBridge on one engine.
//!
//! The RULE asserted here: EventBridge writes into a log-group target as the identity the
//! target names — its `RoleArn` when it names one, otherwise its own service principal — and
//! the write is made of `logs:CreateLogStream` followed by `logs:PutLogEvents`, so EACH call
//! is decided on its own: within one account "the resource-based policy and the identity-based
//! policy are evaluated together, and access is granted if either allows it", so the role
//! grants a call its own permissions name and the documents in force over the group grant the
//! calls they name — while an explicit `Deny` about a call in those documents refuses it
//! whatever the role allows. A role allowed only the write therefore opens no stream, and the
//! event is not written.
//!
//! The rows differ along what the rule generalises over: whether the target names a role at
//! all, WHICH of the two calls that role is allowed, and what the group's own document states
//! about the write (nothing, or an explicit deny). Every row is a FRESH emulator, so one
//! row's documents never decide another row's write.
//!
//! The harness is `target_dispatch`'s, and the group's own calls are
//! `log_group_target_delivery`'s, so the two readings of one wire cannot drift.

use serde_json::json;
use service_com_amazonaws_eventbridge_awsevents::state::{rule_arn, DEFAULT_EVENT_BUS_NAME};

use crate::log_group_target_delivery::{given_log_group, written};
use crate::target_dispatch::{
    emulator, events, iam, trust_policy, xml_text, DETAIL_TYPE, REGION, SOURCE,
};

/// The rule every row publishes through, and the id of its one target.
const RULE: &str = "log-group-role-rule";
const TARGET_ID: &str = "log-group";

/// The service principal a target's role must trust for EventBridge to act as it, and the one
/// a group's own document would have to admit — the literal AWS publishes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The two calls the write is made of, as AWS names them.
const CREATE_LOG_STREAM: &str = "logs:CreateLogStream";
const PUT_LOG_EVENTS: &str = "logs:PutLogEvents";

/// How long the group is looked at for the write a row expects, and how often. Generous: what
/// is asserted is whether the event is written, never how fast the host runs.
const BUDGET: std::time::Duration = std::time::Duration::from_secs(60);
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(50);

/// How long a row that expects NO write keeps looking before concluding it was withheld.
const WITHHELD_WINDOW: std::time::Duration = std::time::Duration::from_secs(3);

/// What the row's target role is allowed, when the row names a role at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Allows {
    /// Both calls the write is made of.
    BothCalls,
    /// The write alone, so the stream the event goes into is never opened.
    WriteOnly,
}

/// One row: the role the target names and what it may do, whether the group's own document
/// denies the write, and whether the event is written.
struct Row {
    case: &'static str,
    role: Option<Allows>,
    group_denies: bool,
    written: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role allowed both calls writes the event, with no document on the group",
            role: Some(Allows::BothCalls),
            group_denies: false,
            written: true,
        },
        Row {
            case: "a role allowed only the write opens no stream, so nothing is written",
            role: Some(Allows::WriteOnly),
            group_denies: false,
            written: false,
        },
        Row {
            case: "a group whose document denies the write refuses it whatever the role allows",
            role: Some(Allows::BothCalls),
            group_denies: true,
            written: false,
        },
        Row {
            case: "a target naming no role is decided by the group's documents alone",
            role: None,
            group_denies: false,
            written: false,
        },
    ]
}

/// Every row: each call of the write is decided against the role the target names and the
/// documents in force over the group together.
#[tokio::test]
async fn each_call_of_a_log_group_write_is_granted_on_its_own() {
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let group = format!("log-group-role-{index}");
        let group_arn = given_log_group(&emulator, case, &group).await;

        // The role the target names, when the row names one: trusted by this service and
        // allowed the calls the row gives it, on every resource.
        let role_arn = match row.role {
            None => None,
            Some(allows) => {
                let name = format!("log-group-role-{index}");
                let trust = trust_policy(EVENTS_PRINCIPAL);
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
                let actions = match allows {
                    Allows::BothCalls => json!([CREATE_LOG_STREAM, PUT_LOG_EVENTS]),
                    Allows::WriteOnly => json!(PUT_LOG_EVENTS),
                };
                let document = json!({
                    "Version": "2012-10-17",
                    "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
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

        // The group's own document, where the row writes one: an explicit deny of the write
        // for this service principal, which no grant overturns.
        if row.group_denies {
            let document = json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "DenyEventBridgeWrite",
                    "Effect": "Deny",
                    "Principal": { "Service": EVENTS_PRINCIPAL },
                    "Action": PUT_LOG_EVENTS,
                    "Resource": "*",
                }],
            })
            .to_string();
            let (status, answered) = crate::log_group_target_delivery::logs(
                &emulator,
                "PutResourcePolicy",
                &json!({
                    "policyName": format!("deny-{index}"),
                    "policyDocument": document,
                    "resourceArn": &group_arn,
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: PutResourcePolicy answered {status} {answered}"
            );
        }

        let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
        let (status, ruled) = events(
            &emulator,
            "PutRule",
            &json!({ "Name": RULE, "EventPattern": pattern, "State": "ENABLED" }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutRule answered {status} {ruled}"
        );
        assert_eq!(
            ruled["RuleArn"].as_str(),
            Some(rule_arn(REGION, DEFAULT_EVENT_BUS_NAME, RULE).as_str()),
            "case {case:?}: expected the rule's own ARN in {ruled}"
        );
        let mut target = json!({ "Id": TARGET_ID, "Arn": &group_arn });
        if let Some(role_arn) = &role_arn {
            target["RoleArn"] = json!(role_arn);
        }
        let (status, targeted) = events(
            &emulator,
            "PutTargets",
            &json!({ "Rule": RULE, "Targets": [target] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {targeted}"
        );
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the log-group target to be accepted, got {targeted}"
        );

        let (status, published) = events(
            &emulator,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "marker": case }).to_string(),
                }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutEvents answered {status} {published}"
        );
        assert_eq!(
            published["FailedEntryCount"],
            json!(0),
            "case {case:?}: the entry is accepted whatever the delivery comes to, got \
             {published}"
        );

        let deadline = std::time::Instant::now()
            + if row.written { BUDGET } else { WITHHELD_WINDOW };
        let events_written = loop {
            let held = written(&emulator, case, &group).await;
            if (row.written && !held.is_empty()) || std::time::Instant::now() >= deadline {
                break held;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        };

        if row.written {
            assert_eq!(
                events_written.len(),
                1,
                "case {case:?}: expected exactly the matched event in the group, got \
                 {events_written:?}"
            );
            let message = events_written[0]["message"].as_str().unwrap_or_default();
            assert!(
                message.contains(case),
                "case {case:?}: the log event carries the envelope, got {message:?}"
            );
        } else {
            assert!(
                events_written.is_empty(),
                "case {case:?}: expected nothing to be written, got {events_written:?}"
            );
        }
    }
}
