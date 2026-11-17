// SPDX-License-Identifier: CC0-1.0

//! The rule `PutPolicy` judges a SUBMITTED DOCUMENT by, over the whole document rather
//! than over one statement of it.
//!
//! "If this policy does not contain all required statements or if it includes any statement
//! that is not allowed, the PutPolicy action returns an InvalidPolicyException"
//! (`PutPolicyRequest$Policy` docs). Two consequences are under test here, each over a
//! table:
//!
//! 1. EVERY statement of the document is judged, so a document is refused when ANY one of
//!    its statements grants an action a certificate authority cannot be shared for, or
//!    names an identity that is not here, or is about another resource — wherever that
//!    statement sits in the document and whether `Statement` carries one statement object
//!    or a list of them. The rows differ along statement count and the offending
//!    statement's position; the accepted rows are documents every statement of which is
//!    attachable.
//! 2. The document is judged BEFORE anything is written, so a refused document leaves the
//!    policy the CA already carried exactly as it was — which `GetPolicy` is what reports.
//!    The rows span each kind of refusal: the grammar, the action, the principal and the
//!    resource.
//! 3. The refusal NAMES the one rule the document broke, so two documents breaking
//!    different rules cannot carry the same message: a document nothing can read as a
//!    policy is reported as the submitted text itself, byte for byte, while a document that
//!    IS a policy and says something a private CA's policy cannot say is named by its rule
//!    and is not echoed. The rows span the whole unreadable class (not JSON, a JSON value
//!    that is not a policy, no usable `Statement`, a statement that is not one) along the
//!    bytes they carry, beside one row per rule a readable policy can break.
//!
//! The per-statement catalogs themselves (which actions and which principal forms resolve)
//! are tabled in `resource_policy_rules.rs`; this suite is about the document the
//! statements sit in. Every call goes over the wire through the harness in
//! [`super::private_ca_lifecycle`].

use serde_json::{json, Value};
use service_com_amazonaws_acmpca_acmprivateca::state::{
    different_resource_message, DISALLOWED_POLICY_ACTION_MESSAGE, POLICY_ALLOWED_ACTIONS,
    POLICY_NOT_FOUND_MESSAGE, POLICY_PRINCIPAL_NOT_FOUND_MESSAGE,
};

use crate::private_ca_lifecycle::{root_ca_request, string, Emulator, ACCOUNT, REGION};
use crate::resource_policy::{error_message, INVALID_POLICY_PREFIX};

/// A private CA created over the wire, named after `label`.
///
/// No wait for the clock: the policy operations are not gated on a CA's lifecycle status,
/// so each row below is indifferent to which status the authority has reached.
async fn certificate_authority(emulator: &Emulator, label: &str) -> String {
    let created = emulator
        .ok(label, "CreateCertificateAuthority", &root_ca_request(label))
        .await;
    string(&created, "CertificateAuthorityArn", label)
}

/// A refusal row whose `submitted` document cannot be read as a policy at all, so the
/// refusal names no statement's rule: it reports the submitted text itself.
fn unreadable(case: &'static str, submitted: String) -> (&'static str, String, String) {
    let expected = format!("{INVALID_POLICY_PREFIX}{submitted}");
    (case, submitted, expected)
}

/// A policy document carrying `statement` as its `Statement` element — one statement
/// object, or a list of them, exactly as submitted.
fn document(statement: Value) -> String {
    json!({"Version": "2012-10-17", "Statement": statement}).to_string()
}

/// One statement of a policy document: it grants `action` to `principal` on `resource`.
fn statement(sid: &str, principal: Value, action: Value, resource: Value) -> Value {
    json!({
        "Sid": sid,
        "Effect": "Allow",
        "Principal": principal,
        "Action": action,
        "Resource": resource
    })
}

/// A statement this service attaches: an allow-listed action granted to the one account
/// this emulator presents, on the CA the policy hangs on.
fn attachable_statement(sid: &str, ca_arn: &str) -> Value {
    statement(
        sid,
        json!({"AWS": ACCOUNT}),
        json!(format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0])),
        json!(ca_arn),
    )
}

/// A statement granting a management-plane action a certificate authority's policy cannot
/// grant.
fn disallowed_action_statement(sid: &str, ca_arn: &str) -> Value {
    statement(
        sid,
        json!({"AWS": ACCOUNT}),
        json!("acm-pca:DeleteCertificateAuthority"),
        json!(ca_arn),
    )
}

