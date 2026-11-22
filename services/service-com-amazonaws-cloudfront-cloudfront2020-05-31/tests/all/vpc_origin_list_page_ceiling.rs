// SPDX-License-Identifier: CC0-1.0

//! The page `ListVpcOrigins` serves is bounded by this SERVICE's own ceiling, end to end over
//! HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes.
//!
//! The rules under test, over a store holding one more VPC origin than the ceiling admits:
//!
//! 1. a page carries at most this service's own page size
//!    ([`service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MAX_ITEMS`]) of summaries,
//!    whatever larger `MaxItems` a request asks for and when it asks for none at all, with
//!    `Quantity` stating the length of `Items` and `MaxItems` echoing the value the request
//!    provided;
//! 2. a page cut at the ceiling is truncated and names the id it ended at, and the rest of the
//!    store is reachable from that marker — so walking the pages still enumerates every stored
//!    VPC origin exactly once even when one request cannot serve the store;
//! 3. the listing is a REPORT of the store: a request served at the ceiling consumes nothing,
//!    so the same request answers the same page again.
//!
//! The rows span what the rules generalise over: the page size asked for (none, one past the
//! ceiling, far past it and the largest value the parameter can carry).
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the create is
//! [`crate::vpc_origin_lifecycle`]'s and the listing route is [`crate::vpc_origin_list`]'s, so
//! every call here travels the same path the suites beside it exercise.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{is_vpc_origin_id, MAX_ITEMS};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{emulator, Node, API_VERSION};
use crate::key_value_store_lifecycle::escape;
use crate::vpc_origin_lifecycle::{create_vpc_origin, load_balancer_arn};
use crate::vpc_origin_list::list_vpc_origins;

/// The `CreateVpcOriginRequest` document for the `index`th seeded VPC origin: a name no other
/// seed holds (the one value this create demands be unique) over an endpoint a caller really
/// would front.
fn create_body(index: usize) -> String {
    let name = format!("ceiling-{index:04}");
    format!(
        "<CreateVpcOriginRequest xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <VpcOriginEndpointConfig>\
         <Name>{}</Name><Arn>{}</Arn>\
         <HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         </VpcOriginEndpointConfig></CreateVpcOriginRequest>",
        escape(&name),
        escape(&load_balancer_arn(&format!("ceiling{index:04}")))
    )
}

