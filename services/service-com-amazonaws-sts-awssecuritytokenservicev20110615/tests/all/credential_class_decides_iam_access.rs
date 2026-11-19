// SPDX-License-Identifier: CC0-1.0

//! What the CLASS of the credential a request presents decides about a call to IAM.
//!
//! The rule under test: **which calls a temporary credential may make is decided by the CLASS
//! of credential it is, not by the documents in force on the identity behind it — and two of
//! the classes STS vends may not call IAM at all.** "You cannot call any IAM operations using
//! the Amazon Web Services CLI or the Amazon Web Services API" (`GetFederationToken`, this
//! service's model) bans a federated-user credential outright, and with a `GetSessionToken`
//! credential "you cannot call any IAM API operations unless MFA authentication information is
//! included in the request" (`GetSessionToken`) bans one whose mint presented no challenge.
//! The ban is a fact about the credential rather than a document, so no `Allow` anywhere lifts
//! it: a federated-user session whose own session policy and whose minting user both allow
//! `iam:*` is refused exactly as one that allows nothing is.
//!
//! The same class decides what the global `aws:PrincipalType` condition key answers —
//! `AssumedRole` for a role session, `FederatedUser` for a federated-user session and `User`
//! for an IAM user's own credential, which is what both a long-term access key and the session
//! `GetSessionToken` vends for that user are — so a statement conditioned on that key selects
//! one class and withholds the grant from the others.
//!
//! The rows differ along what the rule generalises over: WHICH class signs (a long-term access
//! key, a role session, a federated-user session, a `GetSessionToken` session minted with and
//! without an MFA challenge), and — for the condition-key rows — which class the clause names.
//!
//! Both halves need IAM mounted beside this service on the one shared engine: the identities,
//! their documents and the gate that decides the call are IAM's, while the credential and its
//! class are this service's. The MFA device is provisioned and enabled through IAM's own
//! operations and the pass code is computed here, from RFC 6238's definition — the arithmetic
//! the holder of the device does.

use std::time::SystemTime;

use smithy_server_runtime::MultiServiceRouter;

// The enrolment of a virtual MFA device and the pass code it emits, written ONCE in the suite
// whose rule is the challenge itself (`mfa_challenged_mints`) and called here: this suite needs
// a credential whose mint carried MFA, not a second computation of RFC 6238.
use super::mfa_challenged_mints::{current_step, given_enabled_device, pass_code};
use super::sts_sessions::{call, element, emulator_account, emulator_with_iam, member, query};

/// IAM's awsQuery API version, the `Version` every call to it carries.
const IAM_API_VERSION: &str = "2010-05-08";

/// IAM's SigV4 signing name, which is also how these calls are routed to it.
const IAM_SIGNING_NAME: &str = "iam";

/// The access key id standing for the account's own published credential, which the setup
/// calls are made with: neither store holds a record of it, so it speaks for the account
/// itself — the position an administrator is in.
const ACCOUNT_KEY: &str = "AKIDEMULATORCLASSES";

/// The IAM operation every row calls, and the action it is authorized under.
const LIST_USERS: &str = "ListUsers";
const LIST_USERS_ACTION: &str = "iam:ListUsers";

/// One IAM awsQuery call, presented as signed with `access_key_id`.
async fn iam_call(
    router: &MultiServiceRouter,
    access_key_id: &str,
    action: &str,
    params: &[(&str, String)],
) -> (u16, String) {
    query(
        router,
        access_key_id,
        IAM_SIGNING_NAME,
        IAM_API_VERSION,
        action,
        params,
    )
    .await
}

/// One policy document stating `statements`.
fn document(statements: &[String]) -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        statements.join(",")
    )
}

