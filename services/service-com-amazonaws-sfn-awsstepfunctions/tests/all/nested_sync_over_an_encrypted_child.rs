// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ONE identity the mint of an execution's data key is judged
//! for, driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the key policy is the document KMS's own
//! handlers hold and the role documents are the ones IAM's handlers wrote.
//!
//! The rule under test: the data key everything an execution of a
//! `CUSTOMER_MANAGED_KMS_KEY` state machine retains is sealed under is acquired by ONE KMS
//! call, made AS THAT STATE MACHINE'S EXECUTION ROLE ("the IAM role to use for this state
//! machine", and "when the period expires, Step Functions will call `GenerateDataKey`",
//! `smithy_query(query="shape",shape="EncryptionConfiguration")`), so it is judged for that
//! ROLE and for no second identity: where the authorities in force can be weighed over the
//! role at all, a role they admit starts the execution whatever the identity that CALLED
//! the start holds on the key, and a role they refuse is refused naming that role's own
//! assumed-role session. What the CALLER's own documents decide is the data the call itself
//! reports or opens, which a start reports none of.
//!
//! Its consequence for a nested execution: a `Task` state whose `Resource` is the
//! optimized nested-execution integration makes its calls as the PARENT execution's role,
//! so starting a CMK-encrypted child asks that role for nothing of the mint, while reading
//! the child's run back asks it for the `kms:Decrypt` the data the report carries was
//! sealed with — and a parent role the child's key does not allow that decrypt fails the
//! task under the language's own `States.TaskFailed`, carrying the sentence KMS refused it
//! with, instead of reading the child's output.
//!
//! The rows of the first table differ along what the rule generalises over: which revision
//! of the pattern the `Resource` names, whether the child is on a customer managed key at
//! all, and what the parent role's documents say about that key's decrypt. The second
//! table's rows differ along the two identities the one mint could be judged for — what the
//! execution role's documents say about it, and what the CALLER's own say — over all four
//! combinations.
//!
//! The harness (`Chain`, the key policies) is `tests/all/execution_data_encryption.rs`'s,
//! the role documents are `tests/all/execution_role_trust_conditions.rs`'s and
//! `tests/all/role_log_and_key_authorization.rs`'s, the identity-policy document and the
//! sentence AWS states an authorization failure with are
//! `tests/all/identity_policy_authorization.rs`'s, the reading of the session a refusal
//! blames is `tests/all/task_refusal_session_principal.rs`'s and the reading of a
//! refusal's message is `tests/all/encrypted_execution_writes.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::encrypted_execution_writes::message;
use crate::execution_data_encryption::{
    customer_managed, granting_policy, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::execution_role_trust_conditions::{data_key_policy, key_policy_with, started_count};
use crate::identity_policy_authorization::{allowing, no_grant, not_authorized};
use crate::role_log_and_key_authorization::role;
use crate::state_machine_lifecycle::{error_code, string, REGION};
use crate::task_refusal_session_principal::session_principal;

/// How long a row looks for an execution's terminal status before declaring it never
/// arrived, and how often it looks. Generous next to the span a run of these definitions
/// takes, so a loaded host is never the thing that trips it.
const PATIENCE: Duration = Duration::from_secs(180);
const LOOK: Duration = Duration::from_millis(25);

/// The ASL error a `Task` state reports when the work its `Resource` named did not happen
/// (`smithy_query(query="shape",shape="HistoryEventType")` names the event it is recorded
/// under).
const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// The KMS operations this suite names: the mint of a data key, and the decrypt of the
/// data one sealed.
const MINT_ACTION: &str = "kms:GenerateDataKey";
const DECRYPT_ACTION: &str = "kms:Decrypt";

/// The two revisions of the optimized nested-execution integration that WAIT for the
/// child, as a `Resource` names them.
const NESTED_SYNC: &str = "arn:aws:states:::states:startExecution.sync";
const NESTED_SYNC_2: &str = "arn:aws:states:::states:startExecution.sync:2";

/// The child every parent below composes: one `Pass` state that answers the `seed` it was
/// handed under `value`, so the document the parent reads back is the child's own work.
fn child_definition() -> String {
    json!({
        "StartAt": "Echo",
        "States": {
            "Echo": { "Type": "Pass", "Parameters": { "value.$": "$.seed" }, "End": true }
        }
    })
    .to_string()
}

/// A parent whose one `Task` state starts a child through `resource`, waits for it and
/// reports `selector` of what the pattern answered.
fn parent_definition(resource: &str, selector: Value) -> String {
    json!({
        "StartAt": "Nested",
        "States": {
            "Nested": {
                "Type": "Task",
                "Resource": resource,
                "Parameters": {
                    "StateMachineArn.$": "$.childArn",
                    "Input": { "seed.$": "$.seed" },
                },
                "ResultSelector": selector,
                "End": true,
            }
        }
    })
    .to_string()
}

/// A state machine named `name` on `definition`, running as `role_arn`, with `encryption`
/// where the row names one; its ARN.
async fn machine(
    chain: &Chain,
    name: &str,
    definition: String,
    role_arn: &str,
    encryption: Option<Value>,
) -> String {
    let mut request = json!({
        "name": name,
        "definition": definition,
        "roleArn": role_arn,
    });
    if let Some(encryption) = encryption {
        request["encryptionConfiguration"] = encryption;
    }
    string(
        &chain.sfn("CreateStateMachine", &request).await,
        "stateMachineArn",
    )
}

/// Start one execution of `machine_arn` on `input` and report it once it has left
/// `RUNNING`.
///
/// The wait is for the CONDITION — a terminal status — looked for again until [`PATIENCE`]
/// is gone, so nothing here depends on how fast the host runs.
async fn ran(chain: &Chain, machine_arn: &str, input: &Value, case: &str) -> Value {
    let started = chain
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine_arn, "input": input.to_string() }),
        )
        .await;
    let execution = string(&started, "executionArn");
    let until = Instant::now() + PATIENCE;
    loop {
        let described = chain
            .sfn("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected a terminal status, got {described:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// What a nested parent's run must end as.
enum Settles {
    /// It read the child's run back: `SUCCEEDED`, reporting this document.
    Succeeded(fn(&str) -> Value),
    /// The child's key does not let the parent role decrypt what the report carries, so
    /// the task failed with the sentence KMS refused that decrypt with.
    DecryptRefused,
}

/// One nested parent: which revision of the pattern it names, what it reads out of the
/// pattern's answer, whether its child is on the customer managed key, what its role's
/// documents say about that key, and what the run must end as.
struct Nested {
    case: &'static str,
    resource: &'static str,
    selector: fn() -> Value,
    encrypted_child: bool,
    /// Whether the parent role's documents allow the decrypt of the child's key.
    grants_decrypt: bool,
    settles: Settles,
}

/// The parents the rule covers.
fn nested_rows() -> Vec<Nested> {
    vec![
        Nested {
            case: "the second revision over an encrypted child, the decrypt granted",
            resource: NESTED_SYNC_2,
            // The second revision parses each document the report carries as JSON, so the
            // child's own output is addressed member by member.
            selector: || json!({ "fromChild.$": "$.Output.value" }),
            encrypted_child: true,
            grants_decrypt: true,
            settles: Settles::Succeeded(|seed| json!({ "fromChild": seed })),
        },
        Nested {
            case: "the second revision over an encrypted child, the decrypt withheld",
            resource: NESTED_SYNC_2,
            selector: || json!({ "fromChild.$": "$.Output.value" }),
            encrypted_child: true,
            grants_decrypt: false,
            settles: Settles::DecryptRefused,
        },
        Nested {
            case: "the first revision over an encrypted child, the decrypt granted",
            resource: NESTED_SYNC,
            // The first revision reports the child's output as the JSON TEXT the service's
            // own answer carries, so the whole text is what the parent reads.
            selector: || json!({ "fromChild.$": "$.Output" }),
            encrypted_child: true,
            grants_decrypt: true,
            settles: Settles::Succeeded(|seed| {
                json!({ "fromChild": json!({ "value": seed }).to_string() })
            }),
        },
        Nested {
            case: "the first revision over an encrypted child, the decrypt withheld",
            resource: NESTED_SYNC,
            selector: || json!({ "fromChild.$": "$.Output" }),
            encrypted_child: true,
            grants_decrypt: false,
            settles: Settles::DecryptRefused,
        },
        Nested {
            // The boundary: a child on an Amazon Web Services owned key seals its data
            // under a key AWS manages the policy of, so the parent needs nothing of KMS.
            case: "the second revision over a child on no customer managed key",
            resource: NESTED_SYNC_2,
            selector: || json!({ "fromChild.$": "$.Output.value" }),
            encrypted_child: false,
            grants_decrypt: false,
            settles: Settles::Succeeded(|seed| json!({ "fromChild": seed })),
        },
    ]
}

/// A nested `.sync` parent over a CMK-encrypted child makes the pattern's calls as its OWN
/// execution role: the start asks that role for nothing of the child key's mint — which is
/// the child's own role's call — while reading the child's run back asks it for the decrypt
/// of what that key sealed, so a parent role granted the decrypt reads the child's output
/// and one without it fails the task with the sentence KMS refused that decrypt with.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_nested_sync_parent_is_judged_on_the_decrypt_of_the_childs_key() {
    let chain = Chain::new();
    let key = chain.create_key(&granting_policy()).await;

    // The child's own role holds the calls sealing an execution's data makes, so nothing a
    // row observes is the CHILD's run failing.
    let child_role = role(&chain, "nested-child-role", &data_key_policy()).await;
    let encrypted_child = machine(
        &chain,
        "nested-encrypted-child",
        child_definition(),
        &child_role,
        Some(customer_managed(&key)),
    )
    .await;
    let plain_child = machine(
        &chain,
        "nested-plain-child",
        child_definition(),
        &child_role,
        None,
    )
    .await;

    for (index, row) in nested_rows().into_iter().enumerate() {
        let Nested {
            case,
            resource,
            selector,
            encrypted_child: encrypted,
            grants_decrypt,
            settles,
        } = row;
        let role_name = format!("nested-parent-role-{index}");
        // Every row's role may start, report and stop the child's executions; the one
        // difference is what it says about the child key's DECRYPT.
        let mut actions = vec![
            "states:StartExecution",
            "states:DescribeExecution",
            "states:StopExecution",
        ];
        if grants_decrypt {
            actions.push(DECRYPT_ACTION);
        }
        let parent_role = role(&chain, &role_name, &allowing(&actions, &["*"])).await;
        let parent = machine(
            &chain,
            &format!("nested-parent-{index}"),
            parent_definition(resource, selector()),
            &parent_role,
            None,
        )
        .await;

        // A seed of this row's own, so what comes back is this run's work rather than a
        // document another row could have produced.
        let seed = format!("seed-{index}");
        let child_arn = match encrypted {
            true => &encrypted_child,
            false => &plain_child,
        };
        let answered = ran(
            &chain,
            &parent,
            &json!({ "childArn": child_arn, "seed": &seed }),
            case,
        )
        .await;

        match settles {
            Settles::Succeeded(output) => {
                assert_eq!(
                    answered.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the parent to read the child's run back, got \
                     {answered:?}"
                );
                let reported: Value = answered
                    .get("output")
                    .and_then(Value::as_str)
                    .and_then(|text| serde_json::from_str(text).ok())
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: expected a JSON output, got {answered:?}")
                    });
                assert_eq!(
                    reported,
                    output(&seed),
                    "case {case:?}: expected the child's own document, got {answered:?}"
                );
            }
            Settles::DecryptRefused => {
                assert_eq!(
                    (
                        answered.get("status").and_then(Value::as_str),
                        answered.get("error").and_then(Value::as_str),
                    ),
                    (Some("FAILED"), Some(TASK_FAILED_ERROR)),
                    "case {case:?}: expected the task to fail on the child key's decrypt, got \
                     {answered:?}"
                );
                let cause = answered
                    .get("cause")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let principal = session_principal(&parent_role, &cause);
                assert!(
                    cause.starts_with(&not_authorized(
                        &principal,
                        DECRYPT_ACTION,
                        &key,
                        &no_grant(DECRYPT_ACTION),
                    )),
                    "case {case:?}: expected the cause to open with the denial of the parent \
                     role's session, got {cause:?}"
                );
                assert!(
                    !cause.contains(MINT_ACTION),
                    "case {case:?}: the mint is the child role's own call, so no refusal of \
                     this task names it, got {cause:?}"
                );
            }
        }
    }
}

