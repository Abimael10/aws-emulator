// SPDX-License-Identifier: CC0-1.0

//! The crate's ONE page-token codec, driven through the paginated reads that
//! share it.
//!
//! Every response byte these reads answer with is unchanged by the fold onto
//! `state::encode_page_token` / `state::decode_page_token`: what this suite
//! pins is that the reads go through that single pair, so one minting rule and
//! one acceptance rule decide every `nextToken` this service issues and takes
//! back. Each read adds only its own payload on top — the walked view and the
//! entries that walk has reported for `ListSourceViewsForBillingView`, the ARN
//! a page ended at, with the filters that select the walk, for
//! `ListBillingViews` — which travels beside the NAME of
//! the read that minted the token, under an authenticator the store computes
//! with a secret of its own, so a token is taken back only by the read that
//! issued it (see `token_scope.rs`).
//!
//! The rows differ along what the codec generalises over: the SPELLING of the
//! token bytes a client sends (the token as the read issued it, the same bytes
//! with their hex digits in upper case, one digit short of a whole byte,
//! characters outside the hex alphabet, hex too short to carry an
//! authenticator, the cursor with no authenticator in front of it, and the
//! cursor behind an authenticator of the caller's own making). Every row is
//! driven through both reads, and an accepted one must resume the walk while a
//! rejected one must draw the very same refusal from either read.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::encode_page_token;
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use service_common::page_tokens::AUTHENTICATOR_BYTES;
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, plus `call` and
// `error_code` — already exists for the billing-view lifecycle suite, so this
// module reuses it instead of building a second one.
use crate::billing_views::{call, emulator_with_engine, error_code, plant_source_views, ACCOUNT_ID};

/// How many request members narrow which views `ListBillingViews` walks
/// (`arns`, `billingViewTypes`, `names`, `ownerAccountId`, `sourceAccountId`
/// and `activeTimeRange`), and so how many scope fields its payload carries
/// ahead of the cut point — each empty for the unfiltered walk this suite pages.
const LIST_BILLING_VIEWS_FILTERS: usize = 6;

/// A pattern-valid `BillingViewArn` for a source view with resource id `id`.
fn source_arn(id: &str) -> String {
    format!("arn:aws:billing::{ACCOUNT_ID}:billingview/{id}")
}

/// The cursor a `nextToken` of `read_by` carries inside its authenticator: the
/// name of the read that minted the token, the NUL that separates the two
/// fields, then the payload that read resumes after.
pub(crate) fn page_token_cursor(read_by: &str, payload: &str) -> String {
    format!("{read_by}\u{0}{payload}")
}

/// A `nextToken` the STORE behind `engine` really minted for `read_by` and
/// `payload`, through the service's own encoder and under the store's own
/// secret.
///
/// This is how a suite gets a token for a cut point the read would not offer
/// from a page of its own (a position past the end of a list, say): the token is
/// the service's, not one a caller could assemble — which is the point of the
/// authenticator, and why no suite spells a valid token by hand.
pub(crate) fn page_token(
    engine: &Arc<engine::InMemoryEngine>,
    read_by: &str,
    payload: &str,
) -> String {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let key = state.lock().page_token_key();
    encode_page_token(&key, read_by, payload)
}

/// A value with the SHAPE of a minted token for `read_by` and `payload` — an
/// authenticator-width prefix, then that cursor — whose authenticator the
/// CALLER made up. The case only the authenticator can refuse.
pub(crate) fn token_with_a_made_up_authenticator(read_by: &str, payload: &str) -> String {
    let cursor = page_token_cursor(read_by, payload);
    hex::encode(bytes_with_a_made_up_authenticator(
        &[&[0_u8; AUTHENTICATOR_BYTES], cursor.as_bytes()].concat(),
    ))
}

/// The bytes of a minted token with its authenticator replaced by one the
/// CALLER made up, keeping whatever cursor travelled in it: a cut point the
/// store really issued, now behind an authenticator it never computed. For the
/// read whose tokens are not hex (`GetBillingPreferences` spells its own in
/// base64), so the caller of this chooses the spelling.
pub(crate) fn bytes_with_a_made_up_authenticator(token: &[u8]) -> Vec<u8> {
    let mut forged = vec![0_u8; AUTHENTICATOR_BYTES];
    forged.extend_from_slice(token.get(AUTHENTICATOR_BYTES..).unwrap_or_default());
    forged
}

/// The cursor of `read_by` and `payload` with NO authenticator in front of it:
/// the whole token a caller can read out of a minted one and send back on its
/// own.
pub(crate) fn cursor_without_its_authenticator(read_by: &str, payload: &str) -> String {
    hex::encode(page_token_cursor(read_by, payload))
}

