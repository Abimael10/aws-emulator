// SPDX-License-Identifier: CC0-1.0

//! The TEXT `ListHostedZonesByVPC` refuses an unusable `NextToken` with.
//!
//! The rule: the sentence a refusal carries belongs to the OPERATION that
//! answers it rather than to the error shape the model reuses across
//! operations, and where that sentence is fixed text it is a constant — no part
//! of the request, and in particular not the token that was refused, may enter
//! it. For this listing the whole message is the one sentence asserted below,
//! so every refused token earns byte-for-byte the same body.
//!
//! Asserted over a TABLE of tokens spanning what the rule generalises over —
//! length (up to the published maximum of `PaginationToken`, `@length 0..1024`
//! per smithy_query(query="shape",shape="PaginationToken")), character class,
//! and how closely a token resembles one this listing could have minted — plus
//! the served controls that bound the rule: a listing with no token at all, one
//! with the empty token (the published MINIMUM length, which names no page and
//! so is not a refusal either) and one resuming on a token the listing really
//! minted.
//!
//! The sibling listing of a zone's association authorizations words the SAME
//! modelled error from its own request instead, which is what makes the rule
//! per-operation; the last test here pins that the two wordings stay distinct.
//! The harness (`router_holding_vpcs`, `call`, `Reply`) and the request builders
//! come from the suites that own those operations.

use smithy_server_runtime::MultiServiceRouter;

use crate::hosted_zone_records::{router_holding_vpcs, Reply};
use crate::hosted_zones_by_vpc::list_by_vpc;
use crate::refusal_message_rules::message_of;
use crate::vpc_association_authorizations::{authorize_ok, list_authorizations};
use crate::vpc_associations::{create_private_zone, CREATION_VPC, HOME_REGION};

/// The whole message this listing answers `InvalidPaginationToken` with: a
/// constant, carrying no value from the request.
const INVALID_NEXT_TOKEN: &str = "The nextToken provided is invalid.";

/// One token the listing never minted: the spelling the query string carries and
/// the value the service receives after URI decoding.
struct TokenRow {
    case: &'static str,
    sent: String,
    received: String,
}

impl TokenRow {
    /// A row whose token needs no URI encoding.
    fn plain(case: &'static str, token: &str) -> Self {
        Self {
            case,
            sent: token.to_string(),
            received: token.to_string(),
        }
    }
}

/// A router whose account holds the VPC the fixture zone is associated with, and
/// the two zones associated with it (so the listing has pages to serve and the
/// token is the only thing wrong with a refused request).
async fn seeded() -> (MultiServiceRouter, Vec<String>) {
    let router = router_holding_vpcs(&[(CREATION_VPC, HOME_REGION)]);
    let mut zones = vec![
        create_private_zone(&router, "token-text-one.example.com", "next-token-1", CREATION_VPC)
            .await,
        create_private_zone(&router, "token-text-two.example.com", "next-token-2", CREATION_VPC)
            .await,
    ];
    // The listing walks zones in hosted-zone-id order, so the token a page hands
    // out is the id of the zone the page left out.
    zones.sort();
    (router, zones)
}

/// Every token the table drives, built once so the long rows are allocated once.
fn unminted_tokens() -> Vec<TokenRow> {
    vec![
        TokenRow::plain("arbitrary text with hyphens, the shape a garbled token has", "garbled-token-xyz9"),
        TokenRow::plain("a bare number", "4711"),
        TokenRow::plain("a single character", "q"),
        TokenRow::plain("the hosted-zone id prefix with no id body", "Z"),
        TokenRow::plain("a hosted-zone id spelled in lower case", "z1d633pjn98ft9"),
        TokenRow::plain(
            // `ResourceId`, which a minted token is one of, is `@length 0..32`
            // (smithy_query(query="shape",shape="ResourceId")), so this is
            // longer than any id this listing could have handed out.
            "a token longer than any hosted-zone id",
            "Z1D633PJN98FT9EXTRAEXTRAEXTRAEXTRA",
        ),
        TokenRow::plain("the id of a VPC rather than of a zone", "vpc-1a2b3c4d"),
        TokenRow {
            case: "a token carrying XML metacharacters",
            sent: "%3Ctoken%3E".to_string(),
            received: "<token>".to_string(),
        },
        TokenRow {
            // The published MAXIMUM length of `PaginationToken`: modelled-valid,
            // so the handler is what refuses it.
            case: "a token of the maximum modelled length",
            sent: "Z".repeat(1024),
            received: "Z".repeat(1024),
        },
    ]
}

