// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DATA KEY a start of a KMS-encrypted state machine acquires,
//! driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and KMS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the trust document the assume is decided
//! against is the one IAM's own handlers wrote and the key policy is the one KMS's own
//! handlers hold.
//!
//! The rule under test: everything an execution of a state machine whose
//! `encryptionConfiguration` is `CUSTOMER_MANAGED_KMS_KEY` retains is sealed under a DATA
//! KEY, and Step Functions acquires that data key AS THAT STATE MACHINE'S EXECUTION ROLE
//! ("the IAM role to use for this state machine"), only where none is in force —
//! "maximum duration that Step Functions will reuse data keys. When the period expires,
//! Step Functions will call `GenerateDataKey`"
//! (`smithy_query(query="shape",shape="EncryptionConfiguration")`). So:
//!
//! 1. a start that has to acquire a data key and cannot is refused HTTP 400
//!    `KmsAccessDeniedException` ("either your KMS key policy or API caller does not have
//!    the required permissions") and starts nothing, carrying what the acquisition failed
//!    with — the exception the ASSUME of the role raised, where this service may not act
//!    as that role for this state machine, and otherwise the sentence KMS refused the mint
//!    with;
//! 2. the principal such a KMS sentence names is the assumed-role SESSION the mint was
//!    made under, which is the identity that made the refused call — never the identity
//!    that called `StartExecution`, even where the key's policy denies that one too;
//! 3. and a start that finds a data key in force seals this execution's data with it, so
//!    it asks the key for nothing and is served whatever the acquisition would now answer.
//!
//! The rows of the first table differ along WHAT stops the acquisition: which spelling of
//! the trust policy withholds the assume (another service principal, an explicit deny, a
//! condition over either confused-deputy key), and which authority KMS weighs refuses the
//! mint (an explicit deny naming every principal, one naming the role, and a key policy
//! that only delegates to an account whose role allows nothing of KMS). Its boundary is a
//! row of its own: an acquisition every authority admits runs the execution. The second
//! table pairs each of those conditions with the one difference the rule turns on — a data
//! key in force or none — over two state machines on the very same key and role.
//!
//! The harness (`Chain`, the key policies) is `tests/all/execution_data_encryption.rs`'s,
//! and the documents are `tests/all/execution_role_trust_conditions.rs`'s;
//! `tests/all/main.rs` is this binary's crate root, so they are reached through
//! `crate::execution_data_encryption` and `crate::execution_role_trust_conditions`.

use serde_json::{json, Value};

use crate::execution_data_encryption::{
    xml_element, Chain, KMS_ACCESS_DENIED, KMS_ACCESS_DENIED_STATUS, SFN,
};
use crate::execution_role_trust_conditions::{
    data_key_policy, encrypted_machine, key_policy_with, started_count, trust,
};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, REGION};
use crate::unassumable_role_encrypted_execution::{ended, unassumable_cause};

/// An account that is not the one these resources belong to, for a trust clause the assume
/// cannot satisfy.
const ANOTHER_ACCOUNT: &str = "210987654321";

/// The KMS operation the mint of a data key is, which a refusal of the acquisition names.
const MINT_ACTION: &str = "kms:GenerateDataKey";

/// A role of this account named `name`, trusting `trust` to assume it and carrying
/// `policy` as its ONE inline policy, by its own ARN.
///
/// The documents are made with IAM's OWN operations over the same engine, so what the
/// assume and the mint are decided against is what IAM's handlers wrote.
async fn role(chain: &Chain, name: &str, trust: &Value, policy: &Value) -> String {
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
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    arn
}

/// A role policy that allows nothing of KMS: a role the key's delegation to the account
/// leaves without the mint.
fn unrelated_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["states:DescribeExecution"],
            "Resource": "*",
        }]
    })
}

/// A statement denying `principal` the mint sealing an execution's data makes.
fn deny_mint(principal: &str) -> Value {
    json!({
        "Sid": "DenyTheMint",
        "Effect": "Deny",
        "Principal": { "AWS": principal },
        "Action": MINT_ACTION,
        "Resource": "*",
    })
}

