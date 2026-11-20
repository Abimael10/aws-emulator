// SPDX-License-Identifier: CC0-1.0

//! RTB Fabric's three paginated listings take ONE page walk, through the assembled
//! router a client actually talks to.
//!
//! `ListRequesterGateways`, `ListResponderGateways` and `ListLinks` each enumerate a
//! different resource, and the rule each of them follows is the same one: the records
//! are walked in the total order their unique ids give, a page holds at most the
//! `maxResults` the caller stated (the service's own page where the request states
//! none), the cursor a `nextToken` carries resumes strictly after the page that issued
//! it, and a token is issued only while records remain — so a walk reports every
//! stored record exactly once and stops exactly when the enumeration is exhausted. A
//! token is scoped to the listing that issued it, so no listing resumes from another's.
//!
//! This suite drives that one walk through all three operations, the two gateway
//! listings included, so a rule fixed for one of them is checked for every one. The
//! table spans what the rule generalises over: how many records the store holds
//! relative to a page (none, fewer than one page, exactly a page, several pages with a
//! short last one, one past the page a request that states no `maxResults` gets) and
//! the page size the walk asks for, at the published bounds of the modelled
//! `maxResults` range (`range(min: 1, max: 10)`, default 10, from
//! `smithy_query(query="operation", operation="ListRequesterGateways")`).
//!
//! Each listing's own refusals, projections and store scoping are pinned by the
//! service crate's suites; what is read here is the walk the three share.

use std::collections::BTreeSet;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The region the requests address.
const REGION: &str = "us-west-2";

/// The page a listing of this service hands out when the request states no
/// `maxResults`: the modelled default of the member where the model declares one, and
/// the service's one page size where it does not.
const DEFAULT_PAGE: usize = 10;

