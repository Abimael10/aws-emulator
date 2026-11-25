// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH KMS action the denial a task reply is refused with NAMES,
//! driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — so the
//! document the refusal is decided against is the key policy KMS's own handlers stored and
//! the execution role is one IAM's own handlers hold.
//!
//! The rule under test: a reply reports its `error`, `cause` or `output` INTO an execution
//! whose payloads the state machine's customer managed key already sealed — "for an
//! execution with encryption enabled, Step Functions will encrypt the error and cause
//! fields using the KMS key for the execution role"
//! (`smithy_query(query="doc",shape="SendTaskFailure")`) — so the use of the key such a
//! reply makes OPENS the data key those payloads go under, and the relayed denial names
//! exactly that use: the DECRYPT, never the mint of a data key the reply does not ask for.
//! Which action the sentence carries is a requirement, because the documented triage for
//! `KmsAccessDeniedException` is to grant the action the relayed message names on the key.
//!
//! The rows span what the rule generalises over: the reply operation and which of its
//! sealed members it carries, under a deny of the crypto actions an account writes out BY
//! NAME — so the action the denial carries cannot be read off the statement's shape — and,
//! the discriminating case, under a deny of the MINT family alone, which withholds nothing
//! of the use a reply inside the data key's reuse period makes and therefore serves the
//! very same reply. The deny of the decrypt alone, and the wildcard forms of the same
//! statement, are `send_task_failure_denied_key_policy`'s rows.
//!
//! The harness (`Chain`, the key policies, `customer_managed`) is
//! `execution_data_encryption`'s and the execution role is
//! `role_log_and_key_authorization`'s; `tests/all/main.rs` is this binary's crate root, so
//! each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::activity_tasks::activity_definition;
use crate::execution_data_encryption::{
    customer_managed, granting_policy, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::role_log_and_key_authorization::{data_key_policy, role};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT};

/// The use a reply into an already-running execution makes of that execution's key, as a
/// policy statement's `Action` names it: the data key its payloads already go under is
/// opened.
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The mint of the NEXT data key — "when the period expires, Step Functions will call
/// `GenerateDataKey`" (`kmsDataKeyReusePeriodSeconds`) — which a reply inside that period
/// does not make, so no denial of one of these replies names it.
const MINT_ACTION: &str = "kms:GenerateDataKey";

/// The bound the scheduling state declares on its task: longer than this suite takes, so
/// no bound the clock owes closes a task while a key policy is changed under it.
const TASK_BOUND_SECONDS: u32 = 300;

/// The reuse period the state machines here are configured with, at the published ceiling
/// of `kmsDataKeyReusePeriodSeconds` (range 60..900, from
/// `smithy_query(query="shape",shape="EncryptionConfiguration")`): the longest period the
/// model admits, so the rows that rely on a reply being made INSIDE it have generous room
/// and none of them depends on how fast the host runs.
const REUSE_PERIOD_SECONDS: i32 = 900;

/// The name a worker here polls under, which the service only logs.
const WORKER: &str = "reply-denial-worker";

/// The error and the cause a failure reports, inside the modelled lengths (0..256,
/// 0..32768).
const REPORTED_ERROR: &str = "BoomWithKms";
const REPORTED_CAUSE: &str = "needs encryption";

/// The output a successful reply relays, which becomes the execution's own.
const REPORTED_OUTPUT: &str = r#"{"handled":true}"#;

/// The input the executions here are started on, which no refusal may disturb.
const EXECUTION_INPUT: &str = r#"{"leg":"reply-denial"}"#;

/// The status an execution waiting at a claimed task reports.
const RUNNING: &str = "RUNNING";

/// How long a poll for a terminal status waits before giving up: generous, so the outcome
/// is waited for on any host rather than timed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// What the execution reports once the reply is served.
struct Outcome {
    status: &'static str,
    error: Option<&'static str>,
    cause: Option<&'static str>,
    output: Option<&'static str>,
}

