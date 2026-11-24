// SPDX-License-Identifier: CC0-1.0

//! The `iam:PermissionsBoundary` condition key, decided from the boundary the REQUEST sets.
//!
//! RULE: for every operation whose input carries a `PermissionsBoundary` parameter, IAM fills
//! the condition key `iam:PermissionsBoundary` from the value that parameter carries — "checks
//! that the specified policy is attached as permissions boundary on the IAM principal
//! resource" — and publishes the key for such a call whether or not it sets one. So an
//! identity policy that grants the action only under a clause over that key admits exactly
//! the calls that set the boundary the clause names and withholds the grant from a call that
//! sets another or none, while a `Deny` under a negated clause refuses those instead; the key
//! is read off the request's own parameter and off nothing stored, so it decides the same way
//! for every entity the call is taken on.
//!
//! The rows span what the rule generalises over: which operation carries the parameter (the
//! two creates, where it is optional, and the two boundary puts, where it is required), what
//! the request sets (the named boundary, another boundary of the account, nothing at all),
//! which condition operator the clause is written under (`StringEquals`, `ArnEquals`, and the
//! negated `StringNotEquals` inside a `Deny`), and whether the decision is a withheld grant or
//! an explicit deny.
//!
//! The awsQuery harness and the fixture principal are the ones `tests/all/user_lifecycle.rs`
//! and `tests/all/identity_policy_enforcement.rs` own, so a request is built and read exactly
//! one way.

use smithy_server_runtime::MultiServiceRouter;

use crate::identity_policy_enforcement::principal;
use crate::user_lifecycle::{account_id, call, element, error_code, router};

/// The condition key under test, as an identity policy spells it.
const BOUNDARY_KEY: &str = "iam:PermissionsBoundary";

/// The four actions a delegating document grants here: every operation of this service whose
/// input carries a `PermissionsBoundary` parameter
/// (`smithy_query(query="operation",operation="CreateUser,CreateRole,PutUserPermissionsBoundary,PutRolePermissionsBoundary")`).
const DELEGATED_ACTIONS: &[&str] = &[
    "iam:CreateUser",
    "iam:CreateRole",
    "iam:PutUserPermissionsBoundary",
    "iam:PutRolePermissionsBoundary",
];

/// A trust policy admitting the account root, for the rows that create a role.
const TRUST_POLICY: &str = concat!(
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","#,
    r#""Principal":{"AWS":"arn:aws:iam::000000000000:root"},"Action":"sts:AssumeRole"}]}"#
);

/// A document the rows attach as a BOUNDARY: a ceiling wide enough that nothing the rows do
/// is capped by it, since what is under test is which boundary the request NAMES.
const BOUNDARY_DOCUMENT: &str =
    r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#;

/// A delegating document granting [`DELEGATED_ACTIONS`] on every resource under one clause
/// over [`BOUNDARY_KEY`], written with the condition operator `operator`.
fn granted_under(operator: &str, boundary_arn: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":{},"Resource":"*","Condition":{{"{operator}":{{"{BOUNDARY_KEY}":"{boundary_arn}"}}}}}}]}}"#,
        actions()
    )
}

/// A document allowing [`DELEGATED_ACTIONS`] outright and DENYING them wherever the request's
/// boundary is not `boundary_arn` — the other half of the delegation idiom, written with the
/// negated operator, which holds on a request that sets no boundary at all.
fn denied_unless(boundary_arn: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":{0},"Resource":"*"}},{{"Effect":"Deny","Action":{0},"Resource":"*","Condition":{{"StringNotEquals":{{"{BOUNDARY_KEY}":"{boundary_arn}"}}}}}}]}}"#,
        actions()
    )
}

/// [`DELEGATED_ACTIONS`] as a JSON action element.
fn actions() -> String {
    let quoted: Vec<String> = DELEGATED_ACTIONS
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// Which boundary a row's request sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sets {
    /// The boundary the delegating clause names.
    Required,
    /// Another managed policy of the account.
    Another,
    /// No boundary at all, which only the creates can do.
    Nothing,
}

/// What a row expects of the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the clause holds for the boundary the request set.
    Served,
    /// HTTP 403 `AccessDenied`: nothing allows the call under this boundary.
    NoAllow,
    /// HTTP 403 `AccessDenied`: a statement about the request denies it under this boundary.
    ExplicitDeny,
}

