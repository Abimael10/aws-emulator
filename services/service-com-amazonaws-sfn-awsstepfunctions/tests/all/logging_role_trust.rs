// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TRUST a state machine's logging role has to carry: whether the
//! control plane can take that role on to deliver this state machine's logs at all.
//!
//! The rule under test: Step Functions does not write a vended log as the caller — it sets
//! the delivery up ACTING AS the state machine's role — and taking a role on is one
//! PARTICULAR assume, the one made FOR the state machine being configured, which the
//! service presents under the two confused-deputy keys a service trust policy is written
//! against (`aws:SourceArn`, `aws:SourceAccount`). So a trust this account HOLDS that
//! withholds that assume is a configuration the service cannot apply: the call is refused
//! and nothing is created or changed. A role the account does NOT hold shows nothing about
//! the role — the deploy-time/run-time split this service exposes — so such a
//! configuration is served.
//!
//! The rows differ along what the trust's `aws:SourceArn` names (this state machine,
//! another one, another account's, nothing at all), along the principal it trusts (the
//! global service principal and the Region's own), and along the operation that installs
//! the configuration (`CreateStateMachine` and `UpdateStateMachine`, which can install
//! exactly the same one).
//!
//! Step Functions, IAM and KMS run over ONE engine
//! (`crate::execution_data_encryption::Chain`), so the trust a decision reads is the
//! document IAM's own handlers wrote.

use serde_json::{json, Value};

use crate::execution_data_encryption::{Chain, SFN};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, HELLO_DEFINITION, REGION};

/// The code and status a configuration this service cannot apply is refused with.
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: u16 = 400;

/// The IAM policy name each document below is written under.
const POLICY_NAME: &str = "inline";

/// The two spellings of this service's own principal a trust policy can be written for.
const GLOBAL_PRINCIPAL: &str = "states.amazonaws.com";

/// The log-delivery statement every role below carries, so what decides a row is the
/// TRUST and not the role's own permissions.
fn delivery_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogDelivery", "logs:PutLogEvents"],
            "Resource": "*",
        }]
    })
}

/// A trust policy admitting `principal`, narrowed by `aws:SourceArn` to `source` where one
/// is given — the confused-deputy scoping AWS guidance tells an operator to write.
fn trust(principal: &str, source: Option<&str>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": principal },
        "Action": "sts:AssumeRole",
    });
    if let (Some(source), Some(object)) = (source, statement.as_object_mut()) {
        object.insert(
            "Condition".to_string(),
            json!({ "StringEquals": { "aws:SourceArn": source } }),
        );
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// The ARN of the state machine named `name` in the Region every call below is signed for.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// An IAM role the account HOLDS, carrying `trust` and the log-delivery permissions.
async fn role(chain: &Chain, name: &str, trust: &Value) -> String {
    chain
        .iam(
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
    chain
        .iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", delivery_policy().to_string()),
            ],
        )
        .await;
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The logging configuration every row installs: every event delivered to one log group.
fn logging(group: &str) -> Value {
    json!({
        "level": "ALL",
        "includeExecutionData": true,
        "destinations": [{ "cloudWatchLogsLogGroup": { "logGroupArn": group } }],
    })
}

/// The ARN of the log group every row below delivers to: the qualified form a logging
/// destination carries. Nothing has to hold it — what each row decides is the role's trust.
fn log_group() -> String {
    format!("arn:aws:logs:{REGION}:{ACCOUNT}:log-group:logging-role-trust:*")
}

/// One row: what the role's trust names, and whether the configuration is applied.
struct Row {
    case: &'static str,
    /// The principal the trust admits.
    principal: &'static str,
    /// What `aws:SourceArn` narrows it to: the state machine of the row, another one,
    /// another account's, or no condition at all.
    source: Source,
    /// Whether the control plane can take the role on for this state machine.
    served: bool,
}

/// What a row's trust narrows the assume to.
enum Source {
    /// No condition at all, so the trust covers every assume this service makes.
    Unconditioned,
    /// The state machine being configured.
    ThisMachine,
    /// Another state machine of this account.
    AnotherMachine,
    /// A state machine of another account.
    AnotherAccount,
}

/// Every row the rule is taken over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a trust that names no source at all",
            principal: GLOBAL_PRINCIPAL,
            source: Source::Unconditioned,
            served: true,
        },
        Row {
            case: "a trust narrowed to the state machine being configured",
            principal: GLOBAL_PRINCIPAL,
            source: Source::ThisMachine,
            served: true,
        },
        Row {
            case: "a trust narrowed to the Region's own principal and this state machine",
            principal: "states.us-west-2.amazonaws.com",
            source: Source::ThisMachine,
            served: true,
        },
        Row {
            case: "a trust narrowed to ANOTHER state machine",
            principal: GLOBAL_PRINCIPAL,
            source: Source::AnotherMachine,
            served: false,
        },
        Row {
            case: "a trust narrowed to another ACCOUNT's state machine",
            principal: GLOBAL_PRINCIPAL,
            source: Source::AnotherAccount,
            served: false,
        },
        Row {
            case: "a trust that admits another service's principal only",
            principal: "events.amazonaws.com",
            source: Source::Unconditioned,
            served: false,
        },
    ]
}

