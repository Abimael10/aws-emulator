// SPDX-License-Identifier: CC0-1.0

//! WHAT ONE INVOCATION of a command-line-deployed sending function SETTLES ON, outcome class
//! by outcome class: AWS Lambda, Amazon SES API v2, IAM and the token service on ONE engine,
//! over the same deploy path a command-line client drives — the execution role written with
//! IAM's own operations, the code uploaded as a .zip, the provisioning window waited out by
//! reading the function's own state, then `Invoke`.
//!
//! RULE: every `Invoke` of an active function answers HTTP 200 and SETTLES on one terminal
//! outcome, and which outcome it settles on is decided by the service that serves the
//! handler's own SDK call rather than by the runtime: a call that service serves gives the
//! handler the operation's modelled output, a call it refuses gives the handler that service's
//! own error code with the status its model binds that error to and that error's own message,
//! a call the function's execution role does not allow gives the handler the IAM layer's
//! refusal, and a handler that raises instead of returning comes back as the run's own error
//! document under the `X-Amz-Function-Error` signal. No invocation leaves its caller with no
//! answer to poll on, and none of them is a client-side report that the SDK could not build a
//! client for the service.
//!
//! The rows differ along what the rule generalises over: WHICH outcome class the request's own
//! defect falls in — a send naming a configuration set this account holds (served, 200), one
//! naming a configuration set it does not hold (`NotFoundException`, 404), one naming a set
//! whose sending is switched off (`SendingPausedException`, 400), the same send made by a
//! function whose execution role allows only the log writes (`AccessDeniedException`, 403), and
//! a handler that raises (a terminal function error). Each row asserts the class, the status
//! and the message the handler read back; then the state the serving service holds afterwards
//! is asserted too — only the served send's message exists, only its one recipient was charged
//! against the account's sending quota, and the paused set is still paused.
//!
//! The deploy path itself belongs to the sibling suite [`crate::cli_deployed_email_sender`],
//! whose rows span which verified identity covers the sender; it is DRIVEN from there rather
//! than copied, and what this suite adds is the outcome table around it.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `state::execution::max_concurrent_runs` invocations at once for the whole process.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_lambda_awsgirapiservice::state::execution::FUNCTION_ERROR_UNHANDLED;

// The whole command-line deploy path — the role IAM holds, the verified identity, the packaged
// upload, the wait for `Active` and the invocation that answers terminally — is the sibling
// suite's, so this one drives those steps rather than a second copy of them.
use crate::cli_deployed_email_sender::{
    email, emulator, given_active_function, given_execution_role,
    given_message_insights_enabled, given_verified_identity, invoke,
    RECIPIENT, SEND_EMAIL_SUCCESS_STATUS,
};
// The statement builders every role in this target is written with.
use crate::event_source_mapping_role_access::{allow, logs_only};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The address identity the account verifies, which every row sends from: what the rows differ
/// along here is the outcome class, not the sender, so one verified sender serves them all.
const SENDER: &str = "deploy-bot@outcomes.example.com";

/// The configuration set the account holds, the one it holds with sending switched off, and a
/// name it holds no set under at all.
const HELD_SET: &str = "cli-deployed-outcomes-held";
const PAUSED_SET: &str = "cli-deployed-outcomes-paused";
const MISSING_SET: &str = "cli-deployed-outcomes-absent";

/// The execution role allowed the send the handler makes, and the one allowed only the log
/// writes a function makes — the two roles the rows are invoked under.
const SENDING_ROLE: &str = "cli-deployed-outcomes-sender";
const LOGS_ONLY_ROLE: &str = "cli-deployed-outcomes-logs-only";

/// The function running as each of those roles.
const SENDING_FUNCTION: &str = "cli-deployed-outcomes";
const LOGS_ONLY_FUNCTION: &str = "cli-deployed-outcomes-no-grant";

/// The error codes SES v2's model binds `SendEmail`'s refusals to, and the status each one
/// carries (`smithy_query(query="operation",operation="SendEmail")`: `NotFoundException` 404,
/// `SendingPausedException` 400).
const NOT_FOUND: &str = "NotFoundException";
const NOT_FOUND_STATUS: i64 = 404;
const SENDING_PAUSED: &str = "SendingPausedException";
const SENDING_PAUSED_STATUS: i64 = 400;

