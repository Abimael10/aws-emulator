// SPDX-License-Identifier: CC0-1.0

//! What a session's TAGS decide about a session policy written over them — the ABAC boundary
//! a client uses session tags as.
//!
//! RULE: the session tags one mint attaches are what `aws:PrincipalTag/<key>` answers for
//! every later request the minted credential signs ("`aws:PrincipalTag/tag-key` — the tag
//! attached to the principal making the request", AWS General Reference, *Global condition
//! context keys*), and a session policy CAPS what the role's own documents grant: "the
//! resulting session's permissions are the intersection of the role's identity-based policy
//! and the session policies" (`AssumeRoleRequest$Policy`). So a `Condition` on the session
//! policy's only grant is DECIDED against those tags, in both directions — a session whose
//! tags satisfy the clause is served, and a session whose tags do not leaves the intersection
//! granting nothing, so the call is refused in the serving service's own access-denied answer
//! naming the session principal, the action and the session policies as the document that
//! withheld it. Because the mint records every tag it attached, the request answers for a tag
//! key the session does NOT carry as well: an absent key satisfies no equality clause, and an
//! existence clause over it is decided rather than left undecided.
//!
//! The role's own unconditional grant does not widen the session, and the session policy
//! grants nothing of its own: a session of a role whose documents do not allow the action is
//! refused however well its tags satisfy the clause, and the refusal names the ABSENT grant
//! instead.
//!
//! Three crates have to agree for any of it to hold — IAM holds the role and its
//! identity-based document, STS mints the session and records the scope and the tags it was
//! minted with, Amazon S3 resolves the credential, publishes the tags as condition keys and
//! decides the call — so this is the suite that can pin it, and it drives the SAME gate
//! `sts_session_credential_refusals` pins the refusal wording of, through that suite's wire
//! helpers rather than a second copy of them.
//!
//! The action every row drives is the account's own bucket listing ("to grant IAM permission
//! to use this operation, you must add the `s3:ListAllMyBuckets` policy action",
//! `ListBuckets`), which addresses no resource of its own — so nothing but the documents in
//! force and the scope the credential was minted with can decide it, and the refusal carries
//! no resource clause.
//!
//! The rows span what the rule generalises over: WHICH operator the clause is written under
//! (equality, a pattern, negated equality, existence), WHICH tag key it names (a plain key, a
//! key spelled with a hyphen, a key the session does not carry at all), and WHAT the session
//! carries for it (the value the clause holds for, another value, the same value in another
//! case, a value outside ASCII, no tag at all, several tags of which one is the gated one).

use crate::query_wire::element;
use crate::sts_session_credential_refusals::{
    build_router, code_of, document, iam, message_of, no_grant, put_role_policy, s3, statement,
    sts, ACCOUNT, AMBIENT_KEY,
};

/// The error code an S3 authorization refusal carries, and the status it travels with (Amazon
/// S3 "Error responses": `AccessDenied`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u16 = 403;

/// The IAM action the account's own bucket listing is authorized under. It addresses no
/// resource of its own.
const LIST_ALL_MY_BUCKETS: &str = "s3:ListAllMyBuckets";

/// The role every row's session is minted of: its own identity policy allows the listing
/// unconditionally, so the session policy's clause is the only thing a row varies.
const SCOPED_ROLE: &str = "session-tag-scoped-role";

/// A role whose identity policy does NOT allow the listing, which is what shows the session
/// policy grants nothing of its own.
const UNGRANTED_ROLE: &str = "session-tag-ungranted-role";

/// The policy name the roles' inline documents are embedded under.
const POLICY_NAME: &str = "list-all-my-buckets";

/// What a row expects the listing made with its session to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The clause holds for the session's tags, so the intersection grants the listing.
    Served,
    /// The clause does not hold, so the scope withholds the listing.
    Refused,
}

/// The trust policy both roles carry: this account's own principals may assume the role AND
/// attach session tags to the session they mint — "an administrator must grant you the
/// permissions necessary to pass session tags" (`AssumeRole`), which is a grant of its own
/// beside the assume.
fn trust_policy() -> String {
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{ACCOUNT}:root"}},"Action":["sts:AssumeRole","sts:TagSession"]}}]}}"#
    )
}