/// A statement granting to an account id this emulator does not present.
fn absent_principal_statement(sid: &str, ca_arn: &str) -> Value {
    statement(
        sid,
        json!({"AWS": "999999999999"}),
        json!(format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0])),
        json!(ca_arn),
    )
}

/// A statement about a certificate authority other than the one the policy hangs on.
fn other_resource_statement(sid: &str) -> Value {
    statement(
        sid,
        json!({"AWS": ACCOUNT}),
        json!(format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0])),
        json!(other_resource()),
    )
}

/// The resource such a statement names: a certificate authority of this service that is
/// not the one the policy is attached to, which is what the refusal echoes back.
fn other_resource() -> String {
    format!(
        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
         11111111-1111-1111-1111-111111111111"
    )
}

/// The document `ca_arn` carries, as `GetPolicy` displays it, or `None` when it carries
/// none — the read reports an absent policy with the POLICY's own sentence.
async fn carried_policy(emulator: &Emulator, case: &str, ca_arn: &str) -> Option<String> {
    let (status, body) = emulator
        .call("GetPolicy", &json!({"ResourceArn": ca_arn}))
        .await;
    match status {
        200 => Some(string(&body, "Policy", case)),
        400 => {
            let message = error_message(&body, case);
            assert_eq!(
                message, POLICY_NOT_FOUND_MESSAGE,
                "case {case:?}: expected GetPolicy to report the policy as the absent \
                 resource, got {message:?}"
            );
            None
        }
        other => panic!("case {case:?}: GetPolicy answered {other} {body:?}"),
    }
}

/// Rule 1: whichever statement of the document is not one this service attaches, and
/// wherever it sits, the whole document is refused and nothing is attached; a document
/// every statement of which is attachable is accepted and read back.
#[tokio::test]
async fn put_policy_judges_every_statement_of_the_document() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "statement-set").await;

    // The rows differ along statement count, the form `Statement` takes (one object versus
    // a list) and WHERE the offending statement sits — first, middle, last — so no row's
    // outcome can come from reading only the document's first statement.
    let refused: Vec<(&str, Value, String)> = vec![
        (
            "one statement object granting a disallowed action",
            disallowed_action_statement("only", &arn),
            DISALLOWED_POLICY_ACTION_MESSAGE.to_string(),
        ),
        (
            "one statement object naming an account that is not here",
            absent_principal_statement("only", &arn),
            POLICY_PRINCIPAL_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "the first of two statements granting a disallowed action",
            json!([
                disallowed_action_statement("first", &arn),
                attachable_statement("second", &arn)
            ]),
            DISALLOWED_POLICY_ACTION_MESSAGE.to_string(),
        ),
        (
            "the second of two statements granting a disallowed action",
            json!([
                attachable_statement("first", &arn),
                disallowed_action_statement("second", &arn)
            ]),
            DISALLOWED_POLICY_ACTION_MESSAGE.to_string(),
        ),
        (
            "the middle of three statements naming an account that is not here",
            json!([
                attachable_statement("first", &arn),
                absent_principal_statement("second", &arn),
                attachable_statement("third", &arn)
            ]),
            POLICY_PRINCIPAL_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "the last of four statements naming an account that is not here",
            json!([
                attachable_statement("first", &arn),
                attachable_statement("second", &arn),
                attachable_statement("third", &arn),
                absent_principal_statement("fourth", &arn)
            ]),
            POLICY_PRINCIPAL_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "the last of three statements about another authority",
            json!([
                attachable_statement("first", &arn),
                attachable_statement("second", &arn),
                other_resource_statement("third")
            ]),
            different_resource_message(&arn, &json!(other_resource())),
        ),
    ];

    for (case, statements, expected) in &refused {
        let submitted = document(statements.clone());
        let body = emulator
            .refused(
                case,
                "PutPolicy",
                &json!({"ResourceArn": arn, "Policy": submitted}),
                "InvalidPolicyException",
            )
            .await;
        let message = error_message(&body, case);
        assert_eq!(
            &message, expected,
            "case {case:?}: expected the refusal the offending statement earns, got \
             {message:?}"
        );
        let carried = carried_policy(&emulator, case, &arn).await;
        assert!(
            carried.is_none(),
            "case {case:?}: expected the refused document to attach nothing, got \
             {carried:?}"
        );
    }

    // Documents every statement of which is attachable: one statement object, several
    // statements, and a statement naming the CA it hangs on in a list of resources.
    let accepted: Vec<(&str, Value)> = vec![
        ("one statement object", attachable_statement("only", &arn)),
        (
            "one statement in a list",
            json!([attachable_statement("only", &arn)]),
        ),
        (
            "four statements, each of them attachable",
            json!([
                attachable_statement("first", &arn),
                attachable_statement("second", &arn),
                attachable_statement("third", &arn),
                attachable_statement("fourth", &arn)
            ]),
        ),
        (
            "a statement whose Resource is a list naming the CA",
            statement(
                "resource-list",
                json!({"AWS": ACCOUNT}),
                json!(format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0])),
                json!([arn, arn]),
            ),
        ),
    ];

    for (case, statements) in &accepted {
        let submitted = document(statements.clone());
        emulator
            .ok(
                case,
                "PutPolicy",
                &json!({"ResourceArn": arn, "Policy": submitted}),
            )
            .await;
        let carried = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            carried.as_deref(),
            Some(submitted.as_str()),
            "case {case:?}: expected the attached document back, got {carried:?}"
        );
        // Cleared again, so the next row is observably its own attach rather than the one
        // before it.
        emulator
            .ok(case, "DeletePolicy", &json!({"ResourceArn": arn}))
            .await;
    }
}

