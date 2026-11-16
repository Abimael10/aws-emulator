// SPDX-License-Identifier: CC0-1.0

//! A page cursor is continued by the SIGN-IN SESSION it was issued to, not by the one
//! token that happened to present it.
//!
//! RULE: every page cursor this service issues is bound to the listing it walks and to the
//! sign-in session that asked for it, so any token of that sign-in's chain continues the
//! walk — the pair a refresh rotation issues, which supersedes the access token it rotates
//! while the sign-in itself continues, included — while a cursor issued to a different
//! sign-in is still refused.
//!
//! The rows differ along everything the rule generalises over: which paginated operation is
//! walked (the sign-in's accounts, and one account's roles), how many items the listing
//! holds, the page size it is cut at, and how many rotations happen between the cursor
//! being issued and being presented (none, one, and one before every continuation, so a
//! whole chain of tokens walks one listing). Each row asserts the walk reports every item
//! of the listing exactly once and in order, and stops when the listing runs out.
//!
//! Two controls keep that from passing for the wrong reason: a cursor the SAME listing
//! issued to another sign-in is refused with the service's one cursor refusal, and the
//! token each rotation superseded is itself no longer served — so the page really is
//! reachable only through the rotated token.
//!
//! The harness is `minted_sign_in`'s, so the sign-in and the assignments arrive the way
//! they do in a running emulator: the emulator's own OIDC service mints the tokens and
//! rotates them, and an administrator's IAM Identity Center assignments are what the
//! listings report.

use http::{Method, StatusCode};
use serde_json::Value;

use service_com_amazonaws_sso_swbportalservice::state::{
    INVALID_CURSOR_MESSAGE, INVALID_SESSION_MESSAGE,
};

use crate::minted_sign_in::{
    assigned_emulator, error_code, granted, reported_accounts, reported_roles, role_name, Emulator,
    MintedToken,
};

/// Which paginated listing a row walks, and what it must report.
struct Listing {
    /// The operation's name, for the case label.
    operation: &'static str,
    /// The request path, before the paging query parameters.
    path: String,
    /// Reads the items a page of THIS listing reports, from the output member they arrive in.
    /// Held per row rather than matched on the operation name, so a third paginated listing
    /// has to name its reader instead of silently inheriting another's.
    items: fn(&Value) -> Vec<String>,
    /// The items the whole walk must report, in the order the listing reports them.
    expected: Vec<String>,
}

impl Listing {
    /// The items `body` reports, read from the member this listing arrives in.
    fn items(&self, body: &Value) -> Vec<String> {
        (self.items)(body)
    }
}

/// Every paginated listing of this service, pointed at what the assignments of
/// [`granted`] put in force: the sign-in's accounts, and the roles of each account that
/// holds more than one (so a role listing is cut short too).
fn listings() -> Vec<Listing> {
    let rows = granted();
    let mut accounts: Vec<String> = rows.iter().map(|row| row.account_id.clone()).collect();
    accounts.sort();
    let mut listings = vec![Listing {
        operation: "ListAccounts",
        path: "/assignment/accounts".to_string(),
        items: reported_accounts,
        expected: accounts,
    }];
    for row in &rows {
        if row.permission_sets.len() < 2 {
            continue;
        }
        let mut roles: Vec<String> = row
            .permission_sets
            .iter()
            .map(|name| role_name(name, &row.account_id))
            .collect();
        roles.sort();
        listings.push(Listing {
            operation: "ListAccountRoles",
            path: format!("/assignment/roles?account_id={}", row.account_id),
            items: reported_roles,
            expected: roles,
        });
    }
    listings
}

/// How often the client's token pair is rotated while one listing is walked.
#[derive(Debug, Clone, Copy)]
enum Rotations {
    /// Never: the control that the walk works without a rotation at all.
    None,
    /// Once, after the first page: the cursor was minted for a token the mint has since
    /// superseded and dropped, so only the rotated token can present it.
    AfterTheFirstPage,
    /// Before every continuation: one listing walked by a whole chain of tokens.
    BeforeEveryPage,
}

impl Rotations {
    /// Whether the pair is rotated before the page after `pages_read` is asked for.
    fn before_page(self, pages_read: usize) -> bool {
        match self {
            Self::None => false,
            Self::AfterTheFirstPage => pages_read == 1,
            Self::BeforeEveryPage => true,
        }
    }
}

/// The request reading `listing` at `page_size`, resuming from `cursor` when there is one.
fn uri(listing: &Listing, page_size: usize, cursor: Option<&str>) -> String {
    let separator = if listing.path.contains('?') { '&' } else { '?' };
    let mut uri = format!("{}{separator}max_result={page_size}", listing.path);
    if let Some(cursor) = cursor {
        uri.push_str(&format!("&next_token={cursor}"));
    }
    uri
}

/// One paginated GET of the portal.
async fn read(
    emulator: &Emulator,
    listing: &Listing,
    page_size: usize,
    cursor: Option<&str>,
    token: &str,
) -> (StatusCode, Value) {
    emulator
        .portal(Method::GET, &uri(listing, page_size, cursor), token)
        .await
}

/// The cursor `listing`'s first page hands out, read with `token` at `page_size`.
async fn first_cursor(
    emulator: &Emulator,
    listing: &Listing,
    page_size: usize,
    token: &str,
) -> String {
    let (status, body) = read(emulator, listing, page_size, None, token).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "case {:?}: the first page must answer 200, got {status} {body}",
        listing.operation
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {:?}: a listing of {} items read {page_size} at a time is cut short, so \
                 it must issue a cursor, got {body}",
                listing.operation,
                listing.expected.len()
            )
        })
        .to_string()
}

