// SPDX-License-Identifier: CC0-1.0

//! The `PermissionsBoundary` a CREATE carries, end to end over the awsQuery wire.
//!
//! The rule under test: `CreateUser$PermissionsBoundary` and `CreateRole$PermissionsBoundary`
//! are "the ARN of the managed policy that is used to set the permissions boundary" for the
//! identity being created, so the value is a REFERENCE the account RESOLVES rather than a
//! string the identity carries: a boundary "defines the maximum permissions that
//! identity-based policies can grant to an entity", which is a question asked of that
//! policy's document, so a create whose boundary names no managed policy of the account is
//! refused and the identity is NOT created. It is the same resolution, and the same
//! sentence, that `PutUserPermissionsBoundary` and `PutRolePermissionsBoundary` answer for
//! the same value, and it holds whichever of the two identity kinds is being created.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over: which
//! identity is created, which store the ARN resolves through (a policy this account holds,
//! AWS's own catalog), and HOW a value fails to resolve — a well-formed policy ARN of this
//! account that names nothing, the right name under the wrong path, another account's
//! policy, an ARN of another service, an ARN of another IAM resource type, and a value that
//! is no ARN at all.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns, and the AWS-managed
//! policy ARNs are the ones `tests/all/user_permissions_boundary.rs` owns, so a policy is
//! named exactly one way across the suites.

use smithy_server_runtime::MultiServiceRouter;

use crate::role_lifecycle::{borrowed, optional, param};
use crate::user_lifecycle::{account_id, call, element, error_code, router};
use crate::user_permissions_boundary::AWS_MANAGED_POLICIES;

/// A managed policy document with a single statement, the form `CreatePolicy`'s own
/// documentation describes.
const POLICY_DOCUMENT: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"s3:GetObject","Resource":"*"}]}"#;

/// A trust policy `CreateRole` accepts, as `tests/all/role_lifecycle.rs` writes one.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"ec2.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The name of the customer-managed policy the account holds, which the rows that name a
/// held policy point at.
const HELD_POLICY: &str = "held-boundary";

/// The path that policy is filed under, so a row can name the same NAME under another path
/// and resolve to nothing.
const HELD_POLICY_PATH: &str = "/boundaries/";

/// Which identity a row creates: the two operations that take a `PermissionsBoundary` with
/// the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    User,
    Role,
}

impl Identity {
    /// The create, with the boundary the row names.
    async fn create(
        self,
        router: &MultiServiceRouter,
        name: &str,
        boundary: &str,
    ) -> (u16, String) {
        let mut params = match self {
            Self::User => vec![param("UserName", name)],
            Self::Role => vec![
                param("RoleName", name),
                param("AssumeRolePolicyDocument", TRUST_POLICY),
            ],
        };
        params.push(param("PermissionsBoundary", boundary));
        let action = match self {
            Self::User => "CreateUser",
            Self::Role => "CreateRole",
        };
        call(router, action, &borrowed(&params)).await
    }

    /// The read that says whether the account holds that identity.
    async fn read(self, router: &MultiServiceRouter, name: &str) -> (u16, String) {
        match self {
            Self::User => call(router, "GetUser", &[("UserName", name)]).await,
            Self::Role => call(router, "GetRole", &[("RoleName", name)]).await,
        }
    }
}

/// One row of the refusal table: how the value fails to resolve, the value itself, and the
/// refusal it must draw.
struct Refused {
    case: &'static str,
    boundary: String,
    status: u16,
    code: &'static str,
    message: String,
}

/// The sentence IAM refuses a boundary that names no policy with — `Scope ARN: ` and
/// ` does not exist or is not attachable.` around the value the request supplied, written
/// out here rather than read from the implementation's helper, because what the suite pins
/// is the text on the WIRE.
fn scope_not_attachable(arn: &str) -> String {
    format!("Scope ARN: {arn} does not exist or is not attachable.")
}

/// The sentence IAM refuses a value it cannot use as a managed policy's ARN with.
fn arn_not_valid(value: &str) -> String {
    format!("ARN {value} is not valid.")
}

/// The rows: every way a `PermissionsBoundary` can fail to name a managed policy of this
/// account, each with the refusal IAM answers it with.
fn refused_rows() -> Vec<Refused> {
    let account = account_id();
    vec![
        Refused {
            case: "a well-formed policy ARN of this account that names nothing",
            boundary: format!("arn:aws:iam::{account}:policy/never-created"),
            status: 404,
            code: "NoSuchEntity",
            message: scope_not_attachable(&format!("arn:aws:iam::{account}:policy/never-created")),
        },
        Refused {
            case: "the held policy's name under another path",
            boundary: format!("arn:aws:iam::{account}:policy/{HELD_POLICY}"),
            status: 404,
            code: "NoSuchEntity",
            message: scope_not_attachable(&format!("arn:aws:iam::{account}:policy/{HELD_POLICY}")),
        },
        Refused {
            case: "another account's policy",
            boundary: format!("arn:aws:iam::210987654321:policy{HELD_POLICY_PATH}{HELD_POLICY}"),
            status: 404,
            code: "NoSuchEntity",
            message: scope_not_attachable(&format!(
                "arn:aws:iam::210987654321:policy{HELD_POLICY_PATH}{HELD_POLICY}"
            )),
        },
        Refused {
            // The catalog is authoritative about WHERE a policy it carries lives, so a
            // published name under another path names no policy at all.
            case: "a published AWS-managed policy name under another path",
            boundary: "arn:aws:iam::aws:policy/service-role/PowerUserAccess".to_string(),
            status: 404,
            code: "NoSuchEntity",
            message: scope_not_attachable("arn:aws:iam::aws:policy/service-role/PowerUserAccess"),
        },
        Refused {
            case: "an ARN of another service",
            boundary: "arn:aws:s3:::a-bucket-of-objects".to_string(),
            status: 400,
            code: "InvalidInput",
            message: arn_not_valid("arn:aws:s3:::a-bucket-of-objects"),
        },
        Refused {
            case: "an ARN of another IAM resource type",
            boundary: format!("arn:aws:iam::{account}:user/not-a-policy"),
            status: 400,
            code: "InvalidInput",
            message: arn_not_valid(&format!("arn:aws:iam::{account}:user/not-a-policy")),
        },
        Refused {
            // `arnType` is len 20..2048, so the value has to be long enough to reach the
            // handler at all; what makes it no ARN is that it carries none of the frame.
            case: "a value that is no ARN at all",
            boundary: "not-an-arn-but-long-enough-to-pass-the-length-bound".to_string(),
            status: 400,
            code: "ValidationError",
            message: arn_not_valid("not-an-arn-but-long-enough-to-pass-the-length-bound"),
        },
    ]
}

