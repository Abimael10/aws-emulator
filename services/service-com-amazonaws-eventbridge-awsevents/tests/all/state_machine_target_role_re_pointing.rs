// SPDX-License-Identifier: CC0-1.0

//! RE-POINTING a state-machine target's `RoleArn` decides the next matching event's delivery,
//! and nothing else about the rule changes — end to end over the wire through the real
//! `MultiServiceRouter` with Step Functions, IAM and CloudWatch mounted beside EventBridge on
//! one engine.
//!
//! The RULE asserted here: a state machine is one of the kinds EventBridge invokes UNDER the
//! role the target names ("for EC2 instances, Kinesis data streams, Step Functions state
//! machines and API Gateway APIs, EventBridge relies on IAM roles that you specify in the
//! `RoleARN` argument in `PutTargets`"), and both halves of that role are read AT EVERY
//! DELIVERY: the trust document first — a role whose trust policy does not admit this service
//! cannot be assumed, so no execution is started — and then the permissions in force on it,
//! which have to allow `states:StartExecution` on the machine the target names. A
//! `PutTargets` call naming a target id the rule already holds REPLACES that target, so the
//! role the NEXT matching event is delivered under is the one that call installed; the
//! execution it starts carries the whole event envelope as its input and runs the machine's
//! definition to its terminal state.
//!
//! The rows differ along what the rule generalises over: the trust the installed role carries
//! (another service, then this one) and what it is allowed (another action, then the
//! execution's own) — each row RE-POINTING the one target of the one rule and publishing one
//! matching event, so what changed between two deliveries is the role and nothing else.
//!
//! The harness (the router with EventBridge, SQS, Step Functions, IAM and CloudWatch on one
//! engine) is the one `target_dispatch` owns. `target_role_assumption` is the suite for the
//! assume-role step across target KINDS, and `target_role_trust_conditions` for WHICH rule a
//! trust condition admits; what this one adds is the re-pointing and the execution a
//! permitted delivery really starts.

use serde_json::json;

use crate::target_dispatch::{emulator, events, iam, sfn, trust_policy, xml_text, REGION};

/// The rule every delivery below is made for, and the id of its one target.
const RULE: &str = "repointed-flow-rule";
const TARGET_ID: &str = "flow-target";

/// The envelope members the rule's pattern names, including the member nested in `detail`.
const SOURCE: &str = "com.demo.invalidation";
const DETAIL_TYPE: &str = "Distribution Event (Simulated)";
const EVENT_NAME: &str = "CreateInvalidation";

/// A state machine that terminates on its own, so an execution a delivery starts is one that
/// reaches a terminal status: a single `Pass` state that ends the machine.
const DEFINITION: &str = r#"{"StartAt":"Pass","States":{"Pass":{"Type":"Pass","End":true}}}"#;

/// The service principal a target's role must trust for EventBridge to act as it, the
/// principal a state machine's own execution role is trusted by, and a principal that is
/// neither — written as the literals AWS publishes.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
const OTHER_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The action a delivery to a state-machine target performs, and one that is not it.
const START_EXECUTION_ACTION: &str = "states:StartExecution";
const ANOTHER_ACTION: &str = "lambda:InvokeFunction";

/// The status Step Functions reports for an execution of a machine that ran to its end.
const SUCCEEDED: &str = "SUCCEEDED";

/// How long the machine's executions are looked at for the one a row expects, and how often.
/// Generous: what is asserted is which executions exist, never how fast the host runs.
const BUDGET: ::std::time::Duration = ::std::time::Duration::from_secs(60);
const LOOK_AGAIN: ::std::time::Duration = ::std::time::Duration::from_millis(50);

/// How long a row that expects NO execution keeps looking before concluding the delivery was
/// withheld. The delivering row proves the same rule and target start one in a fraction of it.
const WITHHELD_WINDOW: ::std::time::Duration = ::std::time::Duration::from_secs(3);

/// One row: the role installed on the target, what it trusts, what it is allowed, and whether
/// the matching event that follows starts an execution.
struct Row {
    case: &'static str,
    role: &'static str,
    trusts: &'static str,
    allows: &'static str,
    starts: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role trusting another service cannot be assumed, so nothing starts",
            role: "repointed-untrusting",
            trusts: OTHER_PRINCIPAL,
            allows: START_EXECUTION_ACTION,
            starts: false,
        },
        Row {
            case: "an assumable role allowed another action starts nothing either",
            role: "repointed-withholding",
            trusts: EVENTS_PRINCIPAL,
            allows: ANOTHER_ACTION,
            starts: false,
        },
        Row {
            case: "an assumable role allowed the execution starts one carrying the event",
            role: "repointed-allowing",
            trusts: EVENTS_PRINCIPAL,
            allows: START_EXECUTION_ACTION,
            starts: true,
        },
    ]
}