/// The error code the IAM layer in front of the service refuses a call no identity-based policy
/// in force allows with, and the status such a refusal carries. It is not one of `SendEmail`'s
/// modelled errors: the decision is taken ahead of the service, as it is on AWS.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The action a send performs, as a statement names it and as a refusal of one reports it.
const SEND_EMAIL_ACTION: &str = "ses:SendEmail";

/// The message the raising row's handler raises, which is the one variable part of the error
/// document that run answers with.
const RAISED_DETAIL: &str = "the deployment report could not be assembled";

/// The exception class that row raises, which the run reports as the document's `errorType`.
const RAISED_CLASS: &str = "RuntimeError";

/// How SES words `NotFoundException` for a configuration set the account does not hold: fixed
/// text around the one variable part, the name the request named, which the service delimits
/// with angle brackets.
///
/// Spelled here rather than read from the service's own helper
/// (`service-com-amazonaws-sesv2-simpleemailservice-v2/src/state/configuration_sets.rs`,
/// `configuration_set_not_found`), so the wording is pinned independently of the code under
/// test and the two have to be corrected together.
fn expected_set_not_found(name: &str) -> String {
    format!("Configuration set <{name}> does not exist.")
}

/// How SES words `SendingPausedException` for a send whose configuration set has sending
/// switched off (`SendingOptions$SendingEnabled` false, which
/// `PutConfigurationSetSendingOptions` is what turns off). Fixed text around the set's name,
/// taken from the request, and spelled here for the same reason as the sentence above
/// (`configuration_set_sending_paused`).
fn expected_sending_paused(name: &str) -> String {
    format!("Sending is paused for configuration set {name}")
}

/// How IAM opens the refusal of a call no identity-based policy in force allows: the principal
/// the call was signed as — the session of the function's own execution role — and the action it
/// asked to perform. The rest of that sentence (the resource segment and the reason clause) is
/// the subject of its own work, so a row pins the opening, which is what tells an operator
/// WHICH identity was refused WHICH action.
fn expected_denial_opening(role: &str, function: &str) -> String {
    format!(
        "User: arn:aws:sts::{}:assumed-role/{role}/{function} is not authorized to perform: \
         {SEND_EMAIL_ACTION}",
        engine::emulator_account_id()
    )
}

/// The handler every row runs: it builds a client for the email service, sends the message the
/// event describes — under the configuration set the event names, when it names one — and
/// reports what came back, so the invocation's payload IS what the SDK gave the guest. The
/// event may instead ask it to raise, which is the row whose outcome is the run's own failure.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions


def handler(event, context):
    if event.get("raise"):
        raise RuntimeError(event["raise"])
    try:
        client = boto3.client("sesv2")
    except botocore.exceptions.UnknownServiceError as error:
        return {"ok": False, "clientError": type(error).__name__, "detail": str(error)}
    request = {
        "FromEmailAddress": event["from"],
        "Destination": {"ToAddresses": [event["to"]]},
        "Content": {
            "Simple": {
                "Subject": {"Data": event["subject"]},
                "Body": {"Text": {"Data": event["body"]}},
            },
        },
    }
    if event.get("configurationSet"):
        request["ConfigurationSetName"] = event["configurationSet"]
    try:
        answered = client.send_email(**request)
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "errorCode": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "messageId": answered["MessageId"],
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
    }
"#;

/// The sentence a refusal must carry: the whole of it, or its opening when the rest belongs to
/// another piece of work.
enum Sentence {
    Whole(String),
    Opening(String),
}

/// What one row's invocation must settle on.
enum Outcome {
    /// The email service served the send: the handler read back the modelled `MessageId`.
    Served,
    /// A service refused the call with this error code, at this status, in this sentence.
    Refused {
        code: &'static str,
        status: i64,
        sentence: Sentence,
    },
    /// The handler raised instead of returning: the invocation carries the function-error
    /// signal and the run's own error document.
    Raised,
}

/// One row: which deployed function is invoked, what the send it makes names, and what must
/// come of that invocation.
struct Row {
    case: &'static str,
    function: &'static str,
    /// The configuration set the send names, when it names one.
    configuration_set: Option<&'static str>,
    outcome: Outcome,
}