/// The ids of the VPC origin summaries one page carries, in the order the page lists them.
fn listed_ids(list: &Node) -> Vec<String> {
    list.child("Items")
        .map(|items| {
            items
                .children_named("VpcOriginSummary")
                .iter()
                .map(|summary| summary.text_at("Id").unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Seed `count` VPC origins and report their ids, sorted — the order the pages resume through.
async fn seeded(router: &MultiServiceRouter, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let created = create_vpc_origin(router, &create_body(index)).await;
        assert_eq!(
            created.status, 202,
            "case seeding ceiling-{index:04}: expected HTTP 202, got {} with {}",
            created.status, created.raw
        );
        let id = created
            .root()
            .find_text("Id")
            .unwrap_or_default()
            .to_string();
        assert!(
            is_vpc_origin_id(&id),
            "case seeding ceiling-{index:04}: expected a minted VPC origin id, got {id:?}"
        );
        ids.push(id);
    }
    ids.sort();
    ids
}

/// RULE 1, RULE 2 and RULE 3: whatever page size a request asks for, the page stops at this
/// service's own ceiling, the rest of the store is reachable from the marker that page ended at,
/// and asking again answers the same page.
#[tokio::test]
async fn a_page_of_vpc_origins_is_bounded_by_this_services_own_ceiling() {
    let router = emulator();
    let ceiling = usize::try_from(MAX_ITEMS).expect("the ceiling fits a usize");
    // One more VPC origin than the ceiling admits, so a request asking for far more than the
    // ceiling still has to truncate.
    let all = seeded(&router, ceiling + 1).await;

    // No page size at all, one past the ceiling, far past it, and the largest value the
    // parameter can carry.
    for asked in [None, Some(MAX_ITEMS + 1), Some(1_000), Some(i32::MAX)] {
        let case = match asked {
            Some(size) => format!("MaxItems={size}"),
            None => "no page size".to_string(),
        };
        let query = asked.map(|size| format!("MaxItems={size}")).unwrap_or_default();
        let page = list_vpc_origins(&router, &query).await;
        assert_eq!(
            page.status, 200,
            "case {case}: expected HTTP 200, got {} with {}",
            page.status, page.raw
        );
        let list = page.root();
        let raw = page.raw.as_str();
        let first = listed_ids(list);
        assert_eq!(
            first.len(),
            ceiling,
            "case {case}: expected a page bounded at {ceiling}, got {} summaries in {raw}",
            first.len()
        );
        assert_eq!(
            list.text_at("Quantity").unwrap_or_default(),
            first.len().to_string(),
            "case {case}: Quantity states the length of Items, got {raw}"
        );
        // "The maximum number of items included in the list": the value the request provided,
        // which is this service's page size for a request that provided none.
        assert_eq!(
            list.text_at("MaxItems").unwrap_or_default(),
            asked.unwrap_or(MAX_ITEMS).to_string(),
            "case {case}: expected MaxItems to echo the request, got {raw}"
        );
        assert_eq!(
            list.text_at("IsTruncated").unwrap_or_default(),
            "true",
            "case {case}: the store holds more than one page, so the page is truncated, got \
             {raw}"
        );
        let next_marker = list.text_at("NextMarker").unwrap_or_default().to_string();
        assert!(
            is_vpc_origin_id(&next_marker),
            "case {case}: expected a NextMarker naming a VPC origin, got {next_marker:?} in \
             {raw}"
        );
        assert_eq!(
            Some(&next_marker),
            first.last(),
            "case {case}: the next marker is the id the page ended at, got {raw}"
        );

        // And the rest of the store is reachable from that marker: the two pages together are
        // every stored VPC origin, each listed exactly once, in id order.
        let rest = list_vpc_origins(&router, &format!("Marker={next_marker}")).await;
        assert_eq!(
            rest.status, 200,
            "case {case} resumed: expected HTTP 200, got {} with {}",
            rest.status, rest.raw
        );
        let rest_raw = rest.raw.as_str();
        let remaining = listed_ids(rest.root());
        assert_eq!(
            remaining.len(),
            all.len() - ceiling,
            "case {case} resumed: expected the {} remaining VPC origins, got {} in {rest_raw}",
            all.len() - ceiling,
            remaining.len()
        );
        assert_eq!(
            rest.root().text_at("IsTruncated").unwrap_or_default(),
            "false",
            "case {case} resumed: nothing remains after the last page, got {rest_raw}"
        );
        let walked: Vec<String> = first.into_iter().chain(remaining).collect();
        assert_eq!(
            walked, all,
            "case {case}: expected every stored VPC origin exactly once, in id order"
        );

        // Asking twice answers the same page: serving a page at the ceiling consumes nothing.
        // The page a listing REPORTS is what is compared, member by member, rather than the
        // whole document — a summary's `Status` is the clock's to change, and whether it has
        // changed between two reads says nothing about the page this rule is about.
        let again = list_vpc_origins(&router, &query).await;
        let again_raw = again.raw.as_str();
        assert_eq!(
            listed_ids(again.root()),
            walked[..ceiling].to_vec(),
            "case {case} asked twice: expected the same page, got {again_raw}"
        );
        for stated in ["Quantity", "MaxItems", "IsTruncated", "NextMarker", "Marker"] {
            assert_eq!(
                again.root().text_at(stated),
                page.root().text_at(stated),
                "case {case} asked twice: expected the same {stated}, got {again_raw}"
            );
        }
    }
}
