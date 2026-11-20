// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH use of a state machine's customer managed key a write of
//! caller-supplied data INTO AN ALREADY RUNNING EXECUTION makes, driven over each
//! service's own wire protocol through the production `MultiServiceRouter` with Step
//! Functions, IAM and KMS mounted on ONE engine — so every document a decision reads is
//! one IAM's or KMS's own handlers wrote.
//!
//! The rule under test: such a write is placed against data the key already sealed — the
//! execution's own input, and the record the write amends — so it OPENS the data key that
//! execution's payloads already go under, and is refused HTTP 400
//! `KmsAccessDeniedException` whenever the authorities in force do not allow the identity
//! it is made as `kms:Decrypt` on the key, HOWEVER LONG AGO the execution started. The
//! mint of the next data key is what the expiry of `kmsDataKeyReusePeriodSeconds` ADDS to
//! that — "maximum duration that Step Functions will reuse data keys. When the period
//! expires, Step Functions will call `GenerateDataKey`" — rather than what it replaces, so
//! a grant of the mint alone never serves one of these writes.
//!
//! The rows span what the rule generalises over: the writers it covers
//! (`SendTaskSuccess`, `SendTaskFailure`, `StopExecution`), the identity each one's use of
//! the key is decided as (a task reply is work Step Functions does as the state machine's
//! EXECUTION ROLE; a stop is the caller's own), the document the decrypt is withheld in
//! (the role's identity-based policy, and the key's own resource-based policy), and — over
//! the clock — the reuse period in force beside how long after the start the write
//! arrives, at each published bound of that period and past it.
//!
//! The harness (`Chain`, the key policies, `customer_managed`) is
//! `execution_data_encryption`'s, the role is `role_log_and_key_authorization`'s and the
//! narrowed key policy is `encrypted_execution_writes`'s; `tests/all/main.rs` is this
//! binary's crate root, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::encryption::write_of_sealed_data;

use crate::activity_tasks::activity_definition;
use crate::encrypted_execution_writes::policy_delegating;
use crate::execution_data_encryption::{
    customer_managed, granting_policy, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::role_log_and_key_authorization::{data_key_policy, role};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT};

/// The two KMS operations a write into an encrypted execution can make on its key, named
/// as a policy statement's `Action` names them (the KMS API's own `Decrypt` and
/// `GenerateDataKey`).
const DECRYPT: &str = "Decrypt";
const GENERATE_DATA_KEY: &str = "GenerateDataKey";

/// The published bounds of `kmsDataKeyReusePeriodSeconds` (range 60..900, from
/// `smithy_query(query="shape",shape="EncryptionConfiguration")`), which the clock table
/// below is driven at.
const SHORTEST_REUSE_PERIOD_SECONDS: i32 = 60;
const LONGEST_REUSE_PERIOD_SECONDS: i32 = 900;

/// The bound the scheduling state declares on its task: longer than any test here takes,
/// so no bound the clock owes closes a task while a policy is being changed under it.
const TASK_BOUND_SECONDS: u32 = 300;

/// The worker name a poll below claims a task under, which the service only logs.
const WORKER: &str = "data-key-use-worker";

/// The inline policy name every role document here is written under.
const POLICY_NAME: &str = "inline";

/// The error and the cause a reply reports, inside the modelled lengths (0..256, 0..32768).
const REPORTED_ERROR: &str = "WorkerUnavailable";
const REPORTED_CAUSE: &str = "the worker could not reach the dependency it needs";

/// The output a successful reply relays, which becomes the execution's own.
const REPORTED_OUTPUT: &str = r#"{"ok":true}"#;

/// The execution's input, carrying text outside ASCII so the round trip is read back on a
/// payload whose bytes are not its characters.
const EXECUTION_INPUT: &str = r#"{"marker":"start","note":"ünïcode ✓"}"#;

/// How long a poll below waits for a terminal status before giving up: generous, so the
/// outcome is waited for on any host rather than being timed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// An identity policy allowing the MINT of a data key and nothing else of KMS: the grant
/// the account is left holding when `kms:Decrypt` is revoked from an execution role.
fn minting_only_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["kms:GenerateDataKey"],
            "Resource": "*",
        }]
    })
}

/// One row of the clock table: the reuse period in force, how long after the execution
/// started the write arrives, and whether that write ALSO mints the next data key.
struct Timing {
    case: &'static str,
    period_seconds: i32,
    elapsed: Duration,
    mints: bool,
}

