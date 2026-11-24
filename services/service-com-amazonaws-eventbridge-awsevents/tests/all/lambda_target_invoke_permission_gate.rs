// SPDX-License-Identifier: CC0-1.0

//! An EventBridge rule's Lambda target is invoked exactly where the FUNCTION's own
//! resource-based policy admits the invocation for the invoking rule — observed at the
//! handler's own side effect, a message it forwards to an Amazon SQS queue.
//!
//! The RULE pinned here: "for Lambda and Amazon SNS resources, EventBridge relies on
//! resource-based policies" (`PutTargets`), so a matched rule hands the event to its Lambda
//! target only while that function's policy allows `events.amazonaws.com`
//! `lambda:InvokeFunction` on it for the rule that matched; a function whose policy says
//! nothing about that principal is not invoked at all, and the SAME unchanged rule, target and
//! event pattern invoke it on the NEXT matching event once `AddPermission` has granted it. The
//! handler then really runs, so the marker it forwards is the detail of the event that invoked
//! it — which is what tells a client "correctly refused for lack of permission" from "nothing
//! is ever invoked".
//!
//! The rows differ along what the rule generalises over: the FORM the grant takes when it is
//! added — unscoped, scoped by `aws:SourceArn` to the invoking rule, and scoped to ANOTHER
//! rule of the same bus (which grants this delivery nothing, so the function stays
//! uninvoked). Each row drives the same two legs, before and after its own `AddPermission`, so
//! the "no invocation" half is read against a leg that DID invoke rather than against a bare
//! quiet window.
//!
//! What the sibling suites own and this one does not repeat: the whole table of policy shapes a
//! handover is decided by (`core/router/tests/all/events_lambda_target_resource_policy.rs`),
//! the per-delivery accounting of granted and refused legs
//! (`tests/all/function_target_grant_takes_effect_on_the_next_event.rs`), and the invoking
//! rule's ARN reaching the handler
//! (`tests/all/lambda_target_handler_records_the_invoking_rule.rs`). What this one adds is the
//! grant being added BETWEEN publications while a real handler forwards the event's own detail
//! to a queue, which is the shape a client uses to test an invoke-permission gate.
//!
//! The emulator harness — EventBridge, Lambda, Amazon SQS, IAM and the token service on ONE
//! shared engine, the awsQuery IAM caller and the deployment packager — is the one
//! `tests/all/handler_execution_role_publication.rs` owns.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::handler_execution_role_publication::{
    Emulator, EVENTS_PRINCIPAL, FUNCTIONS_URI, REGION,
};

/// The queue the handler forwards each marker to, and the role and function of each row.
const QUEUE: &str = "invoke-gate-markers";
const ROLE: &str = "invoke-gate-handler-role";
const FUNCTION: &str = "invoke-gate-forwarder";

/// The rule every row installs, and the rule a grant scoped to ANOTHER rule names. The second
/// need not exist: `aws:SourceArn` is a value a statement is written against, not a resource
/// the policy resolves.
const RULE: &str = "invoke-gate-rule";
const ANOTHER_RULE: &str = "some-other-rule";

/// The `source` and `detail-type` every event this suite publishes carries, and the `state`
/// the rule's pattern admits — a custom source, so nothing but this suite's own publications
/// can match.
const SOURCE: &str = "com.demo.invoke-gate";
const DETAIL_TYPE: &str = "Backup Job State Change";
const MATCHED_STATE: &str = "COMPLETED";

/// The IAM action an invocation of a function performs, which is what a grant for this
/// service has to name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The id the one statement a row adds is written under, and the id of the control grant a
/// row whose own grant admits nothing adds afterwards.
const STATEMENT_ID: &str = "AllowEventBridgeInvoke";
const CONTROL_STATEMENT_ID: &str = "AllowThisRule";

/// The managed policy that lets the handler make the send it makes, attached to its execution
/// role beside the basic-execution policy `Emulator::execution_role` attaches.
const QUEUE_WRITER_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSQSFullAccess";

