// SPDX-License-Identifier: CC0-1.0

//! What a `Catch` on a `Task` state whose Amazon SNS publish the execution role may not
//! make can route on, driven across the four services the question spans — Step Functions
//! runs the state, IAM holds the documents the role is judged against, Amazon SNS would
//! serve the publish and Amazon SQS holds whatever arrived — mounted on ONE engine exactly
//! as `core/router/src/lib.rs` mounts them.
//!
//! RULE: a `Task` state's integration call the identity-based documents in force on the
//! state machine's `roleArn` withhold is reported to the interpreter as that state's
//! ORDINARY task failure under the invoked service's own error name, so the state's
//! `Catch` decides where the execution goes: a catcher whose `ErrorEquals` names that
//! failure — by the error's own name, by `States.TaskFailed`, or by `States.ALL` — carries
//! the execution down its branch with the `{Error, Cause}` error output, the first such
//! catcher in order winning; a definition whose catchers all name something else leaves
//! the execution `FAILED` under that error. The call itself is never made in either arm,
//! so the target publishes nothing.
//!
//! The rows differ along what the rule generalises over: what the catchers name (the
//! service's own error name, each of the two wildcards the language documents, the name
//! beside unrelated names, an unrelated name alone, another service's refusal), how many
//! catchers the state carries and in which order, and whether it carries any at all — and
//! therefore which terminal status the execution settles on and which branch state is
//! entered.
//!
//! The denied side is only half the rule, so the same catcher shapes run once more under a
//! role that IS granted the publish: the branch exists, and it is the DENIAL that routes
//! into it rather than the catcher's presence.
//!
//! The wiring, the role and the topic-with-subscriber are
//! `all/sfn_sns_publish_task.rs`'s and `all/sfn_task_target_authorization.rs`'s —
//! `tests/all/main.rs` is one crate root — because this suite asks a different question
//! about the same four services: not whether the documents decide the call (that suite's
//! question) but what the decision is ROUTABLE by.

use serde_json::{json, Value};

use crate::sfn_sns_publish_task::{
    delivered, emulator, history, normalized, of_type, topic_with_subscriber,
    AUTHORIZATION_ERROR, PUBLISH_ACTION, REFUSAL_METADATA, TASK_FAILED, TASK_SUCCEEDED,
};
use crate::sfn_task_target_authorization::{
    allow, logs_only, ran_with_input, role, session_principal, sfn, string,
};

/// The two wildcards the Amazon States Language documents for a catcher: "States.ALL — A
/// wildcard that matches any known error name", and "States.TaskFailed [...] matches any
/// known error name except States.Timeout".
const ALL_ERRORS: &str = "States.ALL";
/// See [`ALL_ERRORS`].
const TASK_FAILED_WILDCARD: &str = "States.TaskFailed";

/// A language error name that is NOT the one a withheld call reports, used by the rows
/// whose catchers must not match: "States.Timeout — A Task state either ran longer than
/// the TimeoutSeconds value, or failed to heartbeat".
const TIMEOUT_ERROR: &str = "States.Timeout";

/// Another service's refusal of an unauthorized call, which is a different error name
/// again: a catcher naming it does not answer for Amazon SNS's.
const OTHER_SERVICE_ERROR: &str = "Lambda.AWSLambdaException";

/// The name of the state a catcher that matches routes to, and of the one no row may ever
/// enter.
const CAUGHT: &str = "Caught";
/// See [`CAUGHT`].
const WRONG: &str = "Wrong";

/// One catcher of a `Task` state: what it names, and where it goes.
struct Catcher {
    errors: &'static [&'static str],
    next: &'static str,
}

/// One row: the case, the catchers the `Task` carries in order, and the branch state the
/// execution must enter — `None` where nothing names the failure, so the execution fails
/// instead.
struct Row {
    case: &'static str,
    catchers: &'static [Catcher],
    routes_to: Option<&'static str>,
}