/// One row: the statement the key policy denies with, the reply it is denied against, and
/// whether that deny withholds the use the reply really makes.
struct Row {
    case: &'static str,
    /// The actions the key's own policy explicitly denies to every principal.
    denied: &'static [&'static str],
    operation: &'static str,
    /// The reply body, built from the `taskToken` a worker holds.
    request: fn(&str) -> Value,
    /// Whether the deny covers the use the reply makes, so the reply is refused.
    refused: bool,
    outcome: Outcome,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The policy the account writes without a wildcard: it names the mint beside
            // the decrypt, so the action the denial carries cannot be read off the
            // statement's shape.
            case: "a failure reported while the key denies every crypto action by name",
            denied: CRYPTO_ACTIONS,
            operation: "SendTaskFailure",
            request: |token| json!({
                "taskToken": token,
                "error": REPORTED_ERROR,
                "cause": REPORTED_CAUSE,
            }),
            refused: true,
            outcome: Outcome {
                status: "FAILED",
                error: Some(REPORTED_ERROR),
                cause: Some(REPORTED_CAUSE),
                output: None,
            },
        },
        Row {
            case: "an output reported while the key denies every crypto action by name",
            denied: CRYPTO_ACTIONS,
            operation: "SendTaskSuccess",
            request: |token| json!({ "taskToken": token, "output": REPORTED_OUTPUT }),
            refused: true,
            outcome: Outcome {
                status: "SUCCEEDED",
                error: None,
                cause: None,
                output: Some(REPORTED_OUTPUT),
            },
        },
        Row {
            // The discriminating row: the deny covers the MINT and nothing else, and a
            // reply inside the data key's reuse period mints nothing — so the use it makes
            // is untouched and the reply is served.
            case: "an error reported alone while the key denies the mint family alone",
            denied: MINT_ACTIONS,
            operation: "SendTaskFailure",
            request: |token| json!({ "taskToken": token, "error": REPORTED_ERROR }),
            refused: false,
            outcome: Outcome {
                status: "FAILED",
                error: Some(REPORTED_ERROR),
                cause: None,
                output: None,
            },
        },
        Row {
            case: "an output reported while the key denies the mint family alone",
            denied: MINT_ACTIONS,
            operation: "SendTaskSuccess",
            request: |token| json!({ "taskToken": token, "output": REPORTED_OUTPUT }),
            refused: false,
            outcome: Outcome {
                status: "SUCCEEDED",
                error: None,
                cause: None,
                output: Some(REPORTED_OUTPUT),
            },
        },
    ]
}

/// The cryptographic actions of a symmetric encryption key, spelled out one by one: the
/// list an account denies when it revokes the crypto of a key it keeps administering.
const CRYPTO_ACTIONS: &[&str] = &[
    "kms:Encrypt",
    DECRYPT_ACTION,
    "kms:ReEncryptFrom",
    "kms:ReEncryptTo",
    MINT_ACTION,
    "kms:GenerateDataKeyWithoutPlaintext",
];

/// The two actions that MINT a data key, and nothing that opens one.
const MINT_ACTIONS: &[&str] = &[MINT_ACTION, "kms:GenerateDataKeyWithoutPlaintext"];

/// A key policy that still gives the account that owns the key everything — "the default
/// key policy … gives the Amazon Web Services account that owns the KMS key permission to
/// use it in all cryptographic operations" — and DENIES `actions` to every principal
/// beside it, which is how an account revokes one use of a key it keeps administering.
fn policy_denying(actions: &[&str]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "EnableIAMUserPermissions",
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                "Action": "kms:*",
                "Resource": "*"
            },
            {
                "Sid": "DenyOneUseToEveryone",
                "Effect": "Deny",
                "Principal": "*",
                "Action": actions,
                "Resource": "*"
            },
        ]
    })
}