/// A role called `name` trusting `principal` and allowed `action` on `resource`; answers the
/// ARN IAM persisted.
async fn given_scoped_role(
    emulator: &crate::target_dispatch::Emulator,
    name: &str,
    principal: &str,
    action: &str,
    resource: &str,
) -> String {
    let trust = trust_policy(principal);
    let (status, created) = iam(
        emulator,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(status, 200, "CreateRole({name}) answered {status} {created}");
    let arn = xml_text(&created, "Arn")
        .unwrap_or_else(|| panic!("expected an Arn in the CreateRole({name}) answer: {created}"));
    // The grant names the machine itself, which is the resource `states:StartExecution` is
    // performed on.
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string();
    let (status, written) = iam(
        emulator,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "delivery"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "PutRolePolicy({name}) answered {status} {written}"
    );
    arn
}

/// Every execution the machine at `arn` holds, as its input and the status it reached.
async fn executions(
    emulator: &crate::target_dispatch::Emulator,
    arn: &str,
) -> Vec<(String, String)> {
    let (status, listed) = sfn(
        emulator,
        "ListExecutions",
        &json!({ "stateMachineArn": arn }),
    )
    .await;
    assert_eq!(status, 200, "ListExecutions answered {status} {listed}");
    let mut held = Vec::new();
    for execution in listed["executions"].as_array().cloned().unwrap_or_default() {
        let execution_arn = execution["executionArn"].as_str().unwrap_or_default();
        let (status, described) = sfn(
            emulator,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "DescribeExecution answered {status} {described}"
        );
        held.push((
            described["input"].as_str().unwrap_or_default().to_string(),
            described["status"].as_str().unwrap_or_default().to_string(),
        ));
    }
    held
}

/// The rows in order, against ONE rule and ONE machine: each row re-points the target's role
/// and publishes one matching event, and what the machine then holds is what that role
/// decided.
#[tokio::test]
async fn re_pointing_a_state_machine_targets_role_decides_the_next_delivery() {
    let emulator = emulator();

    // The machine's OWN execution role, which is not the role any target names.
    let machine_role = given_scoped_role(
        &emulator,
        "repointed-machine",
        STATES_PRINCIPAL,
        "states:*",
        "*",
    )
    .await;
    let (status, machine) = sfn(
        &emulator,
        "CreateStateMachine",
        &json!({ "name": "repointed-flow", "definition": DEFINITION, "roleArn": machine_role }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateStateMachine answered {status} {machine}"
    );
    let machine_arn = machine["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a stateMachineArn in {machine}"))
        .to_string();

    let (status, ruled) = events(
        &emulator,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventPattern": json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": { "eventName": [EVENT_NAME] },
            })
            .to_string(),
            "State": "ENABLED",
        }),
    )
    .await;
    assert_eq!(status, 200, "PutRule answered {status} {ruled}");
    let rule_arn = ruled["RuleArn"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a RuleArn in {ruled}"))
        .to_string();
    assert!(
        rule_arn.ends_with(&format!(":rule/{RULE}")),
        "a rule of the default bus carries no bus segment, got {rule_arn}"
    );
    assert!(
        rule_arn.contains(REGION),
        "the rule's ARN carries the Region it was written in, got {rule_arn}"
    );

    // How many executions the machine held before this row, so each row is read against what
    // its OWN delivery added.
    let mut already = 0usize;
    for row in rows() {
        let case = row.case;
        let role_arn =
            given_scoped_role(&emulator, row.role, row.trusts, row.allows, &machine_arn).await;
        // The target id is the one the rule already holds, so this REPLACES the target rather
        // than adding a second one.
        let (status, targeted) = events(
            &emulator,
            "PutTargets",
            &json!({
                "Rule": RULE,
                "Targets": [{ "Id": TARGET_ID, "Arn": &machine_arn, "RoleArn": &role_arn }],
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutTargets answered {status} {targeted}"
        );
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: expected the re-pointed target to be accepted, got {targeted}"
        );
        let (status, listed) = events(
            &emulator,
            "ListTargetsByRule",
            &json!({ "Rule": RULE }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListTargetsByRule answered {status} {listed}"
        );
        assert_eq!(
            listed["Targets"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: re-pointing replaces the target rather than adding one, got \
             {listed}"
        );
        assert_eq!(
            listed["Targets"][0]["RoleArn"].as_str(),
            Some(role_arn.as_str()),
            "case {case:?}: expected the rule to hold the role just installed, got {listed}"
        );

        let marker = format!("marker-{}", row.role);
        let (status, published) = events(
            &emulator,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "eventName": EVENT_NAME, "marker": &marker }).to_string(),
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

        let deadline =
            ::std::time::Instant::now() + if row.starts { BUDGET } else { WITHHELD_WINDOW };
        let held = loop {
            let held = executions(&emulator, &machine_arn).await;
            let settled = row.starts
                && held.len() > already
                && held.iter().all(|(_, status)| status == SUCCEEDED);
            if settled || ::std::time::Instant::now() >= deadline {
                break held;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        };

        let expected = already + usize::from(row.starts);
        assert_eq!(
            held.len(),
            expected,
            "case {case:?}: expected {expected} execution(s) after this delivery, got {held:?}"
        );
        if row.starts {
            let started = held
                .last()
                .unwrap_or_else(|| panic!("case {case:?}: expected an execution, got {held:?}"));
            assert!(
                started.0.contains(&marker),
                "case {case:?}: the execution's input carries the event, got {held:?}"
            );
            assert!(
                started.0.contains(SOURCE) && started.0.contains(DETAIL_TYPE),
                "case {case:?}: the input is the whole envelope, got {held:?}"
            );
            assert_eq!(
                started.1, SUCCEEDED,
                "case {case:?}: the machine runs to its terminal state, got {held:?}"
            );
        }
        already = expected;
    }
}
