// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the responder-gateway surface: `ListResponderGateways`,
//! the one create refusal that leaves the store unchanged — a managed endpoint
//! paired with a domain name — and `UpdateResponderGateway`, which is what happens
//! next to a responder gateway a create has brought up.
//!
//! The rules under test are about state, not about one recorded request:
//!
//! * the listing reports the ids of exactly the responder gateways the store holds,
//!   in the stable total order their ids give, a page of at most `maxResults` at a
//!   time, handing out a `nextToken` only while ids remain and resuming exactly after
//!   the page that issued it — and a token another listing issued is not one this
//!   listing resumes from;
//! * a responder gateway reached through a managed endpoint serves a caller-named
//!   domain only while that domain is allowlisted for managed endpoints; this API
//!   exposes no operation or member that grants an allowlist entry, so a create
//!   naming both is refused and stores nothing, while either member alone creates a
//!   gateway as usual;
//! * an update replaces exactly the configuration members the call states
//!   (`description`, `listenerConfig`, `trustStoreConfiguration`,
//!   `managedEndpointConfiguration`, `clientRoutingPolicy`), keeps every member it
//!   leaves out, never changes the `domainName`, `port` and `protocol` the create
//!   fixed, and reports the gateway as `PENDING_UPDATE` with the clock owing it
//!   `ACTIVE` again — while a gateway in any other status has no configuration in
//!   service to change, an id that names no stored responder gateway resolves to
//!   nothing, and a replayed `clientToken` answers the record the first call
//!   produced rather than applying the change twice.
//!
//! The tables span what the rules generalise over: how many gateways the store holds
//! relative to a page (none, fewer, exactly one page, several pages with a short last
//! one), the page size at each published bound of the modelled `maxResults` range
//! (`range(min: 1, max: 10)`) and one past each, and which of `domainName` /
//! `managedEndpointConfiguration` a create names
//! (`smithy_query(query="shape", shape="CreateResponderGatewayRequest")`), which of
//! the update's own members a call states, every member of the modelled
//! `ClientRoutingPolicy` and `ResponderGatewayStatus` enums, and which parameter a
//! retry under an already-used `clientToken` restates differently.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::ops::types::{
    CLIENT_ROUTING_POLICY_VALUES, RESPONDER_GATEWAY_STATUS_VALUES,
};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    ComAmazonawsRtbfabricRtbfabricState, GATEWAY_STATUS_ACTIVE, GATEWAY_STATUS_PENDING_UPDATE,
    RESOURCE_NOT_FOUND_MESSAGE,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// How long a poll waits for a status the clock owes before it gives up. Far above
/// any span this service owes a transition, so a slow or loaded host changes how
/// many times the loop looks, never whether it succeeds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two looks.
const SETTLE_POLL: Duration = Duration::from_millis(20);

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate -> handle
/// -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
    /// The same shared engine the router hands every handler, so a test can put on
    /// a stored record a status that no implemented operation reaches yet.
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// Send one restJson1 request and return `(status, parsed body)`.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let bytes = match body {
            Some(body) => Bytes::from(serde_json::to_vec(&body).expect("serialize the body")),
            None => Bytes::new(),
        };
        let request = ::http::Request::builder()
            .method(method)
            .uri(format!("https://rtbfabric.{REGION}.amazonaws.com{path}"))
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/rtbfabric/aws4_request, \
                     SignedHeaders=host, Signature=00"
                ),
            )
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

    /// Create `count` responder gateways and return the ids the service minted,
    /// sorted — the order the listing must enumerate them in.
    async fn create_responder_gateways(&self, case: &str, count: usize) -> Vec<String> {
        let mut created = Vec::new();
        for n in 0..count {
            let (status, body) = self
                .send(
                    "POST",
                    "/responder-gateway",
                    Some(responder_body(&format!("token-{n}"))),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateResponderGateway #{n} to answer 200, got {status} {body:?}"
            );
            created.push(text(&body, "gatewayId"));
        }
        created.sort();
        created
    }

    /// Walk the whole listing at `max_results` per call, returning every page's
    /// `gatewayIds` in the order the pages came back.
    async fn walk(&self, case: &str, max_results: Option<i64>) -> Vec<Vec<String>> {
        let mut pages: Vec<Vec<String>> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let (status, body) = self
                .send("GET", &listing_path(max_results, token.as_deref()), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected ListResponderGateways to answer 200, got {status} {body:?}"
            );
            pages.push(ids(&body, case));
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
            assert!(
                pages.len() <= 64,
                "case {case:?}: the walk did not terminate, pages so far {pages:?}"
            );
        }
        pages
    }

    /// Create one responder gateway from `body` and return the id the service
    /// minted.
    async fn create_responder_gateway(&self, case: &str, body: Value) -> String {
        let (status, created) = self.send("POST", "/responder-gateway", Some(body)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected CreateResponderGateway to answer 200, got {status} {created:?}"
        );
        text(&created, "gatewayId")
    }

    /// Read the gateway through `GetResponderGateway` and answer with that
    /// reading, asserting the read itself succeeded.
    async fn read_responder_gateway(&self, case: &str, gateway_id: &str) -> Value {
        let (status, read) = self
            .send("GET", &format!("/responder-gateway/{gateway_id}"), None)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetResponderGateway to answer 200 for {gateway_id:?}, got {status} {read:?}"
        );
        read
    }

    /// Read the gateway until it reports `expected`, and answer with that
    /// reading.
    ///
    /// A settled status is owed by the clock, so the test waits for the
    /// CONDITION rather than for a span: it looks again until the status holds,
    /// giving up only once [`SETTLE_DEADLINE`] has passed.
    async fn poll_gateway_status(&self, case: &str, gateway_id: &str, expected: &str) -> Value {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let read = self.read_responder_gateway(case, gateway_id).await;
            if text(&read, "status") == expected {
                return read;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected gateway {gateway_id:?} to reach {expected:?}, got {:?}",
                read["status"]
            );
            tokio::time::sleep(SETTLE_POLL).await;
        }
    }

    /// Put `status` on every stored responder gateway, with nothing further owed
    /// on it.
    ///
    /// Most members of the modelled `ResponderGatewayStatus` enum are reached by
    /// operations this crate has not implemented yet, so the state is set
    /// directly to cover the whole enum rather than only the part the wire
    /// reaches today.
    fn force_responder_gateway_status(&self, status: &str) {
        let state = self.engine.state::<ComAmazonawsRtbfabricRtbfabricState>();
        let mut store = state.lock();
        for gateway in store.responder_gateways.values_mut() {
            gateway.status = status.to_string();
            gateway.settles_at = None;
        }
    }
}