/// The session policy a row passes: its only statement allows the listing, gated by
/// `condition`.
fn session_policy(condition: &str) -> String {
    document(&[statement(
        "Allow",
        LIST_ALL_MY_BUCKETS,
        "*",
        Some(condition),
    )])
}

/// The clause a refusal closes with for a request the SESSION POLICIES do not allow: the
/// document that withheld the grant, named as AWS names it.
fn no_session_grant(action: &str) -> String {
    format!("because no session policy allows the {action} action")
}

/// The sentence a service refuses an action that addresses NO resource of its own with: who
/// was refused, which action, and the clause saying what withheld it — and no `on resource:`
/// clause, there being no resource the request addressed.
fn account_wide_refusal(principal: &str, action: &str, reason: &str) -> String {
    format!("User: {principal} is not authorized to perform: {action} {reason}")
}

/// Create `role_name` with [`trust_policy`] and embed `inline` as its one identity-based
/// document.
async fn role_with(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    inline: &str,
) {
    let (status, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole({role_name:?}) must succeed, got {status}: {body}"
    );
    put_role_policy(router, case, role_name, POLICY_NAME, inline).await;
}

/// Mint a session of `role_name` under the session policy `policy`, attaching `tags`: answers
/// the access key id of the credential it vends and the `AssumedRoleUser.Arn` it reports for
/// it, which is what a refusal of that session's calls names as the principal.
async fn tagged_session(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    role_name: &str,
    session_name: &str,
    policy: &str,
    tags: &[(&str, &str)],
) -> (String, String) {
    let role_arn = format!("arn:aws:iam::{ACCOUNT}:role/{role_name}");
    // The member names an awsQuery request carries a `tagListType` entry under: one indexed
    // `Key`/`Value` pair per tag. Only as many as the rows below attach.
    const TAG_MEMBERS: [(&str, &str); 2] = [
        ("Tags.member.1.Key", "Tags.member.1.Value"),
        ("Tags.member.2.Key", "Tags.member.2.Value"),
    ];
    assert!(
        tags.len() <= TAG_MEMBERS.len(),
        "case {case:?}: this harness encodes at most {} tags, got {tags:?}",
        TAG_MEMBERS.len()
    );
    let mut members: Vec<(&str, &str)> = vec![
        ("RoleArn", role_arn.as_str()),
        ("RoleSessionName", session_name),
        ("Policy", policy),
    ];
    for ((key_member, value_member), (key, value)) in TAG_MEMBERS.iter().zip(tags) {
        members.push((key_member, key));
        members.push((value_member, value));
    }
    let (status, body) = sts(router, AMBIENT_KEY, "AssumeRole", &members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole({role_name:?}) must mint the session, got {status}: {body}"
    );
    let reported = element(&body, "Arn");
    assert_eq!(
        reported,
        format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role_name}/{session_name}"),
        "case {case:?}: expected the assumed-role session ARN, got {body}"
    );
    (element(&body, "AccessKeyId"), reported)
}

/// The account's own bucket listing, signed with `access_key_id` — the `x-id` an SDK writes
/// the operation's name in.
async fn list_buckets(
    router: &smithy_server_runtime::MultiServiceRouter,
    access_key_id: &str,
) -> (u16, String) {
    s3(router, access_key_id, "GET", "/?x-id=ListBuckets", "").await
}

/// Assert the listing made with `access_key_id` is SERVED.
async fn assert_served(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    access_key_id: &str,
) {
    let (status, body) = list_buckets(router, access_key_id).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the clause holds for this session's tags, so the listing must be \
         served, got {status}: {body}"
    );
}

