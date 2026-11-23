// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ListRequesterGateways`: enumerating the requester
//! gateways this service persisted.
//!
//! The rule under test is about state, not about one recorded request: the
//! listing reports the ids of exactly the requester gateways the store holds, in
//! a stable total order, a page of at most `maxResults` at a time, handing out a
//! `nextToken` only while ids remain and resuming exactly after the page that
//! issued it. The tables span what the rule generalises over — how many gateways
//! the store holds (none, fewer than a page, exactly a page, several pages) and
//! the page size, at each published bound of the modelled `maxResults` range and
//! one past it.
//!
//! How a `nextToken` is REFUSED is covered here too, and for every paginated
//! listing of this service rather than this one alone: a token past the span its
//! own documentation publishes is answered with the error code that same sentence
//! names, and a token no walk of the store ever issued with the operation's
//! modelled client-input error.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_rtbfabric_rtbfabric::state::{
    encode_next_token, service_state, PageTokenKey, Walk, TOKEN_LIFETIME,
};
use service_com_amazonaws_rtbfabric_rtbfabric::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test signs for.
const REGION: &str = "us-west-2";

/// The wire error code an expired pagination token must be answered with, and the
/// status that answer carries, written out here rather than read back from the crate
/// under test: `smithy_query(query="doc", shape="ListRequesterGatewaysRequest",
/// member="nextToken")` states "Using an expired pagination token will return an HTTP
/// 400 InvalidToken error".
const EXPIRED_TOKEN_CODE: &str = "InvalidToken";
const EXPIRED_TOKEN_STATUS: u16 = 400;

/// The code a token no walk of this store issued is answered with: the modelled
/// client-input error every one of these listings declares.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// What an expired token's answer states, for the listing named by `operation`: the
/// span is the documented 24 hours, and the operation is the one asked.
fn expired_message(operation: &str) -> String {
    format!(
        "nextToken has expired: a {operation} pagination token expires 24 hours after it is issued"
    )
}

/// The one emulated service, behind the same `MultiServiceRouter` the production
/// router assembles, so these calls take the real deserialize -> validate ->
/// handle -> serialize path.
struct Fabric {
    router: MultiServiceRouter,
    /// The same engine the service was built on, so a test can mint a token the
    /// store itself would have minted — the only way to hold an authentic token of
    /// any chosen age, since the secret it is authenticated under is never revealed.
    engine: Arc<engine::InMemoryEngine>,
}

