// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHAT a states call is authorized against: the resource ARN a
//! refusal names, the qualifier and tag condition keys a statement about the call can be
//! scoped by, and the permissions a request asks for beside the operation it names.
//!
//! The rules under test, each asserted over a SET of inputs rather than over one call:
//!
//! 1. a call that names no resource of its own is decided against every resource of the
//!    KIND its API is about — `arn:aws:states:<region>:<account>:<type>:*`, the type of
//!    the API's own category, and `:*:*` where the API acts on no resource of the account
//!    at all — and a call that names one is decided against that ARN;
//! 2. a state machine named THROUGH an alias or a version is one state machine: the call
//!    is decided on the state machine's own ARN, with the qualifier carried as
//!    `states:StateMachineQualifier`, so a grant on the state machine covers every form of
//!    its ARN and a grant scoped by qualifier covers exactly the references it names;
//! 3. a request asks for the permission of what it DOES: tagging what a create creates
//!    (`states:TagResource`) and publishing a version (`states:PublishStateMachineVersion`)
//!    are asked for by the members that switch them on, and a caller that holds only the
//!    operation is refused exactly the requests that ask for them — creating nothing;
//! 4. the tag condition keys are decided from the request and the store:
//!    `aws:RequestTag/<key>` and `aws:TagKeys` from the tags the request writes, and
//!    `aws:ResourceTag/<key>` from the tags the resource carries — which for a create is
//!    the tag set the resource it mints will carry;
//! 5. an ARN of ANOTHER account names a version or an alias no document of this account
//!    can grant over, so such a call is refused as a resource the caller may not access
//!    rather than as a resource that is absent.
//!
//! Driven through the production `MultiServiceRouter` with Step Functions, IAM and STS on
//! ONE engine (`crate::identity_policy_authorization::Emulator`), so the documents a
//! decision reads are the ones IAM's own handlers wrote.

use serde_json::{json, Value};

use crate::identity_policy_authorization::{
    every_resource, every_resource_in, message, no_grant, not_authorized, Emulator, ACCESS_DENIED,
    ACCESS_DENIED_STATUS, ACCOUNT_KEY, EVERY_TYPE, STATE_MACHINE_TYPE,
};
use crate::state_machine_lifecycle::{error_code, string, ACCOUNT, HELLO_DEFINITION, REGION, ROLE_ARN};

/// The resource type the ACTIVITY APIs of this service are about, beside the state machine
/// one the shared harness spells.
///
/// Source: this service's own ARN forms — an activity's ARN writes `activity` in the field
/// a state machine's writes `stateMachine` in (`crate::state::ACTIVITY_RESOURCE`).
const ACTIVITY_TYPE: &str = "activity";

/// The clause a refusal of a call whose resource belongs to another account carries: the
/// principal alone, since a caller must not learn from a refusal what another account
/// holds.
fn no_access(principal: &str) -> String {
    format!("User: {principal} is not authorized to access this resource")
}

/// An identity policy allowing `actions` on `resources` under `condition`, beside the
/// unconditioned `iam:PassRole` a create that names a role needs: the condition under test
/// is about the states action, and the pass of a role carries none of the keys it names.
fn allowing_under(actions: &[&str], resources: &[&str], condition: Value) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": actions,
                "Resource": resources,
                "Condition": condition,
            },
            { "Effect": "Allow", "Action": "iam:PassRole", "Resource": "*" },
        ]
    })
}

/// A definition whose one state does nothing, for a state machine these rows only
/// authorize calls about.
fn pass_definition() -> String {
    HELLO_DEFINITION.to_string()
}

