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
//! The harness is `portal_session`'s, so every suite here speaks to one mounted router.

use http::StatusCode;
use service_com_amazonaws_sso_swbportalservice::state::INVALID_CURSOR_MESSAGE;
use std::collections::BTreeSet;

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