/// What the start of the row's state machine answers.
enum Expected {
    /// Served: the acquisition is one every authority admits, so the execution runs.
    Served,
    /// Refused with the exception the ASSUME of the execution role raised, every word of
    /// it fixed but the role the state machine carries.
    AssumeFailed,
    /// Refused with the sentence KMS worded for the mint, whose tail — after the key the
    /// mint was refused on — is this.
    KeyRefused(&'static str),
}

/// One row: the documents in force over the acquisition, and what the start answers.
struct Row {
    case: &'static str,
    /// The trust document IAM holds for the role, built from the ARN of the state machine
    /// the row starts.
    trust: fn(&str) -> Value,
    /// The identity-based document in force on the role.
    role_policy: fn() -> Value,
    /// The statements the key's policy carries beside the account root's, built from the
    /// role's own ARN.
    key_statements: fn(&str) -> Vec<Value>,
    expected: Expected,
}

/// The ARN a state machine of this account named `name` carries, which is the value the
/// assume presents under `aws:SourceArn`.
fn machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a trust policy naming another service principal",
            trust: |_machine| trust_for("lambda.amazonaws.com", None),
            role_policy: data_key_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::AssumeFailed,
        },
        Row {
            case: "a trust policy denying this service",
            trust: |_machine| denying_trust(),
            role_policy: data_key_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::AssumeFailed,
        },
        Row {
            case: "a trust narrowed to another account under StringEquals aws:SourceAccount",
            trust: |_machine| {
                trust(Some(
                    json!({ "StringEquals": { "aws:SourceAccount": ANOTHER_ACCOUNT } }),
                ))
            },
            role_policy: data_key_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::AssumeFailed,
        },
        Row {
            case: "a trust narrowed to another state machine under ArnLike aws:SourceArn",
            trust: |_machine| {
                trust(Some(json!({
                    "ArnLike": { "aws:SourceArn": machine_arn("no-such-machine") }
                })))
            },
            role_policy: data_key_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::AssumeFailed,
        },
        Row {
            // The principal the refusal names is the one that MADE the refused call: a
            // deny covering every principal covers the identity that called
            // `StartExecution` as well, and the mint is still the execution role's.
            case: "a key policy denying every principal the mint",
            trust: |_machine| trust(None),
            role_policy: data_key_policy,
            key_statements: |_role| vec![deny_mint("*")],
            expected: Expected::KeyRefused("with an explicit deny in a resource-based policy"),
        },
        Row {
            case: "a key policy denying the execution role the mint",
            trust: |_machine| trust(None),
            role_policy: data_key_policy,
            key_statements: |role| vec![deny_mint(role)],
            expected: Expected::KeyRefused("with an explicit deny in a resource-based policy"),
        },
        Row {
            // The key policy delegates to the account, so the document the role is
            // missing the mint in is its own.
            case: "a key policy that only delegates to an account whose role allows nothing of KMS",
            trust: |_machine| trust(None),
            role_policy: unrelated_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::KeyRefused(
                "because no identity-based policy allows the kms:GenerateDataKey action",
            ),
        },
        Row {
            // The boundary: an acquisition every authority admits.
            case: "a trust admitting this service and a key policy delegating to its account",
            trust: |_machine| trust(None),
            role_policy: data_key_policy,
            key_statements: |_role| Vec::new(),
            expected: Expected::Served,
        },
    ]
}

/// A trust policy with one `Allow` for the service principal `principal`, narrowed by
/// `condition` where the row states one — [`trust`] for a principal other than this
/// service's own.
fn trust_for(principal: &str, condition: Option<Value>) -> Value {
    let mut statement = json!({
        "Effect": "Allow",
        "Principal": { "Service": principal },
        "Action": "sts:AssumeRole",
    });
    if let Some(condition) = condition {
        statement["Condition"] = condition;
    }
    json!({ "Version": "2012-10-17", "Statement": [statement] })
}

/// A trust policy DENYING this service the assume, which withholds it whatever else any
/// statement allows.
fn denying_trust() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Deny",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }]
    })
}

