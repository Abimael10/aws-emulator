// SPDX-License-Identifier: CC0-1.0

//! An IAM call signed with an assumed-role session is decided on what is in force on the
//! ROLE: its identity-based documents, and the permissions boundary that caps them.
//!
//! RULE: a request to IAM signed with the temporary credential of a session of one of the
//! account's roles is authorized against the identity-based documents in force on that role
//! — "the temporary security credentials … have the permissions of the role" (`AssumeRole`)
//! — capped by the permissions boundary in force on that same role, which "sets the maximum
//! permissions that an identity-based policy can grant to an entity". So the call is served
//! only where those documents allow the asked-for action on the resource the request
//! addresses AND the role's boundary allows it too, and is otherwise refused HTTP 403
//! `AccessDenied` — whichever IAM action it is, whether the role's documents are embedded in
//! it or attached to it, and whether the refusal comes from the documents or from the
//! ceiling.
//!
//! IAM holds the role, its documents and its boundary while STS mints the session, so
//! neither crate's own test target can mint a session and then read the decision IAM makes
//! about it; this is the suite that can pin it.
//!
//! The rows span what the rule generalises over: whether the role carries no document, one
//! that allows the action, one that allows only another action, one that allows the action on
//! another resource only, one attached as a managed policy, and one that allows everything
//! beside an explicit `Deny` of the action; whether a boundary is in force and whether it
//! allows the action; and which IAM action is called on which role.
//!
//! The awsQuery harness, the role fixtures and the XML readers are the ones
//! `iam_role_id_agrees_with_sts.rs` and `iam_session_tag_authorization.rs` own, so a request
//! is built and read exactly one way.

use smithy_server_runtime::MultiServiceRouter;

use crate::iam_role_id_agrees_with_sts::{build_router, iam, iam_as, LONG_TERM_KEY};
use crate::iam_session_tag_authorization::{assume_role, create_role};
use crate::query_wire::{element, opt_element};

/// The account every ARN this emulator mints carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = "000000000000";

/// What the caller's role carries as its identity-based policy, which is what the call is
/// decided against.
enum Carried {
    /// No identity-based document at all.
    Nothing,
    /// One inline document, embedded in the role.
    Inline(String),
    /// One customer-managed policy of the account, attached to the role.
    Attached(String),
}

/// One row: what the role carries, the ceiling on it, the call the session makes and whether
/// IAM serves it.
struct Row {
    case: &'static str,
    carried: Carried,
    /// The document of the managed policy put on the role as its permissions boundary, or
    /// `None` for a role carrying no ceiling.
    boundary: Option<String>,
    /// The path the role is created under: the account root unless a row names another.
    path: &'static str,
    /// The awsQuery action the session calls.
    action: &'static str,
    /// Whether a `ListAttachedRolePolicies` row names the caller's OWN role rather than the
    /// second role the account holds.
    own_role: bool,
    served: bool,
}

/// The path a role is created under when a row names none: the account root, which is what
/// `CreateRole`'s `Path` defaults to.
const ROOT_PATH: &str = "/";

/// One statement, as a whole document.
fn document(effect: &str, action: &str, resource: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{action}","Resource":"{resource}"}}]}}"#
    )
}

/// A document allowing every IAM action on every resource.
fn allow_all() -> String {
    document("Allow", "iam:*", "*")
}

/// The action the attachment-listing rows are decided on.
const LIST_ATTACHED: &str = "ListAttachedRolePolicies";

/// The rows, given the name of the second role the account holds.
fn rows(other_role: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a role carrying no document",
            carried: Carried::Nothing,
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role whose inline document allows the action",
            carried: Carried::Inline(document("Allow", "iam:ListAttachedRolePolicies", "*")),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: true,
        },
        Row {
            case: "a role whose inline document allows another action only",
            carried: Carried::Inline(document("Allow", "iam:ListUsers", "*")),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role under a path whose document allows another action only",
            carried: Carried::Inline(document("Allow", "iam:ListUsers", "*")),
            boundary: None,
            path: "/engineering/",
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role under a nested path whose document allows another action only",
            carried: Carried::Inline(document("Allow", "iam:ListUsers", "*")),
            boundary: None,
            path: "/division_abc/subdivision_xyz/",
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role under a path whose document allows the action",
            carried: Carried::Inline(document("Allow", "iam:ListAttachedRolePolicies", "*")),
            boundary: None,
            path: "/engineering/",
            action: LIST_ATTACHED,
            own_role: true,
            served: true,
        },
        Row {
            case: "a role whose inline document allows the action on another role only",
            carried: Carried::Inline(document(
                "Allow",
                "iam:ListAttachedRolePolicies",
                &format!("arn:aws:iam::{ACCOUNT}:role/{other_role}"),
            )),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role whose inline document allows the action on the role it addresses",
            carried: Carried::Inline(document(
                "Allow",
                "iam:ListAttachedRolePolicies",
                &format!("arn:aws:iam::{ACCOUNT}:role/{other_role}"),
            )),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: false,
            served: true,
        },
        Row {
            case: "a role whose attached managed policy allows the action",
            carried: Carried::Attached(document("Allow", "iam:ListAttachedRolePolicies", "*")),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: true,
        },
        Row {
            case: "a role allowed everything beside an explicit deny of the action",
            carried: Carried::Inline(concat!(
                r#"{"Version":"2012-10-17","Statement":["#,
                r#"{"Effect":"Allow","Action":"iam:*","Resource":"*"},"#,
                r#"{"Effect":"Deny","Action":"iam:ListAttachedRolePolicies","Resource":"*"}]}"#
            ).to_string()),
            boundary: None,
            path: ROOT_PATH,
            action: LIST_ATTACHED,
            own_role: true,
            served: false,
        },
        Row {
            case: "a role whose document allows a listing of every role",
            carried: Carried::Inline(allow_all()),
            boundary: None,
            path: ROOT_PATH,
            action: "ListRoles",
            own_role: true,
            served: true,
        },
        Row {
            case: "a role whose document allows nothing of the listing it asks for",
            carried: Carried::Inline(document("Allow", "s3:GetObject", "*")),
            boundary: None,
            path: ROOT_PATH,
            action: "ListRoles",
            own_role: true,
            served: false,
        },
        Row {
            case: "a role allowed everything, capped by a boundary that allows the action",
            carried: Carried::Inline(allow_all()),
            boundary: Some(document("Allow", "iam:ListRoles", "*")),
            path: ROOT_PATH,
            action: "ListRoles",
            own_role: true,
            served: true,
        },
        Row {
            case: "a role allowed everything, capped by a boundary that does not",
            carried: Carried::Inline(allow_all()),
            boundary: Some(document("Allow", "iam:ListUsers", "*")),
            path: ROOT_PATH,
            action: "ListRoles",
            own_role: true,
            served: false,
        },
        Row {
            case: "a role allowed everything, capped by a boundary denying the action",
            carried: Carried::Inline(allow_all()),
            boundary: Some(document("Deny", "iam:ListRoles", "*")),
            path: ROOT_PATH,
            action: "ListRoles",
            own_role: true,
            served: false,
        },
        Row {
            case: "a role under a path, capped by a boundary that does not allow the action",
            carried: Carried::Inline(allow_all()),
            boundary: Some(document("Allow", "iam:ListUsers", "*")),
            path: "/engineering/",
            action: "ListRoles",
            own_role: true,
            served: false,
        },
    ]
}