/// The `aws:SourceArn` a row's trust names, for the state machine `name`.
fn source_of(source: &Source, name: &str) -> Option<String> {
    match source {
        Source::Unconditioned => None,
        Source::ThisMachine => Some(machine_arn(name)),
        Source::AnotherMachine => Some(machine_arn("another-machine")),
        Source::AnotherAccount => Some(format!(
            "arn:aws:states:{REGION}:111122223333:stateMachine:{name}"
        )),
    }
}

/// The sentence a configuration this service cannot apply is refused with: the two
/// spellings of this service's principal a trust policy can be written for, and no value of
/// the request.
fn not_assumable() -> String {
    format!(
        "Neither the global service principal {GLOBAL_PRINCIPAL}, nor the regional one is \
         authorized to assume the provided role."
    )
}

/// RULE: a logging configuration is applied only where the trust this account holds for the
/// role lets this service assume it FOR the state machine being configured — on the create
/// and on the update alike — and a refused call leaves nothing behind.
#[tokio::test]
async fn a_logging_configuration_needs_a_trust_that_covers_this_state_machine() {
    let chain = Chain::new();
    let group = log_group();

    for (index, row) in rows().into_iter().enumerate() {
        let name = format!("logging-trust-{index}");
        let role_arn = role(
            &chain,
            &format!("LoggingTrustRole{index}"),
            &trust(row.principal, source_of(&row.source, &name).as_deref()),
        )
        .await;
        let case = row.case;

        // The CREATE: the configuration is named at the moment the state machine is made.
        let (status, answer) = chain
            .call(
                &SFN,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                    "loggingConfiguration": logging(&group),
                    "type": "EXPRESS",
                }),
            )
            .await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the create to be served, got {status} with {answer:?}"
            );
            assert_eq!(
                string(&answer, "stateMachineArn"),
                machine_arn(&name),
                "case {case:?}: expected the create to answer the state machine's ARN, got \
                 {answer:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the create to be refused, got {status} with {answer:?}"
        );
        let stated = answer
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            stated,
            not_assumable(),
            "case {case:?}: expected {:?}, got {stated:?}",
            not_assumable()
        );
        // The refused create left nothing behind: the name is still free, which a read of
        // the ARN it would have minted reports.
        let (_, described) = chain
            .call(
                &SFN,
                "DescribeStateMachine",
                &json!({ "stateMachineArn": machine_arn(&name) }),
            )
            .await;
        assert_eq!(
            error_code(&described).as_str(),
            "StateMachineDoesNotExist",
            "case {case:?}: a refused create must create nothing, got {described:?}"
        );

        // The UPDATE installs the same configuration, and is decided the same way: the
        // state machine is created WITHOUT logging first, so what the update changes is the
        // configuration alone.
        let created = chain
            .call(
                &SFN,
                "CreateStateMachine",
                &json!({
                    "name": format!("{name}-updated"),
                    "definition": HELLO_DEFINITION,
                    "roleArn": role_arn,
                }),
            )
            .await;
        assert_eq!(
            created.0, 200,
            "case {case:?}: the setup create must succeed, got {:?}",
            created.1
        );
        let machine = string(&created.1, "stateMachineArn");
        let (status, answer) = chain
            .call(
                &SFN,
                "UpdateStateMachine",
                &json!({
                    "stateMachineArn": machine,
                    "loggingConfiguration": logging(&group),
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the update to be refused too, got {status} with {answer:?}"
        );
        // The refused update changed nothing: the state machine still carries no logging
        // configuration at all.
        let (_, described) = chain
            .call(
                &SFN,
                "DescribeStateMachine",
                &json!({ "stateMachineArn": machine }),
            )
            .await;
        let level = described
            .get("loggingConfiguration")
            .and_then(|configuration| configuration.get("level"))
            .and_then(Value::as_str)
            .unwrap_or("OFF");
        assert_eq!(
            level, "OFF",
            "case {case:?}: a refused update must leave the configuration as it was, got \
             {described:?}"
        );
    }
}
