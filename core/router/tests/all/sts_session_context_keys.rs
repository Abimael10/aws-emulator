// SPDX-License-Identifier: CC0-1.0

//! WHICH values the two credential-borne global condition keys answer in the request context
//! a data-plane authorization gate decides against, for a request signed with credentials STS
//! minted.
//!
//! Two rules, each one AWS states about those keys, and each the difference between a grant
//! that holds and one that never matches:
//!
//! 1. **`aws:PrincipalArn` is the ARN of the IDENTITY whose policies are in force** — "for IAM
//!    roles, the request context returns the ARN of the role, not the ARN of the user that
//!    assumed the role" (AWS General Reference, *Global condition context keys*) — so a
//!    request signed by an assumed-role session answers it with the ROLE's ARN and never with
//!    the assumed-role session ARN `sts:GetCallerIdentity` reports for that credential. The
//!    key is always in the context of such a request, so a clause over it is DECIDED in both
//!    directions: an `Allow` conditioned on the role's ARN grants, the same clause over the
//!    session ARN does not, and a negated clause answers the other way round.
//! 2. **`aws:TokenIssueTime` is in the context only for a TEMPORARY credential** — "this key
//!    is present in the request context only when the principal uses temporary credentials"
//!    (same list) — so a session `sts:GetSessionToken` minted carries the instant it was
//!    issued while the long-term access key of the SAME IAM user carries no value for the key
//!    at all. Both facts are decided, not unknown: `Null` over the key admits exactly one of
//!    the two halves and flipping its value reverses both outcomes, a date operator over it
//!    decides for the temporary half alone, and the `IfExists` spelling holds for the
//!    long-term half as well.
//!
//! A grant neither of those keys satisfies leaves nothing allowing the action, so the read is
//! refused with the no-grant diagnosis and the `HeadObject` of the same object — the request
//! whose answer is its status alone — is refused HTTP 403 too, which is what makes an
//! unsatisfiable condition observable rather than a call that never answers.
//!
//! Three crates have to agree for any of it to hold — IAM holds the role, the user and their
//! documents, STS mints the credential and records what it speaks for, Amazon S3 builds the
//! request context and decides the call — so this is the suite that can pin them. It drives
//! the SAME gate `sts_session_credential_refusals` pins the refusal WORDING of, and shares
//! that suite's wire helpers rather than writing a second copy of them.
//!
//! The rows span what the rules generalise over: WHICH operator the clause is written under
//! (string equality and its negation, ARN equality, an ARN pattern, existence, a date
//! comparison, the `IfExists` spelling), WHICH ARN form the clause names (the role's, the
//! session's, another role's, a pattern over either family, a list of both), and WHICH
//! credential class signs (an assumed-role session, a session-token session, a long-term
//! user key).

use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::sts_session_credential_refusals::{
    assume_role, build_router, code_of, create_role, document, get_object, message_of, no_grant,
    not_authorized, object_arn, put_role_policy, role_arn, s3, seed_bucket, session_arn, statement,
    sts, user_with_key, ACCOUNT, ASCII_KEY, GET_OBJECT, OBJECT_BODY,
};

/// The error code an authorization refusal of an object read carries.
const ACCESS_DENIED: &str = "AccessDenied";

/// The policy name every inline document here is embedded under.
const POLICY_NAME: &str = "read-one-object";

/// What a row expects of a read made with one credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The documents in force grant the read, so the object's bytes come back.
    Served,
    /// Nothing grants it, so the read is refused with the no-grant diagnosis.
    Refused,
}

/// The placeholders a row writes where an ARN it cannot name itself goes. They are not ARN
/// syntax, so a template that still carries one cannot be mistaken for a value.
const ROLE_PLACEHOLDER: &str = "<role>";
const SESSION_PLACEHOLDER: &str = "<session>";
const OTHER_ROLE_PLACEHOLDER: &str = "<other-role>";
const ROLE_FAMILY_PLACEHOLDER: &str = "<role-family>";