/// The listing's path for one call.
fn listing_path(max_results: Option<i64>, token: Option<&str>) -> String {
    let mut query: Vec<String> = Vec::new();
    if let Some(max_results) = max_results {
        query.push(format!("maxResults={max_results}"));
    }
    if let Some(token) = token {
        query.push(format!("nextToken={token}"));
    }
    if query.is_empty() {
        "/responder-gateways".to_string()
    } else {
        format!("/responder-gateways?{}", query.join("&"))
    }
}

/// A `CreateResponderGateway` body naming a `domainName`, whose members satisfy the
/// modelled patterns.
fn responder_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "port": 443,
        "protocol": "HTTPS",
        "listenerConfig": { "protocols": ["HTTPS"] },
        "domainName": "responder.example.com",
        "clientToken": client_token,
    })
}

/// A `ManagedEndpointConfiguration`, as the modelled union's `autoScalingGroups`
/// member declares it (`smithy_query(query="shape",
/// shape="ManagedEndpointConfiguration,AutoScalingGroupsConfiguration")`).
fn managed_endpoint() -> Value {
    json!({
        "autoScalingGroups": {
            "autoScalingGroupNames": ["bidders"],
            "roleArn": "arn:aws:iam::123456789012:role/rtbfabric-managed-endpoint",
        }
    })
}

/// A `CreateRequesterGateway` body; a requester gateway is a different resource and
/// `ListResponderGateways` must not report it.
fn requester_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "clientToken": client_token,
    })
}

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// The `gatewayIds` member of a listing response, which the model declares as a list
/// and the operation always reports — an exhausted enumeration answers an empty
/// list, never an absent member.
fn ids(body: &Value, case: &str) -> Vec<String> {
    let listed = body["gatewayIds"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: expected gatewayIds to be a list, got {body:?}"));
    listed
        .iter()
        .map(|id| {
            id.as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected every gatewayId to be a string, got {id:?}")
                })
                .to_string()
        })
        .collect()
}

/// One row of the pagination table: how many gateways the store holds, and the page
/// size the walk asks for.
struct Row {
    name: &'static str,
    stored: usize,
    /// `None` exercises the member's modelled default of 10.
    max_results: Option<i64>,
}

const ROWS: &[Row] = &[
    Row {
        name: "an empty store",
        stored: 0,
        max_results: None,
    },
    Row {
        name: "fewer gateways than the default page",
        stored: 3,
        max_results: None,
    },
    Row {
        name: "exactly the default page",
        stored: 10,
        max_results: None,
    },
    Row {
        name: "several default pages with a short last one",
        stored: 23,
        max_results: None,
    },
    Row {
        name: "one gateway per page, the range's minimum",
        stored: 4,
        max_results: Some(1),
    },
    Row {
        name: "a page size that divides the store exactly",
        stored: 9,
        max_results: Some(3),
    },
    Row {
        name: "the range's maximum over several pages",
        stored: 17,
        max_results: Some(10),
    },
    Row {
        name: "a page larger than the store",
        stored: 2,
        max_results: Some(10),
    },
];

