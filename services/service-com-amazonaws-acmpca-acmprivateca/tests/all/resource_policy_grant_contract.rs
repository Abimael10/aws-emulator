// SPDX-License-Identifier: CC0-1.0

//! The contract the resource-policy surface of a private CA answers on the wire:
//! which documents `PutPolicy` attaches, what a refusal says, what `GetPolicy` displays and
//! when `DeletePolicy` succeeds.
//!
//! Four rules are under test, each over a table whose rows differ along what the rule
//! generalises over:
//!
//! 1. A resource-based policy of a private CA GRANTS the CA to another identity — "A policy
//!    grants access on a private CA to an Amazon Web Services customer account, to Amazon
//!    Web Services Organizations, or to an Amazon Web Services Organizations unit"
//!    (`PutPolicy` docs) — so every statement's `Effect` is `Allow`, and a document
//!    carrying any other effect is refused with `InvalidPolicyException` and
//!    [`ONLY_ALLOW_MESSAGE`], with nothing attached. The rows differ along where the
//!    non-granting statement sits, how many statements the document holds, the form
//!    `Statement` takes and what the statement's other members carry.
//! 2. The document a CA displays is the policy the submitted document states, rendered the
//!    way a policy store reports a document: the whitespace between the JSON tokens is
//!    gone, and an element the policy language types as a string or an array of strings
//!    (`Action`/`Resource` and the principal lists) that was written as a one-element array
//!    of one string is reported as that string — while everything else is the caller's own,
//!    the member order, the `Statement` array of a one-statement policy and every byte
//!    inside a string literal included. The rows differ along whitespace style, member
//!    order, one-value-versus-list elements, character class and spelling, and a member the
//!    policy language does not declare.
//! 3. A statement is about the CA the policy hangs on, and the refusal of one that is not
//!    IDENTIFIES the offending element: the CA's ARN, the JSON type of the rejected
//!    `Resource` value and that value itself (derived test-side by
//!    [`crate::policy_documents::different_resource_message`], with the recorded class also
//!    pinned as a literal). The rows differ along the JSON type of the rejected value and
//!    along whether it sits alone or inside a list of resources.
//! 4. `DeletePolicy` is delete-if-exists — "If there is no policy attached to the private
//!    CA, this action will return successful" (its docs) — so it resolves only the
//!    AUTHORITY, and `ResourceNotFoundException` is reserved for an ARN no authority of
//!    this service answers to.
//!
//! Every call goes over the wire through the harness in [`super::private_ca_lifecycle`].

use serde_json::{json, Value};
use service_com_amazonaws_acmpca_acmprivateca::state::{
    CA_NOT_FOUND_MESSAGE, POLICY_ALLOWED_ACTIONS, POLICY_NOT_FOUND_MESSAGE,
};

use crate::policy_documents::different_resource_message;
use crate::private_ca_lifecycle::{root_ca_request, string, Emulator, ACCOUNT, REGION};
use crate::resource_policy::error_message;

/// What Amazon Web Services Private CA answers a resource policy whose statement does not
/// grant: wholly fixed text, pinned here as the literal sentence, with nothing of the
/// request in it.
const ONLY_ALLOW_MESSAGE: &str = "InvalidPolicy: Only the Allow effect is supported";

/// What it answers a statement naming no resource at all: this service's own sentence (no
/// recording covers a `Resource` that names nothing), so the row below pins the sentence
/// rather than the recorded diagnostic the rows beside it pin.
const NO_RESOURCE_MESSAGE: &str = "Each policy statement must name the Resource it is about.";

/// A private CA created over the wire, named after `label`.
///
/// No wait for the clock: the policy operations are not gated on a CA's lifecycle status.
async fn certificate_authority(emulator: &Emulator, label: &str) -> String {
    let created = emulator
        .ok(label, "CreateCertificateAuthority", &root_ca_request(label))
        .await;
    string(&created, "CertificateAuthorityArn", label)
}

