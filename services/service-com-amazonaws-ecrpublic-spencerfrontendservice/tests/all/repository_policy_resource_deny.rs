// SPDX-License-Identifier: CC0-1.0

//! What a call refused by the policy ATTACHED TO THE REPOSITORY is answered with, over the
//! awsJson1_1 wire.
//!
//! The rule under test: a call whose grant is withheld by an explicit `Deny` in a policy in
//! force is an AUTHORIZATION refusal — the AWS-common `AccessDeniedException` with AWS's
//! authorization sentence, naming the calling principal, the action attempted, the ARN of
//! the resource and the CLASS of policy the deny was written in — and never one of the
//! operation's modelled input errors: the request is well formed, and what refused it is a
//! policy decision about the caller. A repository policy is that policy class attached to
//! the resource, so its reason clause names a resource-based policy.
//!
//! The table spans what the rule generalises over: every action and principal form by which
//! a stored `Deny` reaches this account, both repository-name forms the model admits (so the
//! resource ARN the refusal names varies), every document the refused call might try to
//! store, and every value of `force` — the refusal is one sentence for all of them. The
//! separate refusal of an UNFORCED document that WOULD cause such a lockout is a different
//! class and keeps its own `InvalidParameterException`
//! (`tests/all/client_error_message_templates.rs`), which is asserted here too so the two
//! cannot collapse into one.

use serde_json::json;

use crate::harness::error_message;
use crate::repository_image_lifecycle::{call, ok, refused, service, text, ARN_PREFIX};

/// The clause AWS's authorization refusal states when the explicit `Deny` was written in a
/// policy attached to the RESOURCE rather than to the caller.
const EXPLICIT_RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// The IAM action `SetRepositoryPolicy` is performed under: this service's ARN namespace and
/// the operation name.
const ACTION: &str = "ecr-public:SetRepositoryPolicy";

/// A benign document that grants an ordinary pull: one of the documents a refused call tries
/// to store.
const PULL_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Sid":"AllowPull","Effect":"Allow","Principal":"*","Action":["ecr-public:BatchGetImage"]}]}"#;

/// The sentence AWS documents the `force` parameter in, which is what an UNFORCED locking
/// document is refused with — a different class from the enforcement refusal above.
const UNFORCED_REFUSAL: &str = "You are about to set a repository policy that will prevent \
                                you from setting another one in the future. Use the force \
                                parameter to override this exception.";

/// The principal every call of this suite speaks for: the account's own published
/// credential, whose ARN is the one `sts:GetCallerIdentity` reports for it — read from the
/// account contract rather than written out, so the refusal is compared against the identity
/// the caller would read back.
fn caller() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// One row: the documents whose stored `Deny` locks the repository's policy, spanning the
/// action and principal forms by which a `Deny` reaches this account.
fn locking_documents() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "every principal, this action",
            r#"{"Version":"2012-10-17","Statement":[{"Sid":"Lock","Effect":"Deny","Principal":"*","Action":["ecr-public:SetRepositoryPolicy"]}]}"#,
        ),
        (
            "this account by ARN, an action prefix",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"ecr-public:Set*"}]}"#,
        ),
        (
            "the account id alone, every action",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":{"AWS":"000000000000"},"Action":"*"}]}"#,
        ),
        (
            "NotAction, which leaves this action denied",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","NotAction":["ecr-public:BatchGetImage"]}]}"#,
        ),
        (
            "an Allow beside a second statement that denies it",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"ecr-public:BatchGetImage"},{"Effect":"Deny","Principal":"*","Action":"ecr-public:SetRepositoryPolicy"}]}"#,
        ),
    ]
}

/// The ways the refused call can be made: the document it tries to store, and what it says
/// about `force`. None of them changes the answer — the stored `Deny` decides.
fn attempts() -> Vec<(&'static str, &'static str, Option<bool>)> {
    vec![
        ("a benign document, force omitted", PULL_POLICY, None),
        ("a benign document, force false", PULL_POLICY, Some(false)),
        ("a benign document, force true", PULL_POLICY, Some(true)),
        (
            "the locking document again, force true",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","Action":"ecr-public:SetRepositoryPolicy"}]}"#,
            Some(true),
        ),
    ]
}

