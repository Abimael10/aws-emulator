// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TWIN pair a trust clause over the account splits: two state
//! machines of one account, on ONE customer managed key, whose execution-role trust admits
//! the assume of one and withholds the assume of the other. Driven over each service's own
//! wire protocol through the production `MultiServiceRouter` with Step Functions, IAM and
//! KMS mounted on ONE engine — the shape `core/router/src/lib.rs` mounts them in — so the
//! trust document the assume is decided against is the one IAM's own handlers wrote and the
//! key is the one KMS's own handlers hold.
//!
//! The rule under test: Step Functions takes a state machine's execution role on through
//! `sts:AssumeRole` to do the work of an execution, presenting THAT state machine under the
//! two confused-deputy keys a service trust policy is written against (`aws:SourceArn`, the
//! machine's own ARN, and `aws:SourceAccount`, the account that owns it), and a statement's
//! `Condition` is part of its grant. So the decision is taken per START, against the values
//! this one assume carries: every start of a `CUSTOMER_MANAGED_KMS_KEY` state machine
//! SETTLES on one of exactly two answers — refused HTTP 400 `KmsAccessDeniedException`
//! carrying the exception the assume raised and creating nothing, where the clause cannot
//! hold for the machine being started, or served, with the execution running to a terminal
//! status, where it can — and neither answer is the other machine's, the role's, the key's
//! or the service's.
//!
//! What this suite adds to the two suites that hold the condition rule over one machine at
//! a time (`tests/all/execution_role_trust_conditions.rs`, one start per clause, and
//! `tests/all/execution_role_trust_refusal_permanence.rs`, the same clause retried) is the
//! PAIR: a refused start and a served start of the same account, over the same key, in one
//! store. Its rows differ along what the rule generalises over: how the clause is written
//! over the account the assume presents (one account under `StringEquals`, a LIST of
//! accounts under it — a clause holds when ANY of its values does — and a pattern over the
//! account under `StringLike`), whether the pair runs as two roles or as ONE role scoped by
//! `aws:SourceArn` to the twin alone, and the ORDER the two starts are made in, because a
//! refusal that latched anything would show as the twin behind it failing too.
//!
//! The harness (`Chain`, the key policies) is `tests/all/execution_data_encryption.rs`'s
//! and the documents are `tests/all/execution_role_trust_conditions.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so they are reached through
//! `crate::execution_data_encryption` and `crate::execution_role_trust_conditions`.

use serde_json::{json, Value};

use crate::execution_data_encryption::{
    Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::execution_role_trust_conditions::{
    encrypted_machine, execution_arn, key_policy_with, machine_arn, role_at, started_count, trust,
    ANOTHER_ACCOUNT,
};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, REGION};
use crate::unassumable_role_encrypted_execution::{ended, unassumable_cause};

/// A third account, so a clause can name SEVERAL accounts none of which owns these
/// resources.
const A_FURTHER_ACCOUNT: &str = "345678901234";

/// The name every start below gives its execution, which is also the last segment of the
/// ARN a refused start must leave resolving to nothing.
const START_NAME: &str = "run";

/// The modelled error class a read of an execution this service does not hold answers,
/// which is how a start that created nothing shows through `DescribeExecution`, and the
/// status every modelled client error of this service carries.
const EXECUTION_DOES_NOT_EXIST: &str = "ExecutionDoesNotExist";
const CLIENT_REFUSAL_STATUS: u16 = 400;

/// A pattern over the first four digits of `account`, which is how a trust clause scopes
/// the assume to a FAMILY of accounts rather than to one.
fn account_pattern(account: &str) -> String {
    let head: String = account.chars().take(4).collect();
    format!("{head}*")
}