/// RULE 1: the resource a refusal names is every resource of the kind the API is about
/// when the request names none of its own, and the ARN the request named otherwise — and
/// where the API acts on no resource of the account at all, the kind is every resource
/// type.
///
/// The rows span the API categories — a state machine listing, an activity listing, a
/// state tested and a definition validated (each deciding only the definition its own
/// request carries, so about no resource of the account), a settlement carrying a token
/// this service never minted, a create (judged against the ARN its name mints), a read that
/// names an execution ARN, and a tagging read that names a state machine ARN — and both of
/// IAM's refusal clauses on one of them.
#[tokio::test]
async fn a_refusal_names_the_resource_pattern_of_the_apis_own_category() {
    let emulator = Emulator::new();
    let caller = emulator
        .user("category-reader", &allowing(&["kms:Decrypt"]))
        .await;

    let execution = format!("arn:aws:states:{REGION}:{ACCOUNT}:execution:runner:one");
    let machine = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:runner");
    // (case, operation, the request body, the resource the refusal must name)
    let rows: Vec<(&str, &str, Value, String)> = vec![
        (
            "a listing of state machines",
            "ListStateMachines",
            json!({}),
            every_resource(STATE_MACHINE_TYPE),
        ),
        (
            "a listing of activities",
            "ListActivities",
            json!({}),
            every_resource(ACTIVITY_TYPE),
        ),
        (
            "a state tested with no state machine around it",
            "TestState",
            json!({ "definition": r#"{"Type":"Pass","End":true}"#, "roleArn": ROLE_ARN }),
            every_resource(EVERY_TYPE),
        ),
        (
            "a definition validated with no state machine around it",
            "ValidateStateMachineDefinition",
            json!({ "definition": pass_definition() }),
            every_resource(EVERY_TYPE),
        ),
        (
            "a settlement carrying a token this service never minted",
            "SendTaskSuccess",
            json!({ "taskToken": "not-a-token", "output": "{}" }),
            every_resource(ACTIVITY_TYPE),
        ),
        (
            "a create, judged against the ARN its name mints",
            "CreateActivity",
            json!({ "name": "category-probe" }),
            format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:category-probe"),
        ),
        (
            "a read that names one execution",
            "DescribeExecution",
            json!({ "executionArn": execution }),
            execution.clone(),
        ),
        (
            "a tagging read that names a state machine",
            "ListTagsForResource",
            json!({ "resourceArn": machine }),
            machine.clone(),
        ),
    ];

    for (case, operation, body, resource) in rows {
        let (status, answer) = emulator
            .call_as(&caller.access_key_id, operation, &body)
            .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the call to be refused, got {status} with {answer:?}"
        );
        let action = format!("states:{operation}");
        let expected = not_authorized(&caller.arn, &action, &resource, &no_grant(&action));
        let stated = message(&answer);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }

    // The same resource under IAM's OTHER clause: a document that denies the action
    // explicitly names the same ARN and only the reason changes.
    emulator
        .put_user_policy(
            "category-reader",
            &json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "states:*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "states:*", "Resource": "*" },
                ]
            }),
        )
        .await;
    let (status, answer) = emulator
        .call_as(&caller.access_key_id, "ListStateMachines", &json!({}))
        .await;
    let expected = not_authorized(
        &caller.arn,
        "states:ListStateMachines",
        &every_resource(STATE_MACHINE_TYPE),
        "with an explicit deny in an identity-based policy",
    );
    assert_eq!(
        (status, message(&answer)),
        (ACCESS_DENIED_STATUS, expected.clone()),
        "case \"an explicit deny\": expected {expected:?}, got {status} with {answer:?}"
    );
}

/// An identity policy allowing `actions` on every resource.
fn allowing(actions: &[&str]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }]
    })
}

/// A state machine with one published version and one alias pointing at it, as
/// `(state machine ARN, version ARN, alias ARN)` — created as the account's own
/// credential, which no identity policy governs.
async fn machine_with_an_alias(emulator: &Emulator, name: &str, alias: &str) -> (String, String, String) {
    let created = emulator
        .call_as(
            ACCOUNT_KEY,
            "CreateStateMachine",
            &json!({
                "name": name,
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "publish": true,
            }),
        )
        .await
        .1;
    let machine = string(&created, "stateMachineArn");
    let version = string(&created, "stateMachineVersionArn");
    let aliased = emulator
        .call_as(
            ACCOUNT_KEY,
            "CreateStateMachineAlias",
            &json!({
                "name": alias,
                "routingConfiguration": [{ "stateMachineVersionArn": version, "weight": 100 }],
            }),
        )
        .await
        .1;
    (machine, version, string(&aliased, "stateMachineAliasArn"))
}

