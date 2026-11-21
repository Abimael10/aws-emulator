// SPDX-License-Identifier: CC0-1.0

//! What a withheld `ListUsers` is refused with, over every user collection a request can
//! address.
//!
//! RULE: an authorization refusal this service decides on the policies in force is ONE
//! message — `User: ` + the principal that signed the request + ` is not authorized to
//! perform: ` + the action + ` on resource: ` + the user collection the request addressed +
//! ` ` + the clause saying why the decision went that way — CLOSED by the pointer at that
//! decision's own authorization details, and nothing selects that pointer away. In
//! particular the `PathPrefix` the request names moves the RESOURCE the message states (the
//! account's `user` resource type under that prefix) and nothing else: not the pointer, not
//! the clause, and not the status or the code the refusal travels in. Grants the caller holds
//! over other actions — including actions of another service entirely, which is the position
//! a client probing two services' permissions with one key is in — do not make the refusal of
//! this action shorter either.
//!
//! The rows are a TABLE spanning what the rule generalises over: the prefix the request names
//! (absent, the root, one segment, two segments, a prefix the account holds no user under,
//! and a prefix spelled with the punctuation `pathPrefixType` admits), what the caller holds
//! (nothing, a grant over another service's actions, a grant over another IAM read, and an
//! allow of everything with an explicit `Deny` over this one), and which of the two decisions
//! refused the call. `tests/all/refusal_message_points_at_the_authorization_detail.rs` is the
//! table over the OPERATIONS; this suite is the table over one operation's collections.
//!
//! The second test is the positive control: the same listings under the same prefixes are
//! SERVED for a caller whose document allows them, and report the users the account holds
//! under each prefix — so every refusal above is about the caller's permissions and not about
//! the prefix being unaddressable.
//!
//! The awsQuery harness, the fixture principal and the refusal reader are the ones
//! `tests/all/user_lifecycle.rs` and `tests/all/identity_policy_enforcement.rs` own, so a
//! request is built and a message read exactly one way.

use crate::identity_policy_enforcement::{collection, principal, statement};
use crate::user_lifecycle::{
    account_id, assert_access_denied_message, authorization_details_id, call, call_signed_by,
    element, elements, error_code, router,
};

/// The IAM action a `ListUsers` call is decided on.
const LIST_USERS: &str = "iam:ListUsers";

/// The user the account holds before any request is served: the IAM user this emulator's own
/// long-term credential belongs to, which every listing under the root path reports beside the
/// fixtures.
const ACCOUNT_CALLER: &str = ::service_common::caller_principal::AMBIENT_USER_NAME;

/// The path that user sits under.
const ACCOUNT_CALLER_PATH: &str = ::service_common::caller_principal::AMBIENT_USER_PATH;

/// The clause closing the sentence of a call nothing in force allows.
fn no_allow(iam_action: &str) -> String {
    format!("because no identity-based policy allows the {iam_action} action")
}

/// The clause closing the sentence of a call a statement about it explicitly denies.
const EXPLICIT_DENY: &str = "with an explicit deny in an identity-based policy";

/// A grant for every action over every resource, with an explicit `Deny` of `action`: what a
/// row about the explicit deny is refused under, so the row demonstrates the `Deny` and not a
/// missing grant.
fn allow_everything_but(action: &str) -> String {
    format!(
        concat!(
            r#"{{"Version":"2012-10-17","Statement":["#,
            r#"{{"Effect":"Allow","Action":"*","Resource":"*"}},"#,
            r#"{{"Effect":"Deny","Action":"{}","Resource":"*"}}]}}"#
        ),
        action
    )
}

/// The users the account is given before a listing is driven, as `(name, path)` — one under a
/// one-segment path and one under the two-segment path the modelled `PathPrefix` gives as its
/// example, so a prefix a row addresses holds a user rather than nothing.
const FIXTURE_USERS: &[(&str, &str)] = &[
    ("eng-reader", "/eng/"),
    ("subdivision-reader", "/division_abc/subdivision_xyz/"),
];