/// One row: how the pair's trust is written, and in which order the pair is started.
struct Row {
    case: &'static str,
    /// The `Condition` carried by the trust of the role the machine whose assume is
    /// WITHHELD runs as, built from the ARN of the twin whose assume is admitted.
    withheld: fn(&str) -> Value,
    /// The `Condition` carried by the trust of the role the ADMITTED twin runs as, or
    /// `None` where the pair shares ONE role — whose single document then has to admit the
    /// twin's assume and withhold the other's by itself.
    admitted: Option<fn(&str) -> Value>,
    /// Whether the refused start is made before the served one.
    refused_first: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: the confused-deputy guard pointed at an account other
            // than the one that owns the state machine being started. The clause itself is
            // `tests/all/execution_role_trust_conditions.rs`'s; what this row adds is the
            // PAIR — the refused start beside a served twin, refused start first.
            case: "another account under StringEquals aws:SourceAccount, refused first",
            withheld: |_twin| json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } }),
            admitted: Some(|_twin| json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } })),
            refused_first: true,
        },
        Row {
            // The same pair started the other way round: the served start comes first, so
            // a refusal that depended on anything the twin's start left behind would show
            // here.
            case: "another account under StringEquals aws:SourceAccount, served first",
            withheld: |_twin| json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } }),
            admitted: Some(|_twin| json!({ "StringEquals": { "aws:SourceAccount": ACCOUNT } })),
            refused_first: false,
        },
        Row {
            // A clause's values are a set: it holds when ANY of them does, so a list of
            // foreign accounts withholds the assume and the same list with the owning
            // account in it admits it.
            case: "a list of accounts under StringEquals aws:SourceAccount",
            withheld: |_twin| {
                json!({
                    "StringEquals": {
                        "aws:SourceAccount": [ANOTHER_ACCOUNT, A_FURTHER_ACCOUNT],
                    }
                })
            },
            admitted: Some(|_twin| {
                json!({
                    "StringEquals": {
                        "aws:SourceAccount": [ANOTHER_ACCOUNT, ACCOUNT],
                    }
                })
            }),
            refused_first: true,
        },
        Row {
            // The account under a PATTERN rather than a name: the family the wildcard
            // covers either contains the account the assume presents or it does not.
            case: "a pattern over the account under StringLike aws:SourceAccount",
            withheld: |_twin| {
                json!({ "StringLike": { "aws:SourceAccount": account_pattern(ANOTHER_ACCOUNT) } })
            },
            admitted: Some(|_twin| {
                json!({ "StringLike": { "aws:SourceAccount": account_pattern(ACCOUNT) } })
            }),
            refused_first: false,
        },
        Row {
            // ONE role for both machines, scoped to the twin's own ARN: the decision is
            // taken per assume, so the same role is one this service may act as for the
            // twin and may not act as for the machine beside it.
            case: "one role scoped to the twin's own ARN under ArnEquals aws:SourceArn",
            withheld: |twin| json!({ "ArnEquals": { "aws:SourceArn": twin } }),
            admitted: None,
            refused_first: true,
        },
        Row {
            // The account segment of the ARN the clause patterns over, rather than the
            // account key: the state machines of another account are a family this one
            // does not belong to.
            case: "another account's state machines under ArnLike aws:SourceArn",
            withheld: |_twin| {
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ANOTHER_ACCOUNT}:stateMachine:*"),
                    }
                })
            },
            admitted: Some(|_twin| {
                json!({
                    "ArnLike": {
                        "aws:SourceArn":
                            format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:*"),
                    }
                })
            }),
            refused_first: false,
        },
    ]
}