/// An action a private CA's resource policy can grant, in this service's namespace, taken
/// from the published catalog rather than written out here.
fn shared_action(index: usize) -> String {
    format!(
        "acm-pca:{}",
        POLICY_ALLOWED_ACTIONS[index % POLICY_ALLOWED_ACTIONS.len()]
    )
}

/// A document carrying `statement` as its `Statement` element, exactly as written.
fn document(statement: Value) -> String {
    json!({"Version": "2012-10-17", "Statement": statement}).to_string()
}

/// A statement this service attaches to `ca_arn`: a shared action granted to the one
/// account this emulator presents, on the CA the policy hangs on.
fn granting_statement(sid: &str, ca_arn: &str) -> Value {
    json!({
        "Sid": sid,
        "Effect": "Allow",
        "Principal": {"AWS": ACCOUNT},
        "Action": shared_action(0),
        "Resource": ca_arn
    })
}

/// The smallest document this service attaches to `ca_arn`.
fn attachable_document(ca_arn: &str) -> String {
    document(json!([granting_statement("granted", ca_arn)]))
}

/// One statement that does NOT grant: the same shape as a granting one, with `Deny` as
/// its effect.
fn denying_statement(sid: &str, principal: Value, action: Value, ca_arn: &str) -> Value {
    json!({
        "Sid": sid,
        "Effect": "Deny",
        "Principal": principal,
        "Action": action,
        "Resource": ca_arn
    })
}

/// `PutPolicy` for `ca_arn`, requiring success.
async fn put_policy(emulator: &Emulator, case: &str, ca_arn: &str, policy: &str) {
    emulator
        .ok(
            case,
            "PutPolicy",
            &json!({"ResourceArn": ca_arn, "Policy": policy}),
        )
        .await;
}

/// The document `ca_arn` carries, as `GetPolicy` displays it, or `None` when it carries
/// none.
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

/// `PutPolicy` for `ca_arn`, requiring HTTP 400 `InvalidPolicyException` with `expected` as
/// its message.
async fn refused_as_invalid_policy(
    emulator: &Emulator,
    case: &str,
    ca_arn: &str,
    policy: &str,
    expected: &str,
) {
    let body = emulator
        .refused(
            case,
            "PutPolicy",
            &json!({"ResourceArn": ca_arn, "Policy": policy}),
            "InvalidPolicyException",
        )
        .await;
    let message = error_message(&body, case);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the refusal AWS words for this class, got {message:?}"
    );
}

