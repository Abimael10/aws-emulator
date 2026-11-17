// SPDX-License-Identifier: CC0-1.0

//! Every paginated portal operation refuses a `nextToken` it did not issue with the SAME
//! answer, the one `crate::state` holds.
//!
//! The rule under test: the cursor codec is one codec for the whole service, so a
//! `nextToken` that is not an output of the listing it is presented to is refused with the
//! one modelled 400 `InvalidRequestException` and the one wording the state module owns
//! (`state::INVALID_CURSOR_MESSAGE`) — whichever listing was addressed and whatever is
//! wrong with the token.
//!
//! The rows differ along everything the rule generalises over: which paginated operation
//! is addressed (the session's accounts, one account's roles), and what makes the token
//! unissued — not hex at all, hex this service's codec never wrote, a cursor of the other
//! listing, a cursor of another account's role listing, a cursor the same listing issued
//! to a DIFFERENT session, and an issued cursor with a digit appended. Each listing also
//! asserts the positive control, that the cursor as issued still resumes, so the refusals
//! are earned by the token and not by a listing that refuses everything.
//!
//! The assertion is on the message the service actually answers compared against the
//! constant `crate::state` defines, so a handler that worded or classified this refusal
//! for itself again fails here.
//!
//! The second half of the suite pins the same rule where it is hardest to keep: a cursor
//! names its listing by bytes the codec wrote into it, so WHICH listing a presented cursor
//! belongs to may not depend on how that listing's name is spelt. A role listing's name
//! embeds the requested account id, and an account id is an unconstrained string
//! (`smithy_query(query="shape", shape="AccountIdType")` reports no `@pattern` and no
//! `@length`), so the rows there spell account ids that contain the codec's own separator —
//! including ones that spell another account's whole cursor scope — and assert both that no
//! two listings can mint the same cursor bytes and that the cursor codec is an exact
//! inverse pair over every such spelling.
//!
//! The harness is `portal_session`'s, so every suite here speaks to one mounted router.

use bytes::Bytes;
use http::{Response, StatusCode};
use service_com_amazonaws_sso_swbportalservice::state::{
    self, InvalidCursor, INVALID_CURSOR_MESSAGE,
};
use std::collections::{BTreeMap, BTreeSet};

use crate::portal_session::{assert_refusal, body_json, body_text, Portal};

/// The sign-in the listings under test are read with, and a second sign-in holding the
/// same assignments, so a cursor issued to one session can be presented by the other.
const ACCESS_TOKEN: &str = "tok-shared-refusal";
const OTHER_ACCESS_TOKEN: &str = "tok-shared-refusal-other";

/// The accounts both sessions are assigned, with the roles assigned in each:
/// `(account id, roles)`. Two accounts carry more than one role, so a role listing read a
/// page at a time issues a cursor in more than one account, and the ids and role names
/// differ in shape so no cursor's key is one kind of string only.
const ASSIGNED: &[(&str, &[&str])] = &[
    ("601111111111", &["AdministratorAccess", "ReadOnly"]),
    ("602222222222", &["Audit_Role-1", "ReadOnly", "ZZZ-Glass"]),
    ("603333333333", &["ReadOnly"]),
];

/// The page size every listing below is read at: small enough that each of the seeded
/// listings is cut short and hands out a cursor.
const PAGE_SIZE: usize = 1;

/// One paginated read of this service: the operation, the route picking the listing out,
/// and the output member the page arrives in.
struct Listing {
    operation: &'static str,
    path: String,
    items: &'static str,
}

/// Every paginated operation of this service, each pointed at what [`ASSIGNED`] seeds:
/// the session's accounts, and the roles in each account that holds more than one.
fn listings() -> Vec<Listing> {
    let mut rows = vec![Listing {
        operation: "ListAccounts",
        path: "/assignment/accounts".to_string(),
        items: "accountList",
    }];
    for (account_id, roles) in ASSIGNED {
        if roles.len() > 1 {
            rows.push(Listing {
                operation: "ListAccountRoles",
                path: format!("/assignment/roles?account_id={account_id}"),
                items: "roleList",
            });
        }
    }
    rows
}