#[tokio::test]
async fn the_listing_reports_every_stored_responder_gateway_exactly_once_in_pages() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let created = fabric.create_responder_gateways(case, row.stored).await;

        let page_size = usize::try_from(row.max_results.unwrap_or(10)).expect("a positive page");
        let pages = fabric.walk(case, row.max_results).await;

        let expected_pages = row.stored.div_euclid(page_size)
            + usize::from(row.stored.rem_euclid(page_size) != 0 || row.stored == 0);
        assert_eq!(
            pages.len(),
            expected_pages,
            "case {case:?}: expected {expected_pages} page(s) for {} gateway(s) at {page_size} per page, got {}: {pages:?}",
            row.stored,
            pages.len()
        );
        for (index, page) in pages.iter().enumerate() {
            assert!(
                page.len() <= page_size,
                "case {case:?}: page {index} holds {} ids, more than the {page_size} asked for: {page:?}",
                page.len()
            );
        }

        let walked: Vec<String> = pages.iter().flatten().cloned().collect();
        assert_eq!(
            walked, created,
            "case {case:?}: expected the walk to report every stored gateway exactly once in id order, got {walked:?}"
        );
        let distinct: BTreeSet<&String> = walked.iter().collect();
        assert_eq!(
            distinct.len(),
            walked.len(),
            "case {case:?}: expected no gateway to be reported twice, got {walked:?}"
        );

        // Every listed id resolves through the implemented reader, so the listing
        // reports records that are really in state rather than ids it invented.
        for id in &walked {
            let (status, read) = fabric
                .send("GET", &format!("/responder-gateway/{id}"), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetResponderGateway to resolve the listed id {id:?}, got {status} {read:?}"
            );
            assert_eq!(
                text(&read, "gatewayId"),
                *id,
                "case {case:?}: expected GetResponderGateway to read back {id:?}, got {read:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_next_token_is_handed_out_only_while_ids_remain() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let created = fabric.create_responder_gateways(case, row.stored).await;
        let page_size = usize::try_from(row.max_results.unwrap_or(10)).expect("a positive page");

        let mut seen = 0usize;
        let mut token: Option<String> = None;
        loop {
            let path = listing_path(row.max_results, token.as_deref());
            let (status, body) = fabric.send("GET", &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected 200 for {path:?}, got {status} {body:?}"
            );
            seen += ids(&body, case).len();

            let remaining = created.len().saturating_sub(seen);
            let more_pages_due = remaining > 0;
            assert_eq!(
                body["nextToken"].is_string(),
                more_pages_due,
                "case {case:?}: with {remaining} of {} gateway(s) still unread at {page_size} per page, expected nextToken present={more_pages_due}, got {:?}",
                created.len(),
                body["nextToken"]
            );
            token = body["nextToken"].as_str().map(str::to_string);
            if token.is_none() {
                break;
            }
        }
        assert_eq!(
            seen,
            created.len(),
            "case {case:?}: expected the walk to read {} gateway(s), got {seen}",
            created.len()
        );
    }
}

#[tokio::test]
async fn a_token_this_listing_never_issued_is_refused() {
    let fabric = Fabric::new();
    fabric.create_responder_gateways("refusal", 4).await;
    // Two requester gateways, so the other listing has more than one page to issue a
    // token for at a page size of one.
    for n in 0..2 {
        fabric
            .send("POST", "/requester-gateway", Some(requester_body(&format!("token-r{n}"))))
            .await;
    }
    let (status, requester_page) = fabric
        .send("GET", "/requester-gateways?maxResults=1", None)
        .await;
    assert_eq!(
        status, 200,
        "expected ListRequesterGateways to answer 200, got {status} {requester_page:?}"
    );
    let other_listings_token = requester_page["nextToken"]
        .as_str()
        .expect("the requester listing hands out a token while ids remain")
        .to_string();

    for (case, token) in [
        ("not hex at all", "not-a-token".to_string()),
        ("an odd number of hex digits", "abc".to_string()),
        ("hex carrying no cursor", "6162".to_string()),
        // Hex of "rtb-gw-0000000000000001" alone: a cursor with no issuing operation
        // and no issuing instant is not a token this listing hands out.
        (
            "hex of a bare gateway id",
            "7274622d67772d30303030303030303030303030303031".to_string(),
        ),
        ("far longer than any issued token", "61".repeat(256)),
        // The codec scopes a token to the operation that issued it, so the other
        // listing's token names no position in this walk.
        ("a token the requester listing issued", other_listings_token),
    ] {
        let (status, body) = fabric
            .send("GET", &listing_path(None, Some(&token)), None)
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {token:?} to be refused with 400, got {status} {body:?}"
        );
    }
}

