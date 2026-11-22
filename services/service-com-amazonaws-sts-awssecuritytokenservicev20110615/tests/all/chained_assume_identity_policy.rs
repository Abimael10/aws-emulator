// SPDX-License-Identifier: CC0-1.0

//! What the documents in force on the CALLER decide about an assume, and the condition keys
//! the calling session's own tags answer for.
//!
//! The rule under test: **an explicit `Deny` in the identity-based documents in force on the
//! caller refuses a mint whatever the target role's trust policy allows, and such a clause is
//! decided against the calling session's own tags, which the mint publishes as
//! `aws:PrincipalTag/<key>`.** An explicit deny "overrides any allow" (AWS, *Policy
//! evaluation logic*), and the documents in force on a session are the ROLE's own — "the
//! temporary security credentials … have the permissions of the role" (`AssumeRole`, this
//! service's model) — so an `AssumeRole` made FROM a session, which is role chaining, is an
//! action of the calling role and a `Deny` naming the target role cuts the chain off. Session
//! tags "can be used … to control access to resources" (`AssumeRoleRequest$Tags`), which is
//! what makes a clause over `aws:PrincipalTag/<key>` decide for a tagged session rather than
//! stay undecided — the tag-gated delegation pattern in both directions: the clause that
//! names the tag the session carries bites, and the one that names a value it does not carry
//! does not.
//!
//! An ABSENT grant is deliberately not the same thing: a role trust policy is a
//! resource-based policy, and within one account such a policy can admit a principal no
//! identity-based document names, so what is asserted here is the DENY verdict and the
//! positive controls either side of it.
//!
//! The rows differ along what the rule generalises over: what the caller's documents say
//! about the target role (nothing, an `Allow` conditioned on the tag the session carries, an
//! unconditioned `Deny`, a `Deny` conditioned on the tag the session carries, a `Deny`
//! conditioned on a tag VALUE it does not carry), and WHICH credential the assume is made
//! with (a session of a role, carrying its session tags, and an IAM user's own long-term
//! access key, which carries none). Each refusal row is bracketed by a serving one, so the
//! chain is shown to work immediately before and after it is cut off — the transition the
//! rule is about.
//!
//! IAM is mounted beside this service on one shared engine: the roles, the user and every
//! document are written through IAM's own operations, so the documents these decisions read
//! are the ones `iam:PutRolePolicy` stored.

use super::sts_sessions::{call, element, emulator_account, emulator_with_iam, member, query};

/// IAM's awsQuery API version and signing name, for the setup calls.
const IAM_API_VERSION: &str = "2010-05-08";
const IAM_SIGNING_NAME: &str = "iam";

/// The access key id the setup calls are made with: the account's own credential, which no
/// identity-based document bounds.
const ACCOUNT_KEY: &str = "AKIDEMULATORCHAINING";

/// The action a chained assume asks to perform, as a policy names it.
const ASSUME_ROLE_ACTION: &str = "sts:AssumeRole";

/// The tag the delegating role's clauses are written over, and the value the tagged session
/// carries.
const TAG_KEY: &str = "department";
const TAG_VALUE: &str = "engineering";

/// One IAM awsQuery call as the account.
async fn iam_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    query(
        router,
        ACCOUNT_KEY,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        action,
        params,
    )
    .await
}

/// A trust policy admitting `principal` to assume the role, and to tag the session it mints.
fn trust(principal: &str) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"{principal}"}},"Action":["sts:AssumeRole","sts:TagSession"]}}]}}"#
    )
}

/// One statement document: `effect` on `ASSUME_ROLE_ACTION` over `resource`, carrying
/// `condition` where a row names one.
fn document(effect: &str, resource: &str, condition: Option<(&str, &str)>) -> String {
    let clause = condition.map_or(String::new(), |(key, value)| {
        format!(r#","Condition":{{"StringEquals":{{"aws:PrincipalTag/{key}":"{value}"}}}}"#)
    });
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"{effect}","Action":"{ASSUME_ROLE_ACTION}","Resource":"{resource}"{clause}}}]}}"#
    )
}