/// An emulator with [`ASSIGNED`] seeded and both sessions signed in to all of it.
fn portal() -> Portal {
    let portal = Portal::new();
    {
        let store = portal.state();
        let mut store = store.lock();
        for (account_id, _) in ASSIGNED {
            store.put_account(
                *account_id,
                format!("Shared-{account_id}"),
                format!("{account_id}@example.com"),
            );
        }
    }
    for token in [ACCESS_TOKEN, OTHER_ACCESS_TOKEN] {
        let assignments: Vec<(&str, Vec<String>)> = ASSIGNED
            .iter()
            .map(|(account_id, roles)| {
                (
                    *account_id,
                    roles
                        .iter()
                        .map(|role| (*role).to_string())
                        .collect::<BTreeSet<String>>()
                        .into_iter()
                        .collect(),
                )
            })
            .collect();
        portal.sign_in_assigned(token, assignments);
    }
    portal
}

/// The request reading `listing` at [`PAGE_SIZE`], resuming from `token` when there is one.
fn uri(listing: &Listing, token: Option<&str>) -> String {
    let separator = if listing.path.contains('?') { '&' } else { '?' };
    let mut uri = format!("{}{separator}max_result={PAGE_SIZE}", listing.path);
    if let Some(token) = token {
        uri.push_str(&format!("&next_token={token}"));
    }
    uri
}

/// The cursor `listing`'s first page hands out, read with `access_token`.
async fn first_cursor(portal: &Portal, listing: &Listing, access_token: &str) -> String {
    let case = format!("{} read with {access_token}", listing.operation);
    let response = portal.get(&uri(listing, None), Some(access_token)).await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "case {case:?}: expected the first page to answer 200, got {} {:?}",
        response.status(),
        body_text(&response)
    );
    body_json(&response)["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the seeded listing is longer than one page, so it must issue a \
                 cursor, got {:?}",
                body_text(&response)
            )
        })
        .to_string()
}

/// Every token under test for `listing`: what makes it a token this listing never issued,
/// and the token itself.
async fn unissued_tokens(portal: &Portal, listing: &Listing) -> Vec<(String, String)> {
    let mut rows = vec![
        (
            "a value that is not hex at all".to_string(),
            "not-a-page-token".to_string(),
        ),
        (
            "hex of a cursor-shaped string the codec never wrote".to_string(),
            hex::encode("v1:accounts:601111111111"),
        ),
        (
            "hex of a payload carrying no cursor".to_string(),
            hex::encode("v1"),
        ),
        (
            "the cursor of this listing with a digit appended".to_string(),
            format!("{}0", first_cursor(portal, listing, ACCESS_TOKEN).await),
        ),
        (
            "the cursor the same listing issued to another session".to_string(),
            first_cursor(portal, listing, OTHER_ACCESS_TOKEN).await,
        ),
    ];
    // A cursor of EVERY other listing of this service, which is scoped to that listing and
    // so cannot be continued here.
    for other in listings() {
        if other.path == listing.path {
            continue;
        }
        rows.push((
            format!("a cursor {} issued for {}", other.operation, other.path),
            first_cursor(portal, &other, ACCESS_TOKEN).await,
        ));
    }
    rows
}

/// RULE: a `nextToken` the addressed listing did not issue is refused 400
/// `InvalidRequestException` with the service's one cursor wording, carrying no page and
/// no cursor — identically on every paginated operation, so the two listings answer the
/// same bytes.
#[tokio::test]
async fn every_paginated_listing_refuses_an_unissued_page_token_with_the_one_shared_refusal() {
    let portal = portal();
    // What each listing answered, so the refusal can be compared ACROSS the operations
    // and not only against the constant.
    let mut answered: Vec<(String, String)> = Vec::new();

    for listing in listings() {
        for (why, token) in unissued_tokens(&portal, &listing).await {
            let case = format!("{} presented {why}", listing.operation);
            let response = portal.get(&uri(&listing, Some(&token)), Some(ACCESS_TOKEN)).await;
            assert_refusal(
                &case,
                &response,
                StatusCode::BAD_REQUEST,
                "InvalidRequestException",
                INVALID_CURSOR_MESSAGE,
            );
            let body = body_json(&response);
            assert!(
                body.get(listing.items).is_none() && body.get("nextToken").is_none(),
                "case {case:?}: a refused cursor carries no page and no cursor, got {body}"
            );
            answered.push((case, body_text(&response)));
        }

        // The positive control: the cursor this listing DID issue resumes.
        let issued = first_cursor(&portal, &listing, ACCESS_TOKEN).await;
        let response = portal.get(&uri(&listing, Some(&issued)), Some(ACCESS_TOKEN)).await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "case {:?}: the cursor this listing issued must resume, got {} {:?}",
            listing.operation,
            response.status(),
            body_text(&response)
        );
    }

    let (first_case, first_body) = answered
        .first()
        .cloned()
        .expect("this service has paginated operations to sweep");
    for (case, body) in &answered {
        assert_eq!(
            body, &first_body,
            "case {case:?}: every paginated listing must refuse an unissued token with the same \
             bytes as case {first_case:?}, expected {first_body:?}, got {body:?}"
        );
    }
}

