// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a task reply whose payload a key policy EXPLICITLY DENIES the
//! crypto of, driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — so the
//! key policy the refusal is decided against is the document KMS's own handlers stored
//! and the execution role is one IAM's own handlers hold.
//!
//! The rule under test: the `error` and the `cause` a reply reports into an execution of a
//! state machine on a customer managed key are sealed under that key — "for an execution
//! with encryption enabled, Step Functions will encrypt the error and cause fields using
//! the KMS key for the execution role"
//! (`smithy_query(query="doc",shape="SendTaskFailure")`) — so a statement of the key's own
//! policy that DENIES the crypto refuses the reply with the modelled
//! `KmsAccessDeniedException` (HTTP 400, "either your KMS key policy or API caller does
//! not have the required permissions") rather than falling back to another key or sealing
//! nothing and succeeding, and the sentence it carries is the one KMS worded for an
//! explicit deny in a resource-based policy, naming the session of the state machine's
//! execution role the use was asked for and that state machine's own `kmsKeyId`. A reply
//! that carries NEITHER field seals nothing and is served while the same deny stands: "a
//! caller can mark a task as fail without using any KMS permissions in the execution role
//! if the caller provides a null value for both error and cause fields". The refusal
//! writes nothing, so the very same reply is served once the deny is lifted.
//!
//! The rows span what the rule generalises over: the shape of the denying statement (the
//! one action the use asks for, the family of crypto actions written with a wildcard, and
//! the whole KMS namespace), which of the two sealed fields the reply carries, and the
//! reply that carries neither.
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

/// The bound the scheduling state declares on its task: longer than this suite takes, so
/// no bound the clock owes closes a task while a key policy is changed under it.
const TASK_BOUND_SECONDS: u32 = 300;

/// The name a worker here polls under, which the service only logs.
const WORKER: &str = "denied-key-worker";

/// The error a reply reports, inside the modelled `error` length (0..256).
const REPORTED_ERROR: &str = "BoomWithKms";

/// The cause it reports beside it, inside the modelled `cause` length (0..32768).
const REPORTED_CAUSE: &str = "needs encryption";

/// The input the executions here are started on, which no refusal may disturb.
const EXECUTION_INPUT: &str = r#"{"leg":"denied-key"}"#;

/// The status an execution waiting at a claimed task reports.
const RUNNING: &str = "RUNNING";

/// How long a poll for a terminal status waits before giving up: generous, so the outcome
/// is waited for on any host rather than timed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The use of the key opening an execution's data key asks for, as a policy statement's
/// `Action` names it.
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// One row: the statement the key policy denies with, the fields the reply carries, and
/// whether the reply therefore needs the key at all.
struct Row {
    case: &'static str,
    /// The actions the key policy explicitly denies to every principal.
    denied: &'static [&'static str],
    /// The `error` the reply reports, and `None` for one that reports none.
    error: Option<&'static str>,
    /// The `cause` it reports, and `None` for one that reports none.
    cause: Option<&'static str>,
}

impl Row {
    /// Whether this reply carries data the key has to seal, which is what needs the key.
    fn seals(&self) -> bool {
        self.error.is_some() || self.cause.is_some()
    }

    /// The reply this row sends on `token`.
    fn request(&self, token: &str) -> Value {
        let mut body = json!({ "taskToken": token });
        let members = body.as_object_mut().expect("the request is an object");
        if let Some(error) = self.error {
            members.insert("error".to_string(), json!(error));
        }
        if let Some(cause) = self.cause {
            members.insert("cause".to_string(), json!(cause));
        }
        body
    }
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a deny of the one action the reply's use of the key asks for",
            denied: &["kms:Decrypt"],
            error: Some(REPORTED_ERROR),
            cause: Some(REPORTED_CAUSE),
        },
        Row {
            case: "a deny of the crypto family, with the error alone reported",
            denied: &[
                "kms:Encrypt",
                "kms:Decrypt",
                "kms:ReEncrypt*",
                "kms:GenerateDataKey*",
            ],
            error: Some(REPORTED_ERROR),
            cause: None,
        },
        Row {
            case: "a deny of the whole KMS namespace, with the cause alone reported",
            denied: &["kms:*"],
            error: None,
            cause: Some(REPORTED_CAUSE),
        },
        Row {
            case: "a reply carrying neither field, under the same deny",
            denied: &[
                "kms:Encrypt",
                "kms:Decrypt",
                "kms:ReEncrypt*",
                "kms:GenerateDataKey*",
            ],
            error: None,
            cause: None,
        },
    ]
}