/// RULE: the denial a task reply is refused with names the DECRYPT of the data key the
/// execution's payloads already go under — and a deny that covers the mint alone withholds
/// nothing of that use, so the same reply is served.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_task_reply_names_the_decrypt_of_the_executions_data_key() {
    for (index, row) in rows().into_iter().enumerate() {
        let chain = Chain::new();
        let case = row.case;
        // The execution role is allowed the crypto an execution's data needs, so what
        // answers for the reply below is the KEY's own document and nothing of the role's.
        let role_name = format!("ReplyDenialRole{index}");
        let role_arn = role(&chain, &role_name, &data_key_policy()).await;
        let key = chain.create_key(&granting_policy()).await;
        let (execution, token) =
            outstanding_task(&chain, &format!("reply-denial-{index}"), &key, &role_arn).await;
        let body = (row.request)(&token);

        // The deny goes in with the task already claimed: what is under test is the reply,
        // not the setup.
        chain.put_policy(&key, &policy_denying(row.denied)).await;
        let (status, answer) = chain.call(&SFN, row.operation, &body).await;

        if row.refused {
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
                "case {case:?}: expected the reply to be refused while the key's policy \
                 denies the use it makes, got {status} with {answer:?}"
            );
            assert_relayed_denial(case, &answer, &role_name, &key);

            // The refusal sealed nothing and consumed nothing: the execution is still
            // running and the token still answers its task. The read is asked for METADATA
            // ONLY, because the deny that refused the reply withholds the decrypt of the
            // execution's own data from every principal for as long as it stands.
            let (status, described) = chain
                .call(
                    &SFN,
                    "DescribeExecution",
                    &json!({ "executionArn": execution, "includedData": "METADATA_ONLY" }),
                )
                .await;
            assert_eq!(
                (status, string(&described, "status").as_str()),
                (200, RUNNING),
                "case {case:?}: expected the refused reply to leave the execution \
                 {RUNNING}, got {status} with {described:?}"
            );

            // With the deny lifted the VERY SAME reply is served.
            chain.put_policy(&key, &granting_policy()).await;
            let (status, answer) = chain.call(&SFN, row.operation, &body).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the same reply to be served once the deny is \
                 lifted, got {status} with {answer:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: a reply inside the data key's reuse period mints no data \
                 key, so a deny of the mint withholds nothing of the use it makes, got \
                 {status} with {answer:?}"
            );
            // The read below opens the execution's own data, which this deny leaves to the
            // account, so the record is reported with no policy change of any kind.
        }

        let described = terminal(&chain, case, &execution).await;
        assert_outcome(case, &described, &row.outcome);
    }
}

/// The sentence a refused reply carries, asserted part by part: the fixed text is KMS's own
/// wording for an explicit deny in a resource-based policy, and each variable part comes
/// from the request and the state — the session of the state machine's EXECUTION ROLE the
/// use was asked for, the use itself, and the key the state machine names — beside the two
/// the SDK's own rendering adds per call.
///
/// The ACTION is what this suite is about, so it is asserted twice over: the sentence
/// carries the decrypt, and the whole message never names the mint.
fn assert_relayed_denial(case: &str, answer: &Value, role_name: &str, key: &str) {
    let refusal = answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {answer:?}"));

    // The principal: Step Functions does not hold the execution role, it assumes it, so
    // the identity the use was asked for is a session of that role.
    let prefix = format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/");
    let rest = refusal.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the refusal to blame a session of {role_name} \
             ({prefix:?}), got {refusal:?}"
        )
    });
    let (session, rest) = rest.split_once(' ').unwrap_or_else(|| {
        panic!("case {case:?}: expected a session name after {prefix:?}, got {refusal:?}")
    });
    assert!(
        !session.is_empty(),
        "case {case:?}: expected a session name after {prefix:?}, got {refusal:?}"
    );

    let sentence = format!(
        "is not authorized to perform: {DECRYPT_ACTION} on resource: {key} \
         with an explicit deny in a resource-based policy \
         (Service: Kms, Status Code: {KMS_ACCESS_DENIED_STATUS}, Request ID: "
    );
    let rest = rest.strip_prefix(sentence.as_str()).unwrap_or_else(|| {
        panic!("case {case:?}: expected the refusal to carry {sentence:?}, got {refusal:?}")
    });
    assert!(
        !refusal.contains(MINT_ACTION),
        "case {case:?}: expected the refusal to name the use the reply makes and never \
         {MINT_ACTION:?}, got {refusal:?}"
    );

    // The two values the SDK's own rendering adds per call: the id of the refused request
    // and how many attempts it took.
    let (request_id, attempts) = rest.split_once(") (SDK Attempt Count: ").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the SDK's own metadata to close the refusal, got \
             {refusal:?}"
        )
    });
    assert!(
        !request_id.is_empty(),
        "case {case:?}: expected the refused request's id, got {refusal:?}"
    );
    let attempts = attempts
        .strip_suffix(')')
        .and_then(|digits| digits.parse::<u32>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a counted attempt to close the refusal, got {refusal:?}")
        });
    assert!(
        attempts >= 1,
        "case {case:?}: expected at least the one attempt that was refused, got {attempts}"
    );
}