/// Rule 2: the document is judged before any state is written, so each refusal leaves the
/// policy the CA already carried exactly as it was — and the delete that follows removes
/// that one policy.
#[tokio::test]
async fn a_refused_document_leaves_the_carried_policy_untouched() {
    let emulator = Emulator::new();
    let case = "a CA already carrying a policy";
    let arn = certificate_authority(&emulator, "refusal-leaves-policy").await;

    let carried = document(json!([
        attachable_statement("first", &arn),
        statement(
            "second",
            json!({"Service": "acm.amazonaws.com"}),
            json!(format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[1])),
            json!(arn),
        )
    ]));
    emulator
        .ok(
            case,
            "PutPolicy",
            &json!({"ResourceArn": arn, "Policy": carried}),
        )
        .await;
    assert_eq!(
        carried_policy(&emulator, case, &arn).await.as_deref(),
        Some(carried.as_str()),
        "case {case:?}: expected the CA to carry the document it was given"
    );

    // One row per kind of refusal: the document is not a policy at all, its statements are
    // not statements, it grants an action a CA cannot be shared for, it names an identity
    // that is not here, and it is about another authority.
    let refused: Vec<(&str, String, String)> = vec![
        // The rows whose document cannot be read as a policy at all are reported as the
        // document itself, so each one's expectation is derived from what it submitted.
        unreadable("a document that is not JSON", "not a policy at all".to_string()),
        unreadable(
            "a document carrying no statement",
            json!({"Version": "2012-10-17"}).to_string(),
        ),
        unreadable(
            "a document whose statement list is empty",
            document(json!([])),
        ),
        unreadable(
            "a statement with no Effect",
            document(json!({
                "Sid": "no-effect",
                "Principal": {"AWS": ACCOUNT},
                "Action": format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0]),
                "Resource": arn
            })),
        ),
        (
            "a statement granting a disallowed action",
            document(disallowed_action_statement("disallowed", &arn)),
            DISALLOWED_POLICY_ACTION_MESSAGE.to_string(),
        ),
        (
            "a statement naming an account that is not here",
            document(absent_principal_statement("absent", &arn)),
            POLICY_PRINCIPAL_NOT_FOUND_MESSAGE.to_string(),
        ),
        (
            "a statement about another authority",
            document(other_resource_statement("elsewhere")),
            different_resource_message(&arn, &json!(other_resource())),
        ),
    ];

    for (row, submitted, expected) in &refused {
        let body = emulator
            .refused(
                row,
                "PutPolicy",
                &json!({"ResourceArn": arn, "Policy": submitted}),
                "InvalidPolicyException",
            )
            .await;
        let message = error_message(&body, row);
        assert_eq!(
            &message, expected,
            "case {row:?}: expected the refusal that kind of document earns, got {message:?}"
        );
        let still = carried_policy(&emulator, row, &arn).await;
        assert_eq!(
            still.as_deref(),
            Some(carried.as_str()),
            "case {row:?}: expected the refusal to leave the carried policy as it was, got \
             {still:?}"
        );
    }

    // The policy the refusals left alone is the one the delete removes.
    emulator
        .ok(case, "DeletePolicy", &json!({"ResourceArn": arn}))
        .await;
    let gone = carried_policy(&emulator, case, &arn).await;
    assert!(
        gone.is_none(),
        "case {case:?}: expected the delete to remove the carried policy, got {gone:?}"
    );
}