/// The ARN of a role this account does not hold, which no row's caller is.
fn other_role_arn() -> String {
    role_arn("SomebodyElsesRole")
}

/// Every role ARN of this account, as an `ArnLike` pattern names the family.
fn role_family() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/*")
}

/// `template` with each placeholder above replaced by the ARN it stands for.
fn resolve(template: &str, role: &str, session: &str) -> String {
    template
        .replace(ROLE_FAMILY_PLACEHOLDER, &role_family())
        .replace(OTHER_ROLE_PLACEHOLDER, &other_role_arn())
        .replace(SESSION_PLACEHOLDER, session)
        .replace(ROLE_PLACEHOLDER, role)
}

/// Assert the read of `key` in `bucket` by `access_key_id` is SERVED and answers the object's
/// bytes.
async fn assert_served(
    router: &MultiServiceRouter,
    case: &str,
    who: &str,
    access_key_id: &str,
    bucket: &str,
    key: &str,
) {
    let (status, body) = get_object(router, access_key_id, bucket, key).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the condition holds for {who}, so the read must be served, got {status}: \
         {body}"
    );
    assert_eq!(
        body, OBJECT_BODY,
        "case {case:?}: the served read must answer the object's bytes for {who}, got {body:?}"
    );
}

/// Assert the read of `key` in `bucket` by `access_key_id` is REFUSED 403 `AccessDenied` with
/// the no-grant diagnosis naming `principal`, and that the `HeadObject` of the same object —
/// the request a caller reads the status of alone — is refused HTTP 403 as well.
async fn assert_refused(
    router: &MultiServiceRouter,
    case: &str,
    who: &str,
    access_key_id: &str,
    principal: &str,
    bucket: &str,
    key: &str,
) {
    let (status, body) = get_object(router, access_key_id, bucket, key).await;
    assert_eq!(
        status, 403,
        "case {case:?}: nothing grants the read to {who}, so expected HTTP 403, got {status}: \
         {body}"
    );
    let code = code_of(&body);
    assert_eq!(
        code, ACCESS_DENIED,
        "case {case:?}: expected {ACCESS_DENIED} for {who}, got {code:?}: {body}"
    );
    let message = message_of(&body);
    assert_eq!(
        message,
        not_authorized(
            principal,
            GET_OBJECT,
            &object_arn(bucket, key),
            &no_grant(GET_OBJECT)
        ),
        "case {case:?}: expected the no-grant diagnosis for {who}, got {message:?}"
    );

    let (status, body) = s3(
        router,
        access_key_id,
        "HEAD",
        &format!("/{bucket}/{key}"),
        "",
    )
    .await;
    assert_eq!(
        status, 403,
        "case {case:?}: the head of the same object must be refused 403 for {who}, got {status}: \
         {body}"
    );
}

/// RULE: `aws:PrincipalArn` is the ARN of the identity whose policies are in force — the ROLE
/// behind an assumed-role session and never the session ARN STS reports for the credential —
/// and it is always in the request context, so a clause over it decides in both directions.
#[tokio::test]
async fn a_session_answers_the_principal_arn_key_with_the_roles_arn() {
    struct Row {
        case: &'static str,
        /// The `Condition` element the only `Allow` of the read carries.
        condition: &'static str,
        expected: Expect,
    }

    let rows = [
        Row {
            case: "an allow pinned to the calling session's ROLE arn",
            condition: r#"{"StringEquals":{"aws:PrincipalArn":"<role>"}}"#,
            expected: Expect::Served,
        },
        Row {
            case: "an allow pinned to the assumed-role SESSION arn",
            condition: r#"{"StringEquals":{"aws:PrincipalArn":"<session>"}}"#,
            expected: Expect::Refused,
        },
        Row {
            case: "an allow pinned to the role arn under the ARN operator",
            condition: r#"{"ArnEquals":{"aws:PrincipalArn":"<role>"}}"#,
            expected: Expect::Served,
        },
        Row {
            case: "an allow pinned to a pattern over this account's role family",
            condition: r#"{"ArnLike":{"aws:PrincipalArn":"<role-family>"}}"#,
            expected: Expect::Served,
        },
        Row {
            case: "an allow pinned to a pattern over the assumed-role family",
            condition: r#"{"ArnLike":{"aws:PrincipalArn":"arn:aws:sts::*:assumed-role/*"}}"#,
            expected: Expect::Refused,
        },
        Row {
            case: "an allow pinned to another role's arn",
            condition: r#"{"StringEquals":{"aws:PrincipalArn":"<other-role>"}}"#,
            expected: Expect::Refused,
        },
        Row {
            case: "an allow of every principal EXCEPT the calling role",
            condition: r#"{"StringNotEquals":{"aws:PrincipalArn":"<role>"}}"#,
            expected: Expect::Refused,
        },
        Row {
            case: "an allow of every principal except the session arn",
            condition: r#"{"StringNotEquals":{"aws:PrincipalArn":"<session>"}}"#,
            expected: Expect::Served,
        },
        Row {
            case: "an allow of any caller the key carries a value for",
            condition: r#"{"Null":{"aws:PrincipalArn":"false"}}"#,
            expected: Expect::Served,
        },
        Row {
            case: "an allow listing both arn forms",
            condition: r#"{"StringEquals":{"aws:PrincipalArn":["<session>","<role>"]}}"#,
            expected: Expect::Served,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        let bucket = format!("principal-arn-bucket-{index}");
        seed_bucket(&router, row.case, &bucket).await;
        let role_name = format!("PrincipalArnRole{index}");
        create_role(&router, row.case, &role_name).await;

        let role = role_arn(&role_name);
        let session = session_arn(&role_name);
        // The vacuity guard: the two ARN forms of one session differ, so a row that passes
        // under one of them is saying something about which form the key answers with.
        assert_ne!(
            role, session,
            "case {:?}: the role and session ARN forms must differ, got {role:?}",
            row.case
        );

        // The ONLY grant of the read is the conditioned one, so the read is served exactly
        // where the clause holds.
        let resource = object_arn(&bucket, ASCII_KEY);
        let condition = resolve(row.condition, &role, &session);
        put_role_policy(
            &router,
            row.case,
            &role_name,
            POLICY_NAME,
            &document(&[statement(
                "Allow",
                GET_OBJECT,
                &resource,
                Some(&condition),
            )]),
        )
        .await;
        let credential = assume_role(&router, row.case, &role_name, None).await;

        match row.expected {
            Expect::Served => {
                assert_served(
                    &router,
                    row.case,
                    "the role session",
                    &credential,
                    &bucket,
                    ASCII_KEY,
                )
                .await;
            }
            Expect::Refused => {
                assert_refused(
                    &router,
                    row.case,
                    "the role session",
                    &credential,
                    &session,
                    &bucket,
                    ASCII_KEY,
                )
                .await;
            }
        }
    }
}

/// The identity `sts:GetCallerIdentity` reports for `access_key_id`: its ARN, its unique id
/// and its account.
async fn caller_identity(
    router: &MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) -> (String, String, String) {
    let (status, body) = sts(router, access_key_id, "GetCallerIdentity", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetCallerIdentity must succeed, got {status}: {body}"
    );
    (
        element(&body, "Arn"),
        element(&body, "UserId"),
        element(&body, "Account"),
    )
}

/// RULE: `aws:TokenIssueTime` is in the request context only for a temporary credential, so
/// the session `sts:GetSessionToken` mints for an IAM user carries the instant it was issued
/// while that same user's long-term access key carries no value for the key — both facts
/// decided, so one clause over the key admits exactly one of the two halves of ONE identity.
#[tokio::test]
async fn the_token_issue_time_key_is_present_only_for_the_temporary_half_of_one_identity() {
    struct Row {
        case: &'static str,
        /// The `Condition` element the only `Allow` of the read carries, if any.
        condition: Option<&'static str>,
        /// What the IAM user's LONG-TERM access key reads.
        long_term: Expect,
        /// What the session `GetSessionToken` minted for that same user reads.
        session: Expect,
    }

    let rows = [
        Row {
            case: "an allow of any caller carrying NO session issue instant",
            condition: Some(r#"{"Null":{"aws:TokenIssueTime":"true"}}"#),
            long_term: Expect::Served,
            session: Expect::Refused,
        },
        Row {
            case: "the same clause with its value flipped, admitting a carried instant",
            condition: Some(r#"{"Null":{"aws:TokenIssueTime":"false"}}"#),
            long_term: Expect::Refused,
            session: Expect::Served,
        },
        Row {
            case: "an allow of every session issued before a far-future cut-off",
            condition: Some(r#"{"DateLessThan":{"aws:TokenIssueTime":"2999-01-01T00:00:00Z"}}"#),
            long_term: Expect::Refused,
            session: Expect::Served,
        },
        Row {
            case: "an allow of every session issued before a cut-off already past",
            condition: Some(r#"{"DateLessThan":{"aws:TokenIssueTime":"2000-01-01T00:00:00Z"}}"#),
            long_term: Expect::Refused,
            session: Expect::Refused,
        },
        Row {
            case: "the same cut-off written so a caller carrying no instant passes it",
            condition: Some(
                r#"{"DateLessThanIfExists":{"aws:TokenIssueTime":"2999-01-01T00:00:00Z"}}"#,
            ),
            long_term: Expect::Served,
            session: Expect::Served,
        },
        Row {
            case: "an allow no clause over the key narrows",
            condition: None,
            long_term: Expect::Served,
            session: Expect::Served,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        let bucket = format!("token-issue-time-bucket-{index}");
        seed_bucket(&router, row.case, &bucket).await;

        // The user's ONLY grant of the read is the row's conditioned statement; it may mint a
        // session of itself either way, which is what makes the two halves comparable.
        let resource = object_arn(&bucket, ASCII_KEY);
        let long_term_key = user_with_key(
            &router,
            row.case,
            &format!("TokenIssueTimeUser{index}"),
            &document(&[
                statement("Allow", GET_OBJECT, &resource, row.condition),
                statement("Allow", "sts:GetSessionToken", "*", None),
            ]),
        )
        .await;

        let (status, body) = sts(
            &router,
            &long_term_key,
            "GetSessionToken",
            &[("DurationSeconds", "900")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: GetSessionToken must mint a session, got {status}: {body}",
            row.case
        );
        let session_key = element(&body, "AccessKeyId");
        let session_token = element(&body, "SessionToken");
        assert!(
            !session_token.is_empty(),
            "case {:?}: the minted credential must carry a session token: {body}",
            row.case
        );

        // The two credentials are two halves of ONE identity: the temporary one speaks for the
        // very IAM user whose long-term key minted it, which is why one clause over the key
        // can tell them apart at all.
        let long_term_identity = caller_identity(&router, row.case, &long_term_key).await;
        let session_identity = caller_identity(&router, row.case, &session_key).await;
        assert_eq!(
            session_identity, long_term_identity,
            "case {:?}: both halves must report one identity, got {session_identity:?} for the \
             session and {long_term_identity:?} for the long-term key",
            row.case
        );
        let principal = long_term_identity.0;

        for (who, credential, expected) in [
            ("the long-term key", &long_term_key, row.long_term),
            ("the session credential", &session_key, row.session),
        ] {
            match expected {
                Expect::Served => {
                    assert_served(&router, row.case, who, credential, &bucket, ASCII_KEY).await;
                }
                Expect::Refused => {
                    assert_refused(
                        &router,
                        row.case,
                        who,
                        credential,
                        &principal,
                        &bucket,
                        ASCII_KEY,
                    )
                    .await;
                }
            }
        }
    }
}