/// What the served reply made of the execution: its status, the failure it reported, the
/// output it relayed, and the input it may not disturb.
fn assert_outcome(case: &str, described: &Value, outcome: &Outcome) {
    assert_eq!(
        (
            described.get("status").and_then(Value::as_str),
            described.get("error").and_then(Value::as_str),
            described.get("cause").and_then(Value::as_str),
        ),
        (Some(outcome.status), outcome.error, outcome.cause),
        "case {case:?}: expected the execution to end exactly as the reply reported, got \
         {described:?}"
    );
    assert_eq!(
        serde_json::from_str::<Value>(&string(described, "input")).ok(),
        serde_json::from_str::<Value>(EXECUTION_INPUT).ok(),
        "case {case:?}: expected the execution's own input to survive every refusal, got \
         {described:?}"
    );
    match outcome.output {
        Some(output) => assert_eq!(
            serde_json::from_str::<Value>(&string(described, "output")).ok(),
            serde_json::from_str::<Value>(output).ok(),
            "case {case:?}: expected the relayed output to be the execution's own, got \
             {described:?}"
        ),
        None => assert!(
            described.get("output").is_none_or(Value::is_null),
            "case {case:?}: expected a failed execution to report no output, got {described:?}"
        ),
    }
}

/// One RUNNING execution of a state machine encrypted under `key` and acting as
/// `role_arn`, with the `taskToken` a worker now holds: the task every reply above reports
/// into.
async fn outstanding_task(
    chain: &Chain,
    name: &str,
    key: &str,
    role_arn: &str,
) -> (String, String) {
    let activity = string(
        &chain
            .sfn("CreateActivity", &json!({ "name": format!("{name}-activity") }))
            .await,
        "activityArn",
    );
    let mut encryption = customer_managed(key);
    encryption["kmsDataKeyReusePeriodSeconds"] = json!(REUSE_PERIOD_SECONDS);
    let machine = string(
        &chain
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": activity_definition(
                        &activity,
                        json!({ "TimeoutSeconds": TASK_BOUND_SECONDS }),
                    ),
                    "roleArn": role_arn,
                    "encryptionConfiguration": encryption,
                }),
            )
            .await,
        "stateMachineArn",
    );
    let execution = string(
        &chain
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "input": EXECUTION_INPUT }),
            )
            .await,
        "executionArn",
    );
    let answer = chain
        .sfn(
            "GetActivityTask",
            &json!({ "activityArn": activity, "workerName": WORKER }),
        )
        .await;
    let token = answer
        .get("taskToken")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {name:?}: expected the poll to be handed a taskToken, got {answer:?}")
        })
        .to_string();
    assert!(
        !token.is_empty(),
        "case {name:?}: expected a non-empty taskToken, got {answer:?}"
    );
    (execution, token)
}

/// Describe `execution` until it reports a status other than `RUNNING`, and answer that
/// record. The wait is for the STATUS and never for a span: the outcome the reply made due
/// takes as long as the host takes, up to [`TERMINAL_DEADLINE`].
async fn terminal(chain: &Chain, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = chain
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if string(&described, "status") != RUNNING {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to leave {RUNNING}, got {described:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
