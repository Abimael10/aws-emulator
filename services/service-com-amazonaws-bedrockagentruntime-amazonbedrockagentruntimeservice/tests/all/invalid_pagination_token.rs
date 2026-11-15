// SPDX-License-Identifier: CC0-1.0

//! How every LISTING of this service words a `nextToken` it did not issue, asserted
//! end-to-end over the wire through the `MultiServiceRouter` the production emulator serves
//! with (the harness in [`crate::sessions`]).
//!
//! THE RULE UNDER TEST: a listing that decodes a `nextToken` refuses a cursor that is not
//! one IT issued with the modelled 400 `ValidationException`
//! (`smithy_query(query="operation",operation="ListSessions,ListInvocations,ListInvocationSteps")`
//! -> `ValidationException` httpError 400) whose message is the one FIXED sentence `Next
//! token for pagination is invalid`. The sentence has no variable part at all: the rejected
//! token is not echoed, and neither the listing that refused it nor the session it was
//! scoped to enters it, so a client matches one string however the cursor went wrong.
//!
//! The rows span the listings of this service and the ways a cursor can fail to be one the
//! listing issued — text that is no cursor at all, a cursor-shaped string of this service's
//! own encoding, a cursor another listing issued, and a cursor the same listing issued for
//! another session — because the rule generalises over the REASON: none of it reaches the
//! message. The positive control at the end of each row set is a cursor the listing really
//! did issue, which still resumes the page.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::sessions::{call, create_invocation, create_session, date_time, error_code, label, router};

/// The status and error code every refusal here expects.
const INVALID: (u16, &str) = (400, "ValidationException");

/// RULE: the one sentence a listing states for a cursor it did not issue. A CONSTANT — it
/// interpolates nothing.
const INVALID_NEXT_TOKEN: &str = "Next token for pagination is invalid";

/// The instant the seeded steps carry, as epoch seconds.
const STEP_TIME: i64 = 1_783_501_811;

/// Which listing a row drives, and what it is addressed by.
#[derive(Clone, Copy, Debug)]
enum Listing {
    /// `POST /sessions/`, which lists the account's sessions and is scoped to nothing else.
    Sessions,
    /// `POST /sessions/{sessionIdentifier}/invocations/`.
    Invocations,
    /// `POST /sessions/{sessionIdentifier}/invocationSteps/`.
    InvocationSteps,
}

impl Listing {
    /// The path one page of this listing is read at, inside `session_id` where the listing
    /// is session-scoped, asking for one item at a time so a token is issued while items
    /// remain.
    fn path(self, session_id: &str, token: Option<&str>) -> String {
        let mut path = match self {
            Self::Sessions => "/sessions/?maxResults=1".to_string(),
            Self::Invocations => {
                format!("/sessions/{}/invocations/?maxResults=1", label(session_id))
            }
            Self::InvocationSteps => {
                format!(
                    "/sessions/{}/invocationSteps/?maxResults=1",
                    label(session_id)
                )
            }
        };
        if let Some(token) = token {
            path.push_str(&format!("&nextToken={token}"));
        }
        path
    }

    /// The body one page of this listing carries: the `invocationIdentifier` filter where the
    /// listing takes one, and nothing otherwise.
    fn body(self, invocation_id: &str) -> Value {
        match self {
            Self::Sessions | Self::Invocations => Value::Null,
            Self::InvocationSteps => json!({ "invocationIdentifier": invocation_id }),
        }
    }

    /// The member a successful page reports its items in.
    fn summaries(self) -> &'static str {
        match self {
            Self::Sessions => "sessionSummaries",
            Self::Invocations => "invocationSummaries",
            Self::InvocationSteps => "invocationStepSummaries",
        }
    }

    /// Whether this listing is scoped to ONE session, so that a cursor it issued inside
    /// another session is a foreign one. The account-wide listing has no such scope: every
    /// cursor it issues names the same listing.
    fn session_scoped(self) -> bool {
        !matches!(self, Self::Sessions)
    }
}

/// One session holding two invocations, each holding two steps, so every listing of this
/// service has more items than one page and issues a cursor.
struct Seeded {
    session_id: String,
    invocation_id: String,
}

/// Seed `sessions` sessions of that shape and report each one.
async fn seed(router: &MultiServiceRouter, sessions: usize) -> Vec<Seeded> {
    let mut seeded = Vec::with_capacity(sessions);
    for _ in 0..sessions {
        let (session_id, _) = create_session(router, Value::Null).await;
        let mut first_invocation = None;
        for invocation in 0..2 {
            let invocation_id = create_invocation(router, &session_id, json!({})).await;
            for step in 0..2 {
                let (status, out) = call(
                    router,
                    "PUT",
                    &format!("/sessions/{session_id}/invocationSteps/"),
                    json!({
                        "invocationIdentifier": invocation_id,
                        "invocationStepTime": date_time(STEP_TIME + step),
                        "payload": {
                            "contentBlocks": [{ "text": format!("step-{invocation}-{step}") }],
                        },
                    }),
                )
                .await;
                assert_eq!(
                    status, 201,
                    "seeding a step: expected 201, got {status} {out:?}"
                );
            }
            if first_invocation.is_none() {
                first_invocation = Some(invocation_id);
            }
        }
        seeded.push(Seeded {
            session_id,
            invocation_id: first_invocation.expect("each seeded session holds an invocation"),
        });
    }
    seeded
}

/// Read one page of `listing`, with the cursor the caller asks for.
async fn page(
    router: &MultiServiceRouter,
    listing: Listing,
    seeded: &Seeded,
    token: Option<&str>,
) -> (u16, Value) {
    call(
        router,
        "POST",
        &listing.path(&seeded.session_id, token),
        listing.body(&seeded.invocation_id),
    )
    .await
}

