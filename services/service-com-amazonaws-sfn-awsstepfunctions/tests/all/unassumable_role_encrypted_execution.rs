// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what happens to an execution whose state machine is encrypted
//! with a CUSTOMER MANAGED key when Step Functions cannot assume that state machine's
//! execution role, driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — the
//! shape `core/router/src/lib.rs` mounts them in — so the trust document the assume is
//! decided against is the one IAM's own handlers hold.
//!
//! The rule under test: taking the execution role on is the first call the acquisition of
//! an execution's DATA KEY makes, and that acquisition is what a start of a state machine
//! on a customer managed key begins with — where it has to make one at all. A start that
//! finds the data key of its state machine already in force seals this execution's data
//! with it and asks the role for nothing, so it is SERVED; the run's own work still has to
//! be done as that role, and a role whose stored trust policy no longer admits this service
//! leaves it no identity to be performed as:
//!
//! 1. `StartExecution` is SERVED (HTTP 200) and the execution exists with its input,
//!    because the data key an earlier start of that state machine acquired is still in
//!    force (`tests/all/encrypted_start_data_key_acquisition.rs` holds the other half:
//!    a start that HAS to acquire one and cannot is refused instead, and starts nothing);
//! 2. that execution ends `FAILED` with the Amazon States Language's own
//!    `States.Runtime` and the exception the assume raised as its cause — the exception's
//!    class followed by the service's fixed sentence for a role it cannot act as, naming
//!    the `roleArn` the state machine carries — in the record and in the
//!    `ExecutionFailed` history event alike, and not one state of the definition runs;
//! 3. nothing is latched: with the trust put back, `RedriveExecution` re-runs THAT SAME
//!    execution — same ARN, `redriveCount` incremented — to the outcome its own work
//!    computes, and a redrive taken while the trust is still withheld fails again the
//!    same way.
//!
//! The rows differ along what the rule generalises over: HOW the stored trust policy
//! withholds the assume (it names another service principal, it denies this one
//! explicitly, it is narrowed by a condition this assume cannot satisfy, and it carries
//! no statement at all) and WHAT the definition holds (a single `Pass`, a `Wait` before
//! one, and a `Task` that would start a child execution — whose child proves no state
//! ran). The boundary is a row of its own: a trust policy that admits this service runs
//! the execution to `SUCCEEDED`.
//!
//! The harness (`Chain`, the key policies) is `tests/all/execution_data_encryption.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so it is reached through
//! `crate::execution_data_encryption`. Whether an ASSUMABLE role may then use the key is
//! the next question, and `tests/all/execution_role_trust_conditions.rs` holds it.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::execution_data_encryption::{
    customer_managed, granting_policy, xml_element, Chain, SFN,
};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};

/// This service's own global service principal, the identity an execution role's trust
/// policy is written for.
const SERVICE_PRINCIPAL: &str = "states.amazonaws.com";

/// The exception class a cause reports when the service could not take the execution role
/// on, which is what the failure of such a run carries before its message.
const ASSUME_ROLE_FAILED_EXCEPTION: &str =
    "com.amazonaws.sfn.invoker.sts.AssumeRoleFailedCacheableException";

/// The error the language reserves for a failure of the EXECUTION itself, as opposed to
/// one a state's own work reported.
const RUNTIME_ERROR: &str = "States.Runtime";

/// How long a row waits for a condition it expects to hold, looking again until it does.
///
/// Generous on purpose: what is under test is WHICH outcome the execution reaches, never
/// how fast this host reaches it, so the deadline is only a backstop against an outcome
/// that never arrives.
const PATIENCE: Duration = Duration::from_secs(60);

/// How long a row waits between two looks at an execution.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// The cause a failure of the RUN carries for a role this service cannot take on: the
/// exception the assume raised, qualified by its class, and then its message — the
/// service's own service principal (the identity a trust policy is written for) and the
/// role reference an operator has to act on, in a parenthesised suffix.
///
/// `pub(crate)` because `tests/all/execution_role_trust_conditions.rs` drives the same
/// rule over the trust-policy CONDITIONS that withhold the assume: `tests/all/main.rs` is
/// one crate root, so the wording and the wait below are stated here once.
pub(crate) fn unassumable_cause(role_arn: &str) -> String {
    format!(
        "{ASSUME_ROLE_FAILED_EXCEPTION}: The principal {SERVICE_PRINCIPAL} is not authorized \
         to assume the provided role. (role: {role_arn})"
    )
}

/// A trust policy with one statement: `effect` for `principal`, narrowed by `condition`
/// where the row states one.
fn trust(effect: &str, principal: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": effect,
        "Principal": { "Service": principal },
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The trust policy that admits this service: the one every row puts back before it
/// redrives.
fn good_trust() -> Value {
    trust("Allow", SERVICE_PRINCIPAL, None)
}

/// What the role may do once it IS assumed: the KMS calls sealing an execution's data
/// makes, so a row that gets past the assume is not then refused on the key, and the Step
/// Functions calls a `Task` state of the definitions below would make.
fn role_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["kms:GenerateDataKey", "kms:Decrypt", "kms:DescribeKey", "states:*"],
            "Resource": "*",
        }]
    })
}

