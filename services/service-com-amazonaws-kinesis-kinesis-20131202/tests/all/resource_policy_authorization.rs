// SPDX-License-Identifier: CC0-1.0

//! What the RESOURCE-BASED policy a Kinesis resource carries does to a request, driven end to
//! end over the real wire path (an awsJson1_1 request through the multi-service router into
//! the mounted service), so what is asserted is what a client holding those credentials sees.
//!
//! THE RULE UNDER TEST: the policy `PutResourcePolicy` attached to the resource a request
//! ADDRESSES is evaluated beside the identity-based documents in force on the principal that
//! signed it. Within one account an `Allow` from EITHER side grants the call, and an explicit
//! `Deny` from either refuses it whatever the other says; a refused call is answered HTTP 400
//! `AccessDeniedException` whose sentence names the principal, the action, the resource the
//! request addressed, and WHICH document withheld the grant — `with an explicit deny in a
//! resource-based policy` for a `Deny` in the resource's own policy, `with an explicit deny in
//! an identity-based policy` for one in the principal's. A statement reaches the caller by
//! every spelling of the principal its `Principal` element may name it by, and detaching the
//! document lifts what it withheld.
//!
//! The recorded flow this was derived from attaches ONE `Deny` of `kinesis:GetRecords` naming
//! one assumed role to one stream and reads as that role — one row of the tables below. The
//! rows differ along what the rule generalises over: WHICH document denies (the resource's,
//! the principal's, neither), WHICH actions the statement covers (exactly the one asked for,
//! the whole namespace, another action), WHOM it names (this caller by each of its spellings,
//! another account's principal), the OPERATION called (a write, two reads of the shard, a
//! stream read), and whether the document is attached at all.
//!
//! The wire harness (Kinesis, IAM and STS over ONE engine), the seeded role, its session and
//! the stream builders are [`crate::request_authorization`]', and the policy calls are
//! [`crate::resource_policies`]'-shaped bodies issued through that harness — reused rather
//! than copied.

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::stream_arn;

use crate::request_authorization::{
    allow_on, given_role_policy, given_session, given_stream, one_shard, role_arn, session_arn,
    Emulator, ACCESS_DENIED_EXCEPTION, ACCESS_DENIED_STATUS, ACCOUNT, ACCOUNT_KEY_ID,
    SESSION_KEY_ID,
};
use crate::stream_tags::{error_code, error_message, REGION};

/// WHICH document decided the call, and so which clause the refusal carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The call reaches the operation.
    Served,
    /// An explicit `Deny` in the RESOURCE's own policy covers it.
    ResourceDeny,
    /// An explicit `Deny` in one of the PRINCIPAL's own documents covers it.
    IdentityDeny,
    /// No readable statement allows it: IAM's default deny.
    DefaultDeny,
}

/// The sentence IAM words a refusal with, naming the document that withheld the grant.
fn refusal(principal: &str, operation: &str, resource: &str, expect: Expect) -> String {
    let action = format!("kinesis:{operation}");
    let reason = match expect {
        Expect::ResourceDeny => "with an explicit deny in a resource-based policy".to_string(),
        Expect::IdentityDeny => "with an explicit deny in an identity-based policy".to_string(),
        _ => format!("because no identity-based policy allows the {action} action"),
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} \
         {reason}"
    )
}

/// A resource-based policy document: one statement with `effect` on `actions`, naming
/// `principal`, scoped to `arn` — the shape this member documents ("It must include the
/// identity of the principal and the actions allowed on this resource",
/// `PutResourcePolicyInput$Policy`).
fn statement(effect: &str, principal: Value, actions: Value, arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "ResourcePolicyCase",
            "Effect": effect,
            "Principal": principal,
            "Action": actions,
            "Resource": arn,
        }],
    })
    .to_string()
}

/// Attach `policy` to `arn`, as the account's own published credential — which this gate does
/// not bound, so what the cases exercise is the policy and not who attached it.
async fn attach(emulator: &Emulator, case: &str, arn: &str, policy: &str) {
    emulator
        .ok(
            case,
            "PutResourcePolicy",
            &json!({ "ResourceARN": arn, "Policy": policy }),
        )
        .await;
}