/// What a document that cannot be READ as a policy at all is, spanning the whole class the
/// grammar rejects, each row returned with the text as it is SUBMITTED.
///
/// The rows differ along the form of the failure (not JSON at all, a JSON value that is not
/// a policy object, a policy object with no usable `Statement`, a statement that is not a
/// statement) and along the BYTES the document carries — whitespace style, character class
/// up to the last code point `AWSPolicy`'s pattern admits (U+00FF, from
/// `smithy_query(query="operation",operation="PutPolicy")`) and length — because what is
/// echoed has to be the submitted text rather than a re-rendering of it.
fn unreadable_documents(ca_arn: &str) -> Vec<(&'static str, String)> {
    vec![
        ("a body that is not JSON", "this-is-not-json".to_string()),
        ("a JSON scalar", "\"Allow\"".to_string()),
        ("a JSON number", "42".to_string()),
        (
            "a JSON array of policy objects",
            json!([{"Statement": [{"Effect": "Allow", "Action": "acm-pca:GetCertificate"}]}])
                .to_string(),
        ),
        (
            "a policy object with no Statement",
            json!({"Version": "2012-10-17"}).to_string(),
        ),
        (
            "a policy object whose statement list is empty",
            json!({"Version": "2012-10-17", "Statement": []}).to_string(),
        ),
        (
            "a statement list of strings",
            json!({"Version": "2012-10-17", "Statement": ["Allow"]}).to_string(),
        ),
        (
            "a statement with no Action",
            document(json!({"Effect": "Allow", "Principal": {"AWS": ACCOUNT},
                            "Resource": ca_arn})),
        ),
        (
            "a statement with an Effect the policy language does not define",
            document(json!({"Effect": "Permit", "Principal": {"AWS": ACCOUNT},
                            "Action": format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0]),
                            "Resource": ca_arn})),
        ),
        (
            // Tabs and newlines are inside `AWSPolicy`'s character class, and an echo of
            // the submitted text keeps them where the caller put them.
            "a document laid out over several lines",
            "{\n\t\"Version\" : \"2012-10-17\",\n\t\"Statements\" : [ ]\n}\n".to_string(),
        ),
        (
            // Up to the pattern's last code point (U+00FF): the echo is the caller's own
            // bytes, so a document carrying non-ASCII text comes back carrying it.
            "a document carrying non-ASCII text",
            json!({"Version": "2012-10-17", "Comment": "grüß \u{00ff}"}).to_string(),
        ),
        (
            "a document far longer than its one broken member",
            json!({"Version": "2012-10-17", "Comment": "p".repeat(4096)}).to_string(),
        ),
    ]
}

/// One row of the refusal-diagnostics table: a document, the refusal it earns and the rule
/// that refusal names.
struct Refusal {
    /// What the row is, for the assertion messages.
    case: String,
    /// The RULE the document breaks. Two rows naming the same rule earn the same message
    /// and two rows naming different rules cannot, which is the contract under test.
    rule: String,
    /// The document as it is submitted.
    submitted: String,
    /// The message the refusal has to carry.
    expected: String,
    /// Whether that message carries the submitted document itself.
    echoes_document: bool,
}

/// A statement that DENIES rather than grants, which the policy language reads perfectly
/// well — so the document is a policy, and what it says is what cannot be accepted.
fn denying_statement(sid: &str, ca_arn: &str) -> Value {
    json!({
        "Sid": sid,
        "Effect": "Deny",
        "Principal": {"AWS": ACCOUNT},
        "Action": format!("acm-pca:{}", POLICY_ALLOWED_ACTIONS[0]),
        "Resource": ca_arn
    })
}

/// What a refused document's `Effect` rule is named by, pinned as the literal sentence: the
/// rule itself, with nothing of the request in it.
const ONLY_ALLOW_MESSAGE: &str = "InvalidPolicy: Only the Allow effect is supported";