/// One start of a CMK-encrypted state machine: what the EXECUTION ROLE's documents say
/// about the mint of the data key, what the CALLER's own say, and whether the start is
/// served.
struct Mint {
    case: &'static str,
    /// Whether the execution role's documents allow the mint.
    role_mints: bool,
    /// Whether the identity that signs the start holds anything of the key at all.
    caller_holds_the_key: bool,
}

/// All four combinations of the two identities the one mint could be judged for.
fn mint_rows() -> Vec<Mint> {
    vec![
        Mint {
            case: "the role may mint and the caller holds the key",
            role_mints: true,
            caller_holds_the_key: true,
        },
        Mint {
            case: "the role may mint and the caller holds nothing of the key",
            role_mints: true,
            caller_holds_the_key: false,
        },
        Mint {
            case: "the role may not mint and the caller holds the key",
            role_mints: false,
            caller_holds_the_key: true,
        },
        Mint {
            case: "the role may not mint and the caller holds nothing of the key",
            role_mints: false,
            caller_holds_the_key: false,
        },
    ]
}

/// The mint an encrypted start acquires its data key with is ONE call, made as the state
/// machine's execution role, so it is judged for that role alone: a role the authorities in
/// force admit starts the execution whatever the caller holds on the key, and a role they
/// refuse is refused naming that role's own session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_mint_of_a_starts_data_key_is_judged_for_the_execution_role() {
    let chain = Chain::new();
    // The key's own policy delegates to the account root, the form the default key policy
    // takes, so each identity is decided on its own documents.
    let key = chain.create_key(&key_policy_with(Vec::new())).await;

    // The caller that holds NOTHING of the key: an IAM user whose documents allow this
    // service's own calls and no KMS action at all. The other caller is the account's own
    // published credential, which no identity policy bounds.
    let (bounded_key, _bounded_user) = chain
        .iam_user("mint-bounded-caller", &allowing(&["states:*"], &["*"]))
        .await;

    for (index, row) in mint_rows().into_iter().enumerate() {
        let Mint {
            case,
            role_mints,
            caller_holds_the_key,
        } = row;
        let role_name = format!("mint-role-{index}");
        let policy = match role_mints {
            true => data_key_policy(),
            // A role the key's delegation to the account leaves without the mint: its
            // documents allow this service's reads and nothing of KMS.
            false => allowing(&["states:DescribeExecution"], &["*"]),
        };
        let role_arn = role(&chain, &role_name, &policy).await;
        // One state machine per row: the data key a start acquires is the state machine's
        // own and is re-used while its period lasts, so a row that shared a machine with
        // another would ask the key for nothing.
        let machine_arn = machine(
            &chain,
            &format!("mint-machine-{index}"),
            child_definition(),
            &role_arn,
            Some(customer_managed(&key)),
        )
        .await;

        let request = json!({
            "stateMachineArn": machine_arn,
            "name": format!("run-{index}"),
            "input": json!({ "seed": format!("mint-{index}") }).to_string(),
        });
        let (status, answered) = match caller_holds_the_key {
            true => chain.call(&SFN, "StartExecution", &request).await,
            false => {
                chain
                    .call_as(&bounded_key, &SFN, "StartExecution", &request)
                    .await
            }
        };

        if role_mints {
            assert_eq!(
                status, 200,
                "case {case:?}: a role the authorities admit starts the execution, got {status} \
                 with {answered:?}"
            );
            // Round trip: the execution the start reports is one this service holds, on
            // the state machine the request named.
            let execution = string(&answered, "executionArn");
            let described = chain
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            assert_eq!(
                described.get("stateMachineArn").and_then(Value::as_str),
                Some(machine_arn.as_str()),
                "case {case:?}: expected the execution of that state machine, got {described:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answered).as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: a role the authorities refuse the mint starts nothing, got {status} \
             with {answered:?}"
        );
        let refusal = message(&answered);
        let principal = session_principal(&role_arn, &refusal);
        assert!(
            refusal.starts_with(&not_authorized(
                &principal,
                MINT_ACTION,
                &key,
                &no_grant(MINT_ACTION),
            )),
            "case {case:?}: expected the refusal to name the session the mint was made under, \
             got {refusal:?}"
        );
        let started = started_count(&chain, &machine_arn).await;
        assert_eq!(
            started, 0,
            "case {case:?} (region {REGION}): a refused start starts nothing, got {started}"
        );
    }
}