/// A key policy that still gives the account that owns the key everything — "the default
/// key policy … gives the Amazon Web Services account that owns the KMS key permission to
/// use it in all cryptographic operations" — and DENIES `actions` to every principal
/// beside it, which is how an account revokes the crypto of a key it keeps administering.
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
                "Sid": "DenyCryptoToEveryone",
                "Effect": "Deny",
                "Principal": "*",
                "Action": actions,
                "Resource": "*"
            },
        ]
    })
}

/// RULE: a reply whose payload the key's own policy denies the crypto of is refused the
/// modelled `KmsAccessDeniedException`, in the words KMS gives an explicit deny in a
/// resource-based policy — and a reply that seals nothing is served under that same deny.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_task_failure_under_a_denying_key_policy_is_refused_with_the_kms_refusal() {
    for (index, row) in rows().into_iter().enumerate() {
        let chain = Chain::new();
        let case = row.case;
        // The execution role is allowed the crypto an execution's data needs, so what
        // refuses the reply below is the KEY's own document and nothing of the role's.
        let role_name = format!("DeniedKeyRole{index}");
        let role_arn = role(&chain, &role_name, &data_key_policy()).await;
        let key = chain.create_key(&granting_policy()).await;
        let (execution, token) =
            outstanding_task(&chain, &format!("denied-key-{index}"), &key, &role_arn).await;
        let body = row.request(&token);

        // The deny goes in with the task already claimed: what is under test is the
        // reply, not the setup.
        chain.put_policy(&key, &policy_denying(row.denied)).await;
        let (status, answer) = chain.call(&SFN, "SendTaskFailure", &body).await;

        if !row.seals() {
            assert_eq!(
                status, 200,
                "case {case:?}: a reply carrying neither field seals nothing, so it needs no \
                 use of the key, got {status} with {answer:?}"
            );
            // The deny is lifted before the record is READ, because a read of an
            // execution's own data opens the key the deny withholds.
            chain.put_policy(&key, &granting_policy()).await;
            let described = terminal(&chain, case, &execution).await;
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("error").and_then(Value::as_str),
                    described.get("cause").and_then(Value::as_str),
                ),
                (Some("FAILED"), None, None),
                "case {case:?}: expected the reported failure to fail the execution with \
                 neither field, got {described:?}"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answer).as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected the reply to be refused while the key's policy denies \
             the crypto, got {status} with {answer:?}"
        );
        assert_refusal_sentence(case, &answer, &role_name, &key);

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
            "case {case:?}: expected the refused reply to leave the execution {RUNNING}, got \
             {status} with {described:?}"
        );

        // With the deny lifted the VERY SAME reply is served, and what it reported is the
        // execution's outcome.
        chain.put_policy(&key, &granting_policy()).await;
        let (status, answer) = chain.call(&SFN, "SendTaskFailure", &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the same reply to be served once the deny is lifted, got \
             {status} with {answer:?}"
        );
        let described = terminal(&chain, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
                described.get("cause").and_then(Value::as_str),
            ),
            (Some("FAILED"), row.error, row.cause),
            "case {case:?}: expected the execution to fail with exactly what the reply \
             reported, got {described:?}"
        );
        assert_eq!(
            string(&described, "input"),
            EXECUTION_INPUT,
            "case {case:?}: expected the execution's own input to be reported back byte for \
             byte, got {described:?}"
        );
        assert!(
            described.get("output").is_none_or(Value::is_null),
            "case {case:?}: expected a failed execution to report no output, got {described:?}"
        );
    }
}

/// The sentence a refused reply carries, asserted part by part: the fixed text is KMS's
/// own wording for an explicit deny in a resource-based policy, and each variable part
/// comes from the request and the state — the session of the state machine's EXECUTION
/// ROLE the use was asked for, the use itself, and the key the state machine names —
/// beside the two the SDK's own rendering adds per call.
fn assert_refusal_sentence(case: &str, answer: &Value, role_name: &str, key: &str) {
    let refusal = answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message in {answer:?}"));

    // The principal: Step Functions does not hold the execution role, it assumes it, so
    // the identity the use was asked for is a session of that role.
    let prefix = format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/");
    let rest = refusal.strip_prefix(&prefix).unwrap_or_else(|| {
        panic!("case {case:?}: expected the refusal to blame a session of {role_name} ({prefix:?}), got {refusal:?}")
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

    // The two values the SDK's own rendering adds per call: the id of the refused request
    // and how many attempts it took.
    let (request_id, attempts) = rest.split_once(") (SDK Attempt Count: ").unwrap_or_else(|| {
        panic!("case {case:?}: expected the SDK's own metadata to close the refusal, got {refusal:?}")
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

/// One RUNNING execution of a state machine encrypted under `key` and acting as
/// `role_arn`, with the `taskToken` a worker now holds: the task every reply above
/// reports into.
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
                    "encryptionConfiguration": customer_managed(key),
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