/// Rule 1: a statement whose `Effect` is not `Allow` is refused, wherever it sits and
/// whatever else it carries, and the refusal attaches nothing — the document the CA already
/// carried stays exactly as it was.
#[tokio::test]
async fn put_policy_refuses_a_statement_that_does_not_grant() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "effect-rule").await;

    // A policy stands on the CA first, so each refusal below is observably a write that
    // did not happen rather than an attach onto an empty CA.
    let standing = attachable_document(&arn);
    put_policy(&emulator, "a policy that stands", &arn, &standing).await;

    // Shorthand for the rows below: one statement that does not grant.
    let denying = denying_statement;

    // The rows differ along the form `Statement` takes (one object versus a list), the
    // number of statements, WHERE the non-granting statement sits, and what that
    // statement's other members carry (principal form, a scalar action versus a list, a
    // statement with no `Sid`) — so no row's outcome can come from anything but the
    // effect.
    let rows: Vec<(String, String)> = vec![
        (
            "one denying statement, given as an object".to_string(),
            document(denying(
                "denied",
                json!({"AWS": ACCOUNT}),
                json!(shared_action(0)),
                &arn,
            )),
        ),
        (
            "one denying statement in a list".to_string(),
            document(json!([denying(
                "denied",
                json!({"AWS": format!("arn:aws:iam::{ACCOUNT}:root")}),
                json!([shared_action(1), shared_action(2)]),
                &arn
            )])),
        ),
        (
            "the first of two statements denying".to_string(),
            document(json!([
                denying("first", json!("*"), json!(shared_action(3)), &arn),
                granting_statement("second", &arn)
            ])),
        ),
        (
            "the last of two statements denying".to_string(),
            document(json!([
                granting_statement("first", &arn),
                denying(
                    "second",
                    json!({"Service": "acm.amazonaws.com"}),
                    json!(shared_action(4)),
                    &arn
                )
            ])),
        ),
        (
            "the middle of three statements denying".to_string(),
            document(json!([
                granting_statement("first", &arn),
                denying(
                    "second",
                    json!({"AWS": [ACCOUNT, format!("arn:aws:iam::{ACCOUNT}:role/reader")]}),
                    json!(shared_action(5)),
                    &arn
                ),
                granting_statement("third", &arn)
            ])),
        ),
        (
            "a denying statement with no Sid".to_string(),
            document(json!([{
                "Effect": "Deny",
                "Principal": {"AWS": ACCOUNT},
                "Action": shared_action(6),
                "Resource": arn
            }])),
        ),
    ];

    for (case, submitted) in &rows {
        refused_as_invalid_policy(&emulator, case, &arn, submitted, ONLY_ALLOW_MESSAGE).await;
        let carried = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            carried.as_deref(),
            Some(standing.as_str()),
            "case {case:?}: expected the refusal to leave the standing policy as it was, \
             got {carried:?}"
        );
    }

    // The same statements, granting instead of denying, are attached: the rows above are
    // refused for their effect and for nothing else.
    for (index, (case, _)) in rows.iter().enumerate() {
        let case = format!("{case}, as an Allow");
        let granting = rows[index].1.replace("\"Deny\"", "\"Allow\"");
        assert_ne!(
            granting, rows[index].1,
            "case {case:?}: the row must carry a Deny to begin with"
        );
        put_policy(&emulator, &case, &arn, &granting).await;
        let carried = carried_policy(&emulator, &case, &arn).await;
        assert_eq!(
            carried.as_deref(),
            Some(granting.as_str()),
            "case {case:?}: expected the granting document to be attached, got {carried:?}"
        );
    }
}

/// Rule 1, with no policy standing: a CA that carries nothing still carries nothing after
/// a document that does not grant was refused.
#[tokio::test]
async fn a_refused_effect_attaches_no_policy_at_all() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "effect-rule-bare").await;

    let case = "a CA carrying no policy";
    let submitted = document(json!([{
        "Sid": "cross-account-deny",
        "Effect": "Deny",
        "Principal": {"AWS": "*"},
        "Action": shared_action(0),
        "Resource": arn
    }]));
    refused_as_invalid_policy(&emulator, case, &arn, &submitted, ONLY_ALLOW_MESSAGE).await;
    let carried = carried_policy(&emulator, case, &arn).await;
    assert_eq!(
        carried, None,
        "case {case:?}: expected the refused document to attach nothing, got {carried:?}"
    );
}