#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused() {
    let fabric = Fabric::new();
    fabric.create_responder_gateways("bounds", 2).await;

    // The published bounds of the modelled `maxResults` range are 1 and 10; one past
    // each is refused before the handler runs, and each bound itself is accepted.
    for (case, max_results, expected) in [
        ("one below the minimum", 0, 400),
        ("the minimum", 1, 200),
        ("the maximum", 10, 200),
        ("one above the maximum", 11, 400),
    ] {
        let (status, body) = fabric
            .send("GET", &listing_path(Some(max_results), None), None)
            .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected maxResults={max_results} to answer {expected}, got {status} {body:?}"
        );
    }
}

#[tokio::test]
async fn the_listing_reports_only_responder_gateways_and_changes_nothing() {
    let fabric = Fabric::new();
    let created = fabric.create_responder_gateways("mixed store", 2).await;

    let (status, requester) = fabric
        .send("POST", "/requester-gateway", Some(requester_body("token-r")))
        .await;
    assert_eq!(
        status, 200,
        "expected CreateRequesterGateway to answer 200, got {status} {requester:?}"
    );
    let requester_id = text(&requester, "gatewayId");

    let (status, first) = fabric.send("GET", "/responder-gateways", None).await;
    assert_eq!(
        status, 200,
        "expected ListResponderGateways to answer 200, got {status} {first:?}"
    );
    assert_eq!(
        ids(&first, "mixed store"),
        created,
        "expected only the responder gateways to be reported, got {first:?}"
    );
    assert!(
        !ids(&first, "mixed store").contains(&requester_id),
        "expected the requester gateway {requester_id:?} to be absent from the responder listing, got {first:?}"
    );

    // A read changes nothing a later caller sees: the same call answers the same
    // body, and the requester gateway is unaffected by it.
    let (status, again) = fabric.send("GET", "/responder-gateways", None).await;
    assert_eq!(
        status, 200,
        "expected the repeated listing to answer 200, got {status} {again:?}"
    );
    assert_eq!(
        again, first,
        "expected a repeated listing to answer identically, got {again:?} after {first:?}"
    );
    let (status, read) = fabric
        .send("GET", &format!("/requester-gateway/{requester_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected the requester gateway to still be readable after the listing, got {status} {read:?}"
    );
}

#[tokio::test]
async fn a_managed_endpoint_paired_with_a_domain_name_is_refused_and_stores_nothing() {
    // (case, whether the body names a domainName, whether it names a managed
    // endpoint): the three combinations the model admits, since neither member is
    // `@required` on its own and at least one must be named.
    for (case, with_domain, with_managed_endpoint) in [
        ("a domain name alone", true, false),
        ("a managed endpoint alone", false, true),
        ("a managed endpoint and a domain name", true, true),
    ] {
        let fabric = Fabric::new();
        let mut body = json!({
            "vpcId": "vpc-0123456789abcdef0",
            "subnetIds": ["subnet-0123456789abcdef0"],
            "securityGroupIds": ["sg-0123456789abcdef0"],
            "port": 443,
            "protocol": "HTTPS",
            "gatewayType": "EXTERNAL",
            "clientToken": "token-responder",
        });
        let domain = "managed.example.com";
        if with_domain {
            body["domainName"] = json!(domain);
        }
        if with_managed_endpoint {
            body["managedEndpointConfiguration"] = managed_endpoint();
        }

        let (status, answered) = fabric.send("POST", "/responder-gateway", Some(body)).await;

        if with_domain && with_managed_endpoint {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the create to be refused with 400, got {status} {answered:?}"
            );
            let expected =
                format!("Domain name '{domain}' for managed endpoint not allowlisted.");
            assert_eq!(
                text(&answered, "message"),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                answered["message"]
            );
            // The refused create stored nothing: the listing reports no gateway.
            let (status, listed) = fabric.send("GET", "/responder-gateways", None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected ListResponderGateways to answer 200, got {status} {listed:?}"
            );
            assert_eq!(
                ids(&listed, case),
                Vec::<String>::new(),
                "case {case:?}: expected the refused create to store no gateway, got {listed:?}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the create to answer 200, got {status} {answered:?}"
        );
        // The created gateway is in the store, and the listing reports exactly it.
        let gateway_id = text(&answered, "gatewayId");
        let (status, listed) = fabric.send("GET", "/responder-gateways", None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ListResponderGateways to answer 200, got {status} {listed:?}"
        );
        assert_eq!(
            ids(&listed, case),
            vec![gateway_id.clone()],
            "case {case:?}: expected the listing to report the created gateway {gateway_id:?}, got {listed:?}"
        );
    }

    // A create naming NEITHER member is refused too — the operation's own
    // documentation is that a domain name or a managed endpoint is required — so the
    // refusal above is not the only thing standing between a caller and a gateway
    // with no endpoint at all.
    let fabric = Fabric::new();
    let (status, answered) = fabric
        .send(
            "POST",
            "/responder-gateway",
            Some(json!({
                "vpcId": "vpc-0123456789abcdef0",
                "subnetIds": ["subnet-0123456789abcdef0"],
                "securityGroupIds": ["sg-0123456789abcdef0"],
                "port": 443,
                "protocol": "HTTPS",
                "clientToken": "token-responder",
            })),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {:?}: expected the create to be refused with 400, got {status} {answered:?}",
        "neither member"
    );
}

// ---------------------------------------------------------------------------
// `UpdateResponderGateway`: what happens next to a gateway a create brought up.
// ---------------------------------------------------------------------------

/// The `domainName`, `port` and `protocol` the gateways below are created with:
/// the three members this operation's own documentation says it never changes.
const BASELINE_DOMAIN_NAME: &str = "responder.example.com";
const BASELINE_PORT: i64 = 443;
const BASELINE_PROTOCOL: &str = "HTTPS";

/// The configuration members `UpdateResponderGateway` applies, as the modelled
/// request declares them (`smithy_query(query="operation",
/// operation="UpdateResponderGateway")`). `managedEndpointConfiguration` is one of
/// them and is covered on its own, because it cannot be paired with the
/// `domainName` the gateway in this table carries.
const UPDATABLE_MEMBERS: &[&str] = &[
    "description",
    "listenerConfig",
    "trustStoreConfiguration",
    "clientRoutingPolicy",
];

/// A `Base64EncodedCertificateChain` value, which the model bounds in length and
/// leaves otherwise unconstrained (`smithy_query(query="shape",
/// shape="CertificateAuthorityCertificates")`).
const CA_CERTIFICATE: &str = "dGVzdC1jYS1jZXJ0aWZpY2F0ZQ==";
/// A second one, so a trust store can carry the list's published maximum of two.
const OTHER_CA_CERTIFICATE: &str = "c2Vjb25kLXRlc3QtY2EtY2VydGlmaWNhdGU=";

/// The `CreateResponderGateway` body every update case starts from: a gateway with
/// a domain name of its own and a value in each member the update can replace,
/// so a member the update leaves out has something to keep.
fn update_baseline_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "port": BASELINE_PORT,
        "protocol": BASELINE_PROTOCOL,
        "domainName": BASELINE_DOMAIN_NAME,
        "description": "Created by the suite",
        "listenerConfig": { "protocols": ["HTTPS"] },
        "clientRoutingPolicy": "AVAILABILITY_ZONE_AFFINITY",
        "clientToken": client_token,
    })
}