/// Detach whatever policy `arn` carries, as the account's own published credential.
async fn detach(emulator: &Emulator, case: &str, arn: &str) {
    emulator
        .ok(case, "DeleteResourcePolicy", &json!({ "ResourceARN": arn }))
        .await;
}

/// A fresh iterator over the stream's one shard, read as the account's own credential: a
/// `GetRecords` names its stream only inside the iterator it was handed, which is where the
/// resource that call is decided against comes from.
async fn fresh_iterator(emulator: &Emulator, case: &str, name: &str, shard_id: &str) -> String {
    let handed = emulator
        .ok(
            case,
            "GetShardIterator",
            &json!({
                "StreamName": name,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        )
        .await;
    handed["ShardIterator"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a ShardIterator, got {handed}"))
        .to_string()
}

/// The calls each row is checked over: a write, the two reads of the shard's position — one of
/// them addressing its stream only through the iterator it carries — and a stream read.
async fn calls(
    emulator: &Emulator,
    case: &str,
    name: &str,
    shard_id: &str,
) -> Vec<(&'static str, Value)> {
    let iterator = fresh_iterator(emulator, case, name, shard_id).await;
    vec![
        (
            "PutRecord",
            json!({ "StreamName": name, "Data": "eA==", "PartitionKey": "pk1" }),
        ),
        (
            "GetShardIterator",
            json!({
                "StreamName": name,
                "ShardId": shard_id,
                "ShardIteratorType": "TRIM_HORIZON",
            }),
        ),
        ("GetRecords", json!({ "ShardIterator": iterator })),
        ("DescribeStreamSummary", json!({ "StreamName": name })),
    ]
}

/// Assert what the session's `operation` answers, given what the documents in force say.
async fn assert_outcome(
    emulator: &Emulator,
    case: &str,
    operation: &str,
    body: &Value,
    resource: &str,
    expect: Expect,
) {
    let (status, response) = emulator.call_as(SESSION_KEY_ID, operation, body).await;
    if expect == Expect::Served {
        assert_eq!(
            status, 200,
            "case {case:?} on {operation}: expected the call to be served, got {status} with \
             {response}"
        );
        return;
    }
    assert_eq!(
        status, ACCESS_DENIED_STATUS,
        "case {case:?} on {operation}: expected HTTP {ACCESS_DENIED_STATUS}, got {status} with \
         {response}"
    );
    assert_eq!(
        error_code(&response),
        ACCESS_DENIED_EXCEPTION,
        "case {case:?} on {operation}: expected {ACCESS_DENIED_EXCEPTION}, got {response}"
    );
    assert_eq!(
        error_message(&response),
        refusal(&session_arn(), operation, resource, expect),
        "case {case:?} on {operation}: expected IAM's wording for that document, got {response}"
    );
}

/// RULE: a `Deny` in the resource's own policy refuses the call whatever the principal's own
/// documents allow, and the refusal names THAT document; a `Deny` in the principal's own
/// documents names the identity-based one; and an `Allow` in the resource's policy grants on
/// its own, within one account.
#[tokio::test]
async fn which_document_denied_the_call_is_what_the_refusal_names() {
    let case = "which document denied the call";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "resource-policy-decisions";
    let arn = given_stream(&emulator, case, name).await;
    let shard_id = one_shard(&emulator, case, name).await;
    let everything = allow_on(json!("kinesis:*"), json!("*"));
    // An identity document that allows everything of this service beside an explicit deny of
    // it: the shape a scoped-down role carries.
    let identity_deny = json!({
        "Version": "2012-10-17",
        "Statement": [
            { "Effect": "Allow", "Action": "kinesis:*", "Resource": "*" },
            { "Effect": "Deny", "Action": "kinesis:*", "Resource": "*" },
        ]
    });
    let role = json!(role_arn());
    let elsewhere = json!("arn:aws:iam::111122223333:root");

    // (case, the resource policy attached — `None` for none at all, the identity document in
    // force, what each call answers, and the one operation that answers differently)
    #[allow(clippy::type_complexity)]
    let rows: Vec<(&str, Option<String>, Option<Value>, Expect, Option<&str>)> = vec![
        (
            "no document on either side",
            None,
            None,
            Expect::DefaultDeny,
            None,
        ),
        (
            "the resource policy denies this one action, the identity allows everything",
            Some(statement(
                "Deny",
                role.clone(),
                json!("kinesis:GetRecords"),
                &arn,
            )),
            Some(everything.clone()),
            Expect::Served,
            Some("GetRecords"),
        ),
        (
            "the resource policy denies the whole namespace to this caller",
            Some(statement("Deny", role.clone(), json!("kinesis:*"), &arn)),
            Some(everything.clone()),
            Expect::ResourceDeny,
            None,
        ),
        (
            "the resource policy denies the whole namespace to ANOTHER account's principal",
            Some(statement("Deny", elsewhere.clone(), json!("kinesis:*"), &arn)),
            Some(everything.clone()),
            Expect::Served,
            None,
        ),
        (
            "the resource policy allows the whole namespace, the role carries no document",
            Some(statement("Allow", role.clone(), json!("kinesis:*"), &arn)),
            None,
            Expect::Served,
            None,
        ),
        (
            "the identity document denies, the resource policy says nothing",
            None,
            Some(identity_deny.clone()),
            Expect::IdentityDeny,
            None,
        ),
    ];

    for (row, policy, document, expect, exception) in &rows {
        given_role_policy(&emulator.engine, document.as_ref());
        match policy {
            Some(policy) => attach(&emulator, row, &arn, policy).await,
            None => {
                // Whatever the previous row attached is taken off, so each row starts from the
                // document it names and nothing else.
                let (status, _) = emulator
                    .call_as(
                        ACCOUNT_KEY_ID,
                        "DeleteResourcePolicy",
                        &json!({ "ResourceARN": arn }),
                    )
                    .await;
                assert!(
                    status == 200 || status == ACCESS_DENIED_STATUS || status == 400,
                    "case {row:?}: expected the detach to be served or to find nothing, got \
                     {status}"
                );
            }
        }
        for (operation, body) in calls(&emulator, row, name, &shard_id).await {
            // The row's one exceptional operation — the action a narrow `Deny` covers — is
            // refused on the resource policy; every other call of the row is served.
            let expect = match exception {
                Some(denied) if *denied == operation => Expect::ResourceDeny,
                _ => *expect,
            };
            assert_outcome(&emulator, row, operation, &body, &arn, expect).await;
        }
    }
}

/// RULE: a resource statement reaches the caller by every spelling its `Principal` element may
/// name it by — the session ARN STS reports, the ARN of the role behind the session, the
/// account's root ARN, its bare account id, and the wildcard that names everyone — and by no
/// spelling that names someone else.
#[tokio::test]
async fn the_statement_reaches_the_caller_by_every_spelling_of_its_principal() {
    let case = "the spelling the statement names the caller by";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "resource-policy-principals";
    let arn = given_stream(&emulator, case, name).await;
    let shard_id = one_shard(&emulator, case, name).await;
    // The role may do everything, so what refuses a call is only the resource policy.
    given_role_policy(
        &emulator.engine,
        Some(&allow_on(json!("kinesis:*"), json!("*"))),
    );

    // (case, the `Principal` element, whether it names this caller)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("the session ARN", json!(session_arn()), true),
        ("the ARN of the role behind the session", json!(role_arn()), true),
        (
            "the account's root ARN",
            json!(format!("arn:aws:iam::{ACCOUNT}:root")),
            true,
        ),
        ("the account's bare id", json!(ACCOUNT), true),
        ("the wildcard naming everyone", json!("*"), true),
        (
            "a list naming the role beside another account's principal",
            json!([role_arn(), "arn:aws:iam::111122223333:root"]),
            true,
        ),
        (
            "another account's root ARN",
            json!("arn:aws:iam::111122223333:root"),
            false,
        ),
        (
            "another role of this account",
            json!(format!("arn:aws:iam::{ACCOUNT}:role/kinesis-reader")),
            false,
        ),
    ];

    for (row, principal, names_caller) in &rows {
        // Written as the `{"AWS": …}` object a `Principal` naming an account principal is
        // written as, which is the form the recorded document uses.
        let policy = statement(
            "Deny",
            json!({ "AWS": principal }),
            json!("kinesis:GetRecords"),
            &arn,
        );
        attach(&emulator, row, &arn, &policy).await;
        let iterator = fresh_iterator(&emulator, row, name, &shard_id).await;
        assert_outcome(
            &emulator,
            row,
            "GetRecords",
            &json!({ "ShardIterator": iterator }),
            &arn,
            if *names_caller {
                Expect::ResourceDeny
            } else {
                Expect::Served
            },
        )
        .await;
    }
}

/// RULE: the decision is taken on the document the resource carries AT THE TIME of the call, so
/// attaching a `Deny` withholds the action from then on, detaching it lifts the refusal, and
/// replacing the document with one that denies another action lifts it too.
#[tokio::test]
async fn the_access_a_document_withholds_comes_back_when_it_is_detached() {
    let case = "attaching and detaching the document";
    let emulator = Emulator::new();
    given_session(&emulator.engine);
    let name = "resource-policy-lifecycle";
    let arn = given_stream(&emulator, case, name).await;
    let shard_id = one_shard(&emulator, case, name).await;
    given_role_policy(
        &emulator.engine,
        Some(&allow_on(json!("kinesis:*"), json!("*"))),
    );
    let denies_the_read = statement(
        "Deny",
        json!({ "AWS": role_arn() }),
        json!("kinesis:GetRecords"),
        &arn,
    );
    let denies_another_action = statement(
        "Deny",
        json!({ "AWS": role_arn() }),
        json!("kinesis:DeleteStream"),
        &arn,
    );

    // Each step is the document the resource carries when the read is attempted, and what the
    // read then answers.
    let mut step = 0;
    for (row, document, expect) in [
        ("before anything is attached", None, Expect::Served),
        (
            "the deny attached",
            Some(denies_the_read.clone()),
            Expect::ResourceDeny,
        ),
        (
            "the deny replaced by one of another action",
            Some(denies_another_action.clone()),
            Expect::Served,
        ),
        (
            "the deny attached again",
            Some(denies_the_read.clone()),
            Expect::ResourceDeny,
        ),
        ("the document detached", None, Expect::Served),
    ] {
        step += 1;
        let row = format!("step {step}: {row}");
        match document {
            Some(document) => attach(&emulator, &row, &arn, &document).await,
            None if step > 1 => detach(&emulator, &row, &arn).await,
            None => {}
        }
        let iterator = fresh_iterator(&emulator, &row, name, &shard_id).await;
        assert_outcome(
            &emulator,
            &row,
            "GetRecords",
            &json!({ "ShardIterator": iterator }),
            &arn,
            expect,
        )
        .await;
    }

    // The policy decides the resource the request ADDRESSES: another stream of the same
    // account and Region carries no document, so the same read of it is served while the
    // first stream's read is refused.
    let other_name = "resource-policy-lifecycle-other";
    let other = given_stream(&emulator, case, other_name).await;
    let other_shard = one_shard(&emulator, case, other_name).await;
    attach(&emulator, case, &arn, &denies_the_read).await;
    let iterator = fresh_iterator(&emulator, case, other_name, &other_shard).await;
    assert_outcome(
        &emulator,
        "another stream of this account and Region",
        "GetRecords",
        &json!({ "ShardIterator": iterator }),
        &other,
        Expect::Served,
    )
    .await;
    assert_eq!(
        other,
        stream_arn(REGION, other_name),
        "the other stream's ARN must be this namespace's form, got {other}"
    );
    let iterator = fresh_iterator(&emulator, case, name, &shard_id).await;
    assert_outcome(
        &emulator,
        "the stream the document is attached to",
        "GetRecords",
        &json!({ "ShardIterator": iterator }),
        &arn,
        Expect::ResourceDeny,
    )
    .await;
}