/// Rule 2: the document a CA displays is the policy the submitted document states, with the
/// whitespace between its tokens dropped and a one-element string-or-array element reported
/// as that string — and a second attach replaces it.
#[tokio::test]
async fn get_policy_displays_the_policy_the_document_states() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "reported-document").await;

    let action = shared_action(0);
    // The statement every row below carries, written compactly in one order, and the
    // document holding it: the form a CA reports when that is what was submitted.
    let statement = format!(
        "{{\"Sid\":\"share\",\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\
         \"Action\":\"{action}\",\"Resource\":\"{arn}\"}}"
    );
    let reported = format!("{{\"Version\":\"2012-10-17\",\"Statement\":[{statement}]}}");

    // The rows differ along what the rendering sees: the whitespace between the tokens
    // (none, one space after each separator as a JSON writer with default separators emits,
    // newlines and tabs, CRLF and a trailing newline), the order the members were written
    // in (which is the caller's and survives), an element written as a list of one (an
    // action, a resource, an identity), a list of two that stays a list, the `Statement`
    // array of a one-statement policy, and a character spelled as a `\uXXXX` escape.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a document already in the reported form",
            reported.clone(),
            reported.clone(),
        ),
        (
            "a space after every separator",
            format!(
                "{{\"Version\": \"2012-10-17\", \"Statement\": [{{\"Sid\": \"share\", \
                 \"Effect\": \"Allow\", \"Principal\": {{\"AWS\": \"{ACCOUNT}\"}}, \
                 \"Action\": \"{action}\", \"Resource\": \"{arn}\"}}]}}"
            ),
            reported.clone(),
        ),
        (
            "newlines and tabs between the tokens",
            format!(
                "{{\n\t\"Version\": \"2012-10-17\",\n\t\"Statement\": [\n\t\t{{\n\
                 \t\t\t\"Sid\": \"share\",\n\t\t\t\"Effect\": \"Allow\",\n\
                 \t\t\t\"Principal\": {{\"AWS\": \"{ACCOUNT}\"}},\n\
                 \t\t\t\"Action\": \"{action}\",\n\t\t\t\"Resource\": \"{arn}\"\n\t\t}}\n\t]\n}}"
            ),
            reported.clone(),
        ),
        (
            "CRLF line endings and a trailing newline",
            format!(
                "{{\r\n\t\"Version\": \"2012-10-17\",\r\n\t\"Statement\": [{{\"Sid\": \"share\",\
                 \r\n\t\t\"Effect\": \"Allow\",\r\n\t\t\"Principal\": {{\"AWS\": \"{ACCOUNT}\"}},\
                 \r\n\t\t\"Action\": \"{action}\",\r\n\t\t\"Resource\": \"{arn}\"}}]\r\n}}\r\n"
            ),
            reported.clone(),
        ),
        {
            // The member ORDER is the caller's data, not the rendering's: `Statement`
            // before `Version`, and the statement's members back to front, come back in
            // the order they were written.
            let written = format!(
                "{{\"Statement\":[{{\"Resource\":\"{arn}\",\"Action\":\"{action}\",\
                 \"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\"Effect\":\"Allow\",\
                 \"Sid\":\"share\"}}],\"Version\":\"2012-10-17\"}}"
            );
            (
                "the members in another order",
                written.clone(),
                written,
            )
        },
        (
            // The policy language types `Action` as a string or an array of strings, so a
            // one-element array of one string says what the string says and is reported as
            // that string.
            "the action as a list of one",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\
                 \"Action\":[\"{action}\"],\"Resource\":\"{arn}\"}}]}}"
            ),
            reported.clone(),
        ),
        (
            // The same for `Resource`, and for an identity list inside `Principal`.
            "the resource and the identity as lists of one",
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":[\"{ACCOUNT}\"]}},\
                 \"Action\":\"{action}\",\"Resource\":[\"{arn}\"]}}]}}"
            ),
            reported.clone(),
        ),
        (
            // A list of two says more than one string says, so it stays a list, in the
            // order it was written.
            "two actions, which stay a list",
            format!(
                "{{\"Version\": \"2012-10-17\", \"Statement\": [{{\"Sid\": \"share\", \
                 \"Effect\": \"Allow\", \"Principal\": {{\"AWS\": \"{ACCOUNT}\"}}, \
                 \"Action\": [\"{second}\", \"{action}\"], \"Resource\": \"{arn}\"}}]}}",
                second = shared_action(1),
            ),
            format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"share\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\
                 \"Action\":[\"{second}\",\"{action}\"],\"Resource\":\"{arn}\"}}]}}",
                second = shared_action(1),
            ),
        ),
        (
            // `Statement` is not one of those elements: a one-statement policy keeps its
            // one-element array rather than being reported as a bare statement object.
            "one statement, whose array survives",
            format!("{{\"Version\": \"2012-10-17\", \"Statement\": [{statement}]}}"),
            reported.clone(),
        ),
        {
            // `AWSPolicy`'s pattern (`^[\u0009\u000A\u000D\u0020-\u00FF]+$`, from
            // `smithy_query(query="operation",operation="PutPolicy")`) admits the whole
            // Latin-1 range, and every byte INSIDE a string literal is the caller's data:
            // the character written as itself and the one written as a `\uXXXX` escape both
            // come back exactly as they were written.
            let written = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"Caf\u{e9}-\\u00ff\",\
                 \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\
                 \"Action\":\"{action}\",\"Resource\":\"{arn}\"}}]}}"
            );
            (
                "Latin-1 text, one character of it written as an escape",
                written.clone(),
                written,
            )
        },
        {
            // A member the policy language does not declare is the caller's data too: it
            // survives where it was written.
            let written = format!(
                "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Comment\":\"for the audit\",\
                 \"Sid\":\"share\",\"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{ACCOUNT}\"}},\
                 \"Action\":\"{action}\",\"Resource\":\"{arn}\"}}]}}"
            );
            (
                "a member the policy language does not declare",
                written.clone(),
                written,
            )
        },
    ];

    for (case, submitted, expected) in &rows {
        // Every row is a document inside the member's constraints, so a refusal below is
        // the emulator's answer and never the row's fault.
        assert!(
            serde_json::from_str::<Value>(submitted).is_ok()
                && serde_json::from_str::<Value>(expected).is_ok(),
            "case {case:?}: both columns of the row must be JSON, got {submitted:?}"
        );

        put_policy(&emulator, case, &arn, submitted).await;
        let carried = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            carried.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the policy the document states, got {carried:?}"
        );
        // A read is a read: the second call answers the same document.
        let again = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            again, carried,
            "case {case:?}: expected a repeated read to answer the same document, got \
             {again:?}"
        );
        // Displaying a document and attaching it again is a fixed point: what a CA
        // displays is a document it holds unchanged.
        put_policy(&emulator, case, &arn, expected).await;
        let settled = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            settled.as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected the displayed document to attach unchanged, got \
             {settled:?}"
        );
    }

    // The overwrite path: the last attach is what the CA carries, and nothing of the one
    // before it.
    let (first_case, first, _) = rows.first().expect("a row").clone();
    let (last_case, last, last_displayed) = rows.last().expect("a row").clone();
    put_policy(&emulator, first_case, &arn, &first).await;
    put_policy(&emulator, last_case, &arn, &last).await;
    let carried = carried_policy(&emulator, last_case, &arn).await;
    assert_eq!(
        carried.as_deref(),
        Some(last_displayed.as_str()),
        "case {last_case:?}: expected the replacing policy back, got {carried:?}"
    );
}