/// Create a role trusted by `principal`, and answer its ARN.
async fn given_role(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    principal: &str,
) -> String {
    let (status, body) = iam_call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust(principal)),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role_name:?}) must succeed, got {status}: {body}"
    );
    member(case, &body, "Arn")
}

/// Embed `policy` in `role_name` as its only inline policy — the write each row makes to put
/// the delegating role's documents in the state the row is about.
async fn put_role_policy(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    policy: &str,
) {
    let (status, body) = iam_call(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "delegation".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({role_name:?}) must succeed, got {status}: {body}"
    );
}

/// Embed `policy` in the user `user_name`, whose long-term key one row assumes with.
async fn put_user_policy(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    user_name: &str,
    policy: &str,
) {
    let (status, body) = iam_call(
        router,
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "delegation".to_string()),
            ("PolicyDocument", policy.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutUserPolicy({user_name:?}) must succeed, got {status}: {body}"
    );
}

/// Assume `role_arn` as `access_key_id`, carrying `tag` where a row names one.
async fn assume(
    router: &smithy_server_runtime::MultiServiceRouter,
    access_key_id: &str,
    role_arn: &str,
    session_name: &str,
    tag: Option<(&str, &str)>,
) -> (u16, String) {
    let mut params = vec![
        ("RoleArn", role_arn.to_string()),
        ("RoleSessionName", session_name.to_string()),
    ];
    if let Some((key, value)) = tag {
        params.push(("Tags.member.1.Key", key.to_string()));
        params.push(("Tags.member.1.Value", value.to_string()));
    }
    call(router, access_key_id, "AssumeRole", &params).await
}

/// What a row expects of the chained assume: the mint is served, or it is refused with the
/// sentence AWS words an unauthorized call in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    Served,
    Refused,
}