/// How long a leg waits for the invocation and the handler's own write before giving up on
/// them, and how long it keeps watching a queue nothing must reach once the other leg has
/// shown the handover was already decided. Both are generous: nothing here measures how fast
/// the host runs.
const WORK_WAIT: Duration = Duration::from_secs(30);
const QUIET_WAIT: Duration = Duration::from_secs(3);

/// How often either looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The handler: it forwards the invoking event's own `detail.backupJobId` to the queue, which
/// is the only way this suite learns the function ran at all.
fn forwarder(queue_url: &str) -> String {
    format!(
        r#"
import json

import boto3


def lambda_handler(event, context):
    boto3.client("sqs").send_message(
        QueueUrl="{queue_url}",
        MessageBody=json.dumps({{"marker": event["detail"]["backupJobId"]}}),
    )
    return {{"forwarded": event["detail"]["backupJobId"]}}
"#
    )
}

/// One row: the grant a client adds between the two legs, and whether it admits THIS rule's
/// delivery.
struct Row {
    case: &'static str,
    /// The `SourceArn` the added statement is scoped by, as a function of the invoking rule's
    /// ARN; `None` for an unscoped grant.
    scope: Option<fn(&str) -> String>,
    /// Whether the function must be invoked on the leg that follows the grant.
    admits: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an unscoped grant",
            scope: None,
            admits: true,
        },
        Row {
            case: "a grant scoped to the invoking rule",
            scope: Some(|rule_arn| rule_arn.to_string()),
            admits: true,
        },
        Row {
            case: "a grant scoped to another rule of the bus",
            scope: Some(|rule_arn| {
                rule_arn.replace(&format!("rule/{RULE}"), &format!("rule/{ANOTHER_RULE}"))
            }),
            admits: false,
        },
    ]
}

/// Publish one matching event carrying `marker` as its `detail.backupJobId`.
async fn publish(emulator: &Emulator, case: &str, marker: &str) {
    let answered = emulator
        .events(
            case,
            "PutEvents",
            &json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({
                        "state": MATCHED_STATE,
                        "backupJobId": marker,
                    })
                    .to_string(),
                }],
            }),
        )
        .await;
    assert_eq!(
        answered["FailedEntryCount"],
        json!(0),
        "case {case:?}: PutEvents must accept the entry, got {answered}"
    );
}

/// Add one `lambda:InvokeFunction` statement for this service to the function's policy, which
/// must be accepted.
async fn add_permission(
    emulator: &Emulator,
    case: &str,
    statement_id: &str,
    source_arn: Option<String>,
) {
    let mut statement = json!({
        "StatementId": statement_id,
        "Action": INVOKE_ACTION,
        "Principal": EVENTS_PRINCIPAL,
    });
    if let Some(source_arn) = source_arn {
        statement["SourceArn"] = json!(source_arn);
    }
    let (status, _headers, granted) = emulator
        .lambda(
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
            &serde_json::to_vec(&statement).expect("the statement serializes"),
        )
        .await;
    assert_eq!(
        status, 201,
        "case {case:?}: AddPermission {statement_id:?} must answer 201, got {status} {granted}"
    );
}

/// Every marker the handler has forwarded to the queue so far, taken off it.
async fn markers(emulator: &Emulator, case: &str, url: &str) -> Vec<String> {
    emulator
        .raw_messages(case, url)
        .await
        .into_iter()
        .filter_map(|message| {
            let raw = message["Body"].as_str().unwrap_or_default().to_string();
            serde_json::from_str::<Value>(&raw)
                .ok()
                .and_then(|body| body["marker"].as_str().map(str::to_string))
        })
        .collect()
}

