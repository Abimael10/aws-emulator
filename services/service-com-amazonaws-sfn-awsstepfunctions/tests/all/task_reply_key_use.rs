// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHOSE use of the key a reply to a task is decided as, and WHICH
//! use of it that reply makes.
//!
//! The rule under test: a reply to a task records its data INTO the execution, which Step
//! Functions does acting as the state machine's EXECUTION ROLE — so the use of the key is
//! the ROLE's, and a refusal names the assumed-role SESSION this service holds that role
//! under rather than the identity that signed the request. The use itself follows the
//! clock: the data key an execution's payloads go under is minted when the execution
//! starts and `kmsDataKeyReusePeriodSeconds` is the "maximum duration that Step Functions
//! will reuse data keys. When the period expires, Step Functions will call
//! `GenerateDataKey`" — so a reply inside that period OPENS the data key already minted
//! (`kms:Decrypt`) and asks nothing else of the key.
//!
//! The rows span the two replies that seal data (`SendTaskSuccess`, `SendTaskFailure`) and
//! both ways the key can withhold that use from the role: an explicit deny of it in the
//! key's own policy, and a policy that grants the role nothing at all.
//!
//! Step Functions, KMS and IAM run over ONE engine
//! (`crate::execution_data_encryption::Chain`), so the role a refusal blames is the role
//! IAM's own handlers hold and the key policy is the document KMS's own handlers wrote.

use serde_json::{json, Value};

use crate::activity_tasks::activity_definition;
use crate::execution_data_encryption::{
    customer_managed, granting_policy, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT};

/// The bound the scheduling state declares on its task: longer than any test here takes,
/// so no bound the clock owes closes a task while a key policy is being changed under it.
const TASK_BOUND_SECONDS: u32 = 300;

/// The worker name a poll below claims a task under, which the service only logs.
const WORKER: &str = "reply-key-worker";

/// The IAM policy name each document below is written under.
const POLICY_NAME: &str = "inline";

/// The error and the cause a reply reports, inside the modelled lengths (0..256, 0..32768).
const REPORTED_ERROR: &str = "WorkerUnavailable";
const REPORTED_CAUSE: &str = "the worker could not reach the dependency it needs";

/// An IAM role a state machine ACTS AS: trusted by this service's own principal, so the
/// service can assume it, and allowed every crypto operation by its own documents, so what
/// withholds a use of the key is the KEY's policy and nothing else.
async fn execution_role(chain: &Chain, role_name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    });
    chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": ["kms:*", "states:*"], "Resource": "*" }]
    });
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
    format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")
}

/// A key policy that delegates everything to the account and EXPLICITLY DENIES `actions`
/// to the one principal `denied` names.
fn denying_policy(denied: &str, actions: &[&str]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "EnableIAMUserPermissions",
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                "Action": "kms:*",
                "Resource": "*",
            },
            {
                "Sid": "DenyTheRole",
                "Effect": "Deny",
                "Principal": { "AWS": denied },
                "Action": actions,
                "Resource": "*",
            },
        ]
    })
}

/// A key policy that grants the account nothing at all: it carries a statement about
/// another principal only, so nothing in it allows the role and nothing denies it either.
fn silent_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "OneOtherRoleOnly",
            "Effect": "Allow",
            "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:role/another-reader") },
            "Action": "kms:*",
            "Resource": "*",
        }]
    })
}

/// One RUNNING execution of a state machine encrypted under `key` and acting as
/// `role_arn`, with the `taskToken` a worker now holds.
async fn outstanding_task(chain: &Chain, name: &str, key: &str, role_arn: &str) -> String {
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
    chain
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "input": "{}" }),
        )
        .await;
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
    token
}

