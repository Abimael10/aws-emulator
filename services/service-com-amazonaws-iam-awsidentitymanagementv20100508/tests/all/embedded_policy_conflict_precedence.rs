// SPDX-License-Identifier: CC0-1.0

//! Which `DeleteConflictException` sentence an identity delete answers when BOTH policy
//! kinds block it, end to end over the awsQuery wire.
//!
//! One rule: a delete blocked by the policies an identity carries names the policy
//! EMBEDDED in it before a managed policy ATTACHED to it — the order AWS's own delete
//! documentation lists the items to remove in ("inline policies, attached managed
//! policies") — so the refusal always names the dependent class the caller must clear
//! next, never one constant.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: the
//! identity kind whose delete is refused (group, user, role) and the NUMBER of embedded
//! and attached policies blocking it. Each row then walks the dependency classes off the
//! identity one at a time and asserts the sentence follows: with the embedded policies
//! gone the same delete reports the attachments, and with those detached it is served and
//! the identity is no longer readable. Every assertion compares the WHOLE message.
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
/// the operations that create it, embed and remove an inline policy, attach and detach a
/// managed policy, delete it and read it back.
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

    /// `(embed, remove-embedded, attach, detach, delete, read-back)`.
    fn actions(self) -> [&'static str; 6] {
        match self {
            Identity::Group => [
                "PutGroupPolicy",
                "DeleteGroupPolicy",
                "AttachGroupPolicy",
                "DetachGroupPolicy",
                "DeleteGroup",
                "GetGroup",
            ],
            Identity::User => [
                "PutUserPolicy",
                "DeleteUserPolicy",
                "AttachUserPolicy",
                "DetachUserPolicy",
                "DeleteUser",
                "GetUser",
            ],
            Identity::Role => [
                "PutRolePolicy",
                "DeleteRolePolicy",
                "AttachRolePolicy",
                "DetachRolePolicy",
                "DeleteRole",
                "GetRole",
            ],
        }
    }
}

/// One identity carrying both policy kinds: how many of each block its delete.
#[derive(Debug)]
struct Row {
    case: &'static str,
    identity: Identity,
    embedded: usize,
    attached: usize,
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
    let delete = identity.actions()[4];
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
        "case {case:?}: expected AWS's sentence for the class it names first, got \
         {message:?}"
    );
    assert!(
        !message.contains(name),
        "case {case:?}: the refusal must name no entity of this account, got {message:?}"
    );
}

/// THE RULE: where both an EMBEDDED inline policy and an ATTACHED managed policy block an
/// identity's delete, the refusal names the embedded one; with the embedded policies gone
/// the same delete names the attachments; with those detached it is served.
#[tokio::test]
async fn a_delete_blocked_by_both_policy_kinds_names_the_embedded_one_first() {
    let rows = &[
        Row {
            case: "a group embedding one policy and carrying one attachment",
            identity: Identity::Group,
            embedded: 1,
            attached: 1,
        },
        Row {
            case: "a group embedding two policies and carrying one attachment",
            identity: Identity::Group,
            embedded: 2,
            attached: 1,
        },
        Row {
            case: "a user embedding one policy and carrying two attachments",
            identity: Identity::User,
            embedded: 1,
            attached: 2,
        },
        Row {
            case: "a user embedding three policies and carrying three attachments",
            identity: Identity::User,
            embedded: 3,
            attached: 3,
        },
        Row {
            case: "a role embedding one policy and carrying one attachment",
            identity: Identity::Role,
            embedded: 1,
            attached: 1,
        },
        Row {
            case: "a role embedding two policies and carrying three attachments",
            identity: Identity::Role,
            embedded: 2,
            attached: 3,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let [embed, remove_embedded, attach, detach, delete, read_back] = row.identity.actions();
        let name_member = row.identity.name_member();
        let name = "blocked-identity";

        match row.identity {
            Identity::Group => {
                expect_ok(&router, case, "CreateGroup", &[("GroupName", name)]).await;
            }
            Identity::User => {
                expect_ok(&router, case, "CreateUser", &[("UserName", name)]).await;
            }
            Identity::Role => {
                expect_ok(
                    &router,
                    case,
                    "CreateRole",
                    &[
                        ("RoleName", name),
                        ("AssumeRolePolicyDocument", TRUST_POLICY),
                    ],
                )
                .await;
            }
        }

        // The embedded policies, and the managed policies attached alongside them.
        let embedded: Vec<String> = (0..row.embedded).map(|n| format!("embedded-{n}")).collect();
        for policy_name in &embedded {
            expect_ok(
                &router,
                case,
                embed,
                &[
                    (name_member, name),
                    ("PolicyName", policy_name),
                    ("PolicyDocument", POLICY_DOCUMENT),
                ],
            )
            .await;
        }
        let mut attachments = Vec::new();
        for n in 0..row.attached {
            let created = expect_ok(
                &router,
                case,
                "CreatePolicy",
                &[
                    ("PolicyName", &format!("managed-{n}")),
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
            attachments.push(arn);
        }

        // Both classes block the delete, and the one AWS names is the EMBEDDED policy.
        expect_refused(&router, case, row.identity, name, MUST_DELETE_POLICIES).await;

        // The sentence was read from the dependency found, not from a constant: with the
        // embedded policies gone the same delete reports the attachments that remain.
        for policy_name in &embedded {
            expect_ok(
                &router,
                case,
                remove_embedded,
                &[(name_member, name), ("PolicyName", policy_name)],
            )
            .await;
        }
        expect_refused(&router, case, row.identity, name, MUST_DETACH_ALL_POLICIES).await;

        // And with the attachments gone too, nothing blocks it any more.
        for arn in &attachments {
            expect_ok(
                &router,
                case,
                detach,
                &[(name_member, name), ("PolicyArn", arn)],
            )
            .await;
        }
        let (status, body) = call(&router, delete, &[(name_member, name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the delete must be served once both policy kinds are gone, \
             got {status}: {body}"
        );
        let (status, read) = call(&router, read_back, &[(name_member, name)]).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the deleted identity must not be readable through \
             {read_back}, got {status}: {read}"
        );
    }
}

/// Each class ALONE still carries its own sentence, so the precedence above did not turn
/// one of the two sentences into the only answer: an identity blocked only by an
/// attachment is still told to detach.
#[tokio::test]
async fn one_policy_kind_alone_still_carries_its_own_sentence() {
    let rows = &[
        (Identity::Group, "CreateGroup"),
        (Identity::User, "CreateUser"),
        (Identity::Role, "CreateRole"),
    ];

    for (identity, create) in rows {
        for attached_only in [false, true] {
            let blocker = if attached_only {
                "an attachment alone"
            } else {
                "an embedded policy alone"
            };
            let case = format!("{identity:?} blocked by {blocker}");
            let case = case.as_str();
            let router = router();
            let [embed, _remove, attach, _detach, _delete, _read] = identity.actions();
            let name_member = identity.name_member();
            let name = "single-blocker";

            let mut create_params = vec![(name_member, name)];
            if *identity == Identity::Role {
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

            expect_refused(&router, case, *identity, name, expected).await;
        }
    }
}