/// Start the state machine named `name` and hold the answer to the refusal a trust this
/// assume cannot satisfy earns: 400 `KmsAccessDeniedException` carrying the exception the
/// assume raised and naming the configured role, with nothing of the execution created —
/// neither in the machine's listing nor under the ARN the start would have minted.
async fn refuses(chain: &Chain, case: &str, name: &str, machine: &str, role_arn: &str) {
    let (status, answered) = chain
        .call(
            &SFN,
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": START_NAME }),
        )
        .await;
    let code = error_code(&answered);
    let expected = unassumable_cause(role_arn);
    assert_eq!(
        (
            status,
            code.as_str(),
            answered.get("message").and_then(Value::as_str),
        ),
        (
            KMS_ACCESS_DENIED_STATUS,
            KMS_ACCESS_DENIED,
            Some(expected.as_str()),
        ),
        "case {case:?}: expected the relayed assume failure for {machine}, got {status} {code} \
         with {answered:?}"
    );
    assert_eq!(
        started_count(chain, machine).await,
        0,
        "case {case:?}: expected a refused start of {machine} to have started nothing"
    );
    let would_be = execution_arn(name, START_NAME);
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
        "case {case:?}: expected {would_be} to resolve to nothing, got {status} {code} with \
         {answered:?}"
    );
}

/// Start `machine` and hold the answer to what a trust this assume satisfies earns: the
/// start is served, the execution it reports is one this service holds, and it settles on
/// the outcome the definition computes.
async fn serves(chain: &Chain, case: &str, machine: &str) {
    let (status, answered) = chain
        .call(
            &SFN,
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": START_NAME }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the start of {machine} to be served, got {status} {answered:?}"
    );
    let execution = string(&answered, "executionArn");
    // Round trip: the execution the start reports is one this service holds.
    let described = chain
        .sfn("DescribeExecution", &json!({ "executionArn": execution }))
        .await;
    assert_eq!(
        described.get("stateMachineArn").and_then(Value::as_str),
        Some(machine),
        "case {case:?}: expected the execution of that machine, got {described:?}"
    );
    let finished = ended(chain, case, &execution).await;
    assert_eq!(
        finished.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the execution to run to a terminal status, got {finished:?}"
    );
}

/// A trust clause over the account the assume presents splits a TWIN PAIR: the machine the
/// clause cannot hold for is refused its start and holds no execution, while its twin —
/// same account, same key, same definition, and in one row the same role — is served and
/// runs to `SUCCEEDED`. Whichever of the two is started first, both starts settle.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_trust_clause_over_the_account_refuses_one_twin_and_serves_the_other() {
    let chain = Chain::new();
    let key = chain.create_key(&key_policy_with(Vec::new())).await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            withheld,
            admitted,
            refused_first,
        } = row;
        let refused_name = format!("twin-refused-{index}");
        let served_name = format!("twin-served-{index}");
        let served_arn = machine_arn(&served_name);

        let refused_role = role_at(
            &chain,
            &format!("twin-refused-role-{index}"),
            "/",
            &trust(Some(withheld(&served_arn))),
        )
        .await;
        // One role for the pair where the row states no second document: that one trust
        // has to admit the twin's assume and withhold the other's by itself.
        let served_role = match admitted {
            Some(condition) => {
                role_at(
                    &chain,
                    &format!("twin-served-role-{index}"),
                    "/",
                    &trust(Some(condition(&served_arn))),
                )
                .await
            }
            None => refused_role.clone(),
        };

        let refused_machine = encrypted_machine(&chain, &refused_name, &refused_role, &key).await;
        let served_machine = encrypted_machine(&chain, &served_name, &served_role, &key).await;
        assert_eq!(
            served_machine, served_arn,
            "case {case:?}: the state machine the condition names is the one created, got \
             {served_machine}"
        );

        if refused_first {
            refuses(&chain, case, &refused_name, &refused_machine, &refused_role).await;
            serves(&chain, case, &served_machine).await;
        } else {
            serves(&chain, case, &served_machine).await;
            refuses(&chain, case, &refused_name, &refused_machine, &refused_role).await;
        }

        // What each machine holds afterwards is what its own start earned, whatever the
        // other one's start did: the refused machine holds nothing and the served one
        // holds exactly the execution it was given.
        assert_eq!(
            (
                started_count(&chain, &refused_machine).await,
                started_count(&chain, &served_machine).await,
            ),
            (0, 1),
            "case {case:?}: expected the refused machine to hold no execution and the twin to \
             hold its own"
        );
    }
}