/// Look for `marker` on the queue until it is there or `budget` has passed; answers every
/// marker seen, so an assertion can name what DID arrive.
async fn looked_for(
    emulator: &Emulator,
    case: &str,
    url: &str,
    marker: &str,
    budget: Duration,
) -> Vec<String> {
    let deadline = Instant::now() + budget;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(markers(emulator, case, url).await);
        if seen.iter().any(|found| found == marker) || Instant::now() >= deadline {
            return seen;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// A rule's Lambda target is invoked only once the function's own policy admits the invoking
/// rule, and the handler's forwarded marker is the detail of the event that invoked it.
#[tokio::test]
async fn a_lambda_target_runs_only_once_its_policy_admits_the_invoking_rule() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let emulator = Emulator::new();
        let (queue_url, _queue_arn) = emulator.queue(case, QUEUE).await;

        // The execution role the handler's own send is judged against, and the function.
        let role_arn = emulator.execution_role(case, ROLE).await;
        emulator
            .iam(
                case,
                "AttachRolePolicy",
                &[
                    ("RoleName", ROLE.to_string()),
                    ("PolicyArn", QUEUE_WRITER_POLICY.to_string()),
                ],
            )
            .await;
        emulator
            .function(case, FUNCTION, &role_arn, &forwarder(&queue_url))
            .await;
        let function_arn = format!(
            "arn:aws:lambda:{REGION}:{}:function:{FUNCTION}",
            engine::emulator_account_id()
        );

        // The rule and its Lambda target, installed ONCE and never touched again: what changes
        // between the two legs is only the function's own policy.
        let pattern = json!({
            "source": [SOURCE],
            "detail-type": [DETAIL_TYPE],
            "detail": { "state": [MATCHED_STATE] },
        })
        .to_string();
        let put = emulator
            .events(
                case,
                "PutRule",
                &json!({ "Name": RULE, "EventPattern": pattern, "State": "ENABLED" }),
            )
            .await;
        let rule_arn = put["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no RuleArn in {put}"))
            .to_string();
        let targeted = emulator
            .events(
                case,
                "PutTargets",
                &json!({
                    "Rule": RULE,
                    "Targets": [{ "Id": "lambda-target", "Arn": &function_arn }],
                }),
            )
            .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets must accept the target, got {targeted}"
        );

        // Leg 1 — the function's policy says nothing about this service: nothing is invoked,
        // so the handler forwards nothing.
        let before = format!("before-grant-{index}");
        publish(&emulator, case, &before).await;
        let seen = looked_for(&emulator, case, &queue_url, &before, QUIET_WAIT).await;
        assert!(
            seen.is_empty(),
            "case {case:?}: a function whose policy admits this service nothing must not be \
             invoked, got {seen:?}"
        );

        // The grant, in this row's own form.
        add_permission(
            &emulator,
            case,
            STATEMENT_ID,
            row.scope.map(|scope| scope(&rule_arn)),
        )
        .await;

        // Leg 2 — the SAME rule, target and pattern, one event later.
        let after = format!("after-grant-{index}");
        publish(&emulator, case, &after).await;
        let budget = if row.admits { WORK_WAIT } else { QUIET_WAIT };
        let seen = looked_for(&emulator, case, &queue_url, &after, budget).await;
        if row.admits {
            assert_eq!(
                seen,
                vec![after.clone()],
                "case {case:?}: the admitted delivery must invoke the function once, whose \
                 handler forwards the invoking event's own marker, got {seen:?}"
            );
        } else {
            assert!(
                seen.is_empty(),
                "case {case:?}: a grant scoped to another rule admits this delivery nothing, \
                 got {seen:?}"
            );
            // The control: the same function, once a grant that DOES admit this rule is added,
            // is invoked on the next event — so the quiet window above was the scope and not
            // a target that never delivers.
            add_permission(
                &emulator,
                case,
                CONTROL_STATEMENT_ID,
                Some(rule_arn.clone()),
            )
            .await;
            let control = format!("control-{index}");
            publish(&emulator, case, &control).await;
            let seen = looked_for(&emulator, case, &queue_url, &control, WORK_WAIT).await;
            assert_eq!(
                seen,
                vec![control.clone()],
                "case {case:?}: the control delivery must invoke the function once, got {seen:?}"
            );
        }
    }
}
