// SPDX-License-Identifier: CC0-1.0

//! The ONE step every `@paginated` operation of this service takes from the `nextToken` a
//! request carries to the position its scan resumes at, driven end to end over awsJson1_0
//! through all five of those operations.
//!
//! RULE under test: resolving a continuation token is one step of this service rather than
//! one step per listing — a token the addressed listing itself issued, still inside the
//! documented day, resumes that listing at exactly the position it names; a request naming
//! no token starts at the first item; and every other token names no position at all and is
//! refused IDENTICALLY, whichever of the ways it failed ("Make the call again using the
//! returned token with all other arguments unchanged to retrieve the next page" and "Each
//! pagination token expires after 24 hours. Using an expired pagination token will return
//! an HTTP 400 InvalidToken error" — the `nextToken` doc every `@paginated` operation of
//! this service carries,
//! `smithy_query(query="operation", operation="ListEnvironmentConnectors")`; this service's
//! model declares no `InvalidToken` shape, so the refusal is carried by its only
//! client-input error, at the documented status).
//!
//! `evs::state::page_cursor` is where that step lives, and this suite drives it through all
//! five paginated operations at once: the refusals are required to be ONE answer, equal
//! member for member across the operations and across the ways a token fails, so a listing
//! that began to tell an expired token from one this service never issued, or stopped
//! refusing, fails here even though each answer on its own reads plausibly.
//!
//! The rows span what the step generalises over:
//!
//! * the OPERATION — all five, over the environments, hosts, connectors, VLANs and
//!   entitlements of a fixture that gives every listing at least two items;
//! * the WAY a token names no position — text that is no token at all, a position of this
//!   listing minted under another store's key, another listing's own authentic token, this
//!   listing's own token issued too long ago to resume, and an authentic cursor that is not
//!   one of the listing's keys;
//! * the CURSOR's own type — the item key each listing is ordered by: text for four of them
//!   and an integer for the VLAN listing, whose cursor the step has to read as the number a
//!   VLAN is keyed by.
//!
//! The WORDING of that one refusal is `tests/all/refusal_wording.rs`'s rule and the LISTING
//! a token belongs to is `tests/all/page_token_binding.rs`'s; this suite asserts the
//! service's own constant, so none of the three can drift from the others.
//!
//! The harness (`emulator`, `call_in_region`, `create_settled`) is the one
//! `environment_lifecycle` owns and the connector helper the one `entitlement_connector`
//! owns, so every listing is driven the way a client drives it.
//!
//! Why this file carries it: this crate's integration tests are one target whose suites are
//! hand-declared in `tests/all.rs`, which this change may not add a declaration to, and
//! this is the one declared file that holds no test (it was superseded by
//! `tests/all/connectors.rs` and `tests/all/hosts.rs`, whose coverage is untouched).

use serde_json::{json, Value};
use service_com_amazonaws_evs_amazonelasticvmwareservice as evs;

use crate::entitlement_connector::connector_in;
use crate::environment_lifecycle::{
    call_in_region, create_settled, emulator, error_type, Emulator, REGION,
};

/// Two of the `ConnectorType` values the model declares
/// (`smithy_query(query="shape", shape="ConnectorType")`), so the connector listing holds
/// more than one item. Only one LIVE connector of a type can exist in an environment, and
/// an entitlement is tracked through the vCenter Server appliance, so `VCENTER` is one.
const SDDC_MANAGER: &str = "SDDC_MANAGER";
/// See [`SDDC_MANAGER`].
const VCENTER: &str = "VCENTER";

/// The only `EntitlementType` the model declares
/// (`smithy_query(query="shape", shape="EntitlementType")`).
const WINDOWS_SERVER: &str = "WINDOWS_SERVER";

/// One listing: the operation, the request that addresses the collection it pages over,
/// and the scope a token of it belongs to.
struct Listing {
    /// How a case names this listing in an assertion message.
    case: &'static str,
    /// The operation the request targets.
    operation: &'static str,
    /// The output member the page arrives in.
    member: &'static str,
    /// The member of a reported item that identifies it, so two reports of one item
    /// compare even while the clock moves its state on.
    identity: &'static str,
    /// Everything the request carries apart from `maxResults` and `nextToken`.
    request: Value,
    /// The request members that SELECT what this listing pages over, in the order the
    /// handler adds them to the scope a token of it is authenticated under.
    selectors: Vec<String>,
    /// A cursor this listing's items are NOT keyed by: text for a listing ordered by a
    /// number, and nothing for a listing ordered by text, where any text is a key.
    cursor_outside_the_keys: Option<&'static str>,
}