/// A role of this account carrying `trust` and [`role_policy`], by its own ARN.
async fn role_with(chain: &Chain, name: &str, trust: &Value) -> String {
    let created = chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
    let arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
    chain
        .iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", role_policy().to_string()),
            ],
        )
        .await;
    arn
}

/// Replace the trust policy IAM holds for the role named `name`, which is how a caller
/// takes this service's assume away and gives it back.
async fn set_trust(chain: &Chain, name: &str, trust: &Value) {
    chain
        .iam(
            "UpdateAssumeRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyDocument", trust.to_string()),
            ],
        )
        .await;
}

/// A state machine named `name` on `key`, running as `role_arn`, by its ARN.
async fn encrypted_machine(
    chain: &Chain,
    name: &str,
    definition: &str,
    role_arn: &str,
    key: &str,
) -> String {
    let created = chain
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": definition,
                "roleArn": role_arn,
                "encryptionConfiguration": customer_managed(key),
            }),
        )
        .await;
    string(&created, "stateMachineArn")
}

/// Describe `execution` again until it reports a status other than `RUNNING`, and answer
/// what it then says.
///
/// The wait is for the CONDITION — the outcome the clock owes this execution — so the row
/// looks again until it is there and [`PATIENCE`] only ends a wait that never resolves.
///
/// `pub(crate)` for the reason [`unassumable_cause`] is.
pub(crate) async fn ended(chain: &Chain, case: &str, execution: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = chain
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described:?}"
        );
        ::tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Every history event of `execution`, oldest first.
async fn history(chain: &Chain, execution: &str) -> Vec<Value> {
    let answered = chain
        .sfn(
            "GetExecutionHistory",
            &json!({ "executionArn": execution, "maxResults": 100 }),
        )
        .await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// How many executions `ListExecutions` reports for `machine`.
async fn started_count(chain: &Chain, machine: &str) -> usize {
    let listed = chain
        .sfn(
            "ListExecutions",
            &json!({ "stateMachineArn": machine, "maxResults": 10 }),
        )
        .await;
    listed
        .get("executions")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}

/// A definition that finishes at once, which a `Task` row's child runs.
const CHILD: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// One row: what the definition holds, and how the trust policy withholds the assume.
struct Row {
    case: &'static str,
    /// The definition the state machine runs, built from the child's ARN for the row
    /// whose `Task` would start it.
    definition: fn(&str) -> String,
    /// The trust document IAM holds while the execution is started, built from the ARN of
    /// the state machine the row runs.
    withheld: fn(&str) -> Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one Pass, under a trust policy naming another service principal",
            definition: |_child| {
                r#"{"StartAt":"Only","States":{"Only":{"Type":"Pass","End":true}}}"#.to_string()
            },
            withheld: |_machine| trust("Allow", "lambda.amazonaws.com", None),
        },
        Row {
            case: "a Wait before a Pass, under a trust policy denying this service",
            definition: |_child| {
                json!({
                    "StartAt": "Hold",
                    "States": {
                        "Hold": { "Type": "Wait", "Seconds": 1, "Next": "Then" },
                        "Then": { "Type": "Pass", "End": true },
                    }
                })
                .to_string()
            },
            withheld: |_machine| trust("Deny", SERVICE_PRINCIPAL, None),
        },
        Row {
            // A statement's `Condition` is part of its grant, so a trust narrowed to
            // another state machine withholds the assume this execution would make.
            case: "a Task starting a child, under a trust narrowed to another state machine",
            definition: |child| {
                json!({
                    "StartAt": "CallChild",
                    "States": {
                        "CallChild": {
                            "Type": "Task",
                            "Resource": "arn:aws:states:::states:startExecution",
                            "Parameters": { "StateMachineArn": child },
                            "End": true,
                        }
                    }
                })
                .to_string()
            },
            withheld: |_machine| {
                trust(
                    "Allow",
                    SERVICE_PRINCIPAL,
                    Some(json!({
                        "ArnEquals": {
                            "aws:SourceArn":
                                format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:elsewhere"),
                        }
                    })),
                )
            },
        },
        Row {
            // A document that grants nobody anything withholds it too: that document IS
            // what IAM answered about the role.
            case: "one Pass, under a trust policy carrying no statement at all",
            definition: |_child| {
                r#"{"StartAt":"Only","States":{"Only":{"Type":"Pass","End":true}}}"#.to_string()
            },
            withheld: |_machine| json!({ "Version": "2012-10-17", "Statement": [] }),
        },
    ]
}