/// RULE 2: a start is decided on the STATE MACHINE's own ARN whichever form named it, and
/// the qualifier it was reached through is a condition key.
///
/// The rows differ along the FORM of the ARN the start names (unqualified, a version ARN,
/// an alias ARN) and along what the grant says about the qualifier: a grant on the state
/// machine alone, one scoped to the alias's qualifier, one scoped to another qualifier,
/// and one scoped to the ABSENCE of a qualifier — which is decidable because the request
/// publishes the key.
#[tokio::test]
async fn a_start_is_decided_on_the_state_machine_and_scoped_by_its_qualifier() {
    let emulator = Emulator::new();
    let alias_name = "PROD";
    let (machine, version, alias) = machine_with_an_alias(&emulator, "qualifier-runner", alias_name).await;
    let caller = emulator.user("qualifier-starter", &allowing(&[])).await;

    let qualifier_key = "states:StateMachineQualifier";
    // (case, the policy in force, the ARN the start names, whether it is served)
    let rows: Vec<(&str, Value, &String, bool)> = vec![
        (
            "a grant on the state machine, started unqualified",
            allowing(&["states:StartExecution"]),
            &machine,
            true,
        ),
        (
            "the same grant, started by the version ARN",
            allowing(&["states:StartExecution"]),
            &version,
            true,
        ),
        (
            "the same grant, started by the alias ARN",
            allowing(&["states:StartExecution"]),
            &alias,
            true,
        ),
        (
            "a grant scoped to the alias's qualifier, started by the alias ARN",
            allowing_under(
                &["states:StartExecution"],
                &[&machine],
                json!({ "StringEquals": { qualifier_key: alias_name } }),
            ),
            &alias,
            true,
        ),
        (
            "the same scoped grant, started by the version ARN",
            allowing_under(
                &["states:StartExecution"],
                &[&machine],
                json!({ "StringEquals": { qualifier_key: alias_name } }),
            ),
            &version,
            false,
        ),
        (
            "a grant scoped to a qualifier the state machine has no reference under",
            allowing_under(
                &["states:StartExecution"],
                &[&machine],
                json!({ "StringEquals": { qualifier_key: "STAGING" } }),
            ),
            &alias,
            false,
        ),
        (
            "a grant scoped to carrying NO qualifier, started unqualified",
            allowing_under(
                &["states:StartExecution"],
                &[&machine],
                json!({ "Null": { qualifier_key: true } }),
            ),
            &machine,
            true,
        ),
        (
            "the same grant, started by the alias ARN",
            allowing_under(
                &["states:StartExecution"],
                &[&machine],
                json!({ "Null": { qualifier_key: true } }),
            ),
            &alias,
            false,
        ),
    ];

    for (index, (case, policy, named, served)) in rows.into_iter().enumerate() {
        emulator.put_user_policy("qualifier-starter", &policy).await;
        let (status, answer) = emulator
            .call_as(
                &caller.access_key_id,
                "StartExecution",
                &json!({ "stateMachineArn": named, "name": format!("run-{index}") }),
            )
            .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the start to be served, got {status} with {answer:?}"
            );
            continue;
        }
        let expected = not_authorized(
            &caller.arn,
            "states:StartExecution",
            // Whichever form named it, the decision — and so the refusal — is about the
            // state machine itself.
            &machine,
            &no_grant("states:StartExecution"),
        );
        assert_eq!(
            (status, message(&answer)),
            (ACCESS_DENIED_STATUS, expected.clone()),
            "case {case:?}: expected {expected:?}, got {status} with {answer:?}"
        );
    }
}