// Which listing a presented cursor belongs to, when the listing's own NAME carries the
// separator the codec writes between a cursor's fields.

/// The sign-in the rows below are read with, and a second one, so a cursor's scope differs
/// in the session that asked for it as well as in the listing it walks.
const BOUNDARY_TOKEN: &str = "tok-cursor-boundary";
const OTHER_BOUNDARY_TOKEN: &str = "tok-cursor-boundary-other";

/// The account id the adversarial spellings below are built around, and the one account id
/// under test that carries no separator at all.
const PLAIN_ACCOUNT: &str = "604444444444";

/// The 32 hex characters a cursor's scope carries for the sign-in session it is bound to,
/// derived the way `state::cursor_scope` derives them (`src/state.rs`).
///
/// A row needs them because the account ids under test are built to SPELL another
/// account's whole cursor scope, which ends in exactly these characters. A sign-in seeded
/// into this store is held under the one token it was seeded with, so that token is the
/// sign-in session identifier the scope digests.
fn session_digest(sign_in_session: &str) -> String {
    let digest = <sha2::Sha256 as sha2::Digest>::digest(sign_in_session.as_bytes());
    hex::encode(&digest[..16])
}

/// The accounts the boundary rows assign the sign-in, each with the roles assigned in it:
/// `(account id, roles)`.
///
/// The ids differ along what the rule generalises over — how the account id is spelt: no
/// separator at all, another account's whole cursor scope plus a segment, that scope
/// exactly, that scope followed by something spelt like a session digest, and segments
/// spelt like the byte counts a framed field carries. Each account holds more than one
/// role, so every role listing here is cut short and hands out a cursor, and the role
/// names carry the separator too, so a cursor's resume key is not one shape of string
/// only.
fn boundary_accounts() -> Vec<(String, Vec<String>)> {
    let digest = session_digest(BOUNDARY_TOKEN);
    let roles = |names: &[&str]| -> Vec<String> {
        names.iter().map(|name| (*name).to_string()).collect()
    };
    vec![
        (
            PLAIN_ACCOUNT.to_string(),
            roles(&["AdministratorAccess", "ReadOnly"]),
        ),
        (
            format!("{PLAIN_ACCOUNT}:{digest}:Z"),
            roles(&["Role:One", "RoleTwo"]),
        ),
        (
            format!("{PLAIN_ACCOUNT}:{digest}"),
            roles(&["Admin", "Role:Two"]),
        ),
        (
            format!("{PLAIN_ACCOUNT}:{digest}:{digest}"),
            roles(&["0:Zero", "ReadOnly"]),
        ),
        (
            "605555555555:2:00:12".to_string(),
            roles(&["3:Three", "Role_1"]),
        ),
    ]
}

/// The request reading all of `listing` in one page: the modelled `MaxResultType` maximum
/// (`smithy_query(query="shape", shape="MaxResultType")` gives the range 1..100) is more
/// than any row seeds.
fn whole_listing_uri(listing: &Listing) -> String {
    let separator = if listing.path.contains('?') { '&' } else { '?' };
    format!("{}{separator}max_result=100", listing.path)
}

/// The member of an item that identifies it in `listing`, and the key its cursor walks.
fn id_key(listing: &Listing) -> &'static str {
    if listing.items == "accountList" {
        "accountId"
    } else {
        "roleName"
    }
}