/// An `UpdateResponderGateway` body: the members the model makes `@required`
/// beside the path's `gatewayId`, with every member of `patch` laid over them.
fn update_body(client_token: &str, patch: &Value) -> Value {
    let mut body = json!({
        "port": BASELINE_PORT,
        "protocol": BASELINE_PROTOCOL,
        "clientToken": client_token,
    });
    for (member, value) in patch.as_object().expect("the patch is an object") {
        body[member] = value.clone();
    }
    body
}

/// A `ManagedEndpointConfiguration` naming the union's `eksEndpoints` member, as
/// `smithy_query(query="shape", shape="EksEndpointsConfiguration")` declares it.
fn eks_managed_endpoint() -> Value {
    json!({
        "eksEndpoints": {
            "endpointsResourceName": "bidders",
            "endpointsResourceNamespace": "rtb",
            "clusterApiServerEndpointUri": "https://cluster.example.com",
            "clusterApiServerCaCertificateChain": CA_CERTIFICATE,
            "clusterName": "fabric-cluster",
            "roleArn": "arn:aws:iam::123456789012:role/rtbfabric-managed-endpoint",
        }
    })
}

/// The one path every update below is sent to.
fn update_path(gateway_id: &str) -> String {
    format!("/responder-gateway/{gateway_id}/update")
}

