// SPDX-License-Identifier: CC0-1.0

//! How this service WORDS the refusal of a page cursor it never issued, over HTTP.
//!
//! THE RULE: a `NextToken` that is not a position in the listing being read is refused
//! with HTTP 400, the input error that listing models, and AWS's own sentence for THAT
//! operation's bad-cursor condition — wholly fixed text, carrying no part of the request
//! and in particular never echoing the rejected token, the same sentence for every form
//! the bad cursor takes. The sentence belongs to the operation, not to the service: the
//! listings whose refusal is rendered as `BadRequestException` read
//! `"Invalid next token."`, while `ListSuppressedDestinations`, whose model binds
//! `InvalidNextTokenException` for the condition, reads `"Token is invalid."` — so no one
//! sentence can stand for both, which the second suite below pins directly.
//! (`ListEmailIdentities` is worded differently again, echoing the token it rejected, and
//! is pinned beside its own operation in
//! `tests/all/email_identity_cursor_refusal_wording.rs`; nothing here is a claim about a
//! listing this table does not drive.)
//!
//! The table spans what the rule generalises over:
//!
//! * the LISTING — one reached by `@httpQuery` on a `GET` and one reached through a
//!   `POST` body, one that models `BadRequestException` for the refusal and one that
//!   models `InvalidNextTokenException`, each carrying its own recorded sentence;
//! * the FORM of the bad cursor — base64 of text no listing ever minted, a cursor
//!   ANOTHER listing of this service did mint, text that is not base64 at all, a bare
//!   resource name, and the empty string.
//!
//! Every row is answered over the real wire path (`MultiServiceRouter::dispatch` with
//! this service registered, through the crate's shared JSON wire harness
//! [`crate::json_wire`]), and each listing's positive control — the same call with no
//! cursor at all — is asserted beside the refusals, so a listing that refused everything
//! could not pass.

use serde_json::json;
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::{
    encode_tagged_page_token, CONTACT_LISTS_PAGE_TOKEN_TAG,
};

use crate::json_wire::{query_component, Ses};

/// AWS's fixed sentence for a cursor a listing that renders the refusal as
/// `BadRequestException` never issued.
const INVALID_NEXT_TOKEN: &str = "Invalid next token.";

/// AWS's fixed sentence for a cursor `ListSuppressedDestinations` never issued, which its
/// modelled `InvalidNextTokenException` carries.
const TOKEN_IS_INVALID: &str = "Token is invalid.";

/// Where a listing carries its `NextToken`: the two wire forms this service's listings
/// take.
#[derive(Debug, Clone, Copy)]
enum Cursor {
    /// `NextToken` is bound to `@httpQuery` on a `GET`.
    Query,
    /// `NextToken` is a member of the `POST` body.
    Body,
}

/// One listing under test: how it is addressed, where its cursor goes, the error
/// shape it models for a cursor that is not a position in it, and the sentence AWS words
/// that refusal with on THIS operation.
struct Listing {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    cursor: Cursor,
    error_code: &'static str,
    message: &'static str,
}

/// The listings the table spans, each with the error code its own model declares for the
/// refusal (`smithy_query(query="operation",operation="…")`) and its own recorded
/// sentence.
fn listings() -> Vec<Listing> {
    vec![
        Listing {
            case: "ListDedicatedIpPools",
            method: "GET",
            path: "/v2/email/dedicated-ip-pools",
            cursor: Cursor::Query,
            error_code: "BadRequestException",
            message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "GetDedicatedIps",
            method: "GET",
            path: "/v2/email/dedicated-ips",
            cursor: Cursor::Query,
            error_code: "BadRequestException",
            message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "ListConfigurationSets",
            method: "POST",
            path: "/v2/email/list-configuration-sets",
            cursor: Cursor::Body,
            error_code: "BadRequestException",
            message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "ListExportJobs",
            method: "POST",
            path: "/v2/email/list-export-jobs",
            cursor: Cursor::Body,
            error_code: "BadRequestException",
            message: INVALID_NEXT_TOKEN,
        },
        Listing {
            case: "ListSuppressedDestinations",
            method: "GET",
            path: "/v2/email/suppression/addresses",
            cursor: Cursor::Query,
            error_code: "InvalidNextTokenException",
            message: TOKEN_IS_INVALID,
        },
    ]
}