/// The error code and message of a refusal, as one value a case can compare.
fn refusal(body: &Value) -> (String, String) {
    (
        error_code(body),
        body["message"].as_str().unwrap_or_default().to_string(),
    )
}

/// Walk `listing` a page at a time, rotating the client's token pair as `rotations` says,
/// and answer every item the walk reported in order.
async fn walked(
    emulator: &Emulator,
    listing: &Listing,
    page_size: usize,
    rotations: Rotations,
    minted: MintedToken,
) -> Vec<String> {
    let case = format!(
        "{} read {page_size} at a time, rotating {rotations:?}",
        listing.operation
    );
    let mut held = minted;
    let mut reported: Vec<String> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0usize;

    loop {
        if rotations.before_page(pages) && pages > 0 {
            let superseded = held.access_token.clone();
            held = emulator.rotated(&held).await;
            // The rotation really did supersede the token the cursor was issued to: that
            // token is no longer served at all, so the page below is reachable only
            // through the token the rotation issued.
            let (status, body) = read(
                emulator,
                listing,
                page_size,
                cursor.as_deref(),
                &superseded,
            )
            .await;
            assert_eq!(
                (status, refusal(&body)),
                (
                    StatusCode::UNAUTHORIZED,
                    (
                        "UnauthorizedException".to_string(),
                        INVALID_SESSION_MESSAGE.to_string()
                    )
                ),
                "case {case:?}: the token the rotation superseded must stop being served, got \
                 {status} {body}"
            );
        }

        let (status, body) = read(
            emulator,
            listing,
            page_size,
            cursor.as_deref(),
            &held.access_token,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: page {} must be served to a token of the sign-in the cursor was \
             issued to, got {status} {body}",
            pages + 1
        );
        let page = listing.items(&body);
        assert!(
            !page.is_empty() && page.len() <= page_size,
            "case {case:?}: page {} must carry between 1 and {page_size} items, got {page:?}",
            pages + 1
        );
        reported.extend(page);
        pages += 1;
        cursor = body["nextToken"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
        assert!(
            pages <= listing.expected.len(),
            "case {case:?}: a walk of {} items kept handing out cursors after {pages} pages, \
             reporting {reported:?}",
            listing.expected.len()
        );
    }

    assert!(
        pages > 1,
        "case {case:?}: the listing must be cut into more than one page for the rule to be \
         under test, got {pages} page(s) of {reported:?}"
    );
    reported
}

/// RULE: a page cursor is bound to the sign-in session and the listing, so every token of
/// that sign-in's chain continues the walk and the whole listing is reported exactly once,
/// a rotation of the client's token pair notwithstanding.
#[tokio::test]
async fn a_page_cursor_is_continued_by_every_token_of_the_sign_in_it_was_issued_to() {
    let assigned = assigned_emulator(&granted()).await;
    let emulator = &assigned.emulator;

    for listing in listings() {
        for page_size in [1, 2] {
            if listing.expected.len() <= page_size {
                // Nothing to continue: this listing is one page at this size.
                continue;
            }
            for rotations in [
                Rotations::None,
                Rotations::AfterTheFirstPage,
                Rotations::BeforeEveryPage,
            ] {
                let minted = emulator.minted_sign_in(&assigned.identity_store_id).await;
                let reported = walked(emulator, &listing, page_size, rotations, minted).await;
                assert_eq!(
                    reported,
                    listing.expected,
                    "case {:?} read {page_size} at a time, rotating {rotations:?}: the walk must \
                     report every item of the listing exactly once and in order, expected {:?}, \
                     got {reported:?}",
                    listing.operation,
                    listing.expected
                );
            }
        }
    }
}

/// RULE (the other half): the cursor is still bound to ONE sign-in — a cursor the same
/// listing issued to a different sign-in session is refused with the service's one cursor
/// refusal, carrying no page.
#[tokio::test]
async fn a_page_cursor_issued_to_another_sign_in_is_still_refused() {
    let assigned = assigned_emulator(&granted()).await;
    let emulator = &assigned.emulator;

    for listing in listings() {
        let page_size = 1;
        let mine = emulator.minted_sign_in(&assigned.identity_store_id).await;
        // A second sign-in of the same user, minted by its own walk of the device flow: a
        // sign-in session of its own, holding the same assignments.
        let other = emulator.minted_sign_in(&assigned.identity_store_id).await;
        let theirs = first_cursor(emulator, &listing, page_size, &other.access_token).await;

        let (status, body) = read(
            emulator,
            &listing,
            page_size,
            Some(&theirs),
            &mine.access_token,
        )
        .await;
        assert_eq!(
            (status, refusal(&body)),
            (
                StatusCode::BAD_REQUEST,
                (
                    "InvalidRequestException".to_string(),
                    INVALID_CURSOR_MESSAGE.to_string()
                )
            ),
            "case {:?}: a cursor issued to another sign-in must be refused, got {status} {body}",
            listing.operation
        );
        assert!(
            body.get("nextToken").is_none() && listing.items(&body).is_empty(),
            "case {:?}: a refused cursor carries no page and no cursor, got {body}",
            listing.operation
        );

        // The control: the cursor the other sign-in was issued continues ITS own walk, and
        // every token of this sign-in continues this one.
        let (status, body) = read(
            emulator,
            &listing,
            page_size,
            Some(&theirs),
            &other.access_token,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {:?}: the sign-in the cursor was issued to must continue its walk, got \
             {status} {body}",
            listing.operation
        );
        let rotated = emulator.rotated(&other).await;
        let (status, body) = read(
            emulator,
            &listing,
            page_size,
            Some(&theirs),
            &rotated.access_token,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {:?}: the token a rotation of that sign-in issued must continue the same \
             walk, got {status} {body}",
            listing.operation
        );
    }
}