/// RULE 3: a request asks for the permission of what it DOES — tagging what a create
/// creates, and publishing a version — so a caller holding only the operation is refused
/// exactly the requests that ask for one, and such a refusal creates nothing.
///
/// The rows differ along WHICH member asks for the further permission and on which
/// operation: tags on both creates, `publish` on the create and on the update, and the
/// plain request of each, which asks for nothing more.
#[tokio::test]
async fn a_create_asks_for_the_permission_of_the_work_its_members_ask_for() {
    let emulator = Emulator::new();
    let caller = emulator
        .user(
            "implied-creator",
            &allowing(&[
                "states:CreateStateMachine",
                "states:CreateActivity",
                "states:UpdateStateMachine",
                "iam:PassRole",
            ]),
        )
        .await;
    let tags = json!([{ "key": "team", "value": "payments" }]);
    // A state machine for the update rows to repoint, created as the account's credential.
    let existing = string(
        &emulator
            .call_as(
                ACCOUNT_KEY,
                "CreateStateMachine",
                &json!({
                    "name": "implied-update-target",
                    "definition": pass_definition(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await
            .1,
        "stateMachineArn",
    );

    // (case, operation, the request body, the action the refusal must name — `None` for a
    // request that asks for nothing beyond its operation)
    let rows: Vec<(&str, &str, Value, Option<&str>)> = vec![
        (
            "a create that tags what it creates",
            "CreateStateMachine",
            json!({
                "name": "implied-tagged",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": tags,
            }),
            Some("states:TagResource"),
        ),
        (
            "an activity create that tags what it creates",
            "CreateActivity",
            json!({ "name": "implied-tagged-activity", "tags": tags }),
            Some("states:TagResource"),
        ),
        (
            "a create that publishes a version",
            "CreateStateMachine",
            json!({
                "name": "implied-published",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "publish": true,
            }),
            Some("states:PublishStateMachineVersion"),
        ),
        (
            "an update that publishes a version",
            "UpdateStateMachine",
            json!({ "stateMachineArn": existing, "publish": true }),
            Some("states:PublishStateMachineVersion"),
        ),
        (
            "a create carrying an empty tag list, which tags nothing",
            "CreateStateMachine",
            json!({
                "name": "implied-untagged",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": [],
            }),
            None,
        ),
        (
            "an activity create that tags nothing",
            "CreateActivity",
            json!({ "name": "implied-plain-activity" }),
            None,
        ),
    ];

    for (case, operation, body, refused_for) in rows {
        let (status, answer) = emulator
            .call_as(&caller.access_key_id, operation, &body)
            .await;
        let Some(action) = refused_for else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the request to be served, got {status} with {answer:?}"
            );
            continue;
        };
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the request to be refused, got {status} with {answer:?}"
        );
        let stated = message(&answer);
        assert!(
            stated.contains(action) && stated.contains(&no_grant(action)),
            "case {case:?}: expected the refusal to name {action:?}, got {stated:?}"
        );
        // A refused create left nothing behind: the name is still free, which the account's
        // own credential reads back.
        if let Some(name) = body.get("name").and_then(Value::as_str) {
            let arn = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}");
            let (_, described) = emulator
                .call_as(
                    ACCOUNT_KEY,
                    "DescribeStateMachine",
                    &json!({ "stateMachineArn": arn }),
                )
                .await;
            assert_eq!(
                error_code(&described).as_str(),
                "StateMachineDoesNotExist",
                "case {case:?}: a refused create must create nothing, got {described:?}"
            );
        }
    }

    // Granted the implied permissions too, the very same requests are served.
    emulator
        .put_user_policy(
            "implied-creator",
            &allowing(&[
                "states:CreateStateMachine",
                "states:CreateActivity",
                "states:UpdateStateMachine",
                "states:TagResource",
                "states:PublishStateMachineVersion",
                "iam:PassRole",
            ]),
        )
        .await;
    for (case, operation, body) in [
        (
            "a create that tags what it creates",
            "CreateStateMachine",
            json!({
                "name": "implied-tagged",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": tags,
            }),
        ),
        (
            "a create that publishes a version",
            "CreateStateMachine",
            json!({
                "name": "implied-published",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "publish": true,
            }),
        ),
    ] {
        let (status, answer) = emulator
            .call_as(&caller.access_key_id, operation, &body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?} granted: expected the request to be served, got {status} with \
             {answer:?}"
        );
    }
}