/// One row: how the delegating document is written, which operation is called, what boundary
/// the request sets, and the answer the rule requires.
struct Row {
    case: &'static str,
    /// The condition operator the grant is written under, or `None` for the `Deny` idiom.
    operator: Option<&'static str>,
    action: &'static str,
    sets: Sets,
    expect: Expect,
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a StringEquals grant, creating a user inside the required boundary",
            operator: Some("StringEquals"),
            action: "CreateUser",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "a StringEquals grant, creating a user inside another boundary",
            operator: Some("StringEquals"),
            action: "CreateUser",
            sets: Sets::Another,
            expect: Expect::NoAllow,
        },
        Row {
            case: "a StringEquals grant, creating a user with no boundary at all",
            operator: Some("StringEquals"),
            action: "CreateUser",
            sets: Sets::Nothing,
            expect: Expect::NoAllow,
        },
        Row {
            case: "a StringEquals grant, creating a role inside the required boundary",
            operator: Some("StringEquals"),
            action: "CreateRole",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "a StringEquals grant, creating a role inside another boundary",
            operator: Some("StringEquals"),
            action: "CreateRole",
            sets: Sets::Another,
            expect: Expect::NoAllow,
        },
        Row {
            case: "a StringEquals grant, putting the required boundary on a held user",
            operator: Some("StringEquals"),
            action: "PutUserPermissionsBoundary",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "a StringEquals grant, putting another boundary on a held user",
            operator: Some("StringEquals"),
            action: "PutUserPermissionsBoundary",
            sets: Sets::Another,
            expect: Expect::NoAllow,
        },
        Row {
            case: "a StringEquals grant, putting the required boundary on a held role",
            operator: Some("StringEquals"),
            action: "PutRolePermissionsBoundary",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "a StringEquals grant, putting another boundary on a held role",
            operator: Some("StringEquals"),
            action: "PutRolePermissionsBoundary",
            sets: Sets::Another,
            expect: Expect::NoAllow,
        },
        Row {
            case: "an ArnEquals grant, creating a user inside the required boundary",
            operator: Some("ArnEquals"),
            action: "CreateUser",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "an ArnEquals grant, creating a user inside another boundary",
            operator: Some("ArnEquals"),
            action: "CreateUser",
            sets: Sets::Another,
            expect: Expect::NoAllow,
        },
        Row {
            case: "a deny of every other boundary, creating a user inside the required one",
            operator: None,
            action: "CreateUser",
            sets: Sets::Required,
            expect: Expect::Served,
        },
        Row {
            case: "a deny of every other boundary, creating a user inside another",
            operator: None,
            action: "CreateUser",
            sets: Sets::Another,
            expect: Expect::ExplicitDeny,
        },
        Row {
            case: "a deny of every other boundary, creating a user with none",
            operator: None,
            action: "CreateUser",
            sets: Sets::Nothing,
            expect: Expect::ExplicitDeny,
        },
        Row {
            case: "a deny of every other boundary, putting the required one on a held role",
            operator: None,
            action: "PutRolePermissionsBoundary",
            sets: Sets::Required,
            expect: Expect::Served,
        },
    ]
}

/// Create a customer-managed policy of the account holding [`BOUNDARY_DOCUMENT`] and answer
/// its ARN.
async fn boundary_policy(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, body) = call(
        router,
        "CreatePolicy",
        &[("PolicyName", name), ("PolicyDocument", BOUNDARY_DOCUMENT)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture CreatePolicy must succeed, got {status}: {body}"
    );
    element(&body, "Arn")
}

