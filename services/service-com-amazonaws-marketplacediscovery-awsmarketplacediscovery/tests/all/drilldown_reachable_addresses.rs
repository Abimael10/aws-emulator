// SPDX-License-Identifier: CC0-1.0

//! Whether the addresses the DRILL-DOWN detail reads publish lead back to the emulator that
//! answered them, for a client that reached it through a front door on another port.
//!
//! The rule under test: every member a response carries for a client to DEREFERENCE —
//! `GetListing`/`GetProduct`'s `logoThumbnailUrl`, `ReviewSourceSummary$sourceUrl` and a SaaS
//! fulfillment option's `fulfillmentUrl` and `launchUrl` — is published on the scheme and
//! authority the ANSWERING REQUEST ARRIVED ON, which is the one authority known to reach this
//! process: the client dialled it and got here, so a fetch it sends there arrives the same way,
//! through whatever stands in between. A published container port or a forwarding front end is
//! exactly that case: the authority the client dialled is NOT the port this process is served
//! on, and announcing the served port instead hands the client the one address it cannot reach.
//! Only a request carrying no authority that can lead back here — one of AWS's own endpoint
//! hosts, which is an in-process call of another service — is answered with the address this
//! process is served at.
//!
//! [`crate::announced_address_authority`] pins the rule over the authorities a client can
//! state; this suite pins what the drill-down flow of this issue's fixture needs: that the
//! three detail reads a client walks after a search (`GetListing` -> `GetProduct` ->
//! `ListFulfillmentOptions`) publish every such member on ONE authority, that it is the one the
//! request arrived on, and that each published address is really served at the authority it was
//! published on — fetched there the way a browser holding it does.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::announced_address_authority::{dialled_at, first_saas_listing};
use crate::catalog_drilldown::{array, text};
use crate::listing_media_addresses::{fetch, review_source, router_served_at};

/// The base this emulator is SERVED at in every row below: the socket it accepts requests on
/// directly, which the rows reach it through something else than.
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The largest page `SearchListingsInput$maxResults` admits (`@range 1..100`), so the search
/// that opens the walk reports as much of the catalog as one page can carry.
const MAX_PAGE: u64 = 100;

/// One row: the authority a client dialled, and the base the addresses of the response it gets
/// have to be published on.
struct Row {
    /// What the row covers, named in every assertion of it.
    case: &'static str,
    /// The authority the client resolved and reached this emulator at.
    dialled: &'static str,
    /// The base every published address of that response names.
    expected_base: &'static str,
}

/// The rows, differing along what the rule generalises over: the kind of authority the client
/// dialled — this process's own served socket, a loopback literal and a loopback NAME on a port
/// it is not served on (the front door a published container port or a forwarding proxy
/// presents), a bracketed IPv6 literal on such a port, an authority naming no port at all, and
/// a host AWS publishes its own service endpoints at.
const ROWS: [Row; 6] = [
    Row {
        case: "the socket this process is served on",
        dialled: "127.0.0.1:9297",
        expected_base: "https://127.0.0.1:9297",
    },
    Row {
        case: "a front door on a port this process is not served on",
        dialled: "127.0.0.1:38377",
        expected_base: "https://127.0.0.1:38377",
    },
    Row {
        case: "a loopback name on such a port",
        dialled: "localhost:41655",
        expected_base: "https://localhost:41655",
    },
    Row {
        case: "a bracketed IPv6 literal on such a port",
        dialled: "[::1]:9443",
        expected_base: "https://[::1]:9443",
    },
    Row {
        case: "an authority naming no port at all",
        dialled: "127.0.0.1",
        expected_base: SERVED_BASE,
    },
    Row {
        case: "one of AWS's own endpoint hosts",
        dialled: "discovery-marketplace.us-east-1.amazonaws.com",
        expected_base: SERVED_BASE,
    },
];

/// The authority of an absolute URL: what a client dials to fetch it.
fn authority_of(url: &str) -> &str {
    url.split_once("://")
        .map_or(url, |(_scheme, rest)| rest)
        .split('/')
        .next()
        .unwrap_or_default()
}

/// One member a client dereferences: where it came from, and the address it carries.
struct Published {
    /// The operation and member the address was read off, for the assertion messages.
    member: String,
    /// The address itself.
    url: String,
}

