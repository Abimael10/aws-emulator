// SPDX-License-Identifier: CC0-1.0

//! What the CLASS of the credential a request presents decides at a DATA PLANE — the service
//! that holds the resource rather than the one that minted the credential.
//!
//! Two rules, both about the class and neither about the identity behind it:
//!
//! 1. **A `GetFederationToken` session that passed no session policy has no permissions at
//!    all.** "Though the session policy parameters are optional, if you do not pass a policy,
//!    then the resulting federated user session has no permissions" (`GetFederationToken`, the
//!    STS model). An empty scope therefore GRANTS NOTHING for that one class, where for every
//!    other credential — a long-term access key, a role session minted without session
//!    policies — it caps nothing: the same absence of documents means opposite things, and
//!    which it means is decided by the class of credential presented.
//! 2. **`aws:PrincipalType` answers the class of the credential the request was signed
//!    with** — `AssumedRole` for a role session, `FederatedUser` for a federated-user session,
//!    `User` for an IAM user's own credential — so a grant conditioned on one class is in
//!    force for that class and withholds the grant from every other. It is a fact about the
//!    CREDENTIAL and not about the identity: the same IAM user is behind all three.
//!
//! The rows differ along what the rules generalise over: WHICH class signs, whether the mint
//! passed a session policy, and — for the second rule — which of the three classes the clause
//! names, over every combination of the two.
//!
//! Three crates have to agree for any of it to hold — IAM holds the identities and their
//! documents, STS mints the credential and records its class, Amazon S3 resolves the
//! credential and decides the call — so this is the suite that can pin them. The harness is
//! the one `sts_session_credential_refusals` builds, called rather than copied.

use crate::sts_session_credential_refusals::{
    assume_role, build_router, code_of, create_role, document, get_object, message_of, no_grant,
    object_arn, put_role_policy, seed_bucket, session_arn, statement, sts, user_with_key, ACCOUNT,
    ASCII_KEY, GET_OBJECT,
};

use smithy_server_runtime::MultiServiceRouter;

/// The three values `aws:PrincipalType` takes for an AWS credential, written out from the AWS
/// General Reference's global condition keys rather than read back from the implementation.
const ASSUMED_ROLE: &str = "AssumedRole";
const FEDERATED_USER: &str = "FederatedUser";
const USER: &str = "User";