/// Create the customer-managed policy the "resolves" rows name, answering its ARN.
async fn given_policy(router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        "CreatePolicy",
        &[
            ("PolicyName", HELD_POLICY),
            ("Path", HELD_POLICY_PATH),
            ("PolicyDocument", POLICY_DOCUMENT),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture CreatePolicy must succeed, got {status}: {body}"
    );
    element(&body, "Arn")
}

/// RULE: a boundary that RESOLVES — to a policy this account holds, or to one AWS's catalog
/// publishes — is accepted and reported back on a read of the identity, so the refusals
/// below are about resolution and not about the member being carried at all.
#[tokio::test]
async fn a_boundary_naming_a_held_policy_is_accepted_and_read_back() {
    let router = router();
    let held = given_policy(&router).await;

    let mut resolving: Vec<String> = vec![held];
    resolving.extend(AWS_MANAGED_POLICIES.iter().map(|arn| (*arn).to_string()));

    for identity in [Identity::User, Identity::Role] {
        for (index, boundary) in resolving.iter().enumerate() {
            let case = format!("{identity:?} bounded by {boundary}");
            let name = format!("bounded-{}-{index}", format!("{identity:?}").to_lowercase());
            let (status, body) = identity.create(&router, &name, boundary).await;
            assert_eq!(
                status, 200,
                "case {case:?}: a boundary naming a held policy must be accepted, got \
                 {status}: {body}"
            );
            assert_eq!(
                element(&body, "PermissionsBoundaryArn"),
                *boundary,
                "case {case:?}: the create must report the boundary it stored, got {body}"
            );

            let (status, read) = identity.read(&router, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the created identity must be readable, got {status}: {read}"
            );
            assert_eq!(
                optional(&read, "PermissionsBoundaryArn").as_deref(),
                Some(boundary.as_str()),
                "case {case:?}: the read must report the stored boundary, got {read}"
            );
        }
    }
}

/// RULE: a boundary that resolves to NO managed policy of the account is refused — under
/// the code for the layer that failed — and the identity is not created, whichever identity
/// the request was creating.
#[tokio::test]
async fn a_boundary_that_resolves_to_nothing_is_refused_and_creates_nothing() {
    let router = router();
    given_policy(&router).await;

    for identity in [Identity::User, Identity::Role] {
        for (index, row) in refused_rows().into_iter().enumerate() {
            let case = format!("{identity:?}: {}", row.case);
            let name = format!("dangling-{}-{index}", format!("{identity:?}").to_lowercase());
            let (status, body) = identity.create(&router, &name, &row.boundary).await;
            assert_eq!(
                status, row.status,
                "case {case:?}: expected HTTP {}, got {status}: {body}",
                row.status
            );
            assert_eq!(
                error_code(&body),
                row.code,
                "case {case:?}: expected the error code {:?}, got {:?}: {body}",
                row.code,
                error_code(&body)
            );
            assert_eq!(
                element(&body, "Message"),
                row.message,
                "case {case:?}: expected the message {:?}, got {:?}",
                row.message,
                element(&body, "Message")
            );

            // Nothing was created: the account holds no identity under that name.
            let (status, read) = identity.read(&router, &name).await;
            assert_eq!(
                status, 404,
                "case {case:?}: a refused create must create nothing, got {status}: {read}"
            );
        }
    }
}

/// RULE: the refusal is about the BOUNDARY and not about the name, so a create that drops
/// the boundary is served for the very name the refused create named — the refused request
/// left no trace of itself.
#[tokio::test]
async fn the_name_a_refused_create_named_is_still_free() {
    let router = router();
    let account = account_id();
    let dangling = format!("arn:aws:iam::{account}:policy/also-never-created");

    for identity in [Identity::User, Identity::Role] {
        let name = format!("retried-{}", format!("{identity:?}").to_lowercase());
        let (status, body) = identity.create(&router, &name, &dangling).await;
        assert!(
            (400..500).contains(&status),
            "case {identity:?}: the dangling boundary must be refused, got {status}: {body}"
        );

        let params = match identity {
            Identity::User => vec![param("UserName", &name)],
            Identity::Role => vec![
                param("RoleName", &name),
                param("AssumeRolePolicyDocument", TRUST_POLICY),
            ],
        };
        let action = match identity {
            Identity::User => "CreateUser",
            Identity::Role => "CreateRole",
        };
        let (status, body) = call(&router, action, &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {identity:?}: the name the refused create named must still be free, got \
             {status}: {body}"
        );
        assert!(
            optional(&body, "PermissionsBoundaryArn").is_none(),
            "case {identity:?}: the identity created without a boundary must carry none, got \
             {body}"
        );
    }
}