/// Assert the listing made with `access_key_id` is REFUSED 403 `AccessDenied` with the
/// sentence naming `principal`, the listing's action and `reason`.
async fn assert_refused(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    access_key_id: &str,
    principal: &str,
    reason: &str,
) {
    let (status, body) = list_buckets(router, access_key_id).await;
    assert_eq!(
        status, ACCESS_DENIED_STATUS,
        "case {case:?}: nothing grants the listing to this session, so expected HTTP \
         {ACCESS_DENIED_STATUS}, got {status}: {body}"
    );
    let code = code_of(&body);
    assert_eq!(
        code, ACCESS_DENIED,
        "case {case:?}: expected the code {ACCESS_DENIED}, got {code:?}: {body}"
    );
    let message = message_of(&body);
    assert_eq!(
        message,
        account_wide_refusal(principal, LIST_ALL_MY_BUCKETS, reason),
        "case {case:?}: expected the refusal to name the session and what withheld the grant, \
         got {message:?}"
    );
}

/// One row: the clause the session policy's only grant carries, the tags the mint attaches,
/// and what the listing made with that session answers.
struct Row {
    case: &'static str,
    condition: &'static str,
    tags: &'static [(&'static str, &'static str)],
    expected: Expect,
}

/// RULE: a session policy's `Condition` over `aws:PrincipalTag/<key>` is decided against the
/// tags the mint attached — a value the clause does not hold for and an ABSENT key both leave
/// the intersection granting nothing — and the refusal names the session policies as the
/// document that withheld the grant.
#[tokio::test]
async fn a_sessions_tags_decide_its_session_policys_condition() {
    let router = build_router();
    role_with(
        &router,
        "the fixture",
        SCOPED_ROLE,
        &document(&[statement("Allow", LIST_ALL_MY_BUCKETS, "*", None)]),
    )
    .await;

    // A session whose tags satisfy the clause, kept for the whole suite: the refused sessions
    // below must leave it exactly as it is, which is what makes each row's refusal a decision
    // about that row's own tags rather than a state the first refusal moved.
    let admitted_policy = session_policy(r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#);
    let (kept, _) = tagged_session(
        &router,
        "a session kept admitted throughout",
        SCOPED_ROLE,
        "kept-admitted",
        &admitted_policy,
        &[("env", "allowed")],
    )
    .await;
    assert_served(&router, "a session kept admitted throughout", &kept).await;

    let rows = [
        Row {
            case: "an equality clause the session's tag satisfies",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("env", "allowed")],
            expected: Expect::Served,
        },
        Row {
            case: "an equality clause the session's tag does not satisfy",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("env", "blocked")],
            expected: Expect::Refused,
        },
        Row {
            case: "an equality clause under a session carrying no tag at all",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[],
            expected: Expect::Refused,
        },
        Row {
            case: "an equality clause over a key the session does not carry",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("tenant", "allowed")],
            expected: Expect::Refused,
        },
        Row {
            case: "an equality clause the session's tag matches only but for its case",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("env", "Allowed")],
            expected: Expect::Refused,
        },
        Row {
            case: "a key spelled with a hyphen, satisfied",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/deploy-env":"allowed"}}"#,
            tags: &[("deploy-env", "allowed")],
            expected: Expect::Served,
        },
        Row {
            case: "a key spelled with a hyphen, not satisfied",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/deploy-env":"allowed"}}"#,
            tags: &[("deploy-env", "blocked")],
            expected: Expect::Refused,
        },
        Row {
            case: "a value outside ASCII the session carries byte-exactly",
            // Inside Latin-1, the character range a session policy document carries literally
            // on the wire (`unrestrictedSessionPolicyDocumentType`'s own pattern,
            // `[\u0009\u000A\u000D\u0020-\u00FF]`).
            condition: r#"{"StringEquals":{"aws:PrincipalTag/city":"Zürich-café"}}"#,
            tags: &[("city", "Zürich-café")],
            expected: Expect::Served,
        },
        Row {
            case: "a value outside ASCII the session does not carry",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/city":"Zürich-café"}}"#,
            tags: &[("city", "Zurich-cafe")],
            expected: Expect::Refused,
        },
        Row {
            case: "a pattern clause the session's tag satisfies",
            condition: r#"{"StringLike":{"aws:PrincipalTag/env":"allow*"}}"#,
            tags: &[("env", "allowed")],
            expected: Expect::Served,
        },
        Row {
            case: "a pattern clause the session's tag does not satisfy",
            condition: r#"{"StringLike":{"aws:PrincipalTag/env":"allow*"}}"#,
            tags: &[("env", "blocked")],
            expected: Expect::Refused,
        },
        Row {
            case: "a negated clause the session's tag satisfies",
            condition: r#"{"StringNotEquals":{"aws:PrincipalTag/env":"blocked"}}"#,
            tags: &[("env", "allowed")],
            expected: Expect::Served,
        },
        Row {
            case: "a negated clause the session's tag does not satisfy",
            condition: r#"{"StringNotEquals":{"aws:PrincipalTag/env":"blocked"}}"#,
            tags: &[("env", "blocked")],
            expected: Expect::Refused,
        },
        Row {
            case: "an existence clause under a session carrying the key",
            condition: r#"{"Null":{"aws:PrincipalTag/env":"false"}}"#,
            tags: &[("env", "whatever")],
            expected: Expect::Served,
        },
        Row {
            case: "an existence clause under a session carrying no tag at all",
            condition: r#"{"Null":{"aws:PrincipalTag/env":"false"}}"#,
            tags: &[],
            expected: Expect::Refused,
        },
        Row {
            case: "several tags, of which the gated one satisfies the clause",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("tenant", "acme"), ("env", "allowed")],
            expected: Expect::Served,
        },
        Row {
            case: "several tags, of which the gated one does not",
            condition: r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#,
            tags: &[("tenant", "acme"), ("env", "blocked")],
            expected: Expect::Refused,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let Row {
            case,
            condition,
            tags,
            expected,
        } = row;
        let policy = session_policy(condition);
        let (access_key_id, principal) = tagged_session(
            &router,
            case,
            SCOPED_ROLE,
            &format!("tag-row-{index}"),
            &policy,
            tags,
        )
        .await;
        match expected {
            Expect::Served => assert_served(&router, case, &access_key_id).await,
            Expect::Refused => {
                assert_refused(
                    &router,
                    case,
                    &access_key_id,
                    &principal,
                    &no_session_grant(LIST_ALL_MY_BUCKETS),
                )
                .await;
            }
        }
    }

    // The admitted session is untouched by every refusal above: a decision about one session's
    // tags is a decision about that session alone.
    assert_served(&router, "the admitted session, after every refusal", &kept).await;
}