fn timings() -> Vec<Timing> {
    vec![
        Timing {
            case: "a write at the instant the execution started",
            period_seconds: SHORTEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::ZERO,
            mints: false,
        },
        Timing {
            case: "a write one second inside the shortest period",
            period_seconds: SHORTEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(59),
            mints: false,
        },
        Timing {
            case: "a write at the shortest period",
            period_seconds: SHORTEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(60),
            mints: true,
        },
        Timing {
            case: "a write well past the shortest period",
            period_seconds: SHORTEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(150),
            mints: true,
        },
        // A period the published bounds only contain, so the rule is read off neither
        // bound.
        Timing {
            case: "a write one second inside a mid-range period",
            period_seconds: 300,
            elapsed: Duration::from_secs(299),
            mints: false,
        },
        Timing {
            case: "a write at a mid-range period",
            period_seconds: 300,
            elapsed: Duration::from_secs(300),
            mints: true,
        },
        Timing {
            case: "a write one second inside the longest period",
            period_seconds: LONGEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(899),
            mints: false,
        },
        Timing {
            case: "a write at the longest period",
            period_seconds: LONGEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(900),
            mints: true,
        },
        Timing {
            case: "a write one second past the longest period",
            period_seconds: LONGEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(901),
            mints: true,
        },
        Timing {
            case: "a write a day past the longest period",
            period_seconds: LONGEST_REUSE_PERIOD_SECONDS,
            elapsed: Duration::from_secs(86_400),
            mints: true,
        },
    ]
}

/// RULE, over the clock: every write into an execution opens the data key its payloads
/// already go under, and the expiry of the reuse period only ADDS the mint of the next one
/// to that use.
///
/// The decision is a pure function of the instants and the period, so it is driven at a
/// fixed instant rather than at the host's clock: nothing here waits, and nothing is timed.
#[test]
fn a_write_into_an_execution_always_opens_its_data_key() {
    // A fixed instant, so the table says the same thing on every host.
    let started = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    for timing in timings() {
        let Timing {
            case,
            period_seconds,
            elapsed,
            mints,
        } = timing;
        let operations = write_of_sealed_data(started, period_seconds, started + elapsed);
        assert_eq!(
            operations.first().copied(),
            Some(DECRYPT),
            "case {case:?}: expected the write to open the data key first, got {operations:?}"
        );
        assert_eq!(
            operations.contains(&GENERATE_DATA_KEY),
            mints,
            "case {case:?}: expected the mint of the next data key to be asked for \
             {mints:?}, got {operations:?}"
        );
    }
}

/// How a row takes the decrypt away from the identity its write is decided as.
#[derive(Clone, Copy)]
enum Withhold {
    /// Rewrite the EXECUTION ROLE's inline policy to allow only the mint, which is how an
    /// account revokes `kms:Decrypt` from the role a state machine acts as.
    RoleIdentityPolicy,
    /// Narrow the KEY's own policy to delegate only the mint to the account.
    KeyPolicy,
}

/// One row of the writer table: the operation, the body it carries, how the decrypt is
/// withheld from the identity it is decided as, and what the execution ends as once the
/// write is served.
struct Writer {
    case: &'static str,
    operation: &'static str,
    /// The request body, built from the `taskToken` a worker holds and the execution ARN.
    request: fn(&str, &str) -> Value,
    withhold: Withhold,
    /// The status the execution reaches once the write is served.
    status: &'static str,
    /// The output the execution reports, for the writer that relays one.
    output: Option<&'static str>,
}

fn writers() -> Vec<Writer> {
    vec![
        Writer {
            case: "a task reply relaying the worker's output",
            operation: "SendTaskSuccess",
            request: |token, _execution| json!({ "taskToken": token, "output": REPORTED_OUTPUT }),
            // A reply records its data INTO the execution, which Step Functions does as
            // the state machine's execution role.
            withhold: Withhold::RoleIdentityPolicy,
            status: "SUCCEEDED",
            output: Some(REPORTED_OUTPUT),
        },
        Writer {
            case: "a task reply reporting the worker's failure",
            operation: "SendTaskFailure",
            request: |token, _execution| json!({
                "taskToken": token,
                "error": REPORTED_ERROR,
                "cause": REPORTED_CAUSE,
            }),
            withhold: Withhold::RoleIdentityPolicy,
            status: "FAILED",
            output: None,
        },
        Writer {
            case: "a stop carrying the caller's error and cause",
            operation: "StopExecution",
            request: |_token, execution| json!({
                "executionArn": execution,
                "error": REPORTED_ERROR,
                "cause": REPORTED_CAUSE,
            }),
            // A stop seals data the CALLER supplied, so the key's own policy is what
            // withholds the use from it.
            withhold: Withhold::KeyPolicy,
            status: "ABORTED",
            output: None,
        },
    ]
}