/// The ids `listing` reported in `response`, in the order it reported them.
fn reported_ids(listing: &Listing, response: &Response<Bytes>) -> Vec<String> {
    body_json(response)
        .get(listing.items)
        .and_then(|items| items.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item[id_key(listing)]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The ids `listing` must report, in order, for the sign-in `accounts` seeds: the account
/// ids ascending, or the roles ascending in the account the listing's route names.
fn expected_ids(accounts: &[(String, Vec<String>)], listing: &Listing) -> Vec<String> {
    let mut expected: Vec<String> = if listing.items == "accountList" {
        accounts.iter().map(|(id, _)| id.clone()).collect()
    } else {
        let account_id = listing
            .path
            .split_once("account_id=")
            .map(|(_, id)| id)
            .unwrap_or_default();
        accounts
            .iter()
            .find(|(id, _)| id == account_id)
            .map(|(_, roles)| roles.clone())
            .unwrap_or_default()
    };
    expected.sort();
    expected
}

/// Every paginated listing the boundary fixture seeds: the session's accounts, and the
/// roles in each of them.
fn boundary_listings(accounts: &[(String, Vec<String>)]) -> Vec<Listing> {
    let mut rows = vec![Listing {
        operation: "ListAccounts",
        path: "/assignment/accounts".to_string(),
        items: "accountList",
    }];
    for (account_id, _) in accounts {
        rows.push(Listing {
            operation: "ListAccountRoles",
            path: format!("/assignment/roles?account_id={account_id}"),
            items: "roleList",
        });
    }
    rows
}

/// RULE: a presented cursor belongs to the listing whose scope the codec wrote into it and
/// to no other, so a cursor minted for one listing of a sign-in is refused by every other
/// listing of that same sign-in — however the account ids naming those listings are spelt,
/// the codec's own separator included — while the listing that refused it goes on
/// reporting its own assignments in full.
#[tokio::test]
async fn a_cursor_belongs_to_one_listing_however_the_account_ids_are_spelt() {
    let portal = Portal::new();
    let accounts = boundary_accounts();
    portal.sign_in_assigned(BOUNDARY_TOKEN, accounts.clone());
    let rows = boundary_listings(&accounts);

    // The cursor each listing hands out, in the order of `rows`.
    let mut issued: Vec<String> = Vec::new();
    for listing in &rows {
        issued.push(first_cursor(&portal, listing, BOUNDARY_TOKEN).await);
    }
    let distinct: BTreeSet<&String> = issued.iter().collect();
    assert_eq!(
        distinct.len(),
        issued.len(),
        "two listings minted the same cursor, which no listing could then refuse: got \
         {issued:?}"
    );

    for (minted_by, token) in rows.iter().zip(&issued) {
        for presented_to in &rows {
            let case = format!(
                "the cursor {} minted for {:?}, presented to {:?}",
                minted_by.operation, minted_by.path, presented_to.path
            );
            let response = portal
                .get(&uri(presented_to, Some(token)), Some(BOUNDARY_TOKEN))
                .await;
            if presented_to.path == minted_by.path {
                // The positive control: its own listing resumes it, with the entries that
                // follow the page that issued it.
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "case {case:?}: a listing must resume the cursor it issued, got {} {:?}",
                    response.status(),
                    body_text(&response)
                );
                let expected: Vec<String> = expected_ids(&accounts, presented_to)
                    .into_iter()
                    .skip(PAGE_SIZE)
                    .take(PAGE_SIZE)
                    .collect();
                let served = reported_ids(presented_to, &response);
                assert_eq!(
                    served, expected,
                    "case {case:?}: expected the page after the one that issued it, \
                     {expected:?}, got {served:?}"
                );
            } else {
                assert_refusal(
                    &case,
                    &response,
                    StatusCode::BAD_REQUEST,
                    "InvalidRequestException",
                    INVALID_CURSOR_MESSAGE,
                );
                let body = body_json(&response);
                assert!(
                    body.get(presented_to.items).is_none() && body.get("nextToken").is_none(),
                    "case {case:?}: a refused cursor carries no page and no cursor, got {body}"
                );
            }
        }
    }

    // What the refusal protects: every listing still reports all of its own entries, so a
    // foreign cursor costs a caller nothing it is entitled to see.
    for listing in &rows {
        let case = format!("{} reading all of {:?}", listing.operation, listing.path);
        let response = portal
            .get(&whole_listing_uri(listing), Some(BOUNDARY_TOKEN))
            .await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "case {case:?}: expected 200, got {} {:?}",
            response.status(),
            body_text(&response)
        );
        let expected = expected_ids(&accounts, listing);
        let served = reported_ids(listing, &response);
        assert_eq!(
            served, expected,
            "case {case:?}: expected the whole listing {expected:?}, got {served:?}"
        );
    }
}