/// A start that re-uses the data key already in force for its state machine is SERVED
/// whatever the role's trust now says, and the execution it started ends `FAILED` with
/// `States.Runtime` and the fixed unassumable-role sentence, having run nothing — then a
/// redrive under the restored trust runs that same execution to `SUCCEEDED`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_encrypted_execution_whose_role_cannot_be_assumed_fails_and_is_redrivable() {
    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            definition,
            withheld,
        } = row;
        let chain = Chain::new();
        let key = chain.create_key(&granting_policy()).await;
        let role_name = format!("unassumable-role-{index}");
        let role_arn = role_with(&chain, &role_name, &good_trust()).await;
        let child = encrypted_machine(
            &chain,
            &format!("unassumable-child-{index}"),
            CHILD,
            &role_arn,
            &key,
        )
        .await;
        let machine = encrypted_machine(
            &chain,
            &format!("unassumable-machine-{index}"),
            &definition(&child),
            &role_arn,
            &key,
        )
        .await;

        // A first start, under the trust the service needs, ACQUIRES the data key this
        // state machine's execution data is sealed under: it is in force for the reuse
        // period the configuration states from here on, so the start below asks the role
        // for nothing.
        chain
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "baseline" }),
            )
            .await;
        let children_of_the_baseline = started_count(&chain, &child).await;

        // The trust this service's assume needs is taken away AFTER the state machine was
        // created, which is the only way a configuration the control plane admitted ends
        // up naming a role the data plane cannot act as.
        set_trust(&chain, &role_name, &withheld(&machine)).await;

        let (status, answered) = chain
            .call(
                &SFN,
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "run" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the start to be served, got {status} with {answered:?}"
        );
        let execution = string(&answered, "executionArn");
        assert_eq!(
            started_count(&chain, &machine).await,
            2,
            "case {case:?}: expected the start to have started the execution"
        );

        let described = ended(&chain, case, &execution).await;
        let expected = unassumable_cause(&role_arn);
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
                described.get("cause").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR), Some(expected.as_str())),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR} and {expected:?}, got \
             {described:?}"
        );
        // Not one state was entered: the attempt opened its history and ended on the
        // execution's own failure, which carries the same error and cause the record
        // reports.
        let events = history(&chain, &execution).await;
        let types: Vec<&str> = events
            .iter()
            .filter_map(|event| event.get("type").and_then(Value::as_str))
            .collect();
        assert_eq!(
            types,
            vec!["ExecutionStarted", "ExecutionFailed"],
            "case {case:?}: expected a run that entered no state, got {events:?}"
        );
        let failed = events
            .last()
            .and_then(|event| event.get("executionFailedEventDetails"))
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            (
                failed.get("error").and_then(Value::as_str),
                failed.get("cause").and_then(Value::as_str),
            ),
            (Some(RUNTIME_ERROR), Some(expected.as_str())),
            "case {case:?}: expected the history event to carry the failure, got {failed:?}"
        );
        // And the work a state of the definition would have done never happened: the
        // `Task` row's child was started only by the baseline run.
        assert_eq!(
            started_count(&chain, &child).await,
            children_of_the_baseline,
            "case {case:?}: expected no child execution for a run that never happened"
        );

        // A redrive taken while the trust is STILL withheld fails the same way: the
        // condition is read again, nothing of the first failure is latched.
        chain
            .sfn("RedriveExecution", &json!({ "executionArn": execution }))
            .await;
        let again = ended(&chain, case, &execution).await;
        assert_eq!(
            (
                again.get("status").and_then(Value::as_str),
                again.get("error").and_then(Value::as_str),
                again.get("cause").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR), Some(expected.as_str())),
            "case {case:?}: expected the redrive under the withheld trust to fail again, got \
             {again:?}"
        );

        // With the trust put back, the SAME execution is redriven to the outcome its own
        // work computes — its ARN unchanged and its redrive count carrying both attempts.
        set_trust(&chain, &role_name, &good_trust()).await;
        let redriven = chain
            .sfn("RedriveExecution", &json!({ "executionArn": execution }))
            .await;
        assert!(
            redriven.get("redriveDate").is_some(),
            "case {case:?}: expected the redrive to answer a redriveDate, got {redriven:?}"
        );
        let finished = ended(&chain, case, &execution).await;
        assert_eq!(
            (
                finished.get("executionArn").and_then(Value::as_str),
                finished.get("status").and_then(Value::as_str),
                finished.get("redriveCount").and_then(Value::as_i64),
            ),
            (Some(execution.as_str()), Some("SUCCEEDED"), Some(2)),
            "case {case:?}: expected the restored trust to run this execution to SUCCEEDED, got \
             {finished:?}"
        );
    }
}

/// The boundary of the rule: a role whose stored trust policy admits this service is one
/// the run is performed as, so the execution reaches the outcome its own definition
/// computes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_encrypted_execution_whose_role_is_trusted_runs() {
    let case = "a trust policy admitting this service";
    let chain = Chain::new();
    let key = chain.create_key(&granting_policy()).await;
    let role_arn = role_with(&chain, "assumable-role", &good_trust()).await;
    let machine = encrypted_machine(
        &chain,
        "assumable-machine",
        r#"{"StartAt":"Only","States":{"Only":{"Type":"Pass","End":true}}}"#,
        &role_arn,
        &key,
    )
    .await;

    let started = chain
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "run" }),
        )
        .await;
    let execution = string(&started, "executionArn");
    let described = ended(&chain, case, &execution).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the execution to run, got {described:?}"
    );
}