/// RULE 4: the tag condition keys are decided from the request and from the store, so a
/// tag-scoped policy of either family decides a call of this service instead of leaving it
/// refused for want of a decision.
///
/// The rows span the three families: `aws:RequestTag/<key>` over the value a create
/// writes, `aws:TagKeys` over the keys a tagging call writes, and `aws:ResourceTag/<key>`
/// over the tags the resource carries — which for a create is the tag set the resource it
/// mints will carry.
#[tokio::test]
async fn a_tag_scoped_policy_decides_a_call_from_the_request_and_the_store() {
    let emulator = Emulator::new();
    let caller = emulator.user("tag-scoped", &allowing(&[])).await;
    // A state machine already carrying one tag, for the resource-tag rows.
    let tagged = string(
        &emulator
            .call_as(
                ACCOUNT_KEY,
                "CreateStateMachine",
                &json!({
                    "name": "tag-scoped-target",
                    "definition": pass_definition(),
                    "roleArn": ROLE_ARN,
                    "tags": [{ "key": "team", "value": "payments" }],
                }),
            )
            .await
            .1,
        "stateMachineArn",
    );

    // (case, the policy in force, operation, the request body, whether it is served)
    let rows: Vec<(&str, Value, &str, Value, bool)> = vec![
        (
            "a create whose request tag the grant names",
            allowing_under(
                &["states:CreateStateMachine", "states:TagResource", "iam:PassRole"],
                &["*"],
                json!({ "StringEquals": { "aws:RequestTag/team": "payments" } }),
            ),
            "CreateStateMachine",
            json!({
                "name": "tag-scoped-allowed",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": [{ "key": "team", "value": "payments" }],
            }),
            true,
        ),
        (
            "a create whose request tag carries another value",
            allowing_under(
                &["states:CreateStateMachine", "states:TagResource", "iam:PassRole"],
                &["*"],
                json!({ "StringEquals": { "aws:RequestTag/team": "payments" } }),
            ),
            "CreateStateMachine",
            json!({
                "name": "tag-scoped-refused",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": [{ "key": "team", "value": "research" }],
            }),
            false,
        ),
        (
            "a tagging call whose keys the grant admits",
            allowing_under(
                &["states:TagResource"],
                &["*"],
                json!({ "ForAllValues:StringEquals": { "aws:TagKeys": ["team", "env"] } }),
            ),
            "TagResource",
            json!({
                "resourceArn": tagged,
                "tags": [{ "key": "env", "value": "prod" }],
            }),
            true,
        ),
        (
            "a tagging call carrying a key outside the grant",
            allowing_under(
                &["states:TagResource"],
                &["*"],
                json!({ "ForAllValues:StringEquals": { "aws:TagKeys": ["team", "env"] } }),
            ),
            "TagResource",
            json!({
                "resourceArn": tagged,
                "tags": [{ "key": "owner", "value": "ops" }],
            }),
            false,
        ),
        (
            "a read scoped to the tag the resource carries",
            allowing_under(
                &["states:DescribeStateMachine"],
                &["*"],
                json!({ "StringEquals": { "aws:ResourceTag/team": "payments" } }),
            ),
            "DescribeStateMachine",
            json!({ "stateMachineArn": tagged }),
            true,
        ),
        (
            "a read scoped to a tag the resource does not carry",
            allowing_under(
                &["states:DescribeStateMachine"],
                &["*"],
                json!({ "StringEquals": { "aws:ResourceTag/team": "research" } }),
            ),
            "DescribeStateMachine",
            json!({ "stateMachineArn": tagged }),
            false,
        ),
        (
            "a create scoped to the tag the resource it mints will carry",
            allowing_under(
                &["states:CreateStateMachine", "states:TagResource", "iam:PassRole"],
                &["*"],
                json!({ "StringEquals": { "aws:ResourceTag/team": "payments" } }),
            ),
            "CreateStateMachine",
            json!({
                "name": "tag-scoped-resource-tag",
                "definition": pass_definition(),
                "roleArn": ROLE_ARN,
                "tags": [{ "key": "team", "value": "payments" }],
            }),
            true,
        ),
    ];

    for (case, policy, operation, body, served) in rows {
        emulator.put_user_policy("tag-scoped", &policy).await;
        let (status, answer) = emulator
            .call_as(&caller.access_key_id, operation, &body)
            .await;
        if served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the call to be served, got {status} with {answer:?}"
            );
            continue;
        }
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the call to be refused, got {status} with {answer:?}"
        );
    }
}