/// The forms a cursor this service never issued can arrive in: base64 of text no listing
/// mints, a cursor another listing of this service DID mint, text that is not base64,
/// a bare resource name, and nothing at all.
fn forged_cursors() -> Vec<(&'static str, String)> {
    vec![
        (
            "base64 of text no listing ever minted",
            "c2ltMnJlYWwtZm9yZ2VkLW5leHQtdG9rZW4=".to_string(),
        ),
        (
            "a cursor another listing of this service minted",
            encode_tagged_page_token(CONTACT_LISTS_PAGE_TOKEN_TAG, "a-list"),
        ),
        ("text that is not base64 at all", "!!not-a-token!!".to_string()),
        (
            "a bare resource name",
            ::aws_smithy_types::base64::encode("a-pool"),
        ),
        ("the empty string", String::new()),
    ]
}

#[tokio::test]
async fn a_cursor_this_service_never_issued_is_refused_with_aws_sentence() {
    let ses = Ses::new();
    for listing in listings() {
        // The positive control first: the same listing, with no cursor at all, is served.
        let served = match listing.cursor {
            Cursor::Query => ses.call(listing.method, listing.path, None).await,
            Cursor::Body => {
                ses.call(listing.method, listing.path, Some(json!({})))
                    .await
            }
        };
        assert_eq!(
            served.status,
            200,
            "case {:?} with no cursor: expected the listing to be served, got {} {:?}",
            listing.case,
            served.status,
            served.body
        );

        for (form, token) in forged_cursors() {
            let case = format!("{} with {form}", listing.case);
            let refused = match listing.cursor {
                Cursor::Query => {
                    let path = format!(
                        "{}?NextToken={}",
                        listing.path,
                        query_component(&token)
                    );
                    ses.call(listing.method, &path, None).await
                }
                Cursor::Body => {
                    ses.call(listing.method, listing.path, Some(json!({"NextToken": token})))
                        .await
                }
            };
            assert_eq!(
                (refused.status, refused.error_code().as_str()),
                (400, listing.error_code),
                "case {case:?}: expected 400 {}, got {} {} {:?}",
                listing.error_code,
                refused.status,
                refused.error_code(),
                refused.body
            );
            assert_eq!(
                refused.message(),
                listing.message,
                "case {case:?}: expected the refusal to read {:?}, got {:?}",
                listing.message,
                refused.message()
            );
            assert!(
                token.is_empty() || !refused.message().contains(&token),
                "case {case:?}: the refusal must not echo the rejected cursor, got {:?}",
                refused.message()
            );
        }
    }
}

#[tokio::test]
async fn the_cursor_sentence_belongs_to_the_operation_not_to_the_service() {
    let ses = Ses::new();
    // The listings whose recorded sentences differ, driven with the SAME forged cursors:
    // whatever a row answers can only be a property of the operation that answered it.
    let listings: Vec<Listing> = listings()
        .into_iter()
        .filter(|listing| {
            matches!(
                listing.case,
                "ListSuppressedDestinations" | "ListDedicatedIpPools" | "ListExportJobs"
            )
        })
        .collect();

    for (form, token) in forged_cursors() {
        let mut sentences: Vec<(&'static str, String)> = Vec::new();
        for listing in &listings {
            let case = format!("{} with {form}", listing.case);
            let refused = match listing.cursor {
                Cursor::Query => {
                    let path = format!(
                        "{}?NextToken={}",
                        listing.path,
                        query_component(&token)
                    );
                    ses.call(listing.method, &path, None).await
                }
                Cursor::Body => {
                    ses.call(listing.method, listing.path, Some(json!({"NextToken": token})))
                        .await
                }
            };
            assert_eq!(
                (refused.status, refused.message()),
                (400, listing.message.to_string()),
                "case {case:?}: expected 400 {:?}, got {} {:?}",
                listing.message,
                refused.status,
                refused.body
            );
            sentences.push((listing.case, refused.message()));
        }

        // The two classes really do read differently, so one shared sentence could not
        // satisfy both of them.
        let suppressed = sentences
            .iter()
            .find(|(case, _)| *case == "ListSuppressedDestinations")
            .map(|(_, message)| message.clone())
            .expect("the suppression listing is driven");
        for (case, message) in sentences
            .iter()
            .filter(|(case, _)| *case != "ListSuppressedDestinations")
        {
            assert_ne!(
                *message, suppressed,
                "case {case:?} with {form}: expected its own sentence rather than the \
                 suppression listing's {suppressed:?}, got {message:?}"
            );
        }
    }
}
