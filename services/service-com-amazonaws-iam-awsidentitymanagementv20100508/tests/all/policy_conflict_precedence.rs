// SPDX-License-Identifier: CC0-1.0

//! Which `DeleteConflictException` sentence an identity delete answers when BOTH policy
//! kinds block it, end to end over the awsQuery wire.
//!
//! One rule: a delete blocked by the policies an identity carries names ONE class of
//! blocking dependent, and which class it names is the precedence that entity's delete is
//! observed answering with — a USER's and a ROLE's delete name the managed policies
//! ATTACHED to the identity before the policies EMBEDDED in it, while a GROUP's delete
//! names the ones it embeds first. The sentence is read from the dependency the delete
//! actually found, never from one constant: clear the class named first and the same
//! delete names the other one.
//!
//! The suite is a TABLE whose rows differ along what the rule generalises over: the
//! identity kind whose delete is refused (group, user, role) and the NUMBER of embedded
//! and attached policies blocking it. Each row then walks the dependency classes off the
//! identity one at a time and asserts the sentence follows, until the delete is served
//! and the identity is no longer readable. Every assertion compares the WHOLE message.
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

    /// Whether this identity's delete names the ATTACHED managed policies while both
    /// classes block it: a user's and a role's do, a group's names its embedded policies.
    fn names_attachments_first(self) -> bool {
        match self {
            Identity::User | Identity::Role => true,
            Identity::Group => false,
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

/// Take one class of blocking policy off the identity: `action` is the operation that
/// removes one (`Delete*Policy` or `Detach*Policy`), and `values` the policies it names
/// through `value_member` (`PolicyName` or `PolicyArn`).
async fn clear(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    name_member: &str,
    name: &str,
    value_member: &str,
    values: &[String],
) {
    for value in values {
        expect_ok(
            router,
            case,
            action,
            &[(name_member, name), (value_member, value)],
        )
        .await;
    }
}

/// THE RULE: where both an EMBEDDED inline policy and an ATTACHED managed policy block an
/// identity's delete, the refusal names the class that identity's delete names — the
/// attachments for a user and a role, the embedded policies for a group; clearing that
/// class leaves the same delete naming the other one, and clearing both serves it.
#[tokio::test]
async fn a_delete_blocked_by_both_policy_kinds_names_the_class_its_entity_names() {
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

        // Clearing the classes in the order the refusal names them: both block the
        // delete, so it names the first of them; with that class gone the same delete
        // names the one that remains, which is how the sentence is shown to be read from
        // the dependency found rather than from a constant.
        if row.identity.names_attachments_first() {
            expect_refused(&router, case, row.identity, name, MUST_DETACH_ALL_POLICIES).await;
            clear(&router, case, detach, name_member, name, "PolicyArn", &attachments).await;
            expect_refused(&router, case, row.identity, name, MUST_DELETE_POLICIES).await;
            clear(
                &router,
                case,
                remove_embedded,
                name_member,
                name,
                "PolicyName",
                &embedded,
            )
            .await;
        } else {
            expect_refused(&router, case, row.identity, name, MUST_DELETE_POLICIES).await;
            clear(
                &router,
                case,
                remove_embedded,
                name_member,
                name,
                "PolicyName",
                &embedded,
            )
            .await;
            expect_refused(&router, case, row.identity, name, MUST_DETACH_ALL_POLICIES).await;
            clear(&router, case, detach, name_member, name, "PolicyArn", &attachments).await;
        }

        // And with both classes gone, nothing blocks it any more.
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

// Each class ALONE still carries its own sentence, so the precedence above did not turn
// one of the two sentences into the only answer: that is the suite
// `tests/all/embedded_policy_conflict_precedence.rs` owns.