/// The prefixes the rows and the positive control address, spanning the forms
/// `pathPrefixType` (len 1..512, pattern `^\u002F[\u0021-\u007F]*$`) admits: absent, the root
/// slash it defaults to, one segment, the two-segment example the member's own documentation
/// gives, a prefix the account holds no user under, and one spelled with the punctuation that
/// pattern allows.
const PREFIXES: &[Option<&str>] = &[
    None,
    Some("/"),
    Some("/eng/"),
    Some("/division_abc/subdivision_xyz/"),
    Some("/no-user-here/"),
    Some("/a.b-c_d/"),
];

/// The prefix a request that names none is decided on: `PathPrefix` "defaults to a slash (/),
/// listing all user names".
const DEFAULT_PREFIX: &str = "/";

/// One refused row: the prefix the request names, what the caller holds, and why the call is
/// refused.
struct Row {
    case: &'static str,
    path_prefix: Option<&'static str>,
    /// The one inline document put in force on the caller, or `None` for a caller holding
    /// none. Whatever it says, it does not allow `iam:ListUsers`.
    document: Option<String>,
    /// The clause the refusal's sentence closes with.
    reason: String,
}

/// The rows.
fn rows() -> Vec<Row> {
    vec![
        // No prefix and no document: the call the recorded cross-protocol check makes.
        Row {
            case: "a listing naming no prefix, by a caller holding no document",
            path_prefix: None,
            document: None,
            reason: no_allow(LIST_USERS),
        },
        // The root prefix stated explicitly: the same collection the default names.
        Row {
            case: "a listing naming the root prefix",
            path_prefix: Some("/"),
            document: None,
            reason: no_allow(LIST_USERS),
        },
        // A caller whose only grant is over ANOTHER SERVICE's actions — one key probing two
        // services' permissions — which leaves this action withheld.
        Row {
            case: "a one-segment prefix, by a caller granted a secret-store read",
            path_prefix: Some("/eng/"),
            document: Some(statement("Allow", "secretsmanager:GetSecretValue", "*")),
            reason: no_allow(LIST_USERS),
        },
        // A caller granted another IAM read: a document was read and does not cover the call.
        Row {
            case: "a two-segment prefix, by a caller granted another listing",
            path_prefix: Some("/division_abc/subdivision_xyz/"),
            document: Some(statement("Allow", "iam:ListRoles", "*")),
            reason: no_allow(LIST_USERS),
        },
        // A prefix the account holds no user under: the decision precedes the enumeration, so
        // an empty collection is refused exactly as a populated one is.
        Row {
            case: "a prefix the account holds no user under",
            path_prefix: Some("/no-user-here/"),
            document: None,
            reason: no_allow(LIST_USERS),
        },
        // A prefix spelled with the punctuation the member's pattern admits.
        Row {
            case: "a prefix spelled with punctuation the pattern admits",
            path_prefix: Some("/a.b-c_d/"),
            document: None,
            reason: no_allow(LIST_USERS),
        },
        // The OTHER decision, on the same collections: a statement about the request says
        // `Deny` over a grant for everything, which moves the reason clause and nothing else.
        Row {
            case: "a one-segment prefix denied by a statement naming the action",
            path_prefix: Some("/eng/"),
            document: Some(allow_everything_but(LIST_USERS)),
            reason: EXPLICIT_DENY.to_string(),
        },
        Row {
            case: "no prefix, denied by a statement naming every action of the service",
            path_prefix: None,
            document: Some(allow_everything_but("iam:*")),
            reason: EXPLICIT_DENY.to_string(),
        },
    ]
}