/// One statement: `effect` on `action` over every resource, carrying `condition` where a case
/// names one.
fn statement(effect: &str, action: &str, condition: Option<&str>) -> String {
    let clause = condition.map_or(String::new(), |condition| {
        format!(r#","Condition":{condition}"#)
    });
    format!(r#"{{"Effect":"{effect}","Action":"{action}","Resource":"*"{clause}}}"#)
}

/// A `StringEquals` clause over the condition key naming the calling principal's class.
fn principal_type_clause(class: &str) -> String {
    format!(r#"{{"StringEquals":{{"aws:PrincipalType":"{class}"}}}}"#)
}

/// Create the IAM user `user_name` holding `inline` as its only policy, mint it a long-term
/// access key, and answer that key's id.
async fn given_user(
    router: &MultiServiceRouter,
    case: &str,
    user_name: &str,
    inline: &str,
) -> String {
    let (status, body) = iam_call(
        router,
        ACCOUNT_KEY,
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateUser({user_name:?}) must succeed, got {status}: {body}"
    );
    let (status, body) = iam_call(
        router,
        ACCOUNT_KEY,
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "granted".to_string()),
            ("PolicyDocument", inline.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutUserPolicy must succeed, got {status}: {body}"
    );
    let (status, body) = iam_call(
        router,
        ACCOUNT_KEY,
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAccessKey must succeed, got {status}: {body}"
    );
    member(case, &body, "AccessKeyId")
}

/// Create the role `role_name`, trusted by the account's root and holding `inline` as its only
/// policy, and answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &str,
) -> String {
    let account = emulator_account();
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    let (status, body) = iam_call(
        router,
        ACCOUNT_KEY,
        "CreateRole",
        &[
            ("RoleName", role_name.to_string()),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role_name:?}) must succeed, got {status}: {body}"
    );
    let arn = member(case, &body, "Arn");
    let (status, body) = iam_call(
        router,
        ACCOUNT_KEY,
        "PutRolePolicy",
        &[
            ("RoleName", role_name.to_string()),
            ("PolicyName", "granted".to_string()),
            ("PolicyDocument", inline.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy must succeed, got {status}: {body}"
    );
    arn
}

/// Assume `role_arn` as `access_key_id` and answer the access key id of the session.
async fn assumed_role_session(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    role_arn: &str,
) -> String {
    let (status, body) = call(
        router,
        access_key_id,
        "AssumeRole",
        &[
            ("RoleArn", role_arn.to_string()),
            ("RoleSessionName", "classes".to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole must succeed, got {status}: {body}"
    );
    member(case, &body, "AccessKeyId")
}

/// Mint a federated-user session as `access_key_id`, passing `policy` where a case names one,
/// and answer the access key id of the session.
async fn federated_session(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    name: &str,
    policy: Option<&str>,
) -> String {
    let mut params = vec![("Name", name.to_string())];
    if let Some(policy) = policy {
        params.push(("Policy", policy.to_string()));
    }
    let (status, body) = call(router, access_key_id, "GetFederationToken", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFederationToken must succeed, got {status}: {body}"
    );
    member(case, &body, "AccessKeyId")
}

/// Mint a `GetSessionToken` session as `access_key_id`, presenting the MFA challenge
/// `challenge` where a case names one, and answer the access key id of the session.
async fn session_token(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    challenge: Option<(&str, String)>,
) -> String {
    let mut params = Vec::new();
    if let Some((serial_number, token_code)) = challenge {
        params.push(("SerialNumber", serial_number.to_string()));
        params.push(("TokenCode", token_code));
    }
    let (status, body) = call(router, access_key_id, "GetSessionToken", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetSessionToken must succeed, got {status}: {body}"
    );
    member(case, &body, "AccessKeyId")
}

/// What a row expects of the IAM call made with its credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// IAM serves it.
    Served,
    /// IAM refuses it, naming the principal the credential speaks for.
    Refused,
}

/// RULE: a credential of a class AWS bans from IAM is refused every IAM call whatever any
/// document allows, while a credential of a class under no such ban is decided on the
/// documents in force on it.
#[tokio::test]
async fn the_class_of_a_credential_decides_whether_it_may_call_iam() {
    let (router, _engine) = emulator_with_iam();
    let case = "the classes of credential calling iam:ListUsers";
    // The user every credential below stands on, allowed every IAM and STS action: so what
    // refuses a row is its credential's CLASS and nothing else.
    let allow_everything = document(&[
        statement("Allow", "iam:*", None),
        statement("Allow", "sts:*", None),
    ]);
    let user_key = given_user(&router, case, "ClassOperator", &allow_everything).await;
    let role_arn = given_role(&router, case, "ClassRole", &allow_everything).await;
    let (serial_number, seed) =
        given_enabled_device(&router, "ClassOperator", "class-operator-device").await;
    let user_arn = format!("arn:aws:iam::{}:user/ClassOperator", emulator_account());
    let federated_arn = format!(
        "arn:aws:sts::{}:federated-user/granted",
        emulator_account()
    );
    let session_arn = format!(
        "arn:aws:sts::{}:assumed-role/ClassRole/classes",
        emulator_account()
    );

    // Every row's credential carries a grant of `iam:*` — through the user's own document, the
    // role's, or the session policy the mint passed — so the only thing that differs is the
    // class of credential presented.
    let rows: Vec<(&str, String, String, Expect)> = vec![
        (
            "an IAM user's own long-term access key",
            user_key.clone(),
            user_arn.clone(),
            Expect::Served,
        ),
        (
            "a session of a role whose policy allows the call",
            assumed_role_session(&router, case, &user_key, &role_arn).await,
            session_arn,
            Expect::Served,
        ),
        (
            "a federated-user session whose session policy allows the call",
            federated_session(
                &router,
                case,
                &user_key,
                "granted",
                Some(&document(&[statement("Allow", "iam:*", None)])),
            )
            .await,
            federated_arn,
            Expect::Refused,
        ),
        (
            "a GetSessionToken session minted with no MFA challenge",
            session_token(&router, case, &user_key, None).await,
            user_arn.clone(),
            Expect::Refused,
        ),
        (
            "a GetSessionToken session minted against the device's own code",
            session_token(
                &router,
                case,
                &user_key,
                Some((
                    serial_number.as_str(),
                    pass_code(&seed, current_step(SystemTime::now())),
                )),
            )
            .await,
            user_arn,
            Expect::Served,
        ),
    ];

    for (row, access_key_id, principal, expected) in rows {
        let (status, body) = iam_call(&router, &access_key_id, LIST_USERS, &[]).await;
        match expected {
            Expect::Served => assert_eq!(
                status, 200,
                "case {row:?}: IAM must serve the call, got {status}: {body}"
            ),
            Expect::Refused => {
                assert_eq!(
                    status, 403,
                    "case {row:?}: IAM must refuse the call, got {status}: {body}"
                );
                let code = element(&body, "Code").unwrap_or_default();
                assert_eq!(
                    code, "AccessDenied",
                    "case {row:?}: expected AccessDenied, got {code:?}: {body}"
                );
                let message = element(&body, "Message").unwrap_or_default();
                let opening = format!(
                    "User: {principal} is not authorized to perform: {LIST_USERS_ACTION}"
                );
                assert!(
                    message.starts_with(&opening),
                    "case {row:?}: expected the refusal to open {opening:?}, got {message:?}"
                );
                assert!(
                    message.contains(&format!(
                        "because no identity-based policy allows the {LIST_USERS_ACTION} action"
                    )),
                    "case {row:?}: expected the no-grant diagnosis, got {message:?}"
                );
            }
        }
    }
}

/// RULE: `aws:PrincipalType` answers the CLASS of the credential the request was signed with,
/// so a grant conditioned on one class is in force for that class and withholds the grant from
/// every other.
#[tokio::test]
async fn the_principal_type_key_selects_on_the_class_of_the_credential() {
    // The three values the key takes, written out from the AWS General Reference's global
    // condition keys rather than read back from the implementation.
    const CLASSES: [&str; 3] = ["AssumedRole", "FederatedUser", "User"];

    let (router, _engine) = emulator_with_iam();
    let case = "aws:PrincipalType deciding an iam:ListUsers grant";
    // The user may mint every class of credential; its grant of the call itself is conditioned
    // per row below.
    let user_key = given_user(
        &router,
        case,
        "TypedOperator",
        &document(&[statement("Allow", "sts:*", None)]),
    )
    .await;

    for named in CLASSES {
        // One role per row, whose only grant of the call holds for the class the clause names.
        let role_name = format!("Typed{named}Role");
        let role_arn = given_role(
            &router,
            case,
            &role_name,
            &document(&[
                statement("Allow", "sts:*", None),
                statement(
                    "Allow",
                    "iam:*",
                    Some(&principal_type_clause(named)),
                ),
            ]),
        )
        .await;
        let session = assumed_role_session(&router, case, &user_key, &role_arn).await;
        let (status, body) = iam_call(&router, &session, LIST_USERS, &[]).await;
        // A session of a role answers the key with `AssumedRole`, so the clause holds for that
        // one value and for neither of the others.
        let expected = if named == "AssumedRole" { 200 } else { 403 };
        assert_eq!(
            status, expected,
            "case {case:?}: a role session under a grant conditioned on {named:?} must answer \
             HTTP {expected}, got {status}: {body}"
        );
    }

    // And the same clause over the OTHER class an IAM user's credentials can be: a long-term
    // access key answers `User`, so a grant conditioned on `User` is in force for it while the
    // two session classes are not it.
    for named in CLASSES {
        let user_name = format!("Typed{named}User");
        let key = given_user(
            &router,
            case,
            &user_name,
            &document(&[
                statement("Allow", "sts:*", None),
                statement(
                    "Allow",
                    "iam:*",
                    Some(&principal_type_clause(named)),
                ),
            ]),
        )
        .await;
        let (status, body) = iam_call(&router, &key, LIST_USERS, &[]).await;
        let expected = if named == "User" { 200 } else { 403 };
        assert_eq!(
            status, expected,
            "case {case:?}: a long-term access key under a grant conditioned on {named:?} must \
             answer HTTP {expected}, got {status}: {body}"
        );
    }
}