/// RULE: the key policy in force at the instant of the reply decides it — even one that
/// was already withholding the decrypt while the task was being handed out, where the
/// reply is the first call that needs the data key opened.
#[tokio::test]
async fn a_reply_follows_a_key_policy_that_was_already_withholding_the_decrypt() {
    let chain = Chain::new();
    let role_arn = execution_role(&chain, "ReplyKeyDeniedRole").await;
    let key = chain
        .create_key(&denying_policy(&role_arn, &["kms:Decrypt"]))
        .await;
    let token = outstanding_task(&chain, "reply-key-denied", &key, &role_arn).await;
    let (status, answer) = chain
        .call(
            &SFN,
            "SendTaskFailure",
            &json!({ "taskToken": token, "error": REPORTED_ERROR, "cause": REPORTED_CAUSE }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
        "case \"a key that withheld the decrypt all along\": expected the reply to be refused, \
         got {status} with {answer:?}"
    );
    let stated = refusal_message(&answer);
    let sentence = format!(" is not authorized to perform: kms:Decrypt on resource: {key} ");
    assert!(
        stated.contains(&sentence),
        "case \"a key that withheld the decrypt all along\": expected the refusal to carry \
         {sentence:?}, got {stated:?}"
    );
}

/// The message a refusal carries.
fn refusal_message(answer: &Value) -> String {
    answer
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// One row of the table below: the case, the key policy in force while the reply is made,
/// the clause the refusal ends with, the reply operation and the body it carries.
type Row = (
    &'static str,
    Value,
    &'static str,
    &'static str,
    fn(&str) -> Value,
);

/// RULE: a reply that seals data is decided as the EXECUTION ROLE's use of the key, for
/// the operation the reply really makes — the DECRYPT of the data key already minted for
/// the execution — and the refusal names the assumed-role session of that role.
#[tokio::test]
async fn a_reply_is_decided_as_the_execution_roles_use_of_the_data_key() {
    let chain = Chain::new();
    let role_arn = execution_role(&chain, "ReplyKeyRole").await;
    let key = chain.create_key(&granting_policy()).await;

    let rows: Vec<Row> = vec![
        (
            "a failure reported while the key denies the role the decrypt",
            denying_policy(&role_arn, &["kms:Decrypt"]),
            "with an explicit deny in a resource-based policy",
            "SendTaskFailure",
            |token| json!({ "taskToken": token, "error": REPORTED_ERROR, "cause": REPORTED_CAUSE }),
        ),
        (
            "an output reported while the key denies the role the decrypt",
            denying_policy(&role_arn, &["kms:Decrypt"]),
            "with an explicit deny in a resource-based policy",
            "SendTaskSuccess",
            |token| json!({ "taskToken": token, "output": r#"{"done":true}"# }),
        ),
        (
            "a failure reported while the key grants the role nothing",
            silent_policy(),
            "because no resource-based policy allows the kms:Decrypt action",
            "SendTaskFailure",
            |token| json!({ "taskToken": token, "error": REPORTED_ERROR }),
        ),
    ];

    for (index, (case, policy, because, operation, body)) in rows.into_iter().enumerate() {
        // The execution is started and its task claimed while the key grants, so what is
        // under test is the REPLY and not the setup.
        chain.put_policy(&key, &granting_policy()).await;
        let token = outstanding_task(&chain, &format!("reply-key-{index}"), &key, &role_arn).await;

        chain.put_policy(&key, &policy).await;
        let (status, answer) = chain.call(&SFN, operation, &body(&token)).await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected the reply to be refused, got {status} with {answer:?}"
        );        let stated = refusal_message(&answer);
        // The principal is an assumed-role SESSION of the execution role — the identity
        // this service really acts as — and the session's own name is minted per assume,
        // so the parts around it are what the rule fixes.
        let prefix = format!("User: arn:aws:sts::{ACCOUNT}:assumed-role/ReplyKeyRole/");
        assert!(
            stated.starts_with(&prefix),
            "case {case:?}: expected the refusal to blame a session of the execution role \
             ({prefix:?}), got {stated:?}"
        );
        assert!(
            !stated.contains(&role_arn),
            "case {case:?}: expected the refusal never to name the role's own ARN \
             {role_arn:?}, got {stated:?}"
        );
        let sentence = format!(
            " is not authorized to perform: kms:Decrypt on resource: {key} {because}"
        );
        assert!(
            stated.contains(&sentence),
            "case {case:?}: expected the refusal to carry {sentence:?}, got {stated:?}"
        );
        // The refusal sealed nothing: the task is still outstanding, so the very same
        // reply is served once the key policy grants again.
        chain.put_policy(&key, &granting_policy()).await;
        let (status, answer) = chain.call(&SFN, operation, &body(&token)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the restored policy serves the same reply, so the refusal \
             consumed nothing, got {status} with {answer:?}"
        );
    }
}