#[tokio::test]
async fn an_unminted_next_token_is_refused_with_this_listings_fixed_sentence() {
    let (router, zones) = seeded().await;

    for row in unminted_tokens() {
        let case = row.case;
        let reply = list_by_vpc(&router, CREATION_VPC, HOME_REGION, None, Some(&row.sent)).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_code(),
            "InvalidPaginationToken",
            "case {case:?}: expected the code \"InvalidPaginationToken\", got {:?}; body {}",
            reply.error_code(),
            reply.body
        );

        let got = message_of(&reply, case);
        assert_eq!(
            got, INVALID_NEXT_TOKEN,
            "case {case:?}: expected message {INVALID_NEXT_TOKEN:?}, got {got:?}"
        );
        // The message is a constant, so a token that is not already part of that
        // constant cannot appear anywhere in it.
        if !INVALID_NEXT_TOKEN.contains(row.received.as_str()) {
            assert!(
                !got.contains(row.received.as_str()),
                "case {case:?}: the message must not report the token {:?}, got {got:?}",
                row.received
            );
        }
    }

    // A refused token leaves the listing readable, and still reporting every
    // zone associated with the VPC.
    let served = list_by_vpc(&router, CREATION_VPC, HOME_REGION, None, None).await;
    let reported = reported_zones(&served);
    assert_eq!(
        reported, zones,
        "a refused token must leave the listing reporting every associated zone, got \
         {reported:?}; body {}",
        served.body
    );
}

/// The hosted-zone ids a reply reports, in document order.
fn reported_zones(reply: &Reply) -> Vec<String> {
    reply
        .all("HostedZoneSummary/HostedZoneId")
        .into_iter()
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn the_tokens_the_listing_mints_are_still_served() {
    let (router, zones) = seeded().await;

    // The token a one-zone page hands out names the zone that page left out, and
    // resuming on it serves the rest of the listing rather than the refusal
    // above.
    let first = list_by_vpc(&router, CREATION_VPC, HOME_REGION, Some(1), None).await;
    assert_eq!(
        (first.status, reported_zones(&first)),
        (200, vec![zones[0].clone()]),
        "the first page must carry the first zone; body {}",
        first.body
    );
    let token = first
        .first("NextToken")
        .unwrap_or_else(|| panic!("a page with a zone left over must carry a NextToken; body {}", first.body))
        .to_string();

    let resumed = list_by_vpc(&router, CREATION_VPC, HOME_REGION, Some(1), Some(&token)).await;
    assert_eq!(
        (resumed.status, reported_zones(&resumed)),
        (200, vec![zones[1].clone()]),
        "the token {token:?} must resume the listing at the zone the first page left out; body {}",
        resumed.body
    );

    // The published MINIMUM length of `PaginationToken` is zero, and a token of
    // no characters names no page to resume at: it is the whole listing, not a
    // refusal.
    for (case, sent) in [("the empty token", ""), ("a token of blanks", "%20%20")] {
        let reply = list_by_vpc(&router, CREATION_VPC, HOME_REGION, None, Some(sent)).await;
        assert_eq!(
            (reply.status, reported_zones(&reply)),
            (200, zones.clone()),
            "case {case:?}: a token naming no page must serve the whole listing; body {}",
            reply.body
        );
    }
}

#[tokio::test]
async fn the_authorization_listing_words_the_same_error_from_its_own_request() {
    let (router, zones) = seeded().await;
    let zone = &zones[0];
    authorize_ok(&router, zone, "vpc-11112222333344445", "us-west-2").await;

    // ONE token, refused by both listings: the error class is the same, the
    // sentence is not, because each listing words its own.
    let token = "garbled-token-xyz9";
    let by_vpc = list_by_vpc(&router, CREATION_VPC, HOME_REGION, None, Some(token)).await;
    let ledger = list_authorizations(&router, zone, &format!("?nexttoken={token}")).await;
    for (operation, reply) in [("ListHostedZonesByVPC", &by_vpc), ("ListVPCAssociationAuthorizations", &ledger)] {
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "InvalidPaginationToken"),
            "case {operation:?}: expected 400 InvalidPaginationToken, got body {}",
            reply.body
        );
    }

    let from_by_vpc = message_of(&by_vpc, "ListHostedZonesByVPC");
    let from_ledger = message_of(&ledger, "ListVPCAssociationAuthorizations");
    assert_eq!(
        from_by_vpc, INVALID_NEXT_TOKEN,
        "case \"ListHostedZonesByVPC\": expected message {INVALID_NEXT_TOKEN:?}, got \
         {from_by_vpc:?}"
    );
    assert_eq!(
        from_ledger,
        format!("Invalid pagination token: {token}"),
        "case \"ListVPCAssociationAuthorizations\": expected the listing's own request-derived \
         sentence, got {from_ledger:?}"
    );
}
