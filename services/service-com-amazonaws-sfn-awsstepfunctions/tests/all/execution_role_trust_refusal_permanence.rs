// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the PERMANENCE of the refusal a mismatched execution-role trust
//! earns, driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the trust document the assume is decided
//! against is the one IAM's own handlers wrote and the key policy is the one KMS's own
//! handlers hold.
//!
//! The rule under test: Step Functions takes a state machine's execution role on through
//! `sts:AssumeRole` to do the execution's work, presenting the state machine the work is
//! for under the two confused-deputy keys a service trust policy is written against
//! (`aws:SourceArn`, that state machine's own ARN, and `aws:SourceAccount`, the account
//! that owns it). A statement's `Condition` is part of its grant, so a trust whose clauses
//! the assume cannot satisfy is a role this service may not act as for THAT state machine —
//! and acquiring the data key everything an execution of a `CUSTOMER_MANAGED_KMS_KEY` state
//! machine retains is sealed under is the first thing such a run does, before any state is
//! entered. So the start is refused HTTP 400 `KmsAccessDeniedException` carrying the
//! exception the assume raised and starts nothing.
//!
//! What this suite adds to `tests/all/execution_role_trust_conditions.rs`, which holds the
//! rule over ONE start each, is the dimension a client depends on: the decision rests on
//! the stored documents alone and on nothing that settles over time, so the refusal is
//! PERMANENT. Every retry of the same start answers the same status, the same error code
//! and the same sentence word for word, and after the last one the state machine still
//! holds no execution — neither in `ListExecutions` nor under the ARN the refused start
//! would have minted. That permanence is how a caller tells a role that can never be
//! assumed from a grant that has not propagated yet, and it is the opposite boundary from
//! a trust the assume DOES satisfy, which keeps serving every retry.
//!
//! The rows of the refused table differ along what the rule generalises over: which
//! confused-deputy key the clause is written over, which operator writes it
//! (`StringEquals`, `ArnEquals`, `ArnLike`), the FORM of the mismatch (another account,
//! another state machine's exact ARN, a name pattern this machine's name does not match,
//! another partition) and how many clauses the statement carries. The served table's rows
//! differ along the forms a matching clause takes, and its last row is the SYNCHRONOUS
//! start of an EXPRESS workflow under a trust scoped to that workflow's exact ARN, the
//! other operation this one assume serves.
//!
//! The harness (`Chain`, the key policies) is `tests/all/execution_data_encryption.rs`'s
//! and the documents are `tests/all/execution_role_trust_conditions.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so they are reached through
//! `crate::execution_data_encryption` and `crate::execution_role_trust_conditions`.

use serde_json::{json, Value};

use crate::execution_data_encryption::{
    customer_managed, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::execution_role_trust_conditions::{
    encrypted_machine, execution_arn, key_policy_with, machine_arn, role_at, started_count, trust,
    ANOTHER_ACCOUNT,
};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, HELLO_DEFINITION, REGION};
use crate::unassumable_role_encrypted_execution::{ended, unassumable_cause};

/// How many times a row starts the same state machine. More than one, because what is
/// under test is that the answer does not change between the first attempt and a later
/// one; a client that retries a transient `Kms*` code makes exactly this sequence.
const ATTEMPTS: usize = 3;

/// The name of the `index`th start a row makes. Each start names its own execution, so a
/// retry is a start this service has never served rather than the idempotent repeat of
/// one it has.
fn attempt_name(index: usize) -> String {
    format!("attempt-{index}")
}

/// The modelled error class a read of an execution this service does not hold answers,
/// which is how a start that created nothing shows through `DescribeExecution`, and the
/// status every modelled client error of this service carries.
const EXECUTION_DOES_NOT_EXIST: &str = "ExecutionDoesNotExist";
const CLIENT_REFUSAL_STATUS: u16 = 400;

/// One row of either table below: how the trust statement's `Condition` is narrowed, built
/// from the ARN of the state machine the row starts.
struct Row {
    case: &'static str,
    condition: fn(&str) -> Value,
}