/// One row of the update table: a name, and the configuration members the call
/// states.
fn update_rows() -> Vec<(String, Value)> {
    let mut rows: Vec<(String, Value)> = vec![
        (
            "the description alone".to_string(),
            json!({ "description": "Reconfigured" }),
        ),
        (
            "one protocol in the listener configuration".to_string(),
            json!({ "listenerConfig": { "protocols": ["HTTP"] } }),
        ),
        (
            "both protocols, the list's published maximum".to_string(),
            json!({ "listenerConfig": { "protocols": ["HTTPS", "HTTP"] } }),
        ),
        (
            "a trust store carrying one certificate".to_string(),
            json!({ "trustStoreConfiguration": { "certificateAuthorityCertificates": [CA_CERTIFICATE] } }),
        ),
        (
            "a trust store carrying the list's published maximum of two".to_string(),
            json!({
                "trustStoreConfiguration": {
                    "certificateAuthorityCertificates": [CA_CERTIFICATE, OTHER_CA_CERTIFICATE],
                }
            }),
        ),
        (
            "a trust store carrying the list's published minimum of none".to_string(),
            json!({ "trustStoreConfiguration": { "certificateAuthorityCertificates": [] } }),
        ),
        (
            "every member this operation applies, at once".to_string(),
            json!({
                "description": "Everything at once",
                "listenerConfig": { "protocols": ["HTTP", "HTTPS"] },
                "trustStoreConfiguration": { "certificateAuthorityCertificates": [OTHER_CA_CERTIFICATE] },
                "clientRoutingPolicy": "ANY_AVAILABILITY_ZONE",
            }),
        ),
        ("no optional member at all".to_string(), json!({})),
    ];
    // Every member of the modelled `ClientRoutingPolicy` enum, read off the
    // generated value list rather than written out here.
    for policy in CLIENT_ROUTING_POLICY_VALUES {
        rows.push((
            format!("the client routing policy {policy}"),
            json!({ "clientRoutingPolicy": policy }),
        ));
    }
    rows
}

#[tokio::test]
async fn an_update_replaces_the_members_it_states_and_keeps_every_other() {
    for (case, patch) in update_rows() {
        let case = case.as_str();
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_responder_gateway(case, update_baseline_body("create"))
            .await;
        let before = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;

        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body("update", &patch)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer 200, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "gatewayId"),
            gateway_id,
            "case {case:?}: expected the update to answer for {gateway_id:?}, got {answered:?}"
        );
        assert_eq!(
            text(&answered, "status"),
            GATEWAY_STATUS_PENDING_UPDATE,
            "case {case:?}: expected a gateway in service to take the change as {GATEWAY_STATUS_PENDING_UPDATE:?}, got {answered:?}"
        );
        let expected_policy = patch
            .get("clientRoutingPolicy")
            .unwrap_or(&before["clientRoutingPolicy"]);
        assert_eq!(
            &answered["clientRoutingPolicy"],
            expected_policy,
            "case {case:?}: expected the update to report the policy {expected_policy:?}, got {:?}",
            answered["clientRoutingPolicy"]
        );

        // The clock owes the gateway its way back into service, and what the next
        // reader sees is the merged configuration: every member the call stated,
        // and the stored value of every member it left out.
        let after = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;
        for member in UPDATABLE_MEMBERS {
            let expected = patch.get(*member).unwrap_or(&before[*member]);
            assert_eq!(
                &after[*member],
                expected,
                "case {case:?}: expected {member:?} to read back as {expected:?}, got {:?}",
                after[*member]
            );
        }
        for member in ["gatewayId", "domainName", "port", "protocol", "createdAt"] {
            assert_eq!(
                after[member], before[member],
                "case {case:?}: expected {member:?} to be unchanged at {:?}, got {:?}",
                before[member], after[member]
            );
        }
    }
}

#[tokio::test]
async fn an_update_never_changes_the_domain_name_port_or_protocol_the_create_fixed() {
    // The operation's own documentation is that these three cannot be changed
    // after the create (`smithy_query(query="doc",
    // shape="UpdateResponderGateway")`), so a call restating them differently is
    // taken and leaves them exactly as the create fixed them.
    for (case, patch) in [
        (
            "a different domain name",
            json!({ "domainName": "other.example.com" }),
        ),
        ("a different port", json!({ "port": 8443 })),
        ("a different protocol", json!({ "protocol": "HTTP" })),
        (
            "all three different",
            json!({ "domainName": "third.example.com", "port": 80, "protocol": "HTTP" }),
        ),
    ] {
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_responder_gateway(case, update_baseline_body("create"))
            .await;
        let before = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;

        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body("update", &patch)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer 200, got {status} {answered:?}"
        );

        let after = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;
        for member in ["domainName", "port", "protocol"] {
            assert_eq!(
                after[member], before[member],
                "case {case:?}: expected {member:?} to stay at {:?}, got {:?}",
                before[member], after[member]
            );
        }
    }
}