/// The `message` of a refused answer.
fn message(answered: &Value) -> String {
    answered
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `message` with the opaque name of the session the role was assumed under replaced by
/// `<session>`, and that name, so a sentence carrying one can be compared word for word.
///
/// The session name is the service's own to choose and is minted per assume, so it is the
/// one part of the sentence no expectation can state.
fn session_masked(message: &str, role_name: &str) -> (String, String) {
    let prefix = format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/");
    let session = message
        .split_once(&prefix)
        .and_then(|(_, rest)| rest.split(' ').next())
        .unwrap_or_default()
        .to_string();
    match session.is_empty() {
        true => (message.to_string(), session),
        false => (message.replacen(&session, "<session>", 1), session),
    }
}

/// The sentence KMS refuses a mint it does not allow `principal` with, whose tail is
/// `why`, followed by the metadata an AWS error of this generation carries.
fn key_refusal(principal: &str, key: &str, why: &str, request_id: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {MINT_ACTION} on resource: {key} \
         {why} (Service: Kms, Status Code: 400, Request ID: {request_id}) (SDK Attempt Count: 1)"
    )
}

/// The id of the refused request a relayed KMS sentence reports, or the empty string where
/// the message carries no such tail.
fn reported_request_id(message: &str) -> String {
    message
        .split_once("Request ID: ")
        .and_then(|(_, rest)| rest.split(')').next())
        .unwrap_or_default()
        .to_string()
}

/// A start that has to ACQUIRE the data key its execution's data is sealed under follows
/// what the acquisition needs: refused with the exception the assume of the execution role
/// raised where this service may not act as it, refused with the sentence KMS worded for
/// the mint — naming the SESSION the mint was made under — where the key does not allow
/// that role the mint, and served where every authority admits it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_that_must_acquire_a_data_key_follows_what_the_acquisition_needs() {
    let chain = Chain::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            trust,
            role_policy,
            key_statements,
            expected,
        } = row;
        let role_name = format!("acquire-role-{index}");
        let name = format!("acquire-machine-{index}");
        let expected_arn = machine_arn(&name);
        let role_arn = role(
            &chain,
            &role_name,
            &trust(&expected_arn),
            &role_policy(),
        )
        .await;
        // The key carries the account root's own statement while the state machine is
        // created, because a create of an encrypted state machine reads the key for the
        // caller too; the row's statements go on afterwards, so what they govern is this
        // start alone.
        let key = chain.create_key(&key_policy_with(Vec::new())).await;
        let machine = encrypted_machine(&chain, &name, &role_arn, &key).await;
        let statements = key_statements(&role_arn);
        if !statements.is_empty() {
            chain.put_policy(&key, &key_policy_with(statements)).await;
        }

        let (status, answered) = chain
            .call(
                &SFN,
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": "run" }),
            )
            .await;

        if matches!(expected, Expected::Served) {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the start to be served, got {status} {answered:?}"
            );
            let execution = string(&answered, "executionArn");
            // Round trip: the execution the start reports is one this service holds.
            let described = chain
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            assert_eq!(
                described.get("stateMachineArn").and_then(Value::as_str),
                Some(machine.as_str()),
                "case {case:?}: expected the execution of that machine, got {described:?}"
            );
            let finished = ended(&chain, case, &execution).await;
            assert_eq!(
                finished.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected the execution to run, got {finished:?}"
            );
            continue;
        }

        let code = error_code(&answered);
        assert_eq!(
            (status, code.as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected the modelled KMS refusal, got {status} {code} with \
             {answered:?}"
        );
        let answer = message(&answered);
        let (sentence, session) = session_masked(&answer, &role_name);
        let expected_message = match expected {
            Expected::Served => unreachable!("the served row is answered above"),
            Expected::AssumeFailed => unassumable_cause(&role_arn),
            Expected::KeyRefused(why) => key_refusal(
                &format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/<session>"),
                &key,
                why,
                &reported_request_id(&answer),
            ),
        };
        assert_eq!(
            sentence, expected_message,
            "case {case:?}: expected {expected_message:?}, got {sentence:?} (session \
             {session:?})"
        );
        if matches!(expected, Expected::KeyRefused(_)) {
            assert!(
                ::service_common::query_error_envelope::is_request_id(&reported_request_id(&answer)),
                "case {case:?}: expected KMS's own metadata to name the refused request, got \
                 {answer:?}"
            );
            // The identity a relayed refusal blames is the session the mint was made
            // under, so neither the `iam` role ARN — whose path a session ARN does not
            // carry — nor the caller that signed the start is named.
            assert!(
                !answer.contains(&role_arn) && !answer.contains(":user/"),
                "case {case:?}: expected only the assumed-role session to be named, got \
                 {answer:?}"
            );
        }
        assert_eq!(
            started_count(&chain, &machine).await,
            0,
            "case {case:?}: expected a refused start to have started nothing"
        );
    }
}