/// Rule 3: an `InvalidPolicyException` names the ONE rule the submitted document broke, so
/// two documents breaking different rules cannot carry the same string.
///
/// A document that cannot be read as a policy at all broke no statement's rule — reading the
/// statements is what failed — so the refusal reports the document itself, byte for byte
/// after the fixed prefix; a document that IS a policy and says something a private CA's
/// policy cannot say is named by its own rule and is NOT echoed. Nothing is attached either
/// way.
#[tokio::test]
async fn put_policy_names_the_rule_each_refused_document_broke() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "refusal-diagnostics").await;

    // A document nothing can read is its own rule, so each of these rows is a rule of its
    // own and the message it earns is the text it submitted.
    let mut rows: Vec<Refusal> = unreadable_documents(&arn)
        .into_iter()
        .map(|(case, submitted)| Refusal {
            case: case.to_string(),
            rule: format!("unreadable: {case}"),
            expected: format!("{INVALID_POLICY_PREFIX}{submitted}"),
            submitted,
            echoes_document: true,
        })
        .collect();
    // The classes a READABLE policy earns, each named by its rule rather than by the text
    // that broke it: the statement does not grant, it grants what a CA cannot be shared
    // for, it names an identity that is not here, it is about another authority. Two rows
    // break the effect rule, through documents that differ, and both earn that rule's one
    // sentence.
    let readable: Vec<(&str, &str, String, String, bool)> = vec![
        (
            "a statement that denies",
            "effect",
            document(json!([denying_statement("denied", &arn)])),
            ONLY_ALLOW_MESSAGE.to_string(),
            false,
        ),
        (
            "a denying statement beside a granting one",
            "effect",
            document(json!([
                attachable_statement("granted", &arn),
                denying_statement("denied", &arn)
            ])),
            ONLY_ALLOW_MESSAGE.to_string(),
            false,
        ),
        (
            "a statement granting a disallowed action",
            "action",
            document(disallowed_action_statement("disallowed", &arn)),
            DISALLOWED_POLICY_ACTION_MESSAGE.to_string(),
            false,
        ),
        (
            "a statement naming an account that is not here",
            "principal",
            document(absent_principal_statement("absent", &arn)),
            POLICY_PRINCIPAL_NOT_FOUND_MESSAGE.to_string(),
            false,
        ),
        (
            "a statement about another authority",
            "resource",
            document(other_resource_statement("elsewhere")),
            different_resource_message(&arn, &json!(other_resource())),
            false,
        ),
    ];
    rows.extend(readable.into_iter().map(
        |(case, rule, submitted, expected, echoes_document)| Refusal {
            case: case.to_string(),
            rule: rule.to_string(),
            submitted,
            expected,
            echoes_document,
        },
    ));

    let mut observed: Vec<(String, String, String)> = Vec::new();
    for row in &rows {
        let case = row.case.as_str();
        let body = emulator
            .refused(
                case,
                "PutPolicy",
                &json!({"ResourceArn": arn, "Policy": row.submitted}),
                "InvalidPolicyException",
            )
            .await;
        let message = error_message(&body, case);
        assert_eq!(
            message, row.expected,
            "case {case:?}: expected the refusal to name the rule this document broke, got \
             {message:?}"
        );
        assert_eq!(
            message.contains(row.submitted.as_str()),
            row.echoes_document,
            "case {case:?}: expected the document echoed in the refusal to be {echoed}, got \
             {message:?}",
            echoed = row.echoes_document
        );
        let carried = carried_policy(&emulator, case, &arn).await;
        assert!(
            carried.is_none(),
            "case {case:?}: expected the refused document to attach nothing, got {carried:?}"
        );
        observed.push((row.case.clone(), row.rule.clone(), message));
    }

    // The whole point of the rule: the message IS the diagnosis, so two rows carry the same
    // string exactly when they broke the same rule.
    for (index, (case, rule, message)) in observed.iter().enumerate() {
        for (other_case, other_rule, other) in &observed[index + 1..] {
            assert_eq!(
                message == other,
                rule == other_rule,
                "case {case:?} and case {other_case:?}: expected the refusals to match only \
                 for one rule, got {message:?} and {other:?}"
            );
        }
    }
}