/// Rule 3: the refusal of a statement about another resource names the CA, the JSON type of
/// the rejected `Resource` value and that value.
#[tokio::test]
async fn put_policy_names_the_resource_value_it_rejected() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "resource-diagnostic").await;
    let another = format!(
        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
         11111111-2222-3333-4444-555555555555"
    );

    // The rows differ along the JSON type of the rejected value — string, number, boolean,
    // object, array — and along whether it stands alone or sits inside a list of
    // resources, which is what decides WHICH value the refusal echoes. The expected
    // message is the one the rule builds, except for the row that names no resource at
    // all, which has its own refusal.
    let rows: Vec<(&str, Value, String)> = vec![
        (
            // The recorded case, pinned as the literal diagnostic AWS answers it with: the
            // fixed prefix, the CA ARN from the request, the fixed " and type: ", the
            // rejected value's JSON type token, a newline, that type's label and the
            // rejected value, and a trailing newline.
            "a wildcard",
            json!("*"),
            format!(
                "InvalidPolicy: The supplied policy contains different resource or incorrect \
                 ARN format: {arn} and type: STRING\nstring: \"*\"\n"
            ),
        ),
        (
            "another certificate authority",
            json!(another),
            different_resource_message(&arn, &json!(another)),
        ),
        (
            "a resource segment under this CA",
            json!(format!("{arn}/certificate/*")),
            different_resource_message(&arn, &json!(format!("{arn}/certificate/*"))),
        ),
        (
            "the empty string",
            json!(""),
            different_resource_message(&arn, &json!("")),
        ),
        ("a number", json!(17), different_resource_message(&arn, &json!(17))),
        (
            "a boolean",
            json!(true),
            different_resource_message(&arn, &json!(true)),
        ),
        (
            "an object",
            json!({"Arn": arn}),
            different_resource_message(&arn, &json!({"Arn": arn})),
        ),
        (
            // A list naming nothing names no resource at all, which is its own refusal.
            "an empty list",
            json!([]),
            NO_RESOURCE_MESSAGE.to_string(),
        ),
    ];

    for (case, rejected, expected) in &rows {
        let submitted = document(json!([{
            "Sid": "wrong-resource",
            "Effect": "Allow",
            "Principal": {"AWS": ACCOUNT},
            "Action": shared_action(0),
            "Resource": rejected
        }]));
        refused_as_invalid_policy(&emulator, case, &arn, &submitted, expected).await;
        let carried = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            carried, None,
            "case {case:?}: expected the refused document to attach nothing, got {carried:?}"
        );
    }

    // A list of resources: the CA's own ARN beside a value that is not it, so the refusal
    // is about the ENTRY and not about the list.
    let case = "one entry of a resource list that is not this CA";
    let submitted = document(json!([{
        "Sid": "resource-list",
        "Effect": "Allow",
        "Principal": {"AWS": ACCOUNT},
        "Action": shared_action(0),
        "Resource": [arn.clone(), another.clone()]
    }]));
    refused_as_invalid_policy(
        &emulator,
        case,
        &arn,
        &submitted,
        &different_resource_message(&arn, &json!(another)),
    )
    .await;

    // The same statement about the CA itself is attached, so the rows above are refused
    // for their resource and for nothing else.
    let case = "the CA's own ARN";
    let attachable = attachable_document(&arn);
    put_policy(&emulator, case, &arn, &attachable).await;
    let carried = carried_policy(&emulator, case, &arn).await;
    assert_eq!(
        carried.as_deref(),
        Some(attachable.as_str()),
        "case {case:?}: expected the document about the CA itself to attach, got {carried:?}"
    );
}