/// The cursor a first page of `listing` issues, which there must be one of: each seeded
/// scope holds more items than the one this page asks for.
async fn issued_token(router: &MultiServiceRouter, listing: Listing, seeded: &Seeded) -> String {
    let (status, first) = page(router, listing, seeded, None).await;
    assert_eq!(
        status, 200,
        "{listing:?}: a first page must be served, got {status} {first:?}"
    );
    first["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("{listing:?}: a first page of 1 must issue a cursor, got {first:?}"))
        .to_string()
}

/// A string this service's own cursor encoding produces for a scope no listing here names:
/// the base64 of a cursor body, so the value is cursor-SHAPED and still not one any listing
/// issued. Encoded with the codec the service encodes a cursor with
/// (`aws_smithy_types::base64`), so nothing about the form is guessed.
fn cursor_shaped() -> String {
    ::aws_smithy_types::base64::encode("v1:NoSuchListing::7")
}

/// The rule over every listing and every way a cursor can fail to be one it issued.
#[tokio::test]
async fn a_cursor_a_listing_did_not_issue_is_refused_with_the_one_fixed_sentence() {
    let router = router();
    let seeded = seed(&router, 2).await;
    let (first, second) = (&seeded[0], &seeded[1]);

    let listings = [
        Listing::Sessions,
        Listing::Invocations,
        Listing::InvocationSteps,
    ];
    // The cursor each listing issues for the FIRST seeded session, and the one the
    // session-scoped listings issue for the second — the cross-scope rows below reuse them.
    let mut issued = Vec::new();
    for listing in listings {
        issued.push((
            listing,
            issued_token(&router, listing, first).await,
            issued_token(&router, listing, second).await,
        ));
    }

    for (listing, own, other_scope) in &issued {
        // (case, the cursor the page carries)
        let mut rows: Vec<(String, String)> = vec![
            (
                "text that is no cursor of this service at all".to_string(),
                "not-a-token-this-service-issued".to_string(),
            ),
            (
                "a cursor-shaped string naming a scope no listing has".to_string(),
                cursor_shaped(),
            ),
            (
                "a cursor a DIFFERENT listing issued".to_string(),
                issued
                    .iter()
                    .find(|(other, _, _)| {
                        std::mem::discriminant(other) != std::mem::discriminant(listing)
                    })
                    .map(|(_, token, _)| token.clone())
                    .expect("another listing issued a cursor"),
            ),
        ];
        if listing.session_scoped() {
            // Only a listing scoped to one session HAS another scope for a cursor to belong
            // to; the account-wide listing's own cursor is not foreign to it.
            rows.push((
                "a cursor this listing issued for ANOTHER session".to_string(),
                other_scope.clone(),
            ));
        }

        for (reason, token) in rows {
            let case = format!("{listing:?} handed {reason}");
            let (status, refused) = page(&router, *listing, first, Some(&token)).await;
            assert_eq!(
                (status, error_code(&refused)),
                INVALID,
                "case {case:?}: expected {INVALID:?}, got {status} {refused:?}"
            );
            let stated = refused["message"].as_str().unwrap_or_default();
            assert_eq!(
                stated, INVALID_NEXT_TOKEN,
                "case {case:?}: expected message {INVALID_NEXT_TOKEN:?}, got {stated:?}"
            );
            assert!(
                !stated.contains(&token)
                    && !stated.contains(&first.session_id)
                    && !stated.contains("nextToken"),
                "case {case:?}: the sentence echoes neither the token, the session nor the \
                 member, got {stated:?}"
            );
        }

        // The positive control: the cursor this listing really did issue still resumes it,
        // so what the rows above pin is the refusal of a FOREIGN cursor and not a blanket
        // refusal of pagination.
        let (status, resumed) = page(&router, *listing, first, Some(own)).await;
        let items = resumed[listing.summaries()].as_array().map(Vec::len);
        assert_eq!(
            (status, items),
            (200, Some(1)),
            "case {listing:?} resumed with its own cursor: expected one more item, got \
             {status} {resumed:?}"
        );
    }
}

/// The refusal is the same sentence whichever session-scoped listing is addressed by an ARN
/// rather than by a bare session id: how the scope was named is not part of the cursor rule.
#[tokio::test]
async fn the_sentence_does_not_depend_on_how_the_session_was_addressed() {
    let router = router();
    let (session_id, session_arn) = create_session(&router, Value::Null).await;
    let invocation_id = create_invocation(&router, &session_id, json!({})).await;

    for (form, identifier) in [("its id", &session_id), ("its ARN", &session_arn)] {
        for (listing, tail) in [
            ("ListInvocations", "invocations"),
            ("ListInvocationSteps", "invocationSteps"),
        ] {
            let case = format!("{listing} addressed by {form}");
            let (status, refused) = call(
                &router,
                "POST",
                &format!(
                    "/sessions/{}/{tail}/?nextToken=not-a-token-this-service-issued",
                    label(identifier)
                ),
                json!({ "invocationIdentifier": invocation_id }),
            )
            .await;
            assert_eq!(
                (status, error_code(&refused)),
                INVALID,
                "case {case:?}: expected {INVALID:?}, got {status} {refused:?}"
            );
            let stated = refused["message"].as_str().unwrap_or_default();
            assert_eq!(
                stated, INVALID_NEXT_TOKEN,
                "case {case:?}: expected message {INVALID_NEXT_TOKEN:?}, got {stated:?}"
            );
        }
    }
}