impl Listing {
    /// The scope a token of this listing is authenticated under — the listing's identity
    /// as the service states it.
    fn scope(&self) -> evs::state::PageScope {
        self.selectors.iter().fold(
            evs::state::PageScope::new(self.operation, REGION),
            |scope, selector| scope.selecting(selector),
        )
    }
}

/// Call one listing, with the page bound and the token the case names.
async fn page(
    router: &Emulator,
    listing: &Listing,
    max_results: Option<i64>,
    token: Option<&str>,
) -> (u16, Value) {
    let mut request = listing.request.clone();
    if let Some(bound) = max_results {
        request["maxResults"] = json!(bound);
    }
    if let Some(token) = token {
        request["nextToken"] = json!(token);
    }
    call_in_region(router, REGION, listing.operation, &request).await
}

/// Every item of a listing's collection, by its identity member, in the order the listing
/// reports them — read through a request that names NO token, which is the step's own
/// answer for a scan that starts at the first item.
async fn identities(router: &Emulator, listing: &Listing) -> Vec<Value> {
    let (status, body) = page(router, listing, None, None).await;
    assert_eq!(
        status, 200,
        "case {}: a listing naming no token expected HTTP 200, got {status} ({}) {body}",
        listing.case,
        error_type(&body)
    );
    body[listing.member]
        .as_array()
        .unwrap_or_else(|| panic!("case {}: no {} in {body}", listing.case, listing.member))
        .iter()
        .map(|item| item[listing.identity].clone())
        .collect()
}

/// The `nextToken` a listing hands out for a page of one item.
async fn one_item_page_token(router: &Emulator, listing: &Listing) -> String {
    let (status, body) = page(router, listing, Some(1), None).await;
    assert_eq!(
        status, 200,
        "case {}: a first page of one expected HTTP 200, got {status} ({}) {body}",
        listing.case,
        error_type(&body)
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {}: the fixture holds a second page, so this listing hands out a token, \
                 got {body}",
                listing.case
            )
        })
        .to_string()
}

/// The secret the emulator's Amazon EVS store authenticates its own page tokens under:
/// what this suite mints a token with, through the very code a handler uses.
fn page_token_key(router: &Emulator) -> evs::state::PageTokenKey {
    evs::state::service_state(&*router.engine)
        .expect("the in-memory engine carries this service's store")
        .lock()
        .page_token_key
}

/// The position a listing's own token names, read back through the very decoder the shared
/// step uses: what an aged or re-minted form of that token carries.
fn cursor_of(listing: &Listing, key: &evs::state::PageTokenKey, token: &str) -> String {
    evs::state::decode_page_token(key, &listing.scope(), token).unwrap_or_else(|error| {
        panic!(
            "case {}: this listing's own token must name a position, got {error:?}",
            listing.case
        )
    })
}