/// One row: what the delegating role's documents say, and what the chained assume answers.
struct Row {
    case: &'static str,
    /// The document embedded in role A before the chain is attempted — `None` leaves the
    /// role with no identity-based document of its own.
    policy: Option<(&'static str, Option<(&'static str, &'static str)>)>,
    expect: Expect,
}

/// RULE: a `Deny` of `sts:AssumeRole` in the documents in force on the calling session
/// refuses a chained assume the target role's trust policy admits, and such a clause is
/// decided against the tags the calling session carries.
#[tokio::test]
async fn an_identity_based_deny_refuses_a_chained_assume_the_trust_policy_admits() {
    let (router, _engine) = emulator_with_iam();
    let case = "a tag-gated chain from one role into another";
    let account = emulator_account();
    let root = format!("arn:aws:iam::{account}:root");

    // Role A delegates; role B is the one the chain targets, and its trust policy admits
    // role A — so every refusal below is the work of A's OWN documents.
    let first = given_role(&router, case, "ChainFirst", &root).await;
    let second = given_role(
        &router,
        case,
        "ChainSecond",
        &format!("arn:aws:iam::{account}:role/ChainFirst"),
    )
    .await;

    let rows = vec![
        Row {
            case: "role A carrying no document of its own",
            policy: None,
            expect: Expect::Served,
        },
        Row {
            case: "role A allowing the chain when the session carries the tag",
            policy: Some(("Allow", Some((TAG_KEY, TAG_VALUE)))),
            expect: Expect::Served,
        },
        Row {
            case: "role A denying the chain outright",
            policy: Some(("Deny", None)),
            expect: Expect::Refused,
        },
        Row {
            case: "role A allowing the chain again",
            policy: Some(("Allow", Some((TAG_KEY, TAG_VALUE)))),
            expect: Expect::Served,
        },
        Row {
            case: "role A denying the chain for the tag the session carries",
            policy: Some(("Deny", Some((TAG_KEY, TAG_VALUE)))),
            expect: Expect::Refused,
        },
        Row {
            case: "role A denying the chain for a tag value the session does not carry",
            policy: Some(("Deny", Some((TAG_KEY, "finance")))),
            expect: Expect::Served,
        },
    ];

    for Row {
        case,
        policy,
        expect,
    } in rows
    {
        match policy {
            Some((effect, condition)) => {
                put_role_policy(
                    &router,
                    case,
                    "ChainFirst",
                    &document(effect, &second, condition),
                )
                .await;
            }
            None => {
                let (status, body) = iam_call(
                    &router,
                    "DeleteRolePolicy",
                    &[
                        ("RoleName", "ChainFirst".to_string()),
                        ("PolicyName", "delegation".to_string()),
                    ],
                )
                .await;
                assert!(
                    status == 200 || status == 404,
                    "case {case:?}: clearing role A's document must answer, got {status}: \
                     {body}"
                );
            }
        }

        // A FRESH session of role A for every row, carrying the session tag the clauses are
        // written over: the documents in force are read at the instant of the chained call,
        // so each row is decided on the document it just wrote.
        let (status, body) = assume(
            &router,
            ACCOUNT_KEY,
            &first,
            "chaining",
            Some((TAG_KEY, TAG_VALUE)),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: assuming role A must succeed, got {status}: {body}"
        );
        let session_key = member(case, &body, "AccessKeyId");
        let session_arn = format!("arn:aws:sts::{account}:assumed-role/ChainFirst/chaining");

        let (status, body) = assume(&router, &session_key, &second, "chained", None).await;
        match expect {
            Expect::Served => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the chained assume must be served, got {status}: {body}"
                );
                let arn = member(case, &body, "Arn");
                assert!(
                    arn.contains(":assumed-role/ChainSecond/"),
                    "case {case:?}: the chained session must speak for the second role, got \
                     {arn:?}"
                );
            }
            Expect::Refused => {
                assert_eq!(
                    status, 403,
                    "case {case:?}: the chained assume must be refused, got {status}: {body}"
                );
                let code = element(&body, "Code").unwrap_or_default();
                assert_eq!(
                    code, "AccessDenied",
                    "case {case:?}: expected an authorization denial, got {code:?}: {body}"
                );
                let message = element(&body, "Message").unwrap_or_default();
                let expected = format!(
                    "User: {session_arn} is not authorized to perform: {ASSUME_ROLE_ACTION} \
                     on resource: {second}"
                );
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
            }
        }
    }
}

/// RULE: the same `Deny` in the documents of an IAM USER refuses the assume made with that
/// user's own long-term access key — the rule is about the documents in force on the caller,
/// whichever class of credential it presents.
#[tokio::test]
async fn an_identity_based_deny_refuses_an_assume_made_with_a_long_term_key() {
    let (router, _engine) = emulator_with_iam();
    let case = "an IAM user denied the assume by its own document";
    let account = emulator_account();
    let root = format!("arn:aws:iam::{account}:root");
    let role = given_role(&router, case, "LongTermTarget", &root).await;

    let (status, body) = iam_call(
        &router,
        "CreateUser",
        &[("UserName", "ChainOperator".to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUser must succeed, got {status}: {body}"
    );
    let user_arn = member(case, &body, "Arn");
    let (status, body) = iam_call(
        &router,
        "CreateAccessKey",
        &[("UserName", "ChainOperator".to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessKey must succeed, got {status}: {body}"
    );
    let user_key = member(case, &body, "AccessKeyId");

    // The positive control: the trust policy admits the account, so the user assumes it.
    let (status, body) = assume(&router, &user_key, &role, "longterm", None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the assume must be served before the deny, got {status}: {body}"
    );

    put_user_policy(
        &router,
        case,
        "ChainOperator",
        &document("Deny", &role, None),
    )
    .await;
    let (status, body) = assume(&router, &user_key, &role, "longterm", None).await;
    assert_eq!(
        status, 403,
        "case {case:?}: the denied assume must be refused, got {status}: {body}"
    );
    let message = element(&body, "Message").unwrap_or_default();
    let expected = format!(
        "User: {user_arn} is not authorized to perform: {ASSUME_ROLE_ACTION} on resource: \
         {role}"
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected {expected:?}, got {message:?}"
    );
}