/// RULE 5: an ARN of ANOTHER account names a version or an alias no document of this
/// account can grant over, so the call is refused as a resource the caller may not access
/// — naming the principal alone — rather than as a resource that is absent.
///
/// The rows span the alias and version operations and the two listings of them, plus the
/// read of a foreign-account VERSION ARN; the last row is the same read of an UNQUALIFIED
/// foreign ARN, which names a state machine that is simply not here.
#[tokio::test]
async fn a_foreign_account_reference_is_refused_as_a_resource_the_caller_cannot_access() {
    let emulator = Emulator::new();
    let caller = emulator
        .user("cross-account-reader", &allowing(&["states:*"]))
        .await;
    let foreign = "111122223333";
    let machine = format!("arn:aws:states:{REGION}:{foreign}:stateMachine:elsewhere");
    let alias = format!("{machine}:PROD");
    let version = format!("{machine}:1");

    // (case, operation, the request body, the code the refusal carries)
    let rows: Vec<(&str, &str, Value, &str)> = vec![
        (
            "an alias read of another account",
            "DescribeStateMachineAlias",
            json!({ "stateMachineAliasArn": alias }),
            ACCESS_DENIED,
        ),
        (
            "an alias repoint of another account",
            "UpdateStateMachineAlias",
            json!({ "stateMachineAliasArn": alias }),
            ACCESS_DENIED,
        ),
        (
            "an alias delete of another account",
            "DeleteStateMachineAlias",
            json!({ "stateMachineAliasArn": alias }),
            ACCESS_DENIED,
        ),
        (
            "a version delete of another account",
            "DeleteStateMachineVersion",
            json!({ "stateMachineVersionArn": version }),
            ACCESS_DENIED,
        ),
        (
            "a listing of another account's aliases",
            "ListStateMachineAliases",
            json!({ "stateMachineArn": machine }),
            ACCESS_DENIED,
        ),
        (
            "a listing of another account's versions",
            "ListStateMachineVersions",
            json!({ "stateMachineArn": machine }),
            ACCESS_DENIED,
        ),
        (
            "a read of another account's version ARN",
            "DescribeStateMachine",
            json!({ "stateMachineArn": version }),
            ACCESS_DENIED,
        ),
        (
            // An UNQUALIFIED ARN names a state machine rather than a reference of one, and
            // one of another account is simply not here: that is the operation's own
            // does-not-exist answer.
            "a read of another account's unqualified ARN",
            "DescribeStateMachine",
            json!({ "stateMachineArn": machine }),
            "StateMachineDoesNotExist",
        ),
    ];

    for (case, operation, body, code) in rows {
        let (status, answer) = emulator
            .call_as(&caller.access_key_id, operation, &body)
            .await;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, code),
            "case {case:?}: expected {code}, got {status} with {answer:?}"
        );
        if code != ACCESS_DENIED {
            continue;
        }
        let expected = no_access(&caller.arn);
        let stated = message(&answer);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// An identity policy allowing `grants` and explicitly denying `denies`, both on every
/// resource — so what a row below varies is the ACTION half of the decision and never the
/// resource half.
fn allowing_and_denying(grants: &[&str], denies: &[&str]) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": grants,
        "Resource": ["*"],
    })];
    if !denies.is_empty() {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denies,
            "Resource": ["*"],
        }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// An identity policy allowing `actions` on exactly the resources `resources` names, which
/// is how a grant is scoped to one kind of resource of this service.
fn allowing_on(actions: &[&str], resources: &[&str]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resources }]
    })
}

/// One refusal the definition-only rule covers: what was called, in which Region, what the
/// documents in force say, and the action and resource type the refusal must name.
struct DefinitionOnlyRow {
    case: &'static str,
    operation: &'static str,
    body: Value,
    /// The Region the call is signed for, which is the Region the refusal's ARN carries.
    region: &'static str,
    /// The actions the caller's inline document allows.
    grants: &'static [&'static str],
    /// The actions it denies explicitly, beside those grants.
    denies: &'static [&'static str],
    /// The action the refusal names: the first one the request asks for that the documents
    /// in force do not allow.
    refused: &'static str,
    /// Whether that action is withheld by an explicit deny rather than by no grant at all,
    /// which is the other of IAM's two refusal clauses.
    explicitly_denied: bool,
    /// The resource TYPE the refusal's ARN carries.
    resource_type: &'static str,
}

