// SPDX-License-Identifier: CC0-1.0

//! RULE: every audit member this service reports — `CreatedBy` and `UpdatedBy` on a user,
//! a group and a group membership — carries the IDENTIFIER of the principal whose
//! credential the request was signed with: the unique id that identifies a principal on
//! AWS, the value the account's identity service reports for that credential. It is never
//! an ARN (an ARN addresses a principal, it does not identify one) and never anything
//! spelled out of the credential's access key id, which no identity service resolves to a
//! principal.
//!
//! Driven end to end over HTTP through [`crate::harness`]: nothing reaches into the state
//! struct, so a value that is not the one the write persisted cannot pass.
//!
//! The suites are TABLES whose rows differ along what the rule generalises over: which
//! resource carries the member (user, group, membership), which READER reports it (a
//! describe and the listing of the same resource, which cannot disagree) and the access
//! key id the call presents — its text, its length and its case — because the rule is that
//! none of that reaches the value.
//!
//! Only ONE principal exists in this suite's engine: it mounts this service alone, so the
//! account holds no identity record for any key a row presents and every one of them
//! speaks for the principal of the credential the emulator publishes. That two callers
//! which ARE two identities are recorded as two principals — an IAM user holding its own
//! access key, a role session — is a question about three services at once and is pinned
//! by the cross-service suite `identitystore_records_the_calling_principal` in
//! `core/router/tests/all/`.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{emulator, ok, ok_as, string, STORE};

/// The type prefixes an AWS principal unique id carries, from IAM's "unique identifier
/// prefixes": `AIDA` for an IAM user and `AROA` for a role (the two principals a request
/// to this emulator can speak for). Stated here so a row asserts the CLASS of identifier
/// rather than one value.
const PRINCIPAL_ID_PREFIXES: [&str; 2] = ["AIDA", "AROA"];

/// How long an AWS unique id is: a four-character type prefix plus 17 more characters.
const PRINCIPAL_ID_LEN: usize = 21;

/// Assert `value` is a principal identifier of the class AWS reports: an AWS unique id —
/// one of [`PRINCIPAL_ID_PREFIXES`] followed by 17 characters of the uppercase
/// alphanumeric alphabet those ids are drawn from — optionally followed by `:<session
/// name>`, which is how a role SESSION is identified.
///
/// Shared with [`crate::identity_store_resolution_and_updates`], which reads the same
/// members from the create/update rules' point of view.
pub fn assert_principal_identifier(value: &str, case: &str) {
    assert!(
        !value.starts_with("arn:"),
        "case {case:?}: a principal identifier is not an ARN, got {value:?}"
    );
    let (principal, session) = match value.split_once(':') {
        Some((principal, session)) => (principal, Some(session)),
        None => (value, None),
    };
    assert!(
        PRINCIPAL_ID_PREFIXES
            .iter()
            .any(|prefix| principal.starts_with(prefix)),
        "case {case:?}: expected one of the AWS unique-id prefixes \
         {PRINCIPAL_ID_PREFIXES:?}, got {value:?}"
    );
    assert_eq!(
        principal.chars().count(),
        PRINCIPAL_ID_LEN,
        "case {case:?}: expected an AWS unique id of {PRINCIPAL_ID_LEN} characters, \
         got {value:?}"
    );
    assert!(
        principal
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit()),
        "case {case:?}: expected an AWS unique id's uppercase alphanumeric alphabet, \
         got {value:?}"
    );
    if let Some(session) = session {
        assert!(
            !session.is_empty(),
            "case {case:?}: a role session identifier names its session, got {value:?}"
        );
    }
}

/// The access key ids the rows present. They differ in TEXT, LENGTH and CASE, which is
/// what the rule generalises over: none of it may reach the recorded principal.
const CALLERS: [(&str, &str); 4] = [
    ("the default caller", "AKIDIDENTITYSTORE"),
    ("a 20-character key id", "AKIAIOSFODNN7EXAMPLE"),
    ("a short key id", "AKIDX"),
    ("a lower-case key id", "akidlowercase"),
];

/// Create a user as `akid` and answer its `UserId`.
async fn create_user(
    router: &MultiServiceRouter,
    akid: &str,
    user_name: &str,
    case: &str,
) -> String {
    let created = ok_as(
        router,
        akid,
        "CreateUser",
        json!({ "IdentityStoreId": STORE, "UserName": user_name }),
        case,
    )
    .await;
    string(&created, "UserId", case)
}