/// The rows both tests below run: the same definitions, judged once under a role that is
/// withheld the publish and once under one that is granted it.
const ROWS: &[Row] = &[
    Row {
        // Nothing to route it: the failure settles the execution itself.
        case: "a state carrying no catcher at all",
        catchers: &[],
        routes_to: None,
    },
    Row {
        case: "a catcher naming the wildcard for every known error",
        catchers: &[Catcher {
            errors: &[ALL_ERRORS],
            next: CAUGHT,
        }],
        routes_to: Some(CAUGHT),
    },
    Row {
        // The narrower wildcard: a withheld call is a FAILED TASK, so the wildcard for one
        // answers for it.
        case: "a catcher naming the wildcard for a failed task",
        catchers: &[Catcher {
            errors: &[TASK_FAILED_WILDCARD],
            next: CAUGHT,
        }],
        routes_to: Some(CAUGHT),
    },
    Row {
        // The name itself, which is how a client routes one service's refusal and no
        // other.
        case: "a catcher naming the invoked service's own error",
        catchers: &[Catcher {
            errors: &[AUTHORIZATION_ERROR],
            next: CAUGHT,
        }],
        routes_to: Some(CAUGHT),
    },
    Row {
        // One catcher may name several errors, and matching ONE of them is a match.
        case: "a catcher naming that error beside unrelated ones",
        catchers: &[Catcher {
            errors: &[TIMEOUT_ERROR, OTHER_SERVICE_ERROR, AUTHORIZATION_ERROR],
            next: CAUGHT,
        }],
        routes_to: Some(CAUGHT),
    },
    Row {
        // Catchers are tried in order and the first match wins, so a catcher naming
        // something else first does not divert the branch the matching one names.
        case: "catchers in order, the matching one second",
        catchers: &[
            Catcher {
                errors: &[TIMEOUT_ERROR],
                next: WRONG,
            },
            Catcher {
                errors: &[ALL_ERRORS],
                next: CAUGHT,
            },
        ],
        routes_to: Some(CAUGHT),
    },
    Row {
        // A catcher that names only another error answers for nothing here.
        case: "a catcher naming an unrelated language error",
        catchers: &[Catcher {
            errors: &[TIMEOUT_ERROR],
            next: WRONG,
        }],
        routes_to: None,
    },
    Row {
        case: "a catcher naming another service's refusal",
        catchers: &[Catcher {
            errors: &[OTHER_SERVICE_ERROR],
            next: WRONG,
        }],
        routes_to: None,
    },
];

/// The definition of a row: one `Task` state publishing to `topic_arn` under `catchers`,
/// plus a `Pass` state for each branch they name — and no state nothing reaches, since a
/// definition names only the states it can enter.
fn definition(topic_arn: &str, catchers: &[Catcher]) -> String {
    let mut task = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::sns:publish",
        "Parameters": { "TopicArn": topic_arn, "Message": "routed" },
        "End": true,
    });
    let mut states = json!({});
    if !catchers.is_empty() {
        task["Catch"] = Value::Array(
            catchers
                .iter()
                .map(|catcher| json!({ "ErrorEquals": catcher.errors, "Next": catcher.next }))
                .collect(),
        );
        for catcher in catchers {
            states[catcher.next] = json!({ "Type": "Pass", "End": true });
        }
    }
    states["Publish"] = task;
    json!({ "StartAt": "Publish", "States": states }).to_string()
}

/// The names of the states an execution entered, in order.
fn entered(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| event.pointer("/stateEnteredEventDetails/name"))
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

/// The whole refusal Amazon SNS words for a publish the role's documents carry no grant
/// for, over `topic_arn` as `role_arn`'s session.
fn refusal(role_arn: &str, topic_arn: &str, cause: &str) -> String {
    let principal = session_principal(role_arn, cause);
    format!(
        "User: {principal} is not authorized to perform: {PUBLISH_ACTION} on resource: \
         {topic_arn} because no identity-based policy allows the {PUBLISH_ACTION} \
         action{REFUSAL_METADATA}"
    )
}

/// RULE: the withheld publish is an ordinary task failure under Amazon SNS's own error
/// name, so a catcher that names that failure routes the execution down its branch with
/// the `{Error, Cause}` output, one that does not leaves the execution `FAILED`, and
/// nothing is published either way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_withheld_publish_is_routed_by_what_the_catchers_name() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (queue_url, topic_arn) =
            topic_with_subscriber(&router, &format!("sfn-sns-denied-catch-{index}")).await;
        // The role says nothing about any call of the service, so the publish is withheld.
        let role_arn = role(
            &router,
            &format!("SnsDeniedCatchRole{index}"),
            &json!({ "Version": "2012-10-17", "Statement": [logs_only()] }),
        )
        .await;
        let created = sfn(
            &router,
            "CreateStateMachine",
            &json!({
                "name": format!("sfn-sns-denied-catch-{index}"),
                "definition": definition(&topic_arn, row.catchers),
                "roleArn": role_arn,
            }),
        )
        .await;
        let machine_arn = string(&created, "stateMachineArn");

        let (execution_arn, described) = ran_with_input(&router, &machine_arn, None, case).await;
        let events = history(&router, &execution_arn).await;

        // Whatever the catchers name, the call was refused: one task failure under the
        // error Amazon SNS models for an unauthorized call, carrying its own sentence.
        let failed = of_type(&events, TASK_FAILED);
        assert_eq!(
            (of_type(&events, TASK_SUCCEEDED).len(), failed.len()),
            (0, 1),
            "case {case:?}: expected exactly one {TASK_FAILED} and no {TASK_SUCCEEDED}, \
             got {events:?}"
        );
        let details = &failed[0]["taskFailedEventDetails"];
        assert_eq!(
            details.get("error").and_then(Value::as_str),
            Some(AUTHORIZATION_ERROR),
            "case {case:?}: expected the error Amazon SNS models for an unauthorized call, \
             got {details:?}"
        );
        let cause = string(details, "cause");
        let expected = refusal(&role_arn, &topic_arn, &cause);
        assert_eq!(
            normalized(case, &cause),
            expected,
            "case {case:?}: expected the refusal {expected:?}, got {cause:?}"
        );

        let visited = entered(&events);
        match row.routes_to {
            Some(branch) => {
                // The catcher that named the failure carried the execution to its own
                // branch, which ran to its end: the fallback a client writes for a denial
                // is reachable.
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the caught execution to reach its end state, \
                     got {described:?}"
                );
                assert!(
                    visited.contains(&branch.to_string()),
                    "case {case:?}: expected {branch:?} to be entered, got {visited:?}"
                );
                // What the branch was handed is the catcher's error output, which its
                // `Pass` carried to the end of the execution.
                let output = string(&described, "output");
                let handed: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
                    panic!("case {case:?}: expected the error output, got {output:?} ({error})")
                });
                assert_eq!(
                    handed.get("Error").and_then(Value::as_str),
                    Some(AUTHORIZATION_ERROR),
                    "case {case:?}: expected the caught error, got {handed:?}"
                );
                assert_eq!(
                    handed
                        .get("Cause")
                        .and_then(Value::as_str)
                        .map(|caught| normalized(case, caught)),
                    Some(expected.clone()),
                    "case {case:?}: expected the caught cause to be the refusal, got {handed:?}"
                );
            }
            None => {
                // Nothing named the failure, so it settled the execution: FAILED under the
                // invoked service's own error, and no branch was taken.
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("FAILED"),
                    "case {case:?}: expected FAILED, got {described:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(AUTHORIZATION_ERROR),
                    "case {case:?}: expected the execution to fail under the refusal, got \
                     {described:?}"
                );
                assert_eq!(
                    normalized(case, &string(&described, "cause")),
                    expected,
                    "case {case:?}: expected the execution's cause to be the refusal, got \
                     {described:?}"
                );
            }
        }
        // The branch no row may take, named by every catcher that does not answer for this
        // failure.
        assert!(
            !visited.contains(&WRONG.to_string()),
            "case {case:?}: expected {WRONG:?} never to be entered, got {visited:?}"
        );
        // And the far end: the call the documents withheld was not made, so the topic's
        // subscriber received nothing.
        let bodies = delivered(&router, &queue_url).await;
        assert!(
            bodies.is_empty(),
            "case {case:?}: expected the subscriber to receive nothing, got {bodies:?}"
        );
    }
}