/// RULE 1, for an API that acts on NO resource of the account: a call that decides only the
/// definition its own request carries is judged against every resource of this service in
/// the Region the request was made for — the resource TYPE wildcarded as well as the name —
/// for EVERY action such a request asks for, while a call of an API that IS about a resource
/// type keeps that type's wildcard.
///
/// The rows differ along what the rule generalises over: the definition-only operation (a
/// state tested, a definition validated), which of the actions the request asks for is the
/// one withheld (the operation's own, and the one the `revealSecrets` capability adds), the
/// Region the call is signed for, which of IAM's two clauses withholds it, and what else the
/// request carries — including a member naming a state machine that this operation does not
/// model, so the resource is decided by the API and never by the shape of the body. The last
/// two rows are the controls: the APIs that ARE about one resource type still name it.
#[tokio::test]
async fn a_definition_only_call_is_judged_against_every_resource_type() {
    let emulator = Emulator::new();
    let foreign_member = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:not-modelled");
    let pass_state = r#"{"Type":"Pass","Result":{"ok":true},"End":true}"#;
    let rows = vec![
        DefinitionOnlyRow {
            case: "a state tested, the operation's own action withheld",
            operation: "TestState",
            body: json!({ "definition": pass_state, "input": "{}" }),
            region: REGION,
            grants: &["kms:Decrypt"],
            denies: &[],
            refused: "states:TestState",
            explicitly_denied: false,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a state tested revealing secrets, the capability's action withheld",
            operation: "TestState",
            body: json!({ "definition": pass_state, "input": "{}", "revealSecrets": true }),
            region: REGION,
            grants: &["states:TestState"],
            denies: &[],
            refused: "states:RevealSecrets",
            explicitly_denied: false,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "the same call signed for another Region",
            operation: "TestState",
            body: json!({ "definition": pass_state, "input": "{}", "revealSecrets": true }),
            region: "eu-central-1",
            grants: &["states:TestState"],
            denies: &[],
            refused: "states:RevealSecrets",
            explicitly_denied: false,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a state tested revealing secrets, the capability denied explicitly",
            operation: "TestState",
            body: json!({
                "definition": pass_state,
                "input": "{}",
                "inspectionLevel": "DEBUG",
                "revealSecrets": true,
            }),
            region: "ap-southeast-2",
            grants: &["states:*"],
            denies: &["states:RevealSecrets"],
            refused: "states:RevealSecrets",
            explicitly_denied: true,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a state tested carrying a member the operation does not model",
            operation: "TestState",
            body: json!({ "definition": pass_state, "stateMachineArn": foreign_member }),
            region: REGION,
            grants: &[],
            denies: &[],
            refused: "states:TestState",
            explicitly_denied: false,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a definition validated",
            operation: "ValidateStateMachineDefinition",
            body: json!({ "definition": HELLO_DEFINITION }),
            region: "eu-central-1",
            grants: &["states:TestState"],
            denies: &[],
            refused: "states:ValidateStateMachineDefinition",
            explicitly_denied: false,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a definition validated at a severity, denied explicitly",
            operation: "ValidateStateMachineDefinition",
            body: json!({
                "definition": HELLO_DEFINITION,
                "severity": "WARNING",
                "maxResults": 1,
            }),
            region: "ap-southeast-2",
            grants: &["states:*"],
            denies: &["states:ValidateStateMachineDefinition"],
            refused: "states:ValidateStateMachineDefinition",
            explicitly_denied: true,
            resource_type: EVERY_TYPE,
        },
        DefinitionOnlyRow {
            case: "a listing of state machines, an API about one resource type",
            operation: "ListStateMachines",
            body: json!({}),
            region: "eu-central-1",
            grants: &[],
            denies: &[],
            refused: "states:ListStateMachines",
            explicitly_denied: false,
            resource_type: STATE_MACHINE_TYPE,
        },
        DefinitionOnlyRow {
            case: "a listing of activities, an API about another resource type",
            operation: "ListActivities",
            body: json!({}),
            region: "ap-southeast-2",
            grants: &[],
            denies: &[],
            refused: "states:ListActivities",
            explicitly_denied: false,
            resource_type: ACTIVITY_TYPE,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let caller = emulator
            .user(
                &format!("definition-only-{index}"),
                &allowing_and_denying(row.grants, row.denies),
            )
            .await;
        let (status, answer) = emulator
            .call_as_in_region(&caller.access_key_id, row.region, row.operation, &row.body)
            .await;
        let case = row.case;
        assert_eq!(
            (status, error_code(&answer).as_str()),
            (ACCESS_DENIED_STATUS, ACCESS_DENIED),
            "case {case:?}: expected the call to be refused, got {status} with {answer:?}"
        );
        let reason = if row.explicitly_denied {
            "with an explicit deny in an identity-based policy".to_string()
        } else {
            no_grant(row.refused)
        };
        let expected = not_authorized(
            &caller.arn,
            row.refused,
            &every_resource_in(row.region, row.resource_type),
            &reason,
        );
        let stated = message(&answer);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// The same rule read from the GRANT's side: because a definition-only call is judged
/// against the both-wildcards ARN, a statement scoped to one resource TYPE of this service
/// does not cover it, while one scoped to every type — or to every resource — does; and a
/// call of an API about a resource type is covered by its own type's wildcard.
///
/// The rows differ along the `Resource` element the grant is written at (one resource type,
/// another type, every type of this service, every resource at all) and along the call it is
/// asked about (a state tested, the same test asking for the reveal capability too, a
/// definition validated, and a listing as the control), with each served call read back
/// through what it answers.
#[tokio::test]
async fn a_grant_scoped_to_one_resource_type_does_not_cover_a_definition_only_call() {
    let emulator = Emulator::new();
    let pass_state = r#"{"Type":"Pass","Result":{"ok":true},"End":true}"#;
    // (case, the `Resource` element the grant of `states:*` is written at)
    let scopes: Vec<(&str, String)> = vec![
        (
            "a grant scoped to the state machines of the Region",
            every_resource(STATE_MACHINE_TYPE),
        ),
        (
            "a grant scoped to the activities of the Region",
            every_resource(ACTIVITY_TYPE),
        ),
        (
            "a grant scoped to every resource type of this service",
            every_resource(EVERY_TYPE),
        ),
        ("a grant scoped to every resource", "*".to_string()),
    ];
    // (case, operation, the request body, the member a served call answers and its value —
    // read back so a served row fails for a reason other than authorization too)
    let calls: Vec<(&str, &str, Value, &str, Value)> = vec![
        (
            "a state tested",
            "TestState",
            json!({ "definition": pass_state, "input": "{}" }),
            "status",
            json!("SUCCEEDED"),
        ),
        (
            "a state tested revealing secrets",
            "TestState",
            json!({ "definition": pass_state, "input": "{}", "revealSecrets": true }),
            "status",
            json!("SUCCEEDED"),
        ),
        (
            "a definition validated",
            "ValidateStateMachineDefinition",
            json!({ "definition": HELLO_DEFINITION }),
            "result",
            json!("OK"),
        ),
        (
            "a listing of state machines",
            "ListStateMachines",
            json!({}),
            "stateMachines",
            json!([]),
        ),
    ];

    for (index, (scope_case, scope)) in scopes.iter().enumerate() {
        let caller = emulator
            .user(
                &format!("scoped-grant-{index}"),
                &allowing_on(&["states:*"], &[scope.as_str()]),
            )
            .await;
        for (call_case, operation, body, member, answered) in &calls {
            // A statement covers the call exactly when its `Resource` element covers the
            // ARN the call is judged against: the both-wildcards ARN for the two
            // definition-only APIs, and the state machines of the Region for the listing.
            let judged = if *operation == "ListStateMachines" {
                every_resource(STATE_MACHINE_TYPE)
            } else {
                every_resource(EVERY_TYPE)
            };
            let covered = scope == "*" || *scope == every_resource(EVERY_TYPE) || *scope == judged;
            let (status, answer) = emulator
                .call_as(&caller.access_key_id, operation, body)
                .await;
            if covered {
                assert_eq!(
                    status, 200,
                    "case {scope_case:?} / {call_case:?}: expected the call to be served, got \
                     {status} with {answer:?}"
                );
                assert_eq!(
                    answer.get(*member),
                    Some(answered),
                    "case {scope_case:?} / {call_case:?}: expected the served call to answer \
                     {member}={answered:?}, got {answer:?}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {scope_case:?} / {call_case:?}: expected the call to be refused, got \
                     {status} with {answer:?}"
                );
                let stated = message(&answer);
                assert!(
                    stated.contains(&format!("on resource: {judged} ")),
                    "case {scope_case:?} / {call_case:?}: expected the refusal to name \
                     {judged:?}, got {stated:?}"
                );
            }
        }
    }
}
