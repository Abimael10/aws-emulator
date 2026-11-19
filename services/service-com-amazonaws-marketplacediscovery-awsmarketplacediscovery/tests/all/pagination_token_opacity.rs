// SPDX-License-Identifier: CC0-1.0

//! What a `nextToken` of this service IS: a continuation handle its holder can hand back
//! and read nothing out of, and that nobody but this emulator can produce.
//!
//! The rule: a pagination token is minted by the SERVICE. Its bytes are opaque — they carry
//! no rendering of the request the page was cut for and none of the position it resumes at,
//! so a token that is logged, cached, forwarded or kept as a fixture discloses neither —
//! and they are unforgeable, so only a value this service minted for THIS query names a
//! page position at all and every other value is refused as invalid input
//! (`ValidationException`, HTTP 400) rather than served as a page. What stays contractual is
//! the member's own shape: `NextToken` is `@length(0..8192)` with
//! `@pattern ^[a-zA-Z0-9+/=]+$`, and the token is "the returned token to retrieve the next
//! page" of the SAME request
//! (`smithy_query(query="operation",operation="SearchListings")`).
//!
//! The rows span the service's `@paginated` reads and the request dimensions a cursor is
//! scoped by: the whole-catalog search, a keyword search re-sorted by another key and
//! direction, a search narrowed by a filter in another locale, a product's fulfillment
//! options and an offer's terms. The recorded fixture's query is one such row.
//!
//! Every request goes through the `MultiServiceRouter`, so the protocol, the routing table,
//! input validation and the handlers are exercised end to end.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::catalog_drilldown::{array, call, ok, router, text};

/// `NextToken` is `@length(0..8192)`
/// (`smithy_query(query="shape",shape="NextToken")`).
const MAX_TOKEN_LENGTH: usize = 8192;

/// One paginated read that truncates, with everything a transparent cursor would have put
/// on the wire for it.
struct Paged {
    /// What the row is, for an assertion message.
    case: &'static str,
    /// The operation's wire name, as the route spells it.
    operation: &'static str,
    /// The request, without `maxResults` (which the helpers set).
    input: Value,
    /// The output member carrying the page's items.
    items: &'static str,
    /// The cursor a caller would WRITE if the token were a readable rendering of the
    /// request: the operation, the resolved locale, and whatever else scopes the query.
    hand_written_identity: String,
    /// Request values whose appearance anywhere in the token's bytes would disclose the
    /// request. Only values of four bytes or more, so a chance byte run cannot be mistaken
    /// for a disclosure.
    disclosures: Vec<String>,
}

/// The rows: one per paginated read, over the request dimensions a cursor is scoped by.
async fn paged_rows(router: &MultiServiceRouter) -> Vec<Paged> {
    let product_id = multi_option_product(router).await;
    let offer_id = multi_term_offer(router).await;
    vec![
        Paged {
            case: "searchListings over the whole catalog",
            operation: "searchListings",
            input: json!({}),
            items: "listingSummaries",
            hand_written_identity: "SearchListings/en-US///RELEVANCE/DESCENDING".to_string(),
            disclosures: vec!["SearchListings".into(), "RELEVANCE".into(), "en-US".into()],
        },
        Paged {
            case: "searchListings re-sorted by rating, ascending",
            operation: "searchListings",
            input: json!({
                "sortBy": "AVERAGE_CUSTOMER_RATING",
                "sortOrder": "ASCENDING",
            }),
            items: "listingSummaries",
            hand_written_identity:
                "SearchListings/en-US///AVERAGE_CUSTOMER_RATING/ASCENDING".to_string(),
            disclosures: vec![
                "AVERAGE_CUSTOMER_RATING".into(),
                "ASCENDING".into(),
                "en-US".into(),
            ],
        },
        Paged {
            case: "searchListings narrowed by a rating filter, in another locale",
            operation: "searchListings",
            input: json!({
                "locale": "fr",
                "filters": [{
                    "filterType": "MIN_AVERAGE_CUSTOMER_RATING",
                    "filterValues": ["0.0"],
                }],
            }),
            items: "listingSummaries",
            hand_written_identity:
                "SearchListings/fr//MIN_AVERAGE_CUSTOMER_RATING=0.0/RELEVANCE/DESCENDING"
                    .to_string(),
            disclosures: vec!["MIN_AVERAGE_CUSTOMER_RATING".into(), "SearchListings".into()],
        },
        Paged {
            case: "listFulfillmentOptions on a multi-option product",
            operation: "listFulfillmentOptions",
            input: json!({ "productId": product_id.clone() }),
            items: "fulfillmentOptions",
            hand_written_identity: format!("ListFulfillmentOptions/en-US/{product_id}"),
            disclosures: vec![product_id, "ListFulfillmentOptions".into()],
        },
        Paged {
            case: "listPurchaseOptions over the whole catalog",
            operation: "listPurchaseOptions",
            input: json!({}),
            items: "purchaseOptions",
            hand_written_identity: "ListPurchaseOptions/en-US/".to_string(),
            disclosures: vec!["ListPurchaseOptions".into(), "en-US".into()],
        },
        Paged {
            case: "getOfferTerms on an offer with several terms",
            operation: "getOfferTerms",
            input: json!({ "offerId": offer_id.clone() }),
            items: "offerTerms",
            hand_written_identity: format!("GetOfferTerms/en-US/{offer_id}"),
            disclosures: vec![offer_id, "GetOfferTerms".into()],
        },
    ]
}