/// The trusts whose clauses the assume CANNOT satisfy.
fn mismatches() -> Vec<Row> {
    vec![
        Row {
            case: "another account under StringEquals aws:SourceAccount",
            condition: |_machine| json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } }),
        },
        Row {
            // The guard pointed at a state machine of this very account and Region that
            // does not exist: the assume presents the machine being started, which no
            // other name matches.
            case: "a state machine name this account holds nothing under, under ArnLike \
                   aws:SourceArn",
            condition: |_machine| {
                json!({ "ArnLike": { "aws:SourceArn": machine_arn("no-such-machine") } })
            },
        },
        Row {
            case: "another state machine's exact ARN under ArnEquals aws:SourceArn",
            condition: |_machine| {
                json!({ "ArnEquals": { "aws:SourceArn": machine_arn("somewhere-else") } })
            },
        },
        Row {
            // A pattern rather than a name: the wildcard covers a family of state machines
            // this one does not belong to.
            case: "a name pattern this state machine does not match, under ArnLike \
                   aws:SourceArn",
            condition: |_machine| {
                json!({ "ArnLike": { "aws:SourceArn": machine_arn("somewhere-else-*") } })
            },
        },
        Row {
            case: "another partition's state machines under ArnLike aws:SourceArn",
            condition: |_machine| {
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws-us-gov:states:{REGION}:{ACCOUNT}:stateMachine:*"),
                    }
                })
            },
        },
        Row {
            // The boundary of the class: one clause the assume cannot satisfy withholds
            // the grant however exactly the clause beside it is satisfied.
            case: "this state machine's own ARN beside another account, both keys together",
            condition: |machine| {
                json!({
                    "ArnEquals": { "aws:SourceArn": machine },
                    "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT },
                })
            },
        },
    ]
}

/// A start of a state machine on a customer managed key whose execution role carries a
/// trust the assume cannot satisfy is refused the SAME way however often it is retried —
/// 400 `KmsAccessDeniedException` carrying the exception the assume raised, every word of
/// it fixed but the configured role — and the state machine holds no execution afterwards,
/// neither in its listing nor under the ARN any of those starts would have minted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mismatched_trust_refuses_every_retry_of_the_same_start_alike() {
    let chain = Chain::new();
    let key = chain.create_key(&key_policy_with(Vec::new())).await;

    for (index, row) in mismatches().into_iter().enumerate() {
        let Row { case, condition } = row;
        let name = format!("permanent-refusal-{index}");
        let expected_arn = machine_arn(&name);
        let role_arn = role_at(
            &chain,
            &format!("permanent-refusal-role-{index}"),
            "/",
            &trust(Some(condition(&expected_arn))),
        )
        .await;
        let machine = encrypted_machine(&chain, &name, &role_arn, &key).await;
        assert_eq!(
            machine, expected_arn,
            "case {case:?}: the state machine the condition names is the one created, got \
             {machine}"
        );

        // Every attempt is held to the SAME expectation, which is what makes the refusal
        // permanent rather than merely repeated: a later attempt that answered anything
        // else — another code, another sentence, or a served start — fails here.
        let expected_message = unassumable_cause(&role_arn);
        for attempt in 0..ATTEMPTS {
            let (status, answered) = chain
                .call(
                    &SFN,
                    "StartExecution",
                    &json!({ "stateMachineArn": machine, "name": attempt_name(attempt) }),
                )
                .await;
            let code = error_code(&answered);
            let message = answered
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                (status, code.as_str(), message.as_str()),
                (
                    KMS_ACCESS_DENIED_STATUS,
                    KMS_ACCESS_DENIED,
                    expected_message.as_str()
                ),
                "case {case:?} attempt {attempt}: expected the relayed assume failure, got \
                 {status} {code} with {answered:?}"
            );
            // Nothing accumulates between the attempts: the refusal is the same before
            // and after every one of them.
            assert_eq!(
                started_count(&chain, &machine).await,
                0,
                "case {case:?} attempt {attempt}: expected a refused start to have started \
                 nothing"
            );
        }

        // The execution each refused start would have minted resolves to nothing, so no
        // reader of this service reports one.
        for attempt in 0..ATTEMPTS {
            let would_be = execution_arn(&name, &attempt_name(attempt));
            let (status, answered) = chain
                .call(
                    &SFN,
                    "DescribeExecution",
                    &json!({ "executionArn": would_be }),
                )
                .await;
            let code = error_code(&answered);
            assert_eq!(
                (status, code.as_str()),
                (CLIENT_REFUSAL_STATUS, EXECUTION_DOES_NOT_EXIST),
                "case {case:?} attempt {attempt}: expected {would_be} to resolve to nothing, \
                 got {status} {code} with {answered:?}"
            );
        }
    }
}

/// The trusts whose clauses the assume DOES satisfy.
fn matches() -> Vec<Row> {
    vec![
        Row {
            case: "every state machine of this account and Region under ArnLike aws:SourceArn",
            condition: |_machine| {
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:*"),
                    }
                })
            },
        },
        Row {
            case: "this state machine's own exact ARN under ArnEquals aws:SourceArn",
            condition: |machine| json!({ "ArnEquals": { "aws:SourceArn": machine } }),
        },
        Row {
            case: "the owning account under StringEquals aws:SourceAccount",
            condition: |_machine| json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } }),
        },
        Row {
            case: "both keys together, each satisfied",
            condition: |machine| {
                json!({
                    "StringEquals": { "aws:SourceAccount": ACCOUNT },
                    "ArnLike": { "aws:SourceArn": machine },
                })
            },
        },
    ]
}