/// The rows, in the order they run: one per outcome class an invocation of a deployed sending
/// function can settle on.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a send naming a configuration set the account holds",
            function: SENDING_FUNCTION,
            configuration_set: Some(HELD_SET),
            outcome: Outcome::Served,
        },
        Row {
            case: "a send naming a configuration set the account does not hold",
            function: SENDING_FUNCTION,
            configuration_set: Some(MISSING_SET),
            outcome: Outcome::Refused {
                code: NOT_FOUND,
                status: NOT_FOUND_STATUS,
                sentence: Sentence::Whole(expected_set_not_found(MISSING_SET)),
            },
        },
        Row {
            case: "a send naming a configuration set whose sending is switched off",
            function: SENDING_FUNCTION,
            configuration_set: Some(PAUSED_SET),
            outcome: Outcome::Refused {
                code: SENDING_PAUSED,
                status: SENDING_PAUSED_STATUS,
                sentence: Sentence::Whole(expected_sending_paused(PAUSED_SET)),
            },
        },
        Row {
            case: "a send the function's execution role does not allow",
            function: LOGS_ONLY_FUNCTION,
            configuration_set: Some(HELD_SET),
            outcome: Outcome::Refused {
                code: ACCESS_DENIED,
                status: ACCESS_DENIED_STATUS,
                sentence: Sentence::Opening(expected_denial_opening(
                    LOGS_ONLY_ROLE,
                    LOGS_ONLY_FUNCTION,
                )),
            },
        },
        Row {
            case: "a handler that raises instead of returning",
            function: SENDING_FUNCTION,
            configuration_set: None,
            outcome: Outcome::Raised,
        },
    ]
}

/// The event one row hands its handler.
fn event(row: &Row) -> Value {
    if matches!(row.outcome, Outcome::Raised) {
        return json!({ "raise": RAISED_DETAIL });
    }
    json!({
        "from": SENDER,
        "to": RECIPIENT,
        "subject": format!("deployment report: {}", row.case),
        "body": format!("sent by the deployed function for {}", row.case),
        "configurationSet": row.configuration_set,
    })
}

/// Create the configuration set `name` through SES v2's own API, switching its sending off when
/// `sending_enabled` is false — which is what makes a set a send naming it is paused for.
async fn given_configuration_set(router: &MultiServiceRouter, name: &str, sending_enabled: bool) {
    let what = format!("the configuration set {name}");
    email(
        router,
        &what,
        "POST",
        "/v2/email/configuration-sets",
        &serde_json::to_vec(&json!({ "ConfigurationSetName": name }))
            .expect("the create body serializes"),
        200,
    )
    .await;
    if sending_enabled {
        return;
    }
    email(
        router,
        &what,
        "PUT",
        &format!("/v2/email/configuration-sets/{name}/sending"),
        &serde_json::to_vec(&json!({ "SendingEnabled": false }))
            .expect("the sending body serializes"),
        200,
    )
    .await;
}

/// The `SendingOptions$SendingEnabled` SES reports for the configuration set `name`.
async fn sending_enabled(router: &MultiServiceRouter, what: &str, name: &str) -> Value {
    let set = email(
        router,
        what,
        "GET",
        &format!("/v2/email/configuration-sets/{name}"),
        &[],
        200,
    )
    .await;
    set["SendingOptions"]["SendingEnabled"].clone()
}