/// RTB Fabric mounted on the shared engine, behind the same `MultiServiceRouter` the
/// production router assembles, so every call below takes the real deserialize ->
/// validate -> handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // This suite exercises the listings, not SigV4: the requests carry no signature.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Send one restJson1 request and answer `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = if response.body().is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(response.body()).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Create one requester gateway and answer the id the service minted.
    async fn create_requester_gateway(&self, case: &str, client_token: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/requester-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "clientToken": client_token,
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateRequesterGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// Create one responder gateway and answer the id the service minted.
    async fn create_responder_gateway(&self, case: &str, client_token: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                "/responder-gateway",
                Some(json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": 443,
                    "protocol": "HTTPS",
                    "listenerConfig": { "protocols": ["HTTPS"] },
                    "domainName": "responder.example.com",
                    "clientToken": client_token,
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {body:?}"
        );
        text(&body, "gatewayId")
    }

    /// Create one link between two stored gateways and answer the id the service
    /// minted. `logSettings` carries the one member
    /// `smithy_query(query="shape", shape="LinkLogSettings")` marks required.
    async fn create_link(&self, case: &str, gateway_id: &str, peer_gateway_id: &str) -> String {
        let (status, body) = self
            .send(
                "POST",
                &format!("/gateway/{gateway_id}/create-link"),
                Some(json!({
                    "peerGatewayId": peer_gateway_id,
                    "logSettings": {
                        "applicationLogs": { "sampling": { "errorLog": 100.0, "filterLog": 0.0 } }
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateLink to answer 200, got {status} {body:?}"
        );
        text(&body, "linkId")
    }
}

/// The three paginated listings of this service.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Listing {
    RequesterGateways,
    ResponderGateways,
    Links,
}

/// What one listing was seeded with: the records it must report, in the order their
/// ids give, and the gateway a listing addressed to one carries.
struct Seeded {
    scope: Option<String>,
    ids: Vec<String>,
}

impl Listing {
    /// Every listing, so each rule below is read for all three.
    const ALL: [Listing; 3] = [
        Listing::RequesterGateways,
        Listing::ResponderGateways,
        Listing::Links,
    ];

    /// The operation this listing is served by.
    fn operation(self) -> &'static str {
        match self {
            Listing::RequesterGateways => "ListRequesterGateways",
            Listing::ResponderGateways => "ListResponderGateways",
            Listing::Links => "ListLinks",
        }
    }

    /// Store `count` records this listing reports, and answer them in the order it
    /// must enumerate them in.
    async fn seed(self, fabric: &Fabric, case: &str, count: usize) -> Seeded {
        let mut ids = Vec::new();
        let mut scope = None;
        match self {
            Listing::RequesterGateways => {
                for n in 0..count {
                    ids.push(
                        fabric
                            .create_requester_gateway(case, &format!("requester-{n}"))
                            .await,
                    );
                }
            }
            Listing::ResponderGateways => {
                for n in 0..count {
                    ids.push(
                        fabric
                            .create_responder_gateway(case, &format!("responder-{n}"))
                            .await,
                    );
                }
            }
            Listing::Links => {
                // A link joins two stored gateways, so the walk of a gateway's links
                // needs one requester gateway and one responder gateway per link.
                let gateway_id = fabric.create_requester_gateway(case, "link-hub").await;
                for n in 0..count {
                    let peer = fabric
                        .create_responder_gateway(case, &format!("link-peer-{n}"))
                        .await;
                    ids.push(fabric.create_link(case, &gateway_id, &peer).await);
                }
                scope = Some(gateway_id);
            }
        }
        ids.sort();
        Seeded { scope, ids }
    }

    /// This listing's path for one call.
    fn path(self, seeded: &Seeded, max_results: Option<i64>, token: Option<&str>) -> String {
        let base = match self {
            Listing::RequesterGateways => "/requester-gateways".to_string(),
            Listing::ResponderGateways => "/responder-gateways".to_string(),
            Listing::Links => format!(
                "/gateway/{}/links/",
                seeded
                    .scope
                    .as_deref()
                    .expect("a links listing is addressed to a gateway")
            ),
        };
        let mut query: Vec<String> = Vec::new();
        if let Some(max_results) = max_results {
            query.push(format!("maxResults={max_results}"));
        }
        if let Some(token) = token {
            query.push(format!("nextToken={token}"));
        }
        if query.is_empty() {
            base
        } else {
            format!("{base}?{}", query.join("&"))
        }
    }

    /// The ids one page of this listing reports, read out of the member the model
    /// declares for it: the gateway listings report their ids directly, and the link
    /// listing reports a record per link carrying its `linkId`.
    fn reported(self, body: &Value, case: &str) -> Vec<String> {
        match self {
            Listing::RequesterGateways | Listing::ResponderGateways => {
                let listed = body["gatewayIds"].as_array().unwrap_or_else(|| {
                    panic!("case {case:?}: expected gatewayIds to be a list, got {body:?}")
                });
                listed
                    .iter()
                    .map(|id| {
                        id.as_str()
                            .unwrap_or_else(|| {
                                panic!("case {case:?}: expected a gatewayId string, got {id:?}")
                            })
                            .to_string()
                    })
                    .collect()
            }
            Listing::Links => {
                let listed = body["links"].as_array().unwrap_or_else(|| {
                    panic!("case {case:?}: expected links to be a list, got {body:?}")
                });
                listed.iter().map(|link| text(link, "linkId")).collect()
            }
        }
    }
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// One row of the table: how many records the store holds, and the page size the walk
/// asks for (`None` states no `maxResults` at all).
struct Row {
    name: &'static str,
    stored: usize,
    max_results: Option<i64>,
}

const ROWS: &[Row] = &[
    Row {
        name: "an empty store",
        stored: 0,
        max_results: None,
    },
    Row {
        name: "fewer records than the page a stated maxResults omits",
        stored: 3,
        max_results: None,
    },
    Row {
        name: "one record past that page, so a second page is due",
        stored: 11,
        max_results: None,
    },
    Row {
        name: "one record per page, the range's minimum",
        stored: 4,
        max_results: Some(1),
    },
    Row {
        name: "a page size that divides the store exactly",
        stored: 6,
        max_results: Some(3),
    },
    Row {
        name: "the range's maximum over several pages",
        stored: 11,
        max_results: Some(10),
    },
    Row {
        name: "a page larger than the store",
        stored: 2,
        max_results: Some(10),
    },
];

#[tokio::test]
async fn every_listing_walks_its_records_once_in_pages_of_the_size_asked_for() {
    for listing in Listing::ALL {
        for row in ROWS {
            let case = format!("{} / {}", listing.operation(), row.name);
            let fabric = Fabric::new();
            let seeded = listing.seed(&fabric, &case, row.stored).await;
            let page_size = usize::try_from(row.max_results.unwrap_or_default())
                .ok()
                .filter(|asked| *asked > 0)
                .unwrap_or(DEFAULT_PAGE);

            let mut pages: Vec<Vec<String>> = Vec::new();
            let mut token: Option<String> = None;
            loop {
                let path = listing.path(&seeded, row.max_results, token.as_deref());
                let (status, body) = fabric.send("GET", &path, None).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected {path:?} to answer 200, got {status} {body:?}"
                );
                pages.push(listing.reported(&body, &case));

                let seen: usize = pages.iter().map(Vec::len).sum();
                let remaining = seeded.ids.len().saturating_sub(seen);
                assert_eq!(
                    body["nextToken"].is_string(),
                    remaining > 0,
                    "case {case:?}: with {remaining} of {} record(s) unread at {page_size} per page, expected a nextToken present={}, got {:?}",
                    seeded.ids.len(),
                    remaining > 0,
                    body["nextToken"]
                );
                token = body["nextToken"].as_str().map(str::to_string);
                if token.is_none() {
                    break;
                }
                assert!(
                    pages.len() <= 64,
                    "case {case:?}: the walk did not terminate, pages so far {pages:?}"
                );
            }

            let expected_pages = seeded.ids.len().div_euclid(page_size)
                + usize::from(seeded.ids.len().rem_euclid(page_size) != 0 || seeded.ids.is_empty());
            assert_eq!(
                pages.len(),
                expected_pages,
                "case {case:?}: expected {expected_pages} page(s) for {} record(s) at {page_size} per page, got {}: {pages:?}",
                seeded.ids.len(),
                pages.len()
            );
            for (index, page) in pages.iter().enumerate() {
                assert!(
                    page.len() <= page_size,
                    "case {case:?}: page {index} holds {} record(s), more than the {page_size} asked for: {page:?}",
                    page.len()
                );
            }

            let walked: Vec<String> = pages.iter().flatten().cloned().collect();
            assert_eq!(
                walked, seeded.ids,
                "case {case:?}: expected the walk to report every stored record exactly once in id order, got {walked:?}"
            );
            let distinct: BTreeSet<&String> = walked.iter().collect();
            assert_eq!(
                distinct.len(),
                walked.len(),
                "case {case:?}: expected no record to be reported twice, got {walked:?}"
            );
        }
    }
}

#[tokio::test]
async fn no_listing_resumes_from_a_token_another_listing_issued() {
    let fabric = Fabric::new();
    // Every listing holds more records than a page of one, so each of them issues a
    // token the others are then asked to resume from.
    let mut seeded: Vec<(Listing, Seeded)> = Vec::new();
    for listing in Listing::ALL {
        let case = format!("{} seeded", listing.operation());
        seeded.push((listing, listing.seed(&fabric, &case, 2).await));
    }

    for (issuer, issuer_seeded) in &seeded {
        let path = issuer.path(issuer_seeded, Some(1), None);
        let (status, page) = fabric.send("GET", &path, None).await;
        assert_eq!(
            status, 200,
            "case {:?}: expected {path:?} to answer 200, got {status} {page:?}",
            issuer.operation()
        );
        let token = page["nextToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected a nextToken while records remain, got {page:?}",
                    issuer.operation()
                )
            })
            .to_string();

        for (other, other_seeded) in &seeded {
            if other == issuer {
                continue;
            }
            let case = format!(
                "{} resuming from a {} token",
                other.operation(),
                issuer.operation()
            );
            let path = other.path(other_seeded, None, Some(&token));
            let (status, body) = fabric.send("GET", &path, None).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the token to be refused with 400, got {status} {body:?}"
            );
            assert!(
                !text(&body, "message").is_empty(),
                "case {case:?}: expected the refusal to say why, got {body:?}"
            );
        }
    }
}