/// Create a group as `akid` and answer its `GroupId`.
async fn create_group(
    router: &MultiServiceRouter,
    akid: &str,
    display_name: &str,
    case: &str,
) -> String {
    let created = ok_as(
        router,
        akid,
        "CreateGroup",
        json!({ "IdentityStoreId": STORE, "DisplayName": display_name }),
        case,
    )
    .await;
    string(&created, "GroupId", case)
}

/// The `CreatedBy` and `UpdatedBy` a response reports for one resource.
fn audit_members(body: &Value, case: &str) -> (String, String) {
    (
        string(body, "CreatedBy", case),
        string(body, "UpdatedBy", case),
    )
}

/// Every reader of every resource reports the calling principal's identifier, in the
/// class AWS reports it, whatever access key id the call presented — and the describe and
/// the listing of one resource report the same value.
#[tokio::test]
async fn every_audit_member_reports_the_calling_principal_identifier() {
    let router = emulator();
    let mut recorded: Vec<(&str, String)> = Vec::new();

    for (index, (case, akid)) in CALLERS.iter().enumerate() {
        let user = create_user(&router, akid, &format!("audited-user-{index}"), case).await;
        let group = create_group(&router, akid, &format!("audited-group-{index}"), case).await;
        let membership = ok_as(
            &router,
            akid,
            "CreateGroupMembership",
            json!({
                "IdentityStoreId": STORE,
                "GroupId": group.clone(),
                "MemberId": { "UserId": user.clone() },
            }),
            case,
        )
        .await;
        let membership_id = string(&membership, "MembershipId", case);

        // (reader, the body the audit members are read from)
        let described_user = ok(
            &router,
            "DescribeUser",
            json!({ "IdentityStoreId": STORE, "UserId": user.clone() }),
            case,
        )
        .await;
        let listed_users = ok(
            &router,
            "ListUsers",
            json!({ "IdentityStoreId": STORE }),
            case,
        )
        .await;
        let listed_user = listed_users["Users"]
            .as_array()
            .and_then(|users| {
                users
                    .iter()
                    .find(|listed| listed["UserId"].as_str() == Some(user.as_str()))
            })
            .unwrap_or_else(|| panic!("case {case:?}: the created user is listed: {listed_users}"))
            .clone();
        let described_group = ok(
            &router,
            "DescribeGroup",
            json!({ "IdentityStoreId": STORE, "GroupId": group.clone() }),
            case,
        )
        .await;
        let listed_groups = ok(
            &router,
            "ListGroups",
            json!({ "IdentityStoreId": STORE }),
            case,
        )
        .await;
        let listed_group = listed_groups["Groups"]
            .as_array()
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|listed| listed["GroupId"].as_str() == Some(group.as_str()))
            })
            .unwrap_or_else(|| {
                panic!("case {case:?}: the created group is listed: {listed_groups}")
            })
            .clone();
        let described_membership = ok(
            &router,
            "DescribeGroupMembership",
            json!({ "IdentityStoreId": STORE, "MembershipId": membership_id.clone() }),
            case,
        )
        .await;
        let listed_memberships = ok(
            &router,
            "ListGroupMemberships",
            json!({ "IdentityStoreId": STORE, "GroupId": group.clone() }),
            case,
        )
        .await;
        let listed_membership = listed_memberships["GroupMemberships"][0].clone();

        let readers = [
            ("DescribeUser", described_user),
            ("ListUsers", listed_user),
            ("DescribeGroup", described_group),
            ("ListGroups", listed_group),
            ("DescribeGroupMembership", described_membership),
            ("ListGroupMemberships", listed_membership),
        ];
        for (reader, body) in readers {
            let row = format!("{case} read by {reader}");
            let (created_by, updated_by) = audit_members(&body, &row);
            assert_principal_identifier(&created_by, &row);
            assert_principal_identifier(&updated_by, &row);
            assert_eq!(
                created_by, updated_by,
                "case {row:?}: nothing has updated the resource, so the last write is the \
                 create, got {body}"
            );
            assert!(
                !created_by.contains(akid),
                "case {row:?}: the recorded principal carries nothing of the access key id \
                 {akid:?}, got {created_by:?}"
            );
            recorded.push((*case, created_by));
        }
    }

    // Every key a row presented is one the account holds no identity record for, so all of
    // them speak for the one principal the published credential speaks for: the recorded
    // value follows the IDENTITY, not the key text.
    let first = recorded
        .first()
        .map(|(_, principal)| principal.clone())
        .expect("the table recorded at least one principal");
    for (case, principal) in &recorded {
        assert_eq!(
            principal, &first,
            "case {case:?}: every caller the account holds no identity record for speaks \
             for one principal, got {principal:?} beside {first:?}"
        );
    }
}