/// One invocation per outcome class, each asserted on what the handler read back and then on
/// what the email service holds once every row has run.
#[tokio::test]
async fn every_invocation_of_a_deployed_sender_settles_on_one_outcome_class() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();

    // The deploy path, twice over: a role that may send and a role that may only write logs,
    // each with its own function, so the refusal the IAM layer takes is taken on a function
    // that differs from the served one in its ROLE alone.
    let sending_role = given_execution_role(
        &router,
        SENDING_ROLE,
        json!([logs_only(), allow("ses", &["SendEmail"], "*")]),
    )
    .await;
    let logs_only_role = given_execution_role(&router, LOGS_ONLY_ROLE, json!([logs_only()])).await;
    given_verified_identity(&router, SENDER).await;
    given_message_insights_enabled(&router).await;
    given_configuration_set(&router, HELD_SET, true).await;
    given_configuration_set(&router, PAUSED_SET, false).await;
    given_active_function(&router, SENDING_FUNCTION, HANDLER_SOURCE, &sending_role).await;
    given_active_function(
        &router,
        LOGS_ONLY_FUNCTION,
        HANDLER_SOURCE,
        &logs_only_role,
    )
    .await;
    assert_eq!(
        sending_enabled(&router, "setup the paused set", PAUSED_SET).await,
        json!(false),
        "setup the paused set: expected SES to report sending switched off for {PAUSED_SET:?}"
    );

    let mut served: Vec<String> = Vec::new();
    for row in rows() {
        let case = row.case;
        let (signal, payload) = invoke(&router, case, row.function, &event(&row)).await;

        match &row.outcome {
            Outcome::Raised => {
                // A run that failed is still a terminal answer: the signal says the handler
                // faulted, and the document names the class and the message it faulted with.
                assert_eq!(
                    signal.as_deref(),
                    Some(FUNCTION_ERROR_UNHANDLED),
                    "case {case:?}: expected the function-error signal of a handler that \
                     raised, got {signal:?} with {payload}"
                );
                assert_eq!(
                    payload["errorType"],
                    json!(RAISED_CLASS),
                    "case {case:?}: expected the class the handler raised, got {payload}"
                );
                assert_eq!(
                    payload["errorMessage"],
                    json!(RAISED_DETAIL),
                    "case {case:?}: expected the message the handler raised with, got {payload}"
                );
            }
            Outcome::Served | Outcome::Refused { .. } => {
                // However the call went, the RUN completed: the handler's own document is the
                // answer, and the SDK reached the service rather than failing to build a
                // client for it.
                assert_eq!(
                    signal, None,
                    "case {case:?}: the invocation must carry no function error, got {signal:?} \
                     with {payload}"
                );
                assert_eq!(
                    payload["clientError"],
                    Value::Null,
                    "case {case:?}: the runtime's SDK must reach the email service, got {payload}"
                );
            }
        }

        match &row.outcome {
            Outcome::Raised => {}
            Outcome::Served => {
                assert_eq!(
                    payload["ok"],
                    json!(true),
                    "case {case:?}: expected the send to be served, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(SEND_EMAIL_SUCCESS_STATUS),
                    "case {case:?}: expected the modelled success status, got {payload}"
                );
                let message_id = payload["messageId"].as_str().unwrap_or_default().to_string();
                assert!(
                    !message_id.is_empty(),
                    "case {case:?}: expected the message id the service minted, got {payload}"
                );

                // What SES v2 itself holds about that message: the send the handler reported is
                // the send that happened.
                let insights = email(
                    &router,
                    case,
                    "GET",
                    &format!("/v2/email/insights/{message_id}"),
                    &[],
                    200,
                )
                .await;
                assert_eq!(
                    insights["FromEmailAddress"],
                    json!(SENDER),
                    "case {case:?}: expected the sender the handler sent from, got {insights}"
                );
                served.push(message_id);
            }
            Outcome::Refused {
                code,
                status,
                sentence,
            } => {
                assert_eq!(
                    payload["ok"],
                    json!(false),
                    "case {case:?}: expected the call to be refused, got {payload}"
                );
                assert_eq!(
                    payload["errorCode"],
                    json!(code),
                    "case {case:?}: expected the serving side's own error code, got {payload}"
                );
                assert_eq!(
                    payload["status"],
                    json!(status),
                    "case {case:?}: expected the status bound to {code}, got {payload}"
                );
                let message = payload["message"].as_str().unwrap_or_default();
                match sentence {
                    Sentence::Whole(expected) => assert_eq!(
                        message, expected,
                        "case {case:?}: expected the refusal's own sentence, got {message:?}"
                    ),
                    Sentence::Opening(expected) => assert!(
                        message.starts_with(expected),
                        "case {case:?}: expected the refusal to open {expected:?}, got {message:?}"
                    ),
                }
            }
        }
    }

    // The other half of every row: the refused sends did none of the work. Exactly the served
    // rows minted a message, the account was charged for that message's one recipient alone
    // (`SendQuota$SentLast24Hours`, which `GetAccount` reports), and the paused set is still
    // paused.
    let expected_served = rows()
        .iter()
        .filter(|row| matches!(row.outcome, Outcome::Served))
        .count();
    assert_eq!(
        served.len(),
        expected_served,
        "expected one message id per served send, got {served:?}"
    );
    let account = email(
        &router,
        "the account's own view of what the rows sent",
        "GET",
        "/v2/email/account",
        &[],
        200,
    )
    .await;
    // The quantity, not the JSON number encoding: a count SES reports as `1` and one it
    // reports as `1.0` are the same charge.
    assert_eq!(
        account["SendQuota"]["SentLast24Hours"].as_f64(),
        Some(expected_served as f64),
        "expected the account to have been charged for the served sends alone, got {account}"
    );
    assert_eq!(
        sending_enabled(&router, "the paused set after the rows", PAUSED_SET).await,
        json!(false),
        "expected the refused send to leave {PAUSED_SET:?} switched off"
    );
}