#[tokio::test]
async fn a_managed_endpoint_change_applies_to_a_gateway_that_has_no_domain_name() {
    for (case, patch) in [
        (
            "another auto scaling group",
            json!({
                "managedEndpointConfiguration": {
                    "autoScalingGroups": {
                        "autoScalingGroupNames": ["bidders", "spot-bidders"],
                        "roleArn": "arn:aws:iam::123456789012:role/rtbfabric-managed-endpoint",
                    }
                }
            }),
        ),
        (
            "the union's other member",
            json!({ "managedEndpointConfiguration": eks_managed_endpoint() }),
        ),
    ] {
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_responder_gateway(
                case,
                json!({
                    "vpcId": "vpc-0123456789abcdef0",
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": ["sg-0123456789abcdef0"],
                    "port": BASELINE_PORT,
                    "protocol": BASELINE_PROTOCOL,
                    "managedEndpointConfiguration": managed_endpoint(),
                    "clientToken": "create",
                }),
            )
            .await;
        fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;

        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body("update", &patch)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the update to answer 200, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "status"),
            GATEWAY_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the gateway to take the change as {GATEWAY_STATUS_PENDING_UPDATE:?}, got {answered:?}"
        );

        let after = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;
        assert_eq!(
            &after["managedEndpointConfiguration"], &patch["managedEndpointConfiguration"],
            "case {case:?}: expected the stated managed endpoint to read back, got {:?}",
            after["managedEndpointConfiguration"]
        );
        assert_eq!(
            after["domainName"],
            Value::Null,
            "case {case:?}: expected a gateway reached through a managed endpoint to have no domain name, got {:?}",
            after["domainName"]
        );
    }
}

#[tokio::test]
async fn a_managed_endpoint_named_for_a_gateway_with_its_own_domain_name_is_refused() {
    // The same rule the create refuses the pairing by: a managed endpoint serves a
    // caller-named domain only while that domain is allowlisted, and this API
    // grants no allowlist entry.
    let case = "a managed endpoint for a gateway with its own domain name";
    let fabric = Fabric::new();
    let gateway_id = fabric
        .create_responder_gateway(case, update_baseline_body("create"))
        .await;
    let before = fabric
        .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
        .await;

    let (status, answered) = fabric
        .send(
            "POST",
            &update_path(&gateway_id),
            Some(update_body(
                "update",
                &json!({ "managedEndpointConfiguration": managed_endpoint() }),
            )),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the update to be refused with 400, got {status} {answered:?}"
    );
    let expected = format!("Domain name '{BASELINE_DOMAIN_NAME}' for managed endpoint not allowlisted.");
    assert_eq!(
        text(&answered, "message"),
        expected,
        "case {case:?}: expected {expected:?}, got {:?}",
        answered["message"]
    );

    let after = fabric.read_responder_gateway(case, &gateway_id).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected the refused update to change nothing, got {after:?} after {before:?}"
    );
}

#[tokio::test]
async fn a_retried_client_token_answers_the_record_the_first_call_produced() {
    let case = "a retry of the same change under the same token";
    let fabric = Fabric::new();
    let gateway_id = fabric
        .create_responder_gateway(case, update_baseline_body("create"))
        .await;
    fabric
        .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
        .await;

    let patch = json!({ "description": "Reconfigured once" });
    let (status, first) = fabric
        .send(
            "POST",
            &update_path(&gateway_id),
            Some(update_body("retry-me", &patch)),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first update to answer 200, got {status} {first:?}"
    );
    let settled = fabric
        .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
        .await;

    let (status, repeated) = fabric
        .send(
            "POST",
            &update_path(&gateway_id),
            Some(update_body("retry-me", &patch)),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the retry to answer 200, got {status} {repeated:?}"
    );
    assert_eq!(
        text(&repeated, "status"),
        GATEWAY_STATUS_ACTIVE,
        "case {case:?}: expected the retry to report the record the first call produced, got {repeated:?}"
    );

    let after = fabric.read_responder_gateway(case, &gateway_id).await;
    assert_eq!(
        after, settled,
        "case {case:?}: expected the retry to apply nothing a second time, got {after:?} after {settled:?}"
    );
}