/// The five listings this suite drives the shared step through, each holding at least two
/// items so each really does hand out a token of its own.
async fn listings(router: &Emulator) -> Vec<Listing> {
    // Settled, because a list addressed to an environment that is still initializing is
    // refused (`tests/all/list_while_initializing.rs` owns that rule), and because a host
    // can only join a deployed environment.
    let environment = create_settled(router, Some("page-cursor-first")).await;
    // A second environment, so `ListEnvironments` pages over more than one item too.
    create_settled(router, Some("page-cursor-second")).await;

    for index in 1..=2 {
        // "DNS hostnames for hosts must be unique across Amazon EVS environments"
        // (`Host$hostName` doc).
        let host_name = format!("esx-page-cursor-{index}");
        let (status, body) = call_in_region(
            router,
            REGION,
            "CreateEnvironmentHost",
            &json!({
                "environmentId": environment,
                "host": {
                    "hostName": host_name,
                    "keyName": "evs-key",
                    "instanceType": "i4i.metal",
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "the fixture host {host_name}: expected HTTP 200, got {status} ({}) {body}",
            error_type(&body)
        );
    }

    let mut vcenter = String::new();
    for connector_type in [SDDC_MANAGER, VCENTER] {
        let connector =
            connector_in(router, "the page cursor fixture", &environment, connector_type).await;
        if connector_type == VCENTER {
            vcenter = connector;
        }
    }

    let (status, body) = call_in_region(
        router,
        REGION,
        "CreateEntitlement",
        &json!({
            "environmentId": environment,
            "connectorId": vcenter,
            "entitlementType": WINDOWS_SERVER,
            // The modelled `VmId` pattern is `^vm-[0-9]+$`; two of them, so the
            // entitlement listing holds more than one item.
            "vmIds": ["vm-101", "vm-102"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the fixture entitlements: expected HTTP 200, got {status} ({}) {body}",
        error_type(&body)
    );

    vec![
        Listing {
            case: "ListEnvironments",
            operation: "ListEnvironments",
            member: "environmentSummaries",
            identity: "environmentId",
            request: json!({}),
            selectors: Vec::new(),
            cursor_outside_the_keys: None,
        },
        Listing {
            case: "ListEnvironmentHosts",
            operation: "ListEnvironmentHosts",
            member: "environmentHosts",
            identity: "hostName",
            request: json!({ "environmentId": environment }),
            selectors: vec![environment.clone()],
            cursor_outside_the_keys: None,
        },
        Listing {
            case: "ListEnvironmentConnectors",
            operation: "ListEnvironmentConnectors",
            member: "connectors",
            identity: "connectorId",
            request: json!({ "environmentId": environment }),
            selectors: vec![environment.clone()],
            cursor_outside_the_keys: None,
        },
        Listing {
            case: "ListEnvironmentVlans",
            operation: "ListEnvironmentVlans",
            member: "environmentVlans",
            identity: "vlanId",
            request: json!({ "environmentId": environment }),
            selectors: vec![environment.clone()],
            // A VLAN is keyed by its `VlanId`, an integer, so text that is not one names
            // no position in this listing even when the token carrying it is authentic.
            cursor_outside_the_keys: Some("not-a-vlan-id"),
        },
        Listing {
            case: "ListVmEntitlements",
            operation: "ListVmEntitlements",
            member: "entitlements",
            identity: "vmId",
            request: json!({
                "environmentId": environment,
                "connectorId": vcenter,
                "entitlementType": WINDOWS_SERVER,
            }),
            selectors: vec![
                environment.clone(),
                vcenter.clone(),
                WINDOWS_SERVER.to_string(),
            ],
            cursor_outside_the_keys: None,
        },
    ]
}

#[tokio::test]
async fn every_paginated_listing_resumes_at_the_position_its_own_token_names() {
    let router = emulator();
    let listings = listings(&router).await;
    let key = page_token_key(&router);

    for listing in &listings {
        let items = identities(&router, listing).await;
        assert!(
            items.len() >= 2,
            "case {}: the fixture holds at least two items, got {items:?}",
            listing.case
        );

        // A page of one stops at the first item and hands out the position of the next —
        // a position the shared step reads back, under this listing's own scope, as the
        // cursor the listing minted it for.
        let token = one_item_page_token(&router, listing).await;
        assert!(
            !cursor_of(listing, &key, &token).is_empty(),
            "case {}: this listing's own token must carry the position it resumes at, got \
             {token:?}",
            listing.case
        );

        // Each row: how the scan is started, and the item the first reported item must be.
        let rows: [(&str, Option<&str>, &Value); 2] = [
            ("naming no token", None, &items[0]),
            ("naming its own token", Some(token.as_str()), &items[1]),
        ];
        for (how, carried, expected) in rows {
            let case = format!("{} {how}", listing.case);
            let (status, body) = page(&router, listing, Some(1), carried).await;
            assert_eq!(
                status, 200,
                "case {case}: expected HTTP 200, got {status} ({}) {body}",
                error_type(&body)
            );
            assert_eq!(
                &body[listing.member][0][listing.identity],
                expected,
                "case {case}: expected the page to start at {expected}, got {}",
                body[listing.member][0][listing.identity]
            );
        }
    }
}

#[tokio::test]
async fn every_paginated_listing_refuses_a_token_that_names_no_position_with_one_answer() {
    let router = emulator();
    let listings = listings(&router).await;
    let key = page_token_key(&router);
    // A key no store of this emulator holds: what another emulator process, or a restarted
    // one, would have minted its tokens under.
    let another_store = evs::state::PageTokenKey::new();
    let past_the_day = evs::state::PAGE_TOKEN_TTL.as_millis() as i64 + 1;

    // The token each listing hands out for a page of one, so a row can hand one listing's
    // own authentic token to another.
    let mut own_tokens: Vec<String> = Vec::new();
    for listing in &listings {
        own_tokens.push(one_item_page_token(&router, listing).await);
    }

    // The one answer every row is required to be, as the first refusal states it.
    let mut one_answer: Option<(String, Value)> = None;

    for (index, listing) in listings.iter().enumerate() {
        let own = own_tokens[index].as_str();
        let cursor = cursor_of(listing, &key, own);
        // Every way a token can fail to name a position in this listing, each form
        // carrying a position that WOULD report a further page if it were honoured
        // unchecked.
        let mut rows: Vec<(String, String)> = vec![
            ("text that is no token at all".to_string(), "not-a-token".to_string()),
            (
                "this listing's own position minted under another store's key".to_string(),
                evs::state::encode_page_token(&another_store, &listing.scope(), &cursor),
            ),
            (
                "this listing's own token issued past the documented day".to_string(),
                evs::state::encode_page_token_issued_at(
                    &key,
                    &listing.scope(),
                    &cursor,
                    evs::state::now_ms() - past_the_day,
                ),
            ),
        ];
        // Another listing's own authentic token: a position in a scan this operation does
        // not page over.
        for (other_index, other) in listings.iter().enumerate() {
            if other_index != index {
                rows.push((
                    format!("the {} token", other.case),
                    own_tokens[other_index].clone(),
                ));
            }
        }
        // And, where the listing's items are keyed by a number, an authentic token of this
        // very listing whose cursor is not one of those keys.
        if let Some(outside) = listing.cursor_outside_the_keys {
            rows.push((
                format!("an authentic token carrying the cursor {outside:?}"),
                evs::state::encode_page_token(&key, &listing.scope(), outside),
            ));
        }

        for (form, token) in rows {
            let case = format!("{form}, handed to {}", listing.case);
            let (status, body) = page(&router, listing, Some(1), Some(&token)).await;
            assert_eq!(
                status, 400,
                "case {case}: expected HTTP 400, got {status} ({}) {body}",
                error_type(&body)
            );
            assert!(
                error_type(&body).contains("ValidationException"),
                "case {case}: expected a ValidationException, got {} in {body}",
                error_type(&body)
            );
            assert_eq!(
                body["message"].as_str(),
                Some(evs::state::INVALID_NEXT_TOKEN_MESSAGE),
                "case {case}: expected the message {:?}, got {}",
                evs::state::INVALID_NEXT_TOKEN_MESSAGE,
                body["message"]
            );
            assert_eq!(
                body["fieldList"][0]["name"],
                json!("nextToken"),
                "case {case}: expected the refusal reported against nextToken, got {}",
                body["fieldList"]
            );
            assert_eq!(
                body[listing.member],
                Value::Null,
                "case {case}: a refused request reports no page, got {}",
                body[listing.member]
            );
            // The rule itself: ONE answer for every token that names no position, whatever
            // operation was called and however the token failed.
            match &one_answer {
                None => one_answer = Some((case, body)),
                Some((first_case, first)) => assert_eq!(
                    &body, first,
                    "case {case}: every listing must state this refusal the way {first_case} \
                     states it, expected {first}, got {body}"
                ),
            }
        }
    }

    // Every listing is still served at its first page, so the refusals above are the
    // token's answer and not the operations' answer to everything.
    for listing in &listings {
        let items = identities(&router, listing).await;
        assert!(
            items.len() >= 2,
            "case {}: the listing still reports its items, got {items:?}",
            listing.case
        );
    }
}