/// Create the users the rows' collections hold.
async fn given_fixture_users(router: &smithy_server_runtime::MultiServiceRouter) {
    for (user_name, path) in FIXTURE_USERS {
        let (status, body) = call(
            router,
            "CreateUser",
            &[("UserName", user_name), ("Path", path)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {user_name:?}: the fixture CreateUser must succeed, got {status}: {body}"
        );
    }
}

/// The `ListUsers` form parameters for a row's prefix: none where the request names no prefix.
fn parameters(path_prefix: Option<&str>) -> Vec<(&'static str, &str)> {
    match path_prefix {
        Some(prefix) => vec![("PathPrefix", prefix)],
        None => Vec::new(),
    }
}

/// RULE: every withheld `ListUsers` is refused HTTP 403 `AccessDenied` with the sentence
/// naming the caller, the action and the user collection the request addressed, the clause
/// saying why, and the pointer at that decision's authorization details — whichever prefix the
/// request named, whatever else the caller was granted, and whichever of the two decisions
/// refused it — and every refusal names an authorization id of its own.
#[tokio::test]
async fn every_withheld_user_listing_names_its_collection_and_points_at_its_details() {
    let router = router();
    given_fixture_users(&router).await;

    let mut minted: Vec<String> = Vec::new();
    for (row, case) in rows().into_iter().enumerate() {
        let user_name = format!("prefixrow{row}");
        let access_key_id = principal(
            &router,
            case.case,
            &user_name,
            &case
                .document
                .map(|document| vec![("in-force".to_string(), document)])
                .unwrap_or_default(),
        )
        .await;

        let (status, body) = call_signed_by(
            &router,
            Some(&access_key_id),
            "ListUsers",
            &parameters(case.path_prefix),
        )
        .await;
        assert_eq!(
            status, 403,
            "case {:?}: expected HTTP 403, got {status}: {body}",
            case.case
        );
        let code = error_code(&body);
        assert_eq!(
            code, "AccessDenied",
            "case {:?}: expected the error code AccessDenied, got {code:?}: {body}",
            case.case
        );

        let message = element(&body, "Message");
        assert_access_denied_message(
            case.case,
            &message,
            &format!("arn:aws:iam::{}:user/{user_name}", account_id()),
            LIST_USERS,
            // The resource the decision was made on: the account's users under the prefix the
            // request addressed, which for a request naming none is the default prefix.
            &collection("user", case.path_prefix.unwrap_or(DEFAULT_PREFIX)),
            &case.reason,
        );
        minted.push(authorization_details_id(case.case, &message));
    }

    let distinct: std::collections::BTreeSet<&String> = minted.iter().collect();
    assert_eq!(
        distinct.len(),
        minted.len(),
        "expected each refusal to point at its own authorization details, got {minted:?}"
    );
}

/// The users the account holds under `prefix`, in the order this service enumerates them:
/// ascending by the case-folded name.
fn expected_under(prefix: &str, caller: &str) -> Vec<String> {
    let mut held: Vec<(&str, &str)> = FIXTURE_USERS.to_vec();
    // The caller of the listing and the account's own user both sit at the root path.
    held.push((caller, ACCOUNT_CALLER_PATH));
    held.push((ACCOUNT_CALLER, ACCOUNT_CALLER_PATH));
    let mut names: Vec<String> = held
        .into_iter()
        .filter(|(_, path)| path.starts_with(prefix))
        .map(|(name, _)| name.to_string())
        .collect();
    names.sort_by_key(|name| name.to_ascii_lowercase());
    names
}

/// RULE, as the positive control on the table above: a caller whose document allows the action
/// is SERVED the same listings, which report the users the account holds under the prefix the
/// request named — so the refusals above follow from the documents in force and not from the
/// collection a prefix addresses.
#[tokio::test]
async fn a_granted_user_listing_is_served_under_the_same_prefixes() {
    let router = router();
    given_fixture_users(&router).await;
    let caller = "prefix-lister";
    let access_key_id = principal(
        &router,
        "the granted lister",
        caller,
        &[(
            "allow-listusers".to_string(),
            statement("Allow", LIST_USERS, "*"),
        )],
    )
    .await;

    for prefix in PREFIXES {
        let case = format!("a granted listing under {prefix:?}");
        let (status, body) = call_signed_by(
            &router,
            Some(&access_key_id),
            "ListUsers",
            &parameters(*prefix),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a caller whose document allows the action must be served, got \
             {status}: {body}"
        );
        let reported = elements(&body, "UserName");
        let expected = expected_under(prefix.unwrap_or(DEFAULT_PREFIX), caller);
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the users the account holds under the prefix, got \
             {reported:?}"
        );
    }
}