/// RULE: the clause over `iam:PermissionsBoundary` is decided from the boundary the request
/// sets, for every operation that carries such a parameter.
#[tokio::test]
async fn the_boundary_the_request_sets_decides_the_conditioned_grant() {
    let router = router();
    let required = boundary_policy(&router, "the fixtures", "required-boundary").await;
    let another = boundary_policy(&router, "the fixtures", "another-boundary").await;

    for (position, row) in rows().into_iter().enumerate() {
        let document = match row.operator {
            Some(operator) => granted_under(operator, &required),
            None => denied_unless(&required),
        };
        let delegate = format!("delegate{position}");
        let access_key_id = principal(
            &router,
            row.case,
            &delegate,
            &[("delegation".to_string(), document)],
        )
        .await;

        // The entity a boundary PUT is taken on has to exist, so each row that puts one is
        // given its own target; a create addresses the entity it is about to mint.
        let target = format!("target{position}");
        if row.action == "PutUserPermissionsBoundary" {
            let (status, body) = call(&router, "CreateUser", &[("UserName", &target)]).await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture CreateUser must succeed, got {status}: {body}",
                row.case
            );
        }
        if row.action == "PutRolePermissionsBoundary" {
            let (status, body) = call(
                &router,
                "CreateRole",
                &[
                    ("RoleName", &target),
                    ("AssumeRolePolicyDocument", TRUST_POLICY),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?}: the fixture CreateRole must succeed, got {status}: {body}",
                row.case
            );
        }

        let boundary = match row.sets {
            Sets::Required => Some(required.as_str()),
            Sets::Another => Some(another.as_str()),
            Sets::Nothing => None,
        };
        let created = format!("created{position}");
        let mut params: Vec<(&str, &str)> = match row.action {
            "CreateUser" => vec![("UserName", &created)],
            "CreateRole" => vec![
                ("RoleName", &created),
                ("AssumeRolePolicyDocument", TRUST_POLICY),
            ],
            "PutUserPermissionsBoundary" => vec![("UserName", &target)],
            _ => vec![("RoleName", &target)],
        };
        if let Some(boundary) = boundary {
            params.push(("PermissionsBoundary", boundary));
        }

        let (status, body) = crate::user_lifecycle::call_signed_by(
            &router,
            Some(&access_key_id),
            row.action,
            &params,
        )
        .await;

        match row.expect {
            Expect::Served => {
                assert_eq!(
                    status, 200,
                    "case {:?}: expected the conditioned grant to admit the call, got \
                     {status}: {body}",
                    row.case
                );
            }
            Expect::NoAllow | Expect::ExplicitDeny => {
                assert_eq!(
                    status, 403,
                    "case {:?}: expected HTTP 403, got {status}: {body}",
                    row.case
                );
                assert_eq!(
                    error_code(&body),
                    "AccessDenied",
                    "case {:?}: expected the error code AccessDenied, got {:?}: {body}",
                    row.case,
                    error_code(&body)
                );
                let reason = if row.expect == Expect::ExplicitDeny {
                    "with an explicit deny in an identity-based policy".to_string()
                } else {
                    format!(
                        "because no identity-based policy allows the iam:{} action",
                        row.action
                    )
                };
                let message = element(&body, "Message");
                assert!(
                    message.contains(&reason),
                    "case {:?}: expected the refusal to close with {reason:?}, got {message:?}",
                    row.case
                );
                assert!(
                    message.contains(&format!("arn:aws:iam::{}:user/{delegate}", account_id())),
                    "case {:?}: expected the refusal to name the delegate that signed it, got \
                     {message:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE: the key is read off the REQUEST's parameter and off nothing stored, so a delegate
/// granted one boundary can set it on one entity after another — the grant is not consumed —
/// and the entity the call named is the one that carries it afterwards.
#[tokio::test]
async fn the_conditioned_grant_admits_every_call_that_sets_the_named_boundary() {
    let router = router();
    let required = boundary_policy(&router, "repeated puts", "required-boundary").await;
    let another = boundary_policy(&router, "repeated puts", "another-boundary").await;
    let access_key_id = principal(
        &router,
        "repeated puts",
        "repeater",
        &[(
            "delegation".to_string(),
            granted_under("StringEquals", &required),
        )],
    )
    .await;

    // Three rounds, each creating a user inside the required boundary and then failing to
    // create one outside it: a grant that held only for the first call would fail here.
    for round in 1..=3 {
        let inside = format!("inside{round}");
        let (status, body) = crate::user_lifecycle::call_signed_by(
            &router,
            Some(&access_key_id),
            "CreateUser",
            &[("UserName", &inside), ("PermissionsBoundary", &required)],
        )
        .await;
        assert_eq!(
            status, 200,
            "round {round}: expected the create inside the required boundary to be served, \
             got {status}: {body}"
        );
        assert_eq!(
            element(&body, "PermissionsBoundaryArn"),
            required,
            "round {round}: expected the created user to carry the boundary the call set, \
             got {body}"
        );

        let outside = format!("outside{round}");
        let (status, body) = crate::user_lifecycle::call_signed_by(
            &router,
            Some(&access_key_id),
            "CreateUser",
            &[("UserName", &outside), ("PermissionsBoundary", &another)],
        )
        .await;
        assert_eq!(
            status, 403,
            "round {round}: expected the create outside the required boundary to be refused, \
             got {status}: {body}"
        );

        // And the refused create left nothing behind: the user it named does not exist.
        let (status, body) = call(&router, "GetUser", &[("UserName", &outside)]).await;
        assert_eq!(
            status, 404,
            "round {round}: a refused create must leave no user behind, got {status}: {body}"
        );
    }
}
