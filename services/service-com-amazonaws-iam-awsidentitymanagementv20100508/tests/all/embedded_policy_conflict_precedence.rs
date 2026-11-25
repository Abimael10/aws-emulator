// SPDX-License-Identifier: CC0-1.0

//! Which `DeleteConflictException` sentence an identity delete answers when exactly ONE
//! policy kind blocks it, end to end over the awsQuery wire.
//!
//! One rule: the refusal is read from the dependency the delete found, so each policy
//! class carries AWS's own sentence for it — an identity that only EMBEDS a policy is
//! told to delete policies, one that only CARRIES an attachment to detach them — and
//! neither sentence is the answer to the other case. This is the counterpart of
//! `tests/all/policy_conflict_precedence.rs`, which pins which of the two an identity
//! blocked by BOTH names: together they show the sentence is selected rather than
//! constant.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: the
//! identity kind whose delete is refused (group, user, role) and which single class
//! blocks it. Every assertion compares the WHOLE message.
//!
//! Every assertion drives a `MultiServiceRouter` holding this service over one shared
//! `InMemoryEngine`, exactly as the production router does. The awsQuery harness is the
//! one `tests/all/user_lifecycle.rs` owns and every suite in this crate shares.

use smithy_server_runtime::MultiServiceRouter;

use crate::user_lifecycle::{call, element, error_code, router};

/// A policy document with a single statement — the form `CreatePolicy`'s own modelled
/// example carries.
const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// A trust policy with a single statement, which a fixture role needs to exist.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// AWS's text for each class of blocking dependent, written out here rather than taken
/// from the implementation's constants: what this suite pins is the SENTENCE AWS answers,
/// so a reworded constant has to fail here rather than agree with itself.
const MUST_DELETE_POLICIES: &str = "Cannot delete entity, must delete policies first.";
const MUST_DETACH_ALL_POLICIES: &str = "Cannot delete entity, must detach all policies first.";

/// The identity kinds whose delete can be blocked by the policies they carry, each with
/// the operations that create it, embed an inline policy, attach a managed policy and
/// delete it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    Group,
    User,
    Role,
}

impl Identity {
    /// The member every operation below names this identity by.
    fn name_member(self) -> &'static str {
        match self {
            Identity::Group => "GroupName",
            Identity::User => "UserName",
            Identity::Role => "RoleName",
        }
    }

    /// `(create, embed, attach, delete)`.
    fn actions(self) -> [&'static str; 4] {
        match self {
            Identity::Group => [
                "CreateGroup",
                "PutGroupPolicy",
                "AttachGroupPolicy",
                "DeleteGroup",
            ],
            Identity::User => [
                "CreateUser",
                "PutUserPolicy",
                "AttachUserPolicy",
                "DeleteUser",
            ],
            Identity::Role => [
                "CreateRole",
                "PutRolePolicy",
                "AttachRolePolicy",
                "DeleteRole",
            ],
        }
    }
}

/// Drive one fixture call, asserting it succeeded, and report the body.
async fn expect_ok(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture call {action} must succeed, got {status}: {body}"
    );
    body
}

/// Assert the delete of `name` is refused with `expected`, and nothing of the account is
/// in the sentence.
async fn expect_refused(
    router: &MultiServiceRouter,
    case: &str,
    identity: Identity,
    name: &str,
    expected: &str,
) {
    let delete = identity.actions()[3];
    let (status, body) = call(router, delete, &[(identity.name_member(), name)]).await;
    assert_eq!(
        status, 409,
        "case {case:?}: expected the delete conflict's HTTP 409 from {delete}, got \
         {status}: {body}"
    );
    assert_eq!(
        error_code(&body),
        "DeleteConflict",
        "case {case:?}: expected DeleteConflict from {delete}, got {:?}: {body}",
        error_code(&body)
    );
    let message = element(&body, "Message");
    assert_eq!(
        message, expected,
        "case {case:?}: expected AWS's sentence for the class blocking this delete, got \
         {message:?}"
    );
    assert!(
        !message.contains(name),
        "case {case:?}: the refusal must name no entity of this account, got {message:?}"
    );
}

/// THE RULE: each policy class ALONE carries its own sentence, for every identity kind —
/// an identity blocked only by an attachment is told to detach, and one blocked only by
/// an embedded policy to delete.
#[tokio::test]
async fn one_policy_kind_alone_still_carries_its_own_sentence() {
    for identity in [Identity::Group, Identity::User, Identity::Role] {
        for attached_only in [false, true] {
            let blocker = if attached_only {
                "an attachment alone"
            } else {
                "an embedded policy alone"
            };
            let case = format!("{identity:?} blocked by {blocker}");
            let case = case.as_str();
            let router = router();
            let [create, embed, attach, _delete] = identity.actions();
            let name_member = identity.name_member();
            let name = "single-blocker";

            let mut create_params = vec![(name_member, name)];
            if identity == Identity::Role {
                create_params.push(("AssumeRolePolicyDocument", TRUST_POLICY));
            }
            expect_ok(&router, case, create, &create_params).await;

            let expected = if attached_only {
                let created = expect_ok(
                    &router,
                    case,
                    "CreatePolicy",
                    &[
                        ("PolicyName", "managed-only"),
                        ("PolicyDocument", POLICY_DOCUMENT),
                    ],
                )
                .await;
                let arn = element(&created, "Arn");
                expect_ok(
                    &router,
                    case,
                    attach,
                    &[(name_member, name), ("PolicyArn", &arn)],
                )
                .await;
                MUST_DETACH_ALL_POLICIES
            } else {
                expect_ok(
                    &router,
                    case,
                    embed,
                    &[
                        (name_member, name),
                        ("PolicyName", "embedded-only"),
                        ("PolicyDocument", POLICY_DOCUMENT),
                    ],
                )
                .await;
                MUST_DELETE_POLICIES
            };

            expect_refused(&router, case, identity, name, expected).await;
        }
    }
}