/// Rule 4: `DeletePolicy` succeeds against a CA that carries no policy — never attached,
/// or attached and already deleted — and reports `ResourceNotFoundException` only for an
/// ARN no authority answers to.
#[tokio::test]
async fn delete_policy_is_delete_if_exists() {
    let emulator = Emulator::new();
    let arn = certificate_authority(&emulator, "delete-if-exists").await;

    // The rows are the states a CA can be in when the delete arrives: it never carried a
    // policy, it carries one, its policy has just been deleted, and the delete is repeated
    // once more. Each is a success with an empty body.
    let rows = [
        "never attached",
        "carrying a policy",
        "already deleted",
        "deleted twice over",
    ];

    for case in rows {
        if case == "carrying a policy" {
            put_policy(&emulator, case, &arn, &attachable_document(&arn)).await;
            assert!(
                carried_policy(&emulator, case, &arn).await.is_some(),
                "case {case:?}: the row needs a CA that carries a policy"
            );
        }
        let answer = emulator
            .ok(case, "DeletePolicy", &json!({"ResourceArn": arn}))
            .await;
        assert!(
            answer.is_null() || answer.as_object().is_some_and(serde_json::Map::is_empty),
            "case {case:?}: expected DeletePolicy to answer an empty body, got {answer:?}"
        );
        let carried = carried_policy(&emulator, case, &arn).await;
        assert_eq!(
            carried, None,
            "case {case:?}: expected the CA to carry no policy afterwards, got {carried:?}"
        );
    }

    // The one absence this operation reports is the AUTHORITY's. Each row satisfies the
    // modelled `Arn` pattern and names a certificate authority this emulator never minted.
    let absent: Vec<(&str, String)> = vec![
        (
            "an id no CA was minted with",
            format!(
                "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                 00000000-0000-0000-0000-000000000000"
            ),
        ),
        (
            "another account's CA",
            format!("arn:aws:acm-pca:{REGION}:999999999999:certificate-authority/abcdef"),
        ),
    ];
    for (case, absent_arn) in &absent {
        let body = emulator
            .refused(
                case,
                "DeletePolicy",
                &json!({"ResourceArn": absent_arn}),
                "ResourceNotFoundException",
            )
            .await;
        let message = error_message(&body, case);
        assert_eq!(
            message, CA_NOT_FOUND_MESSAGE,
            "case {case:?}: expected the CA's own not-found sentence, got {message:?}"
        );
    }
}