/// Create a customer-managed policy of the account holding `document` and answer its ARN.
async fn create_policy(router: &MultiServiceRouter, case: &str, name: &str, document: &str) -> String {
    let (status, body) = iam(
        router,
        "CreatePolicy",
        &[
            ("PolicyName", name.to_string()),
            ("PolicyDocument", document.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreatePolicy expected 200, got {status} with body {body}"
    );
    element(&body, "Arn")
}

/// The trust policy a role created under a path is given: the one the shared role fixture
/// uses, admitting the account root to assume the role.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"#,
    r#""Action":["sts:AssumeRole","sts:TagSession"]}]}"#
);

/// Create a role under `path` whose trust policy admits the account root, and answer its
/// ARN — the role fixture `create_role` builds, with the `Path` stated per row.
async fn create_role_at(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    path: &str,
) -> String {
    if path == ROOT_PATH {
        return create_role(router, role_name).await;
    }
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("Path", path.to_string()),
            ("AssumeRolePolicyDocument", TRUST_POLICY.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole under {path:?} expected 200, got {status} with body {body}"
    );
    element(&body, "Arn")
}

#[tokio::test]
async fn an_iam_call_by_a_role_session_is_decided_on_what_is_in_force_on_the_role() {
    let router = build_router();
    // The second role every row can address: held by the account, so a refusal is about the
    // caller's permissions and not about the role being absent. It carries no attachment of
    // its own, so a served listing of it reports an empty list.
    let other_role = "session-authorization-other";
    create_role(&router, other_role).await;

    for (position, row) in rows(other_role).iter().enumerate() {
        let role_name = format!("session-authorization-{position}");
        let role_arn = create_role_at(&router, row.case, &role_name, row.path).await;
        match &row.carried {
            Carried::Nothing => {}
            Carried::Inline(document) => {
                let (status, body) = iam(
                    &router,
                    "PutRolePolicy",
                    &[
                        ("RoleName", role_name.clone()),
                        ("PolicyName", "carried".to_string()),
                        ("PolicyDocument", document.clone()),
                    ],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: PutRolePolicy expected 200, got {status} with body {body}",
                    row.case
                );
            }
            Carried::Attached(document) => {
                let policy_arn = create_policy(
                    &router,
                    row.case,
                    &format!("{role_name}-attached"),
                    document,
                )
                .await;
                let (status, body) = iam(
                    &router,
                    "AttachRolePolicy",
                    &[("RoleName", role_name.clone()), ("PolicyArn", policy_arn)],
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {:?}: AttachRolePolicy expected 200, got {status} with body {body}",
                    row.case
                );
            }
        }
        if let Some(document) = &row.boundary {
            let boundary_arn = create_policy(
                &router,
                row.case,
                &format!("{role_name}-boundary"),
                document,
            )
            .await;
            let (status, body) = iam(
                &router,
                "PutRolePermissionsBoundary",
                &[
                    ("RoleName", role_name.clone()),
                    ("PermissionsBoundary", boundary_arn),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: PutRolePermissionsBoundary expected 200, got {status} with \
                 body {body}",
                row.case
            );
        }

        let session = assume_role(
            &router,
            LONG_TERM_KEY,
            &role_arn,
            &format!("session-{position}"),
            &[],
            &[],
        )
        .await;
        let parameters: Vec<(&str, String)> = if row.action == LIST_ATTACHED {
            vec![(
                "RoleName",
                if row.own_role {
                    role_name.clone()
                } else {
                    other_role.to_string()
                },
            )]
        } else {
            Vec::new()
        };
        let (status, body) = iam_as(&router, &session, row.action, &parameters).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {:?}: expected HTTP 200 from {}, got {status} with body {body}",
                row.case, row.action
            );
        } else {
            assert_eq!(
                status, 403,
                "case {:?}: expected HTTP 403 from {}, got {status} with body {body}",
                row.case, row.action
            );
            assert_eq!(
                opt_element(&body, "Code").as_deref(),
                Some("AccessDenied"),
                "case {:?}: expected the AccessDenied code, got body {body}",
                row.case
            );
        }
    }
}