/// The other half of the rule: the same catcher shapes under a role the documents DO grant
/// the publish never route anywhere, because there is no failure to route — the branch is
/// entered by the DENIAL, not by the catcher being written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_granted_publish_takes_no_catch_branch() {
    let router = emulator();
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (queue_url, topic_arn) =
            topic_with_subscriber(&router, &format!("sfn-sns-granted-catch-{index}")).await;
        let role_arn = role(
            &router,
            &format!("SnsGrantedCatchRole{index}"),
            &json!({
                "Version": "2012-10-17",
                "Statement": [logs_only(), allow("sns:Publish", &topic_arn)],
            }),
        )
        .await;
        let created = sfn(
            &router,
            "CreateStateMachine",
            &json!({
                "name": format!("sfn-sns-granted-catch-{index}"),
                "definition": definition(&topic_arn, row.catchers),
                "roleArn": role_arn,
            }),
        )
        .await;
        let machine_arn = string(&created, "stateMachineArn");

        let (execution_arn, described) = ran_with_input(&router, &machine_arn, None, case).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the granted publish to succeed, got {described:?}"
        );
        let events = history(&router, &execution_arn).await;
        let succeeded = of_type(&events, TASK_SUCCEEDED);
        assert_eq!(
            (succeeded.len(), of_type(&events, TASK_FAILED).len()),
            (1, 0),
            "case {case:?}: expected exactly one {TASK_SUCCEEDED} and no {TASK_FAILED}, \
             got {events:?}"
        );
        // No branch of this definition was entered: every state a catcher names is a
        // failure path.
        let visited = entered(&events);
        for branch in [CAUGHT, WRONG] {
            assert!(
                !visited.contains(&branch.to_string()),
                "case {case:?}: expected {branch:?} never to be entered, got {visited:?}"
            );
        }
        // What the task reports is Amazon SNS's own answer, and the subscriber really
        // received the notification.
        let output = string(&succeeded[0]["taskSucceededEventDetails"], "output");
        let result: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
            panic!("case {case:?}: expected the publish result, got {output:?} ({error})")
        });
        let message_id = result["MessageId"].as_str().unwrap_or_default().to_string();
        assert!(
            !message_id.is_empty(),
            "case {case:?}: expected the MessageId Amazon SNS minted, got {result:?}"
        );
        let bodies = delivered(&router, &queue_url).await;
        assert_eq!(
            bodies.len(),
            1,
            "case {case:?}: expected one notification at the subscriber, got {bodies:?}"
        );
        let envelope: Value = serde_json::from_str(&bodies[0]).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the subscriber receives a notification document, got {:?} \
                 ({error})",
                bodies[0]
            )
        });
        assert_eq!(
            envelope["MessageId"].as_str(),
            Some(message_id.as_str()),
            "case {case:?}: expected the notification of the very message the task \
             published, got {envelope:?}"
        );
    }
}