/// Every address `body` publishes for a client to dereference, under `operation`: the record's
/// logo, the reviews page of a review source that keeps one, and the two pages of every SaaS
/// fulfillment option that names them.
fn published_addresses(operation: &str, body: &Value) -> Vec<Published> {
    let mut found: Vec<Published> = Vec::new();
    if let Some(logo) = body.get("logoThumbnailUrl").and_then(Value::as_str) {
        found.push(Published {
            member: format!("{operation} logoThumbnailUrl"),
            url: logo.to_string(),
        });
    }
    if let Some(url) = review_source(body).and_then(|(_source, url)| url) {
        found.push(Published {
            member: format!("{operation} reviewSummary.reviewSourceSummaries[0].sourceUrl"),
            url,
        });
    }
    for option in array(body, "fulfillmentOptions") {
        let Some(saas) = option.get("saasFulfillmentOption") else {
            continue;
        };
        let option_id = text(saas, "fulfillmentOptionId");
        for member in ["fulfillmentUrl", "launchUrl"] {
            let Some(url) = saas.get(member).and_then(Value::as_str) else {
                continue;
            };
            found.push(Published {
                member: format!("{operation} {option_id} {member}"),
                url: url.to_string(),
            });
        }
    }
    found
}

/// RULE: every address the drill-down detail reads publish for a client to dereference is
/// published on the authority the answering request arrived on — the one authority known to
/// reach this process — and is served there; a request carrying no such authority is answered
/// with the address this process is served at.
///
/// Rows: [`ROWS`], each walked through `SearchListings` -> `GetListing` -> `GetProduct` ->
/// `ListFulfillmentOptions` over one listing fulfilled as SaaS, so every published member of
/// the flow is covered on every kind of authority.
#[tokio::test]
async fn the_drilldown_publishes_addresses_a_client_can_fetch_where_it_reached_this_emulator() {
    let router = router_served_at(SERVED_BASE);

    for Row {
        case,
        dialled,
        expected_base,
    } in ROWS
    {
        let page = dialled_at(
            &router,
            dialled,
            "searchListings",
            &json!({ "maxResults": MAX_PAGE }),
        )
        .await;
        let summary = first_saas_listing(&array(&page, "listingSummaries"));
        let listing_id = text(&summary, "listingId");

        let listing = dialled_at(
            &router,
            dialled,
            "getListing",
            &json!({ "listingId": listing_id.clone() }),
        )
        .await;
        let mut announced = published_addresses("GetListing", &listing);
        let mut products = 0_usize;
        for entity in array(&listing, "associatedEntities") {
            let Some(product) = entity.get("product") else {
                continue;
            };
            let product_id = text(product, "productId");
            let detail = dialled_at(
                &router,
                dialled,
                "getProduct",
                &json!({ "productId": product_id.clone() }),
            )
            .await;
            announced.extend(published_addresses("GetProduct", &detail));
            let fulfillment = dialled_at(
                &router,
                dialled,
                "listFulfillmentOptions",
                &json!({ "productId": product_id.clone(), "maxResults": 50 }),
            )
            .await;
            announced.extend(published_addresses("ListFulfillmentOptions", &fulfillment));
            products += 1;
        }
        assert!(
            products > 0,
            "case {case:?} ({listing_id}): a catalog listing must associate at least one product"
        );

        let mut authorities: BTreeSet<&str> = BTreeSet::new();
        let mut fetched = 0_usize;
        for Published { member, url } in &announced {
            // The address names the authority this request arrived on, and the `URL` shape's
            // own constraints hold whichever authority that is (`@length(1..2048)`,
            // `@pattern ^https?://.*$`).
            assert!(
                url.starts_with(&format!("{expected_base}/")),
                "case {case:?} {member}: expected the address published on {expected_base:?}, got \
                 {url:?}"
            );
            assert!(
                (url.starts_with("https://") || url.starts_with("http://"))
                    && (1..=2048).contains(&url.len()),
                "case {case:?} {member}: a published address must be a URL of 1..2048 characters \
                 matching ^https?://.*$, got {url:?} ({} characters)",
                url.len()
            );
            authorities.insert(authority_of(url));

            // And it is served at the authority it was published on: the fetch states exactly
            // the address the response handed out, the way a browser holding one does.
            let document = fetch(&router, "GET", url).await;
            assert_eq!(
                document.status, 200,
                "case {case:?} {member}: expected the address announced at {url:?} to be served \
                 on that authority, got {} ({:?})",
                document.status,
                document.as_text()
            );
            fetched += 1;
        }
        assert!(
            fetched > 2,
            "case {case:?}: the walk must cover the logo, the reviews page and the SaaS pages the \
             flow publishes, got {fetched} addresses"
        );
        assert_eq!(
            authorities,
            BTreeSet::from([authority_of(expected_base)]),
            "case {case:?}: every address of one walk is published on the single authority the \
             requests arrived on, got {authorities:?}"
        );
    }
}