/// The clause naming which class of credential a grant is in force for.
fn principal_type_clause(class: &str) -> String {
    format!(r#"{{"StringEquals":{{"aws:PrincipalType":"{class}"}}}}"#)
}

/// The ARN a federated-user session named `name` speaks under.
fn federated_arn(name: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:federated-user/{name}")
}

/// Mint a federated-user session as `access_key_id`, passing `policy` where a case names one,
/// and answer the access key id of the session.
async fn federation_token(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    name: &str,
    policy: Option<&str>,
) -> String {
    let mut members = vec![("Name", name)];
    if let Some(policy) = policy {
        members.push(("Policy", policy));
    }
    let (status, body) = sts(router, access_key_id, "GetFederationToken", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetFederationToken must succeed, got {status}: {body}"
    );
    crate::query_wire::element(&body, "AccessKeyId")
}

/// RULE 1: an empty scope grants nothing for a federated-user session, whose permissions come
/// from its session policies alone, and caps nothing for every other credential.
#[tokio::test]
async fn an_empty_scope_grants_nothing_only_for_a_federated_user_session() {
    let router = build_router();
    let bucket = "class-scope-bucket";
    let case = "an empty scope read per class of credential";
    seed_bucket(&router, case, bucket).await;

    // The minting user may federate and may read the whole bucket: what a row's read turns on
    // is its credential's class and the scope its mint passed, not this document.
    let user_key = user_with_key(
        &router,
        case,
        "ScopeFederator",
        &document(&[
            statement("Allow", "sts:*", "*", None),
            statement("Allow", GET_OBJECT, &object_arn(bucket, "*"), None),
        ]),
    )
    .await;
    let role_name = "ScopeRole";
    create_role(&router, case, role_name).await;
    put_role_policy(
        &router,
        case,
        role_name,
        "read",
        &document(&[statement(
            "Allow",
            GET_OBJECT,
            &object_arn(bucket, "*"),
            None,
        )]),
    )
    .await;
    let scoped_policy = document(&[statement(
        "Allow",
        GET_OBJECT,
        &object_arn(bucket, "*"),
        None,
    )]);

    // (case, the access key id presented, the principal a refusal would name, served)
    let rows: Vec<(&str, String, String, bool)> = vec![
        (
            "a federated-user session minted with NO session policy",
            federation_token(&router, case, &user_key, "unscoped", None).await,
            federated_arn("unscoped"),
            false,
        ),
        (
            "a federated-user session whose session policy allows the read",
            federation_token(&router, case, &user_key, "scoped", Some(&scoped_policy)).await,
            federated_arn("scoped"),
            true,
        ),
        (
            "a role session minted with no session policy at all",
            assume_role(&router, case, role_name, None).await,
            session_arn(role_name),
            true,
        ),
        (
            "an IAM user's own long-term access key",
            user_key.clone(),
            format!("arn:aws:iam::{ACCOUNT}:user/ScopeFederator"),
            true,
        ),
    ];

    for (row, access_key_id, principal, served) in rows {
        let (status, body) = get_object(&router, &access_key_id, bucket, ASCII_KEY).await;
        if served {
            assert_eq!(
                status, 200,
                "case {row:?}: the read must be served, got {status}: {body}"
            );
            continue;
        }
        assert_eq!(
            status, 403,
            "case {row:?}: the read must be refused, got {status}: {body}"
        );
        let code = code_of(&body);
        assert_eq!(
            code, "AccessDenied",
            "case {row:?}: expected AccessDenied, got {code:?}: {body}"
        );
        let message = message_of(&body);
        let opening = format!("User: {principal} is not authorized to perform: {GET_OBJECT}");
        assert!(
            message.starts_with(&opening),
            "case {row:?}: expected the refusal to open {opening:?}, got {message:?}"
        );
        assert!(
            message.contains("because no session policy allows the s3:GetObject action"),
            "case {row:?}: expected the session-policy diagnosis, got {message:?}"
        );
        // Nothing is left granted by the class either: a second read answers the same way, so
        // the refusal is the scope's and not a consumed permission.
        let (status, _) = get_object(&router, &access_key_id, bucket, ASCII_KEY).await;
        assert_eq!(
            status, 403,
            "case {row:?}: the refusal must hold for a second read too, got {status}"
        );
    }
}

/// RULE 2: a grant conditioned on `aws:PrincipalType` is in force for the class of credential
/// the request was signed with, and withholds the grant from the other classes.
#[tokio::test]
async fn the_principal_type_key_selects_on_the_class_at_the_data_plane() {
    // The whole published catalogue of classes an AWS credential can be, each as the value a
    // clause names and the class of credential that answers it.
    const NAMED: [&str; 3] = [ASSUMED_ROLE, FEDERATED_USER, USER];

    for (index, named) in NAMED.into_iter().enumerate() {
        let router = build_router();
        let bucket = format!("class-condition-bucket-{index}");
        let case = format!("a grant conditioned on {named:?}");
        seed_bucket(&router, &case, &bucket).await;

        // The user may federate, and may read the bucket only while the clause holds: a
        // long-term key and the federated-user session it mints are judged on this one
        // document, so the two rows below differ in nothing but the class of credential.
        let user_name = format!("Typed{named}");
        let user_key = user_with_key(
            &router,
            &case,
            &user_name,
            &document(&[
                statement("Allow", "sts:*", "*", None),
                statement(
                    "Allow",
                    GET_OBJECT,
                    &object_arn(&bucket, "*"),
                    Some(&principal_type_clause(named)),
                ),
            ]),
        )
        .await;
        let role_name = format!("Typed{named}Role");
        create_role(&router, &case, &role_name).await;
        put_role_policy(
            &router,
            &case,
            &role_name,
            "read",
            &document(&[statement(
                "Allow",
                GET_OBJECT,
                &object_arn(&bucket, "*"),
                Some(&principal_type_clause(named)),
            )]),
        )
        .await;
        // The federated session's own scope allows the read unconditionally, so what decides
        // its row is the clause in the minting user's document.
        let scope = document(&[statement(
            "Allow",
            GET_OBJECT,
            &object_arn(&bucket, "*"),
            None,
        )]);

        // (the class of credential presented, the access key id, the class it answers with)
        let rows: Vec<(&str, String, &str)> = vec![
            (
                "an IAM user's own long-term access key",
                user_key.clone(),
                USER,
            ),
            (
                "a session of a role",
                assume_role(&router, &case, &role_name, None).await,
                ASSUMED_ROLE,
            ),
            (
                "a federated-user session",
                federation_token(&router, &case, &user_key, "typed", Some(&scope)).await,
                FEDERATED_USER,
            ),
        ];

        for (row, access_key_id, answers) in rows {
            let (status, body) = get_object(&router, &access_key_id, &bucket, ASCII_KEY).await;
            let expected = if answers == named { 200 } else { 403 };
            assert_eq!(
                status, expected,
                "case {case:?}, {row:?} (which answers {answers:?}): expected HTTP {expected}, \
                 got {status}: {body}"
            );
            if expected == 403 {
                let message = message_of(&body);
                assert!(
                    message.ends_with(&no_grant(GET_OBJECT)),
                    "case {case:?}, {row:?}: expected the no-grant diagnosis, got {message:?}"
                );
            }
        }
    }
}