/// RULE (the other half of the intersection): a session policy GRANTS nothing of its own, so a
/// session of a role whose identity-based document does not allow the action is refused
/// however well its tags satisfy the clause — and the refusal names the ABSENT identity-based
/// grant rather than the scope.
#[tokio::test]
async fn a_satisfied_session_policy_grants_nothing_the_role_does_not() {
    let case = "a role whose own policy allows another service's read only";
    let router = build_router();
    role_with(
        &router,
        case,
        UNGRANTED_ROLE,
        &document(&[statement("Allow", "sts:GetCallerIdentity", "*", None)]),
    )
    .await;
    let policy = session_policy(r#"{"StringEquals":{"aws:PrincipalTag/env":"allowed"}}"#);
    let (access_key_id, principal) = tagged_session(
        &router,
        case,
        UNGRANTED_ROLE,
        "satisfied-but-ungranted",
        &policy,
        &[("env", "allowed")],
    )
    .await;

    // The credential is live: the one action the role's own document allows is served with it,
    // so the refusal below is about authorization of the listing alone.
    let (status, body) = sts(&router, &access_key_id, "GetCallerIdentity", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the allowed read must be served on the same credential, got {status}: \
         {body}"
    );
    assert_eq!(
        element(&body, "Arn"),
        principal,
        "case {case:?}: the credential must speak for the session, got {body}"
    );

    assert_refused(
        &router,
        case,
        &access_key_id,
        &principal,
        &no_grant(LIST_ALL_MY_BUCKETS),
    )
    .await;
}