/// A product the catalog offers more than one fulfillment option for, so a one-item page
/// of its options truncates and the service mints a token at all.
async fn multi_option_product(router: &MultiServiceRouter) -> String {
    let catalog = ok(router, "searchListings", &json!({ "maxResults": 100 })).await;
    for listing in array(&catalog, "listingSummaries") {
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            if product_id.is_empty() {
                continue;
            }
            let options = ok(
                router,
                "listFulfillmentOptions",
                &json!({ "productId": product_id.clone(), "maxResults": 50 }),
            )
            .await;
            if array(&options, "fulfillmentOptions").len() > 1 {
                return product_id;
            }
        }
    }
    panic!("case setup: the catalog must hold a product with several fulfillment options");
}

/// An offer the catalog stores more than one term against.
async fn multi_term_offer(router: &MultiServiceRouter) -> String {
    let options = ok(router, "listPurchaseOptions", &json!({ "maxResults": 100 })).await;
    for option in array(&options, "purchaseOptions") {
        for entity in array(&option, "associatedEntities") {
            let Some(offer) = entity.get("offer") else {
                continue;
            };
            let offer_id = text(offer, "offerId");
            if offer_id.is_empty() {
                continue;
            }
            let terms = ok(
                router,
                "getOfferTerms",
                // `GetOfferTermsInput$maxResults` is `@range(1..25)`
                // (`smithy_query(query="operation",operation="GetOfferTerms")`), so this is
                // the whole set of terms for an offer the catalog stores fewer than that for.
                &json!({ "offerId": offer_id.clone(), "maxResults": 25 }),
            )
            .await;
            if array(&terms, "offerTerms").len() > 1 {
                return offer_id;
            }
        }
    }
    panic!("case setup: the catalog must hold an offer with several terms");
}

/// One page of a row at `max_results`, resuming at `next_token`: the items it carries and
/// the token it handed out.
async fn page(
    router: &MultiServiceRouter,
    row: &Paged,
    max_results: u64,
    next_token: Option<&str>,
) -> (Vec<Value>, Option<String>) {
    let mut input = row.input.clone();
    input["maxResults"] = json!(max_results);
    if let Some(token) = next_token {
        input["nextToken"] = json!(token);
    }
    let body = ok(router, row.operation, &input).await;
    let token = body
        .get("nextToken")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    (array(&body, row.items), token)
}

/// The token a row's FIRST one-item page hands out, which every row has because each row's
/// result set holds more than one item.
async fn minted_token(router: &MultiServiceRouter, row: &Paged) -> String {
    let (items, token) = page(router, row, 1, None).await;
    assert_eq!(
        items.len(),
        1,
        "case {}: maxResults=1 must truncate the page, got {} items",
        row.case,
        items.len()
    );
    token.unwrap_or_else(|| {
        panic!(
            "case {}: more items remain past a one-item page, so a nextToken must be minted",
            row.case
        )
    })
}

/// The bytes a token carries, which is what anything holding the token can read.
fn token_bytes(case: &str, token: &str) -> Vec<u8> {
    ::aws_smithy_types::base64::decode(token)
        .unwrap_or_else(|error| panic!("case {case}: a token must be base64, got {token:?}: {error}"))
}

/// Whether `haystack` carries `needle` anywhere in it.
fn carries(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|window| window == needle)
}