#[tokio::test]
async fn the_same_client_token_with_different_parameters_is_refused_and_changes_nothing() {
    const TOKEN: &str = "used-once";
    // What the first call stated; each row below restates one parameter
    // differently under the same token.
    let first_patch = json!({ "description": "Reconfigured once" });

    for (case, patch) in [
        (
            "a different description",
            json!({ "description": "Something else" }),
        ),
        (
            "a different client routing policy",
            json!({ "description": "Reconfigured once", "clientRoutingPolicy": "ANY_AVAILABILITY_ZONE" }),
        ),
        (
            "a different protocols list",
            json!({ "description": "Reconfigured once", "listenerConfig": { "protocols": ["HTTP"] } }),
        ),
        // `port` is a member this operation does not apply, and a retry that
        // restates it differently is still a different request.
        (
            "a different port",
            json!({ "description": "Reconfigured once", "port": 8443 }),
        ),
        ("no parameter at all", json!({})),
    ] {
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_responder_gateway(case, update_baseline_body("create"))
            .await;
        fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;
        let (status, first) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body(TOKEN, &first_patch)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the first update to answer 200, got {status} {first:?}"
        );
        let settled = fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;

        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body(TOKEN, &patch)),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the reused token to be refused with 400, got {status} {answered:?}"
        );
        let expected = format!(
            "clientToken {TOKEN} was already used for a UpdateResponderGateway request with different parameters"
        );
        assert_eq!(
            text(&answered, "message"),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answered["message"]
        );
        let after = fabric.read_responder_gateway(case, &gateway_id).await;
        assert_eq!(
            after, settled,
            "case {case:?}: expected the refused retry to change nothing, got {after:?} after {settled:?}"
        );

        // The same parameters under a token of their own are applied: it is the
        // reuse of the token that is refused, not the change.
        let (status, accepted) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body("a-fresh-token", &patch)),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a fresh token to carry the same change, got {status} {accepted:?}"
        );
        assert_eq!(
            text(&accepted, "status"),
            GATEWAY_STATUS_PENDING_UPDATE,
            "case {case:?}: expected the fresh token's change to be taken as {GATEWAY_STATUS_PENDING_UPDATE:?}, got {accepted:?}"
        );
    }
}

#[tokio::test]
async fn a_gateway_that_is_not_in_service_has_no_configuration_to_change() {
    // Every member of the modelled `ResponderGatewayStatus` enum, read off the
    // generated value list rather than written out here.
    for status_value in RESPONDER_GATEWAY_STATUS_VALUES {
        let case = *status_value;
        let fabric = Fabric::new();
        let gateway_id = fabric
            .create_responder_gateway(case, update_baseline_body("create"))
            .await;
        fabric
            .poll_gateway_status(case, &gateway_id, GATEWAY_STATUS_ACTIVE)
            .await;
        fabric.force_responder_gateway_status(status_value);
        let before = fabric.read_responder_gateway(case, &gateway_id).await;

        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body(
                    "update",
                    &json!({ "description": "Reconfigured" }),
                )),
            )
            .await;

        if [GATEWAY_STATUS_ACTIVE, GATEWAY_STATUS_PENDING_UPDATE].contains(status_value) {
            assert_eq!(
                status, 200,
                "case {case:?}: expected a gateway in service to take the change, got {status} {answered:?}"
            );
            assert_eq!(
                text(&answered, "status"),
                GATEWAY_STATUS_PENDING_UPDATE,
                "case {case:?}: expected the change to be taken as {GATEWAY_STATUS_PENDING_UPDATE:?}, got {answered:?}"
            );
            continue;
        }

        assert_eq!(
            status, 409,
            "case {case:?}: expected a gateway that is {case:?} to be refused with 409, got {status} {answered:?}"
        );
        let expected = format!("responder gateway {gateway_id} is {case} and cannot be updated");
        assert_eq!(
            text(&answered, "message"),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            answered["message"]
        );
        let after = fabric.read_responder_gateway(case, &gateway_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: expected the refused update to change nothing, got {after:?} after {before:?}"
        );
    }
}

#[tokio::test]
async fn an_update_of_an_id_that_names_no_stored_responder_gateway_resolves_to_nothing() {
    let fabric = Fabric::new();
    let released = fabric
        .create_responder_gateway("released", update_baseline_body("released"))
        .await;
    let (status, deleted) = fabric
        .send("DELETE", &format!("/responder-gateway/{released}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected DeleteResponderGateway to answer 200, got {status} {deleted:?}"
    );
    let (status, requester) = fabric
        .send("POST", "/requester-gateway", Some(requester_body("token-r")))
        .await;
    assert_eq!(
        status, 200,
        "expected CreateRequesterGateway to answer 200, got {status} {requester:?}"
    );

    for (case, gateway_id) in [
        // An id of the modelled shape that nothing ever minted.
        ("an id nothing minted", "rtb-gw-zzzzzzzz".to_string()),
        // A gateway of the OTHER kind: a different resource, reached through its
        // own surface, so this one resolves to nothing.
        ("a requester gateway's id", text(&requester, "gatewayId")),
        ("a released responder gateway's id", released.clone()),
    ] {
        let (status, answered) = fabric
            .send(
                "POST",
                &update_path(&gateway_id),
                Some(update_body("update", &json!({}))),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected {gateway_id:?} to be refused with 404, got {status} {answered:?}"
        );
        assert_eq!(
            text(&answered, "message"),
            RESOURCE_NOT_FOUND_MESSAGE,
            "case {case:?}: expected {RESOURCE_NOT_FOUND_MESSAGE:?}, got {:?}",
            answered["message"]
        );
    }
}