/// How a client spells the token bytes, and whether the crate's one codec
/// accepts that spelling.
struct Spelling {
    case: &'static str,
    /// The token a client sends, given the token the read ITSELF issued for the
    /// page boundary (see [`Walk::issued`]) and the cursor inside it.
    spell: fn(&Walk) -> String,
    accepted: bool,
}

fn spellings() -> Vec<Spelling> {
    vec![
        Spelling {
            case: "the token the read issued",
            spell: |walk| walk.issued.clone(),
            accepted: true,
        },
        Spelling {
            case: "the same bytes with upper-case hex digits",
            spell: |walk| walk.issued.to_uppercase(),
            accepted: true,
        },
        Spelling {
            case: "one hex digit short of a whole byte",
            spell: |walk| {
                let mut token = walk.issued.clone();
                token.pop();
                token
            },
            accepted: false,
        },
        Spelling {
            case: "characters outside the hex alphabet",
            // Inside the modeled `PageToken` pattern `^[-a-zA-Z0-9+=/_]+$`, so
            // the boundary validator passes it through.
            spell: |_| "zzzz".to_string(),
            accepted: false,
        },
        Spelling {
            case: "hex too short to carry an authenticator",
            spell: |_| "ff".to_string(),
            accepted: false,
        },
        Spelling {
            case: "the cursor with no authenticator in front of it",
            spell: |walk| cursor_without_its_authenticator(walk.operation, &walk.payload),
            accepted: false,
        },
        Spelling {
            case: "the cursor behind an authenticator the caller made up",
            spell: |walk| token_with_a_made_up_authenticator(walk.operation, &walk.payload),
            accepted: false,
        },
    ]
}

/// One paginated read of this service: the request fields that select its walk,
/// the payload its own first page of one entry ends at, the token that page
/// carried, and the entries a page resuming after it reports.
struct Walk {
    operation: &'static str,
    base: Value,
    payload: String,
    issued: String,
    expected_after: Vec<String>,
}