impl Fabric {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// A `nextToken` this store itself would have handed out for `walk`, issued
    /// `age` before now.
    ///
    /// The issuing instant is computed rather than waited for, so nothing here
    /// depends on how long the test takes or how fast the host runs.
    fn token_aged(&self, walk: &Walk, cursor: &str, age: Duration) -> String {
        let key: PageTokenKey = service_state(&*self.engine)
            .expect("the service's own store")
            .lock()
            .page_tokens;
        let issued_at = SystemTime::now()
            .checked_sub(age)
            .expect("an issuing instant inside the clock's range");
        encode_next_token(&key, walk, cursor, issued_at)
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

    /// Create `count` requester gateways and return the ids the service minted,
    /// sorted — the order the listing must enumerate them in.
    async fn create_requester_gateways(&self, case: &str, count: usize) -> Vec<String> {
        let mut ids = Vec::new();
        for n in 0..count {
            let (status, body) = self
                .send(
                    "POST",
                    "/requester-gateway",
                    Some(requester_body(&format!("token-{n}"))),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected CreateRequesterGateway #{n} to answer 200, got {status} {body:?}"
            );
            ids.push(text(&body, "gatewayId"));
        }
        ids.sort();
        ids
    }

    /// Walk the whole listing at `max_results` per call, returning every page's
    /// `gatewayIds` in the order the pages came back.
    async fn walk(&self, case: &str, max_results: Option<i64>) -> Vec<Vec<String>> {
        let mut pages: Vec<Vec<String>> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<String> = Vec::new();
            if let Some(max_results) = max_results {
                query.push(format!("maxResults={max_results}"));
            }
            if let Some(token) = &token {
                query.push(format!("nextToken={token}"));
            }
            let path = if query.is_empty() {
                "/requester-gateways".to_string()
            } else {
                format!("/requester-gateways?{}", query.join("&"))
            };

            let (status, body) = self.send("GET", &path, None).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected ListRequesterGateways to answer 200 for {path:?}, got {status} {body:?}"
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
}

/// A `CreateRequesterGateway` body whose members satisfy the modelled patterns:
/// `VpcId` is `^vpc-\w{8,17}$`, `SubnetId` is `^subnet-\w{8,17}$` and
/// `SecurityGroupId` is `^sg-[0-9a-f]{8,40}$`, from
/// `smithy_query(query="shape", shape="SubnetIdList,SecurityGroupIdList")`.
fn requester_body(client_token: &str) -> Value {
    json!({
        "vpcId": "vpc-0123456789abcdef0",
        "subnetIds": ["subnet-0123456789abcdef0"],
        "securityGroupIds": ["sg-0123456789abcdef0"],
        "clientToken": client_token,
    })
}

/// A `CreateResponderGateway` body; a responder gateway is a different resource
/// and `ListRequesterGateways` must not report it.
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

fn text(body: &Value, member: &str) -> String {
    body[member].as_str().unwrap_or_default().to_string()
}

/// The `gatewayIds` member of a listing response, which the model declares as a
/// list and the operation always reports — an exhausted enumeration answers an
/// empty list, never an absent member.
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

/// One row of the pagination table: how many gateways the store holds, and the
/// page size the walk asks for.
struct Row {
    name: &'static str,
    stored: usize,
    /// `None` exercises the member's modelled default of 10.
    max_results: Option<i64>,
}

/// The rows differ along what the rule generalises over: the number of stored
/// gateways relative to a page (none, fewer, exactly one page, several pages
/// with a short last one) and the page size, at both published bounds of the
/// modelled `maxResults` range (`range(min: 1, max: 10)`).
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
async fn the_listing_reports_every_stored_requester_gateway_exactly_once_in_pages() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let created = fabric.create_requester_gateways(case, row.stored).await;

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
                .send("GET", &format!("/requester-gateway/{id}"), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected GetRequesterGateway to resolve the listed id {id:?}, got {status} {read:?}"
            );
            assert_eq!(
                text(&read, "gatewayId"),
                *id,
                "case {case:?}: expected GetRequesterGateway to read back {id:?}, got {read:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_next_token_is_handed_out_only_while_ids_remain() {
    for row in ROWS {
        let case = row.name;
        let fabric = Fabric::new();
        let created = fabric.create_requester_gateways(case, row.stored).await;
        let page_size = usize::try_from(row.max_results.unwrap_or(10)).expect("a positive page");

        let mut seen = 0usize;
        let mut token: Option<String> = None;
        loop {
            let mut query: Vec<String> = Vec::new();
            if let Some(max_results) = row.max_results {
                query.push(format!("maxResults={max_results}"));
            }
            if let Some(token) = &token {
                query.push(format!("nextToken={token}"));
            }
            let path = match query.is_empty() {
                true => "/requester-gateways".to_string(),
                false => format!("/requester-gateways?{}", query.join("&")),
            };
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
async fn a_token_this_operation_never_issued_is_refused() {
    let fabric = Fabric::new();
    let created = fabric.create_requester_gateways("refusal", 4).await;
    let elsewhere = Walk::of("ListRequesterGateways");

    for (case, token) in [
        ("not hex at all", "not-a-token".to_string()),
        ("an odd number of hex digits", "abc".to_string()),
        ("hex carrying no cursor", "6162".to_string()),
        // Hex of "rtb-gw-0000000000000001" alone: a cursor with no issuing
        // instant is not a token this operation hands out.
        (
            "hex of a bare gateway id",
            "7274622d67772d30303030303030303030303030303031".to_string(),
        ),
        // Longer than any token this operation issues, so it is refused without
        // being decoded at all.
        ("far longer than any issued token", "61".repeat(256)),
        // This very walk's scope and cursor, authenticated under a secret this
        // store never minted under: the cursor cannot be read at all, so its age
        // is unknown and the refusal is not the expired one, however old the
        // instant inside it claims to be.
        (
            "this walk's own shape under another store's secret",
            encode_next_token(
                &PageTokenKey::new(),
                &elsewhere,
                &created[0],
                SystemTime::now(),
            ),
        ),
        (
            "the same, claiming an instant long past the documented span",
            encode_next_token(
                &PageTokenKey::new(),
                &elsewhere,
                &created[0],
                SystemTime::now()
                    .checked_sub(TOKEN_LIFETIME * 2)
                    .expect("an issuing instant inside the clock's range"),
            ),
        ),
    ] {
        let (status, body) = fabric
            .send(
                "GET",
                &format!("/requester-gateways?nextToken={token}"),
                None,
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected {token:?} to be refused with 400, got {status} {body:?}"
        );
        assert_eq!(
            text(&body, "__type"),
            VALIDATION_EXCEPTION,
            "case {case:?}: expected a token no walk issued to carry the modelled error code, got {:?}",
            body["__type"]
        );
        let stated = text(&body, "message");
        assert_ne!(
            stated,
            expired_message("ListRequesterGateways"),
            "case {case:?}: a token that cannot be read at all is not refused as expired, got {stated:?}"
        );
    }
}

/// One paginated listing of this service: the path a token is replayed on, the walk
/// its tokens are bound to, and a cursor of the kind that walk pages by.
struct TokenWalk {
    name: &'static str,
    operation: &'static str,
    path: String,
    walk: Walk,
    cursor: String,
}

#[tokio::test]
async fn an_expired_pagination_token_is_answered_with_the_documented_error_code() {
    let fabric = Fabric::new();
    let requesters = fabric.create_requester_gateways("expiry", 2).await;
    let (status, responder) = fabric
        .send("POST", "/responder-gateway", Some(responder_body("expiry")))
        .await;
    assert_eq!(
        status, 200,
        "expected CreateResponderGateway to answer 200, got {status} {responder:?}"
    );
    let responder_id = text(&responder, "gatewayId");
    let hub = requesters[0].clone();

    // The rows differ along what the rule generalises over: which listing issued
    // the token, and whether its walk is scoped to the whole service or to one
    // parent resource.
    let walks = [
        TokenWalk {
            name: "a listing of every requester gateway",
            operation: "ListRequesterGateways",
            path: "/requester-gateways".to_string(),
            walk: Walk::of("ListRequesterGateways"),
            cursor: requesters[0].clone(),
        },
        TokenWalk {
            name: "a listing of every responder gateway",
            operation: "ListResponderGateways",
            path: "/responder-gateways".to_string(),
            walk: Walk::of("ListResponderGateways"),
            cursor: responder_id.clone(),
        },
        TokenWalk {
            name: "a listing scoped to one gateway's links",
            operation: "ListLinks",
            path: format!("/gateway/{hub}/links/"),
            walk: Walk::within("ListLinks", &[hub.as_str()]),
            // A `LinkId` of the form this listing pages by; which links the
            // gateway holds does not bear on how its token's age is read.
            cursor: "link-0000000000000001".to_string(),
        },
    ];

    // The published span is 24 hours, so the ages below are the bound itself, one
    // second past it, and far past it; a token at the bound is already spent,
    // since the span is how long it stays usable.
    let expired_ages = [
        ("exactly the published span", TOKEN_LIFETIME),
        (
            "one second past the published span",
            TOKEN_LIFETIME + Duration::from_secs(1),
        ),
        ("a week of spans past it", TOKEN_LIFETIME * 7),
    ];
    // Inside the span, with generous room on either side of the bound so the
    // outcome cannot turn on how fast the host runs.
    let live_ages = [
        ("just issued", Duration::from_secs(0)),
        (
            "an hour inside the published span",
            TOKEN_LIFETIME - Duration::from_secs(3_600),
        ),
    ];

    for walk in &walks {
        let case = walk.name;
        for (age_case, age) in expired_ages {
            let token = fabric.token_aged(&walk.walk, &walk.cursor, age);
            let (status, body) = fabric
                .send("GET", &format!("{}?nextToken={token}", walk.path), None)
                .await;
            assert_eq!(
                status, EXPIRED_TOKEN_STATUS,
                "case {case:?} / {age_case:?}: expected the documented status for an expired token, got {status} {body:?}"
            );
            assert_eq!(
                text(&body, "__type"),
                EXPIRED_TOKEN_CODE,
                "case {case:?} / {age_case:?}: expected the error code the nextToken documentation names, got {:?}",
                body["__type"]
            );
            assert_eq!(
                text(&body, "message"),
                expired_message(walk.operation),
                "case {case:?} / {age_case:?}: expected the refusal to state the documented span, got {:?}",
                body["message"]
            );
        }

        for (age_case, age) in live_ages {
            let token = fabric.token_aged(&walk.walk, &walk.cursor, age);
            let (status, body) = fabric
                .send("GET", &format!("{}?nextToken={token}", walk.path), None)
                .await;
            assert_eq!(
                status, 200,
                "case {case:?} / {age_case:?}: expected a token inside its span to be resumed from, got {status} {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_page_size_past_a_published_bound_is_refused() {
    let fabric = Fabric::new();
    fabric.create_requester_gateways("bounds", 2).await;

    // The published bounds of the modelled `maxResults` range are 1 and 10; one
    // past each is refused before the handler runs, and each bound itself is
    // accepted.
    for (case, max_results, expected) in [
        ("one below the minimum", 0, 400),
        ("the minimum", 1, 200),
        ("the maximum", 10, 200),
        ("one above the maximum", 11, 400),
    ] {
        let (status, body) = fabric
            .send(
                "GET",
                &format!("/requester-gateways?maxResults={max_results}"),
                None,
            )
            .await;
        assert_eq!(
            status, expected,
            "case {case:?}: expected maxResults={max_results} to answer {expected}, got {status} {body:?}"
        );
    }
}

#[tokio::test]
async fn the_listing_reports_only_requester_gateways_and_changes_nothing() {
    let fabric = Fabric::new();
    let created = fabric.create_requester_gateways("mixed store", 2).await;

    let (status, responder) = fabric
        .send("POST", "/responder-gateway", Some(responder_body("token-r")))
        .await;
    assert_eq!(
        status, 200,
        "expected CreateResponderGateway to answer 200, got {status} {responder:?}"
    );
    let responder_id = text(&responder, "gatewayId");

    let (status, first) = fabric.send("GET", "/requester-gateways", None).await;
    assert_eq!(
        status, 200,
        "expected ListRequesterGateways to answer 200, got {status} {first:?}"
    );
    assert_eq!(
        ids(&first, "mixed store"),
        created,
        "expected only the requester gateways to be reported, got {first:?}"
    );
    assert!(
        !ids(&first, "mixed store").contains(&responder_id),
        "expected the responder gateway {responder_id:?} to be absent from the requester listing, got {first:?}"
    );

    // A read changes nothing a later caller sees: the same call answers the same
    // body, and the responder listing is unaffected by it.
    let (status, again) = fabric.send("GET", "/requester-gateways", None).await;
    assert_eq!(
        status, 200,
        "expected the repeated listing to answer 200, got {status} {again:?}"
    );
    assert_eq!(
        again, first,
        "expected a repeated listing to answer identically, got {again:?} after {first:?}"
    );
    let (status, read) = fabric
        .send("GET", &format!("/responder-gateway/{responder_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "expected the responder gateway to still be readable after the listing, got {status} {read:?}"
    );
}