#[tokio::test]
async fn a_stored_deny_refuses_the_next_call_as_an_authorization_decision() {
    let router = service();

    for (index, (case, document)) in locking_documents().into_iter().enumerate() {
        // Both repository-name forms the modelled pattern admits, so the resource ARN the
        // refusal names varies with the row; one repository per row, because the lockout
        // this row installs must not decide the next.
        let name = if index % 2 == 0 {
            format!("resource-deny-{index}")
        } else {
            format!("policy/resource-deny-{index}")
        };
        let expected = format!(
            "User: {} is not authorized to perform: {ACTION} on resource: {ARN_PREFIX}{name} \
             {EXPLICIT_RESOURCE_DENY}",
            caller()
        );
        ok(
            &router,
            "CreateRepository",
            json!({"repositoryName": name}),
            case,
        )
        .await;

        // The document needs `force` to be stored at all, and the unforced attempt is the
        // OTHER class of refusal: nothing is stored yet, so what is refused is the request's
        // own combination of a locking document and an absent `force`.
        let unforced = refused(
            &router,
            "SetRepositoryPolicy",
            json!({"repositoryName": name, "policyText": document}),
            "InvalidParameterException",
            case,
        )
        .await;
        let message = error_message(&unforced);
        assert_eq!(
            message, UNFORCED_REFUSAL,
            "case {case:?}: expected the unforced attempt refused with {UNFORCED_REFUSAL:?}, \
             got {message:?}"
        );
        ok(
            &router,
            "SetRepositoryPolicy",
            json!({"repositoryName": name, "policyText": document, "force": true}),
            case,
        )
        .await;
        let read_back = ok(
            &router,
            "GetRepositoryPolicy",
            json!({"repositoryName": name}),
            case,
        )
        .await;
        let stored = text(&read_back, &["policyText"], case);

        // Now the `Deny` is in force, and every way of asking is refused identically.
        for (attempt, policy_text, force) in attempts() {
            let case = format!("{case} / {attempt}");
            let mut request = json!({"repositoryName": name, "policyText": policy_text});
            if let Some(force) = force {
                request["force"] = json!(force);
            }
            let body = refused(
                &router,
                "SetRepositoryPolicy",
                request,
                "AccessDeniedException",
                &case,
            )
            .await;
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
            assert!(
                !message.contains("Invalid parameter"),
                "case {case:?}: an authorization refusal states no input constraint, \
                 got {message:?}"
            );

            // The refusal changed nothing: the stored document is still the one in force.
            let read_back = ok(
                &router,
                "GetRepositoryPolicy",
                json!({"repositoryName": name}),
                &case,
            )
            .await;
            let read = text(&read_back, &["policyText"], &case);
            assert_eq!(
                read, stored,
                "case {case:?}: expected the stored document untouched by the refusal"
            );
        }

        // Deleting the policy lifts the deny: the identical call then succeeds.
        ok(
            &router,
            "DeleteRepositoryPolicy",
            json!({"repositoryName": name}),
            case,
        )
        .await;
        let set = ok(
            &router,
            "SetRepositoryPolicy",
            json!({"repositoryName": name, "policyText": PULL_POLICY}),
            case,
        )
        .await;
        assert_eq!(
            text(&set, &["repositoryName"], case),
            name,
            "case {case:?}: expected the call to succeed once the deny is deleted, got {set}"
        );
    }
}

#[tokio::test]
async fn a_repository_whose_policy_denies_nothing_is_not_refused() {
    let router = service();

    // The complement of the rule: a stored document that does NOT deny the policy-setting
    // action to this account withholds no grant, so the next call is served. The rows span
    // the near misses — a `Deny` of another action, of another principal, one excluded by
    // `NotAction`, and one that applies only under a condition this service does not
    // evaluate.
    let rows: [(&str, &str); 4] = [
        (
            "a deny of another action",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","Action":"ecr-public:DeleteRepository"}]}"#,
        ),
        (
            "a deny aimed at another account",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":{"AWS":"arn:aws:iam::111122223333:root"},"Action":"*"}]}"#,
        ),
        (
            "NotAction that spares this action",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","NotAction":["ecr-public:SetRepositoryPolicy"]}]}"#,
        ),
        (
            "a deny that applies only under a condition",
            r#"{"Version":"2012-10-17","Statement":[{"Effect":"Deny","Principal":"*","Action":"*","Condition":{"StringNotEquals":{"aws:PrincipalAccount":"000000000000"}}}]}"#,
        ),
    ];
    for (index, (case, document)) in rows.into_iter().enumerate() {
        let name = format!("policy/resource-allows-{index}");
        ok(
            &router,
            "CreateRepository",
            json!({"repositoryName": name}),
            case,
        )
        .await;
        ok(
            &router,
            "SetRepositoryPolicy",
            json!({"repositoryName": name, "policyText": document}),
            case,
        )
        .await;
        let (status, body) = call(
            &router,
            "SetRepositoryPolicy",
            json!({"repositoryName": name, "policyText": PULL_POLICY}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the next call served, got {status} with {body}"
        );
    }
}