/// The entries a page reports, whichever read answered. A protocol that omits
/// an empty list and one that writes `[]` mean the same thing, so both read
/// back as no entries.
fn entries(operation: &str, body: &Value) -> Vec<String> {
    match operation {
        "ListSourceViewsForBillingView" => body
            .get("sourceViews")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .map(|entry| entry.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        _ => body
            .get("billingViews")
            .and_then(Value::as_array)
            .map(|listed| {
                listed
                    .iter()
                    .map(|view| view["arn"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The token a read's own first page of one entry carries.
async fn issued_token(router: &MultiServiceRouter, operation: &str, base: &Value) -> String {
    let mut request = base.clone();
    request["maxResults"] = json!(1);
    let (status, page) = call(router, operation, &request).await;
    assert_eq!(
        status, 200,
        "setup {operation:?}: expected 200 for a first page of one entry, got {status} {page:?}"
    );
    page["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup {operation:?}: a first page of one of many entries must carry a nextToken, got {page:?}")
        })
        .to_string()
}

/// The world both reads page over: a `CUSTOM` view holding three source views,
/// beside the account's `PRIMARY` view.
///
/// The three source views are planted first: a create resolves every ARN its
/// `sourceViews` names against the store, and this suite is about the token
/// codec, not about whether a source exists.
async fn walks(
    router: &MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
) -> Vec<Walk> {
    let sources = vec![
        source_arn("codec-src-a"),
        source_arn("codec-src-b"),
        source_arn("codec-src-c"),
    ];
    plant_source_views(engine, &sources);
    let (status, created) = call(
        router,
        "CreateBillingView",
        &json!({ "name": "page-token-codec", "sourceViews": sources }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateBillingView to answer 200, got {status} {created:?}"
    );
    let view_arn = created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateBillingView must answer an arn, got {created:?}"))
        .to_string();

    // `ListBillingViews` walks the views in ARN order and its page boundary is
    // the ARN the page ended at, so the unpaginated answer says both what the
    // first page's cursor is and what a page resuming after it must report.
    let (status, all) = call(router, "ListBillingViews", &json!({})).await;
    assert_eq!(
        status, 200,
        "setup: expected ListBillingViews to answer 200, got {status} {all:?}"
    );
    let every_view = entries("ListBillingViews", &all);
    assert!(
        every_view.len() >= 2,
        "setup: expected the created view beside the PRIMARY one, got {every_view:?}"
    );

    let sources_base = json!({ "arn": view_arn });
    let views_base = json!({});
    vec![
        Walk {
            operation: "ListSourceViewsForBillingView",
            issued: issued_token(router, "ListSourceViewsForBillingView", &sources_base).await,
            base: sources_base,
            // The payload a page of one entry ends at: the view being walked,
            // then the one entry that page reported.
            payload: format!("{view_arn}#{}", sources[0]),
            expected_after: sources[1..].to_vec(),
        },
        Walk {
            operation: "ListBillingViews",
            issued: issued_token(router, "ListBillingViews", &views_base).await,
            base: views_base,
            // The payload a page of one view ends at: one field per filter
            // member that selects which views are walked — each empty, because
            // this request names none of them — and then the ARN the page ended
            // at.
            payload: format!(
                "{}{}",
                "#".repeat(LIST_BILLING_VIEWS_FILTERS),
                every_view[0]
            ),
            expected_after: every_view[1..].to_vec(),
        },
    ]
}

#[tokio::test]
async fn every_paginated_read_mints_its_token_with_the_shared_encoder() {
    let (router, engine) = emulator_with_engine();

    for walk in walks(&router, &engine).await {
        let case = walk.operation;
        // The shared encoder's form: hex of the authenticator, then the cursor
        // the read names itself and its page boundary in. The authenticator
        // cannot be recomputed without the store's secret — that is what makes
        // the token unforgeable — so what is pinned here is the rest: the
        // spelling, the width of the authenticator, and the cursor behind it.
        let bytes = hex::decode(&walk.issued).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a hex token, got {:?} ({error})", walk.issued)
        });
        assert_eq!(
            walk.issued,
            walk.issued.to_lowercase(),
            "case {case:?}: expected the encoder's lower-case hex, got {:?}",
            walk.issued
        );
        assert!(
            bytes.len() > AUTHENTICATOR_BYTES,
            "case {case:?}: expected an authenticator and a cursor, got {} byte(s)",
            bytes.len()
        );
        assert_eq!(
            String::from_utf8(bytes[AUTHENTICATOR_BYTES..].to_vec()).ok(),
            Some(page_token_cursor(walk.operation, &walk.payload)),
            "case {case:?}: expected the shared encoder's cursor for payload {:?}, got {:?}",
            walk.payload,
            walk.issued
        );
    }
}

#[tokio::test]
async fn a_token_spelling_the_shared_codec_accepts_resumes_every_paginated_read() {
    let (router, engine) = emulator_with_engine();

    for walk in walks(&router, &engine).await {
        for spelling in spellings().into_iter().filter(|spelling| spelling.accepted) {
            let case = format!("{} / {}", walk.operation, spelling.case);
            let mut request = walk.base.clone();
            request["nextToken"] = json!((spelling.spell)(&walk));
            let (status, page) = call(&router, walk.operation, &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200, got {status} {page:?}"
            );
            assert_eq!(
                entries(walk.operation, &page),
                walk.expected_after,
                "case {case:?}: expected the entries after {:?}, got {page:?}",
                walk.payload
            );
        }
    }
}

#[tokio::test]
async fn a_token_spelling_the_shared_codec_rejects_is_refused_the_same_way_by_every_read() {
    let (router, engine) = emulator_with_engine();
    let walks = walks(&router, &engine).await;

    for spelling in spellings()
        .into_iter()
        .filter(|spelling| !spelling.accepted)
    {
        let mut refusals: Vec<(String, Value)> = Vec::new();
        for walk in &walks {
            let case = format!("{} / {}", walk.operation, spelling.case);
            let mut request = walk.base.clone();
            request["nextToken"] = json!((spelling.spell)(walk));
            let (status, refused) = call(&router, walk.operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (400, "ValidationException"),
                "case {case:?}: expected 400 ValidationException, got {status} {refused:?}"
            );
            assert!(
                !refused["message"].as_str().unwrap_or_default().is_empty(),
                "case {case:?}: expected a refusal message, got {refused:?}"
            );
            // `cannotParse` is the `ValidationExceptionReason` value for input the
            // service cannot read back (the enum declares `unknownOperation`,
            // `cannotParse`, `fieldValidationFailed` and `other`), and the
            // `fieldList` entry names the member that carried it.
            assert_eq!(
                refused["reason"].as_str(),
                Some("cannotParse"),
                "case {case:?}: expected the reason for input that cannot be read back, \
                 got {refused:?}"
            );
            assert_eq!(
                refused["fieldList"][0]["name"].as_str(),
                Some("nextToken"),
                "case {case:?}: expected the fieldList to name nextToken, got {refused:?}"
            );
            refusals.push((
                case,
                json!({
                    "message": refused["message"].clone(),
                    "reason": refused["reason"].clone(),
                    "fieldList": refused["fieldList"].clone(),
                }),
            ));
        }

        // One codec, one refusal: whichever read answered, the client sees the
        // same message, the same reason and the same offending field.
        let (first_case, first) = &refusals[0];
        for (case, refusal) in &refusals[1..] {
            assert_eq!(
                refusal, first,
                "case {case:?}: expected the refusal {first_case:?} answers with ({first:?}), \
                 got {refusal:?}"
            );
        }
    }
}