/// The other boundary: a trust whose clauses the assume satisfies is a role this service
/// may act as for that state machine, so every retry of the start is SERVED and every
/// execution it begins runs to `SUCCEEDED`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trust_the_assume_satisfies_keeps_serving_every_retry() {
    let chain = Chain::new();
    let key = chain.create_key(&key_policy_with(Vec::new())).await;

    for (index, row) in matches().into_iter().enumerate() {
        let Row { case, condition } = row;
        let name = format!("permanent-grant-{index}");
        let expected_arn = machine_arn(&name);
        let role_arn = role_at(
            &chain,
            &format!("permanent-grant-role-{index}"),
            "/",
            &trust(Some(condition(&expected_arn))),
        )
        .await;
        let machine = encrypted_machine(&chain, &name, &role_arn, &key).await;

        let mut started = Vec::with_capacity(ATTEMPTS);
        for attempt in 0..ATTEMPTS {
            let (status, answered) = chain
                .call(
                    &SFN,
                    "StartExecution",
                    &json!({ "stateMachineArn": machine, "name": attempt_name(attempt) }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?} attempt {attempt}: expected the start to be served, got {status} \
                 {answered:?}"
            );
            started.push(string(&answered, "executionArn"));
        }
        assert_eq!(
            started_count(&chain, &machine).await,
            ATTEMPTS,
            "case {case:?}: expected every start to have started an execution"
        );
        // Round trip: each execution those starts report is one this service holds, and
        // each runs to the outcome the definition computes.
        for (attempt, execution) in started.iter().enumerate() {
            let described = chain
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            assert_eq!(
                described.get("stateMachineArn").and_then(Value::as_str),
                Some(machine.as_str()),
                "case {case:?} attempt {attempt}: expected the execution of that machine, got \
                 {described:?}"
            );
            let finished = ended(&chain, case, execution).await;
            assert_eq!(
                finished.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?} attempt {attempt}: expected the execution to run, got {finished:?}"
            );
        }
    }
}

/// The workflow type the synchronous start serves: "Starts a Synchronous Express state
/// machine execution. StartSyncExecution is not available for STANDARD workflows"
/// (`smithy_query(query="operation",operation="StartSyncExecution")`).
const EXPRESS: &str = "EXPRESS";

/// The SYNCHRONOUS start of an EXPRESS workflow is the other operation this one assume
/// serves, so a trust scoped to that workflow's exact ARN runs it: the call answers the
/// run's own terminal outcome, and the state machine reports the key it was configured
/// with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_synchronous_start_runs_under_a_trust_scoped_to_its_own_state_machine() {
    let chain = Chain::new();
    let key = chain.create_key(&key_policy_with(Vec::new())).await;
    let name = "permanent-grant-express";
    let expected_arn = machine_arn(name);
    let role_arn = role_at(
        &chain,
        "permanent-grant-express-role",
        "/",
        &trust(Some(
            json!({ "ArnEquals": { "aws:SourceArn": &expected_arn } }),
        )),
    )
    .await;
    let created = chain
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": HELLO_DEFINITION,
                "roleArn": role_arn,
                "type": EXPRESS,
                "encryptionConfiguration": customer_managed(&key),
            }),
        )
        .await;
    let machine = string(&created, "stateMachineArn");
    assert_eq!(
        machine, expected_arn,
        "case the EXPRESS workflow: the state machine the condition names is the one created, \
         got {machine}"
    );

    let (status, answered) = chain
        .call(
            &SFN,
            "StartSyncExecution",
            &json!({ "stateMachineArn": machine, "name": "run" }),
        )
        .await;
    assert_eq!(
        (status, answered.get("status").and_then(Value::as_str)),
        (200, Some("SUCCEEDED")),
        "case the EXPRESS workflow: expected the synchronous run to succeed, got {status} \
         {answered:?}"
    );

    // Round trip: the state machine that ran reports the customer managed key it was
    // configured with.
    let described = chain
        .sfn("DescribeStateMachine", &json!({ "stateMachineArn": machine }))
        .await;
    assert_eq!(
        described
            .get("encryptionConfiguration")
            .and_then(|configuration| configuration.get("kmsKeyId"))
            .and_then(Value::as_str),
        Some(key.as_str()),
        "case the EXPRESS workflow: expected the configured key to be reported, got {described:?}"
    );
}