#[tokio::test]
async fn a_minted_token_discloses_neither_the_request_nor_the_position() {
    let router = router();
    let rows = paged_rows(&router).await;

    let mut widths: Vec<(&str, usize)> = Vec::new();
    for row in &rows {
        let token = minted_token(&router, row).await;
        assert!(
            !token.is_empty()
                && token.len() <= MAX_TOKEN_LENGTH
                && token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"+/=".contains(&byte)),
            "case {}: nextToken must be within @length(0..8192) and match ^[a-zA-Z0-9+/=]+$, \
             got {token:?}",
            row.case
        );

        let bytes = token_bytes(row.case, &token);
        for disclosed in &row.disclosures {
            assert!(
                disclosed.len() >= 4,
                "case {}: a disclosure row must be long enough to be unmistakable, got \
                 {disclosed:?}",
                row.case
            );
            assert!(
                !carries(&bytes, disclosed.as_bytes())
                    && !carries(&bytes, disclosed.to_lowercase().as_bytes()),
                "case {}: the token must carry no rendering of {disclosed:?}, got {bytes:?}",
                row.case
            );
        }
        assert!(
            !carries(&bytes, row.hand_written_identity.as_bytes()),
            "case {}: the token must carry no rendering of the query it belongs to, got \
             {bytes:?}",
            row.case
        );
        // Not text at all: a readable cursor is printable throughout, a sealed one is not.
        assert!(
            bytes
                .iter()
                .any(|byte| !byte.is_ascii_graphic() && !byte.is_ascii_whitespace()),
            "case {}: the token must not be a readable rendering, got {bytes:?}",
            row.case
        );

        // Nor is the token a function of the request and the position: paging the same
        // request again hands back DIFFERENT bytes for the same page boundary, which a
        // cursor derived from the request could not do, and both are honoured.
        let again = minted_token(&router, row).await;
        assert_ne!(
            again, token,
            "case {}: two mints of the same page boundary must not repeat the same bytes",
            row.case
        );
        let (first_page, _) = page(&router, row, 1, Some(&token)).await;
        let (second_page, _) = page(&router, row, 1, Some(&again)).await;
        assert_eq!(
            first_page, second_page,
            "case {}: two tokens for the same boundary must resume at the same item",
            row.case
        );

        widths.push((row.case, token.len()));
    }

    // Every token is the same width, whatever the request behind it: its size discloses
    // nothing either.
    let (first_case, width) = widths[0];
    for (case, found) in &widths {
        assert_eq!(
            *found, width,
            "case {case}: every token must be as wide as {first_case}'s ({width}), got {found}"
        );
    }
}

#[tokio::test]
async fn only_a_token_this_service_minted_for_this_query_is_honoured() {
    let router = router();
    let rows = paged_rows(&router).await;
    // A second emulator, serving the same catalog: a token it minted for the IDENTICAL
    // request is still a token THIS service never issued.
    let another_emulator = crate::catalog_drilldown::router();

    let mut tokens: Vec<String> = Vec::new();
    for row in &rows {
        tokens.push(minted_token(&router, row).await);
    }

    for (index, row) in rows.iter().enumerate() {
        let token = tokens[index].clone();
        let width = token_bytes(row.case, &token).len();

        // The token this query minted resumes it: the next page carries the next item.
        let (first_page, _) = page(&router, row, 1, None).await;
        let (resumed, _) = page(&router, row, 1, Some(&token)).await;
        assert_eq!(
            resumed.len(),
            1,
            "case {}: the minted token must resume the query, got {} items",
            row.case,
            resumed.len()
        );
        assert_ne!(
            resumed, first_page,
            "case {}: the resumed page must not repeat the first one",
            row.case
        );

        // Rows of values the service did not mint for this query: the hand-written cursor
        // of the readable form (at offsets a caller picks, including one it was never
        // offered), the query identity alone, a blob of a minted token's own width, base64
        // of text, a token another emulator minted for the same request, and every other
        // row's token — one minted for another query or another operation.
        let mut forged: Vec<(String, String)> = vec![
            (
                "hand-written cursor at offset 0".to_string(),
                ::aws_smithy_types::base64::encode(format!("0:{}", row.hand_written_identity)),
            ),
            (
                "hand-written cursor at offset 1".to_string(),
                ::aws_smithy_types::base64::encode(format!("1:{}", row.hand_written_identity)),
            ),
            (
                "hand-written cursor at an offset never offered".to_string(),
                ::aws_smithy_types::base64::encode(format!("97:{}", row.hand_written_identity)),
            ),
            (
                "the query identity alone".to_string(),
                ::aws_smithy_types::base64::encode(row.hand_written_identity.clone()),
            ),
            (
                "zero bytes of a minted token's width".to_string(),
                ::aws_smithy_types::base64::encode(vec![0_u8; width]),
            ),
            (
                "base64 of text".to_string(),
                ::aws_smithy_types::base64::encode("not-a-token"),
            ),
            (
                "a token another emulator minted for the same request".to_string(),
                minted_token(&another_emulator, row).await,
            ),
        ];
        for (other, other_token) in rows.iter().zip(&tokens) {
            if other.case != row.case {
                forged.push((
                    format!("a token minted for {}", other.case),
                    other_token.clone(),
                ));
            }
        }

        for (case, bad_token) in forged {
            let mut input = row.input.clone();
            input["maxResults"] = json!(1);
            input["nextToken"] = json!(bad_token);
            let (status, body) = call(&router, row.operation, &input).await;
            assert_eq!(
                status, 400,
                "case {} / {case}: a token this service did not mint for this query must be \
                 refused, got {status} with {body}",
                row.case
            );
        }
    }
}