/// RULE: the cursor codec mints one token per (scope, resume key) — no two pairs spell the
/// same token — and is an exact inverse pair for each, so a cursor decodes under the scope
/// it was minted for and under no other, whatever bytes either field carries.
///
/// The table is the codec's own fields: every listing name this service walks (a role
/// listing's carries the requested account id verbatim, which is unconstrained), under two
/// sign-in sessions, with every resume key shape a listing hands out plus keys that spell
/// a scope's own tail.
#[test]
fn the_cursor_codec_mints_one_token_per_scope_and_key_and_decodes_it_under_no_other_scope() {
    let digest = session_digest(BOUNDARY_TOKEN);
    let listings = vec![
        "accounts".to_string(),
        format!("roles:{PLAIN_ACCOUNT}"),
        format!("roles:{PLAIN_ACCOUNT}:{digest}:Z"),
        format!("roles:{PLAIN_ACCOUNT}:{digest}"),
        format!("roles:{PLAIN_ACCOUNT}:{digest}:{digest}"),
        "roles:605555555555:2:00:12".to_string(),
        "roles:".to_string(),
        format!("roles:{PLAIN_ACCOUNT}:"),
    ];
    let keys = vec![
        String::new(),
        PLAIN_ACCOUNT.to_string(),
        "AdministratorAccess".to_string(),
        "Role:One".to_string(),
        format!("Z:{digest}:RoleOne"),
        format!("{digest}:RoleOne"),
        "3:Three".to_string(),
        "0".to_string(),
        // A multibyte key: a field's byte count is not its character count.
        "rôle-é".to_string(),
    ];

    // Every token minted, against the (scope, key) it was minted for.
    let mut minted: BTreeMap<String, (String, String)> = BTreeMap::new();
    for listing in &listings {
        for session in [BOUNDARY_TOKEN, OTHER_BOUNDARY_TOKEN] {
            let scope = state::cursor_scope(listing, session);
            for key in &keys {
                let case = format!("listing {listing:?}, session {session:?}, key {key:?}");
                let token = state::encode_cursor(&scope, key);
                let decoded = state::decode_cursor(&scope, &token);
                assert_eq!(
                    decoded.as_deref(),
                    Ok(key.as_str()),
                    "case {case:?}: a cursor must decode to the key it was minted for, got \
                     {decoded:?}"
                );
                if let Some(previous) = minted.insert(token.clone(), (scope.clone(), key.clone()))
                {
                    panic!(
                        "case {case:?}: expected a token no other scope and key mints, but \
                         {token:?} was already minted for {previous:?}"
                    );
                }
            }
        }
    }

    let scopes: BTreeSet<String> = minted.values().map(|(scope, _)| scope.clone()).collect();
    for (token, (scope, key)) in &minted {
        for other in &scopes {
            if other == scope {
                continue;
            }
            let case = format!(
                "the cursor for scope {scope:?} and key {key:?}, read under scope {other:?}"
            );
            let decoded = state::decode_cursor(other, token);
            assert_eq!(
                decoded,
                Err(InvalidCursor),
                "case {case:?}: a cursor of another scope must be refused, got {decoded:?}"
            );
        }
    }
}

/// RULE: only the ONE spelling the mint writes is admitted, so a payload merely shaped like
/// a cursor — the fields run together with a bare separator, a field's byte count spelt
/// another way, a field cut short or run long — is refused rather than mis-read.
#[test]
fn a_cursor_payload_the_mint_never_wrote_is_refused() {
    let scope = state::cursor_scope(&format!("roles:{PLAIN_ACCOUNT}"), BOUNDARY_TOKEN);
    let width = scope.len();
    let key = "AdministratorAccess";
    let crafted = [
        (
            "the fields run together with a bare separator",
            format!("v1:{scope}:{key}"),
        ),
        (
            "a resume key whose byte count carries a leading zero",
            format!("2:v1{width}:{scope}0{}:{key}", key.len()),
        ),
        (
            "a resume key shorter than its own byte count claims",
            format!("2:v1{width}:{scope}{}:{key}", key.len() + 1),
        ),
        (
            "a resume key longer than its own byte count claims",
            format!("2:v1{width}:{scope}{}:{key}", key.len() - 1),
        ),
        (
            "a payload that stops before the resume key",
            format!("2:v1{width}:{scope}"),
        ),
        (
            "a version this codec never stamped",
            format!("2:v2{width}:{scope}{}:{key}", key.len()),
        ),
        (
            "a scope framed with no byte count at all",
            format!("2:v1{scope}{}:{key}", key.len()),
        ),
    ];

    for (why, payload) in crafted {
        let token = hex::encode(&payload);
        let decoded = state::decode_cursor(&scope, &token);
        assert_eq!(
            decoded,
            Err(InvalidCursor),
            "case {why:?}: expected the payload {payload:?} to be refused, got {decoded:?}"
        );
    }

    // The positive control: the spelling the mint DOES write is admitted.
    let token = state::encode_cursor(&scope, key);
    let decoded = state::decode_cursor(&scope, &token);
    assert_eq!(
        decoded.as_deref(),
        Ok(key),
        "case \"the token as minted\": expected the key {key:?}, got {decoded:?}"
    );
}