/// One row of the pair below: what is taken away from the acquisition after a first start
/// has made it.
struct Pair {
    case: &'static str,
    /// Whether the role's trust is withdrawn, which withholds the assume the acquisition
    /// begins with.
    withhold_trust: bool,
    /// Whether the key's policy is replaced by one granting this account nothing, which
    /// withholds the mint itself.
    withhold_key: bool,
}

/// A start that finds a DATA KEY IN FORCE for its state machine seals this execution's
/// data with it and asks the key for nothing, so it is served where a start that would
/// have to acquire one is refused — the two differing in nothing else, being starts of two
/// state machines on the same key and the same execution role under the same documents.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_start_that_finds_a_data_key_in_force_asks_the_key_for_nothing() {
    let pairs = [
        Pair {
            case: "the trust the assume needs is taken away",
            withhold_trust: true,
            withhold_key: false,
        },
        Pair {
            case: "the key's own policy stops allowing the mint",
            withhold_trust: false,
            withhold_key: true,
        },
        Pair {
            case: "both at once",
            withhold_trust: true,
            withhold_key: true,
        },
    ];

    for (index, pair) in pairs.into_iter().enumerate() {
        let Pair {
            case,
            withhold_trust,
            withhold_key,
        } = pair;
        let chain = Chain::new();
        let role_name = format!("in-force-role-{index}");
        let role_arn = role(&chain, &role_name, &trust(None), &data_key_policy()).await;
        let key = chain.create_key(&key_policy_with(Vec::new())).await;
        // Two state machines of the same key and role: one that will hold a data key in
        // force when the conditions change, and one that has never started anything.
        let holding = encrypted_machine(
            &chain,
            &format!("in-force-machine-{index}"),
            &role_arn,
            &key,
        )
        .await;
        let fresh = encrypted_machine(
            &chain,
            &format!("no-data-key-machine-{index}"),
            &role_arn,
            &key,
        )
        .await;

        // The first start ACQUIRES the data key, which is in force for the reuse period
        // the configuration states from here on.
        let (status, answered) = chain
            .call(
                &SFN,
                "StartExecution",
                &json!({ "stateMachineArn": holding, "name": "first" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first start to acquire the data key, got {status} \
             {answered:?}"
        );

        if withhold_trust {
            chain
                .iam(
                    "UpdateAssumeRolePolicy",
                    &[
                        ("RoleName", role_name.clone()),
                        ("PolicyDocument", trust_for("lambda.amazonaws.com", None).to_string()),
                    ],
                )
                .await;
        }
        if withhold_key {
            chain.put_policy(&key, &key_policy_with(vec![deny_mint("*")])).await;
        }

        // The data key is in force, so this start seals its own data with it and asks the
        // key for nothing.
        let (status, answered) = chain
            .call(
                &SFN,
                "StartExecution",
                &json!({ "stateMachineArn": holding, "name": "second" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a start re-using the data key in force to be served, got \
             {status} {answered:?}"
        );
        // Round trip: the execution that start reports is one this service holds.
        let described = chain
            .sfn(
                "DescribeExecution",
                &json!({ "executionArn": string(&answered, "executionArn") }),
            )
            .await;
        assert_eq!(
            described.get("stateMachineArn").and_then(Value::as_str),
            Some(holding.as_str()),
            "case {case:?}: expected the execution of that machine, got {described:?}"
        );
        assert_eq!(
            started_count(&chain, &holding).await,
            2,
            "case {case:?}: expected both starts to have started an execution"
        );

        // The other state machine holds no data key, so its start has to acquire one
        // under those very same documents — and cannot.
        let (status, answered) = chain
            .call(
                &SFN,
                "StartExecution",
                &json!({ "stateMachineArn": fresh, "name": "first" }),
            )
            .await;
        let code = error_code(&answered);
        assert_eq!(
            (status, code.as_str()),
            (KMS_ACCESS_DENIED_STATUS, KMS_ACCESS_DENIED),
            "case {case:?}: expected a start that must acquire a data key to be refused, got \
             {status} {code} with {answered:?}"
        );
        assert_eq!(
            started_count(&chain, &fresh).await,
            0,
            "case {case:?}: expected the refused start to have started nothing"
        );
    }
}