/// RULE, end to end: a write into an encrypted execution is refused HTTP 400
/// `KmsAccessDeniedException` while the decrypt of its data key is withheld from the
/// identity it is made as, naming that use and that key, and the refusal writes nothing —
/// the very same request is served once the grant is put back, and the execution then
/// reports what it recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_is_refused_while_the_decrypt_of_its_data_key_is_withheld() {
    for (index, writer) in writers().into_iter().enumerate() {
        let Writer {
            case,
            operation,
            request,
            withhold,
            status: expected,
            output,
        } = writer;

        let chain = Chain::new();
        let role_name = format!("DataKeyUseRole{index}");
        // The role is allowed both cryptographic operations while the execution is
        // started and its task claimed: what is under test is the WRITE, not the setup.
        let role_arn = role(&chain, &role_name, &data_key_policy()).await;
        let key = chain.create_key(&granting_policy()).await;
        let (execution, token) =
            outstanding_task(&chain, &format!("data-key-use-{index}"), &key, &role_arn).await;
        let body = request(&token, &execution);

        withdraw(&chain, withhold, &key, &role_name).await;
        let (status, answer) = chain.call(&SFN, operation, &body).await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected the write to be refused while the decrypt is \
             withheld, got {status} with {answer:?}"
        );
        // Every variable part of the relayed sentence comes from the request and the
        // state: the use of the key the write makes, and the key it is made on.
        let refusal = message(&answer);
        let sentence = format!("is not authorized to perform: kms:Decrypt on resource: {key} ");
        assert!(
            refusal.contains(&sentence),
            "case {case:?}: expected the refusal to carry {sentence:?}, got {refusal:?}"
        );
        if matches!(withhold, Withhold::RoleIdentityPolicy) {
            // A reply is refused for the SESSION this service holds the execution role
            // under, and the missing permission is the role's own.
            let prefix = format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/");
            assert!(
                refusal.starts_with(&prefix),
                "case {case:?}: expected the refusal to blame a session of the execution \
                 role ({prefix:?}), got {refusal:?}"
            );
            assert!(
                refusal.contains("because no identity-based policy allows the kms:Decrypt action"),
                "case {case:?}: expected the refusal to state which document is \
                 withholding the use, got {refusal:?}"
            );
        }

        // The refusal sealed nothing: the token is still outstanding and the execution
        // still running, so the VERY SAME request is served once the grant is back.
        restore(&chain, withhold, &key, &role_name).await;
        let (status, answer) = chain.call(&SFN, operation, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the restored grant serves the same write, so the refusal \
             consumed nothing, got {status} with {answer:?}"
        );

        let described = poll_until_terminal(&chain, case, &execution).await;
        assert_eq!(
            string(&described, "status"),
            expected,
            "case {case:?}: expected the execution to end {expected}, got {described:?}"
        );
        // The execution's own input is reported back exactly as it was started on, and
        // the output a reply relayed is the execution's own.
        assert_eq!(
            string(&described, "input"),
            EXECUTION_INPUT,
            "case {case:?}: expected the input to be reported back byte for byte, got \
             {described:?}"
        );
        if let Some(output) = output {
            assert_eq!(
                serde_json::from_str::<Value>(&string(&described, "output")).ok(),
                serde_json::from_str::<Value>(output).ok(),
                "case {case:?}: expected the relayed output to be the execution's own, got \
                 {described:?}"
            );
        }
    }
}

/// One RUNNING execution of a state machine encrypted under `key` and acting as
/// `role_arn`, with the `taskToken` a worker now holds: the records every write above
/// reports into.
async fn outstanding_task(chain: &Chain, name: &str, key: &str, role_arn: &str) -> (String, String) {
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

/// Take the decrypt away from the identity the row's write is decided as.
async fn withdraw(chain: &Chain, withhold: Withhold, key: &str, role_name: &str) {
    match withhold {
        Withhold::RoleIdentityPolicy => put_role_policy(chain, role_name, &minting_only_policy()).await,
        Withhold::KeyPolicy => {
            chain
                .put_policy(key, &policy_delegating(&["kms:GenerateDataKey"]))
                .await;
        }
    }
}

/// Put that grant back, exactly as it was before the row withdrew it.
async fn restore(chain: &Chain, withhold: Withhold, key: &str, role_name: &str) {
    match withhold {
        Withhold::RoleIdentityPolicy => put_role_policy(chain, role_name, &data_key_policy()).await,
        Withhold::KeyPolicy => chain.put_policy(key, &granting_policy()).await,
    }
}

/// Rewrite the role's one inline policy, which is how an account grants and revokes a
/// permission of an execution role.
async fn put_role_policy(chain: &Chain, role_name: &str, policy: &Value) {
    chain
        .iam(
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
}

/// The message a refusal carries, for an assertion that names it.
fn message(answer: &Value) -> String {
    answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Describe `execution` until it reports a status other than `RUNNING`, and answer that
/// record. The wait is for the STATUS and never for a span: the outcome the write made due
/// takes as long as the host takes, up to [`TERMINAL_DEADLINE`].
async fn poll_until_terminal(chain: &Chain, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = chain
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to leave RUNNING, got {described:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
