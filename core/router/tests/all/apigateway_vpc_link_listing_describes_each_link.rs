// SPDX-License-Identifier: CC0-1.0

//! What a VPC link LISTING says about each link's provisioning status, end to end over HTTP.
//!
//! RULE: **a VPC link's status description is derived from the status THAT link reports at
//! the instant of the read** — a provisioned link is described by the service's fixed
//! sentence for that status, a link whose provisioning failed by the cause of its own
//! failure, and a link that is not provisioned is never described as ready — so a collection
//! reader projects EVERY item from its own status rather than describing a page from one
//! status, whichever page size the walk asks for and whichever reader is asked.
//! `VpcLink$statusMessage` is "a description about the VPC link status" on the REST-API
//! surface and `VpcLink$VpcLinkStatusMessage` is "a message summarizing the cause of the
//! status of the VPC link" on the HTTP-API one; neither model fixes the text, so the
//! sentence a provisioned link carries is pinned to the one the service is known to serve
//! and the statuses no response has been observed in are held to the invariants above
//! instead.
//!
//! Only this test crate can assert it that way. A page mixing statuses has to be a page of
//! links in TERMINAL statuses, or which status a reading catches would turn on how fast the
//! host runs: on the REST-API surface a link settles `FAILED` when Elastic Load Balancing
//! holds none of its targets and `AVAILABLE` when it holds them all, and that service is a
//! crate of its own — so the mixed page exists only where both are mounted on one engine, as
//! `core/router/src/lib.rs` mounts them. The HTTP-API surface attaches its links to subnets
//! Amazon EC2 owns, which is a third crate, and the two API Gateway control planes are a
//! fourth and a fifth.
//!
//! The rows differ along what the rule generalises over: how many targets or subnets a link
//! carries, which of them is unresolvable, whether the link is described, tagged or carries
//! security groups, the length of its name, and — across the two reads of each surface — the
//! page size the collection is walked with. The statuses are INTERLEAVED in creation order,
//! which is the order a listing reports, so a reader that described a page from one status
//! could not pass by accident.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The API version an ELBv2 client names in its awsQuery envelope, which is what the shared
/// `elasticloadbalancing` endpoint serves the ELBv2 model for.
const ELB_VERSION: &str = "2015-12-01";

/// The sentence a provisioned VPC link is described by.
///
/// Spelled out here rather than read from either service crate: a test that asserts a
/// handler's own constant back at it cannot catch the wording drifting away from the
/// service's. It is the one sentence the service is known to serve, so it is pinned; the
/// statuses with no observed sentence are held to the rule's invariants instead.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";

/// The description a link whose target names no load balancer the account holds settles
/// `FAILED` with — the cause of that link's own failure, and fixed text like the sentence
/// above.
const NLB_NOT_FOUND_SENTENCE: &str = "NLB ARN not found";

/// The statuses a VPC link passes through and settles in.
const PENDING: &str = "PENDING";
const AVAILABLE: &str = "AVAILABLE";
const FAILED: &str = "FAILED";

/// How long a poll waits for the provisioning the clock owes before the run is called a
/// defect, and how long it waits between reads. Generous on purpose, and more generous than
/// the siblings of this suite: it is a BOUND, never a wait — the transition is polled for, so
/// a longer bound costs a passing run nothing and only stops a test whose premise never
/// arrives, which is what keeps the cases from turning on how loaded the host is.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// How many pages a walk reads before it is called a defect: a bound on the walk, so a
/// collection reader that kept handing out the same cursor fails the test instead of looping
/// forever. Far above the number of links any case below creates.
const MAX_PAGES_WALKED: usize = 64;

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not JSON).
struct Reply {
    status: u16,
    body: Value,
}

/// One of the two API surfaces a VPC link is projected on: where its collection hangs, what
/// its status pair and its id are called, the success status its create answers, the member
/// its listing reports a page under, and how a walk of that collection is paged.
struct Surface {
    label: &'static str,
    collection: &'static str,
    status_member: &'static str,
    message_member: &'static str,
    id_member: &'static str,
    created_status: u16,
    page_member: &'static str,
    /// The query parameter a page size is asked for with, and the one a cursor is sent back
    /// in; the cursor is reported under `token_member` of the page.
    page_size_param: &'static str,
    token_param: &'static str,
    token_member: &'static str,
}

/// The REST-API surface (`com.amazonaws.apigateway#VpcLink`), whose pair is
/// `status`/`statusMessage`, whose create is "an asynchronous operation" answered 202, and
/// whose collection is walked with `limit`/`position`.
const REST: Surface = Surface {
    label: "the REST-API surface",
    collection: "/vpclinks",
    status_member: "status",
    message_member: "statusMessage",
    id_member: "id",
    created_status: 202,
    page_member: "item",
    page_size_param: "limit",
    token_param: "position",
    token_member: "position",
};

/// The HTTP-API surface (`com.amazonaws.apigatewayv2#VpcLink`), whose pair is
/// `vpcLinkStatus`/`vpcLinkStatusMessage`, whose create answers 201, and whose collection is
/// walked with `maxResults`/`nextToken`.
const V2: Surface = Surface {
    label: "the HTTP-API surface",
    collection: "/v2/vpclinks",
    status_member: "vpcLinkStatus",
    message_member: "vpcLinkStatusMessage",
    id_member: "vpcLinkId",
    created_status: 201,
    page_member: "items",
    page_size_param: "maxResults",
    token_param: "nextToken",
    token_member: "nextToken",
};

/// The emulator with the services these cases need, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: both API Gateway control planes, Amazon EC2, which
/// owns the subnets an HTTP-API link attaches to, and both Elastic Load Balancing models —
/// the Classic one, which owns the `elasticloadbalancing` signing name the two APIs share,
/// and the v2 one, whose load balancers a REST-API link targets.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_elasticloadbalancing_elasticloadbalancing_v7::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_apigatewayv2_apigatewayv2::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One API Gateway control-plane call over its restJson1 wire. Both surfaces answer to the
/// one signing name and are told apart by the path, as they are in the assembled emulator.
async fn apigw(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let payload = match &body {
        Some(document) => bytes::Bytes::from(document.to_string()),
        None => bytes::Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("apigateway"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    }
}

/// One form-encoded action at `signing_name`'s endpoint that must succeed, answered with its
/// XML body: this is how the EC2 and Elastic Load Balancing setup below is driven, through
/// each owner's OWN API rather than by writing into a store.
async fn query_api(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: {signing_name} {action} must succeed, got {status} with {text}"
    );
    text
}

/// Three subnets of ONE VPC, minted through Amazon EC2's own operations: an HTTP-API link's
/// interfaces all live in one VPC, so this is the inventory those rows attach to.
async fn ec2_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc = element(
        &query_api(
            router,
            "ec2",
            EC2_API_VERSION,
            "CreateVpc",
            &[("CidrBlock", "10.70.0.0/16")],
        )
        .await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in ["us-west-2a", "us-west-2b", "us-west-2c"]
        .into_iter()
        .enumerate()
    {
        let cidr = format!("10.70.{}.0/24", index + 1);
        subnets.push(element(
            &query_api(
                router,
                "ec2",
                EC2_API_VERSION,
                "CreateSubnet",
                &[
                    ("VpcId", vpc.as_str()),
                    ("CidrBlock", cidr.as_str()),
                    ("AvailabilityZone", zone),
                ],
            )
            .await,
            "subnetId",
        ));
    }
    subnets
}

/// A NETWORK load balancer of this account, created through Elastic Load Balancing's own
/// `CreateLoadBalancer` over a subnet Amazon EC2 holds, answering the ARN that service
/// assigned it.
async fn create_network_load_balancer(
    router: &MultiServiceRouter,
    name: &str,
    subnet_id: &str,
) -> String {
    let body = query_api(
        router,
        "elasticloadbalancing",
        ELB_VERSION,
        "CreateLoadBalancer",
        &[
            ("Name", name),
            ("Type", "network"),
            ("Scheme", "internal"),
            ("Subnets.member.1", subnet_id),
        ],
    )
    .await;
    element(&body, "LoadBalancerArn")
}

/// A well-formed ARN of a network load balancer that NOBODY created: the form Elastic Load
/// Balancing writes over this emulator's own account, so only the resource it names is
/// missing.
fn unheld_target(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{}:loadbalancer/net/{name}/0123456789abcdef",
        engine::emulator_account_id()
    )
}

/// One string member of a projection.
fn member(case: &str, label: &str, projection: &Value, name: &str) -> String {
    projection
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            panic!("case {case:?} ({label}): expected a string {name:?}, got {projection}")
        })
        .to_string()
}

/// The status/description pair a projection carries.
fn pair(case: &str, label: &str, surface: &Surface, projection: &Value) -> (String, String) {
    (
        member(case, label, projection, surface.status_member),
        member(case, label, projection, surface.message_member),
    )
}

/// The description each status was seen carrying: the rule says a status whose description is
/// FIXED text carries one description for every link in it, so the first reading of such a
/// status is what every later reading of it is held to. A `FAILED` link describes the cause of
/// ITS OWN failure, so that status is not keyed here.
type FixedPerStatus = BTreeMap<String, String>;

/// Assert what the rule says of the status/description pair `projection` carries, and answer
/// that pair.
///
/// Every reading is held to carrying a description at all, to describing a provisioned link
/// with [`AVAILABLE_SENTENCE`] and a link that is NOT provisioned with something else, and to
/// interpolating none of `carries_nothing_of`.
fn assert_rule(
    case: &str,
    surface: &Surface,
    reader: &str,
    projection: &Value,
    carries_nothing_of: &[String],
    fixed: &mut FixedPerStatus,
) -> (String, String) {
    let label = format!("{} / {reader}", surface.label);
    let (status, message) = pair(case, &label, surface, projection);
    assert!(
        !message.is_empty(),
        "case {case:?} ({label}): expected a description of status {status:?}, got {projection}"
    );
    if status == AVAILABLE {
        assert_eq!(
            message, AVAILABLE_SENTENCE,
            "case {case:?} ({label}): expected a provisioned link to be described \
             {AVAILABLE_SENTENCE:?}, got {message:?}"
        );
    } else {
        assert_ne!(
            message, AVAILABLE_SENTENCE,
            "case {case:?} ({label}): expected a link in {status:?} not to be described as \
             ready, got {message:?}"
        );
    }
    for value in carries_nothing_of {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?} ({label}): expected the description of {status:?} to carry nothing \
             of the request or the record, but {message:?} carries {value:?}"
        );
    }
    // Neither the Region a request was signed for nor the account it was signed by is part of
    // a description either.
    for value in [REGION, engine::emulator_account_id()] {
        assert!(
            !message.contains(value),
            "case {case:?} ({label}): expected the description of {status:?} to name no \
             environment of the call, but {message:?} carries {value:?}"
        );
    }
    if status != FAILED {
        match fixed.get(&status) {
            Some(first) => assert_eq!(
                &message, first,
                "case {case:?} ({label}): expected every link in {status:?} to be described by \
                 the one sentence {first:?}, got {message:?}"
            ),
            None => {
                fixed.insert(status.clone(), message.clone());
            }
        }
    }
    (status, message)
}

/// Read `GetVpcLink` on `surface` until it reports a settled status — anything other than
/// [`PENDING`] — and answer that reading.
///
/// The provisioning is a transition the CLOCK owes, so this LOOKS AGAIN until the record
/// reports it rather than waiting the span out: nothing is nudged between reads.
async fn settled(router: &MultiServiceRouter, surface: &Surface, case: &str, id: &str) -> Value {
    let path = format!("{}/{id}", surface.collection);
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = apigw(router, "GET", &path, None).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?} ({}): expected 200 from GetVpcLink({id}), got {} with {}",
            surface.label,
            read.status,
            read.body
        );
        if member(case, surface.label, &read.body, surface.status_member) != PENDING {
            return read.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?} ({}): expected the clock to settle {id} out of {PENDING}, last read \
             {}",
            surface.label,
            read.body
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// Every item of `surface`'s collection, read one page of at most `page_size` at a time and
/// resumed through the cursor each page reports.
///
/// Answers the items in the order the pages reported them, so one walk with the whole
/// collection in one page and one with a page per link can be compared item by item.
async fn walk(
    router: &MultiServiceRouter,
    surface: &Surface,
    case: &str,
    page_size: usize,
) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES_WALKED {
        let mut path = format!(
            "{}?{}={page_size}",
            surface.collection, surface.page_size_param
        );
        if let Some(token) = &cursor {
            // A cursor is OPAQUE: it is echoed back as the service minted it, percent-encoded
            // like every other value this suite puts in a request, so a token carrying a byte
            // a query string reserves still resumes the walk it was issued for.
            path.push('&');
            path.push_str(surface.token_param);
            path.push('=');
            path.push_str(&form_encode(token));
        }
        let page = apigw(router, "GET", &path, None).await;
        assert_eq!(
            page.status,
            200,
            "case {case:?} ({}): expected 200 from GetVpcLinks({path}), got {} with {}",
            surface.label,
            page.status,
            page.body
        );
        let reported = page
            .body
            .get(surface.page_member)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            reported.len() <= page_size,
            "case {case:?} ({}): expected at most {page_size} item(s) in a page, got {}",
            surface.label,
            page.body
        );
        items.extend(reported);
        cursor = page
            .body
            .get(surface.token_member)
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            return items;
        }
    }
    panic!(
        "case {case:?} ({}): expected the walk to end within {MAX_PAGES_WALKED} pages",
        surface.label
    );
}

/// The item a walk reported for `id`.
fn reported_item(case: &str, surface: &Surface, items: &[Value], id: &str) -> Value {
    items
        .iter()
        .find(|item| item.get(surface.id_member).and_then(Value::as_str) == Some(id))
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?} ({}): expected the listing to report {id}, got {items:?}",
                surface.label
            )
        })
}

/// One link a surface's rows create: the create body, the status it must settle in, the
/// description it must settle with, and the values of the request no description may carry.
struct Row {
    case: &'static str,
    request: Value,
    settles: &'static str,
    described: &'static str,
    carries_nothing_of: Vec<String>,
}

/// Create every row's link, settle it, and answer the ids in creation order beside the values
/// no description may carry — asserting on the way that the create and the link's own read
/// obey the rule.
async fn settled_links(
    router: &MultiServiceRouter,
    surface: &Surface,
    rows: &[Row],
    fixed: &mut FixedPerStatus,
) -> Vec<(String, Vec<String>)> {
    let mut links: Vec<(String, Vec<String>)> = Vec::new();
    for row in rows {
        let case = row.case;
        let created = apigw(router, "POST", surface.collection, Some(row.request.clone())).await;
        assert_eq!(
            created.status,
            surface.created_status,
            "case {case:?} ({}): expected {} from CreateVpcLink, got {} with {}",
            surface.label,
            surface.created_status,
            created.status,
            created.body
        );
        let id = member(case, surface.label, &created.body, surface.id_member);
        // Nothing of the request, and nothing of the record, may be interpolated into a
        // description — the minted id included.
        let mut forbidden = row.carries_nothing_of.clone();
        forbidden.push(id.clone());

        // A link is still being provisioned when the create answers, so the create's own
        // projection describes THAT status, not the one the link will settle in.
        let (status, _) = assert_rule(
            case,
            surface,
            "CreateVpcLink",
            &created.body,
            &forbidden,
            fixed,
        );
        assert_eq!(
            status, PENDING,
            "case {case:?} ({}): expected CreateVpcLink to report {PENDING}, got {}",
            surface.label, created.body
        );

        let read = settled(router, surface, case, &id).await;
        let (status, message) = assert_rule(case, surface, "GetVpcLink", &read, &forbidden, fixed);
        assert_eq!(
            status, row.settles,
            "case {case:?} ({}): expected the link to settle {}, got {read}",
            surface.label, row.settles
        );
        assert_eq!(
            message, row.described,
            "case {case:?} ({}): expected the settled link to be described {:?}, got {message:?}",
            surface.label, row.described
        );
        links.push((id, forbidden));
    }
    links
}

/// Assert the rule of every item a walk of `surface`'s collection reported, hold each row's
/// own link to the status and description that row settles in, and answer the pairs the walk
/// reported for the rows' links.
async fn assert_walk(
    router: &MultiServiceRouter,
    surface: &Surface,
    rows: &[Row],
    links: &[(String, Vec<String>)],
    page_size: usize,
    fixed: &mut FixedPerStatus,
) -> Vec<(String, String)> {
    let reader = format!("GetVpcLinks, {page_size} per page");
    let items = walk(router, surface, "the whole collection", page_size).await;
    // EVERY item of the collection is held to the rule, not only the rows' own links: a
    // reader that described a page from one status would be caught on any of them.
    for item in &items {
        assert_rule(
            "every link the account holds",
            surface,
            &reader,
            item,
            &[],
            fixed,
        );
    }
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (row, (id, forbidden)) in rows.iter().zip(links) {
        let item = reported_item(row.case, surface, &items, id);
        let (status, message) = assert_rule(row.case, surface, &reader, &item, forbidden, fixed);
        assert_eq!(
            (status.as_str(), message.as_str()),
            (row.settles, row.described),
            "case {:?} ({}): expected the listed link to be reported ({:?}, {:?}), got {item}",
            row.case,
            surface.label,
            row.settles,
            row.described
        );
        pairs.push((status, message));
    }
    pairs
}

#[tokio::test]
async fn a_rest_api_page_describes_each_link_from_its_own_status() {
    let router = build_router();
    let subnets = ec2_subnets(&router).await;
    // The load balancers the rows resolve against, created through Elastic Load Balancing's
    // own API over the subnets Amazon EC2 holds: two this account holds, so a link over them
    // provisions.
    let held_one = create_network_load_balancer(&router, "listing-held-one", &subnets[0]).await;
    let held_two = create_network_load_balancer(&router, "listing-held-two", &subnets[1]).await;
    let absent_one = unheld_target("listing-absent-one");
    let absent_two = unheld_target("listing-absent-two");

    // The statuses INTERLEAVE in creation order, which is the order this collection is
    // reported in, and both are TERMINAL, so the mixed page does not depend on the clock.
    let rows: Vec<Row> = vec![
        Row {
            case: "one target this account holds",
            request: json!({ "name": "listing-rest-one", "targetArns": [held_one.clone()] }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec!["listing-rest-one".to_string(), held_one.clone()],
        },
        Row {
            case: "one target no load balancer answers to",
            request: json!({ "name": "listing-rest-two", "targetArns": [absent_one.clone()] }),
            settles: FAILED,
            described: NLB_NOT_FOUND_SENTENCE,
            carries_nothing_of: vec!["listing-rest-two".to_string(), absent_one.clone()],
        },
        Row {
            case: "two targets this account holds, described and tagged",
            request: json!({
                "name": "listing-rest-three",
                "description": "the private path to the fleet",
                "targetArns": [held_one.clone(), held_two.clone()],
                "tags": { "team": "platform", "env": "test" },
            }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec![
                "listing-rest-three".to_string(),
                "the private path to the fleet".to_string(),
                held_two.clone(),
                "platform".to_string(),
            ],
        },
        Row {
            case: "two targets, the FIRST of which is unheld",
            request: json!({
                "name": "listing-rest-four",
                "targetArns": [absent_two.clone(), held_two.clone()],
            }),
            settles: FAILED,
            described: NLB_NOT_FOUND_SENTENCE,
            carries_nothing_of: vec!["listing-rest-four".to_string(), absent_two.clone()],
        },
        Row {
            // `VpcLink$name` is a `String` the create bounds at 128 characters, so this is a
            // link named at the longest the surface stores: the size of a link's own members
            // changes nothing about the description of its status.
            case: "a name at the 128-character bound this surface accepts",
            request: json!({ "name": "n".repeat(128), "targetArns": [held_two.clone()] }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec!["n".repeat(128)],
        },
    ];

    let mut fixed = FixedPerStatus::new();
    let links = settled_links(&router, &REST, &rows, &mut fixed).await;

    // One page carrying every link: each is described from its OWN status, so the provisioned
    // ones carry the service's sentence and the failed ones the cause of their own failure.
    let one_page = assert_walk(&router, &REST, &rows, &links, 500, &mut fixed).await;
    // A page per link reports the same pairs, so the description is projected per ITEM rather
    // than per page.
    let page_per_link = assert_walk(&router, &REST, &rows, &links, 1, &mut fixed).await;
    assert_eq!(
        page_per_link, one_page,
        "expected a walk of one link per page to report the pairs one page of the whole \
         collection reported, got {page_per_link:?} against {one_page:?}"
    );
    // The two terminal statuses were both observed in that one page, described differently:
    // the listing moved the description with each item's status rather than standing still.
    let settled_statuses: Vec<&str> = one_page.iter().map(|(status, _)| status.as_str()).collect();
    for status in [AVAILABLE, FAILED] {
        assert!(
            settled_statuses.contains(&status),
            "expected the rows to put a link in {status:?} into the page, got \
             {settled_statuses:?}"
        );
    }
    let described: BTreeMap<&str, &str> = one_page
        .iter()
        .map(|(status, message)| (status.as_str(), message.as_str()))
        .collect();
    assert_ne!(
        described[AVAILABLE], described[FAILED],
        "expected a provisioned link and a failed one in the same page to be described \
         differently, got {:?} for both",
        described[AVAILABLE]
    );
}

#[tokio::test]
async fn an_http_api_page_describes_each_link_from_its_own_status() {
    let router = build_router();
    let subnets = ec2_subnets(&router).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "the required members only: one subnet",
            request: json!({ "name": "listing-http-one", "subnetIds": [subnets[0]] }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec!["listing-http-one".to_string(), subnets[0].clone()],
        },
        Row {
            case: "two subnets, one security group",
            request: json!({
                "name": "listing-http-two",
                "subnetIds": [subnets[0], subnets[1]],
                "securityGroupIds": ["sg-0123456789abcdef0"],
            }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec![
                "listing-http-two".to_string(),
                subnets[1].clone(),
                "sg-0123456789abcdef0".to_string(),
            ],
        },
        Row {
            case: "three subnets, two security groups, tagged",
            request: json!({
                "name": "listing-http-three",
                "subnetIds": subnets,
                "securityGroupIds": ["sg-0a1b2c3d", "sg-00112233445566778"],
                "tags": { "team": "platform", "env": "test" },
            }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec![
                "listing-http-three".to_string(),
                subnets[2].clone(),
                "sg-00112233445566778".to_string(),
                "platform".to_string(),
            ],
        },
        Row {
            // `Name` targets `StringWithLengthBetween1And128`, so this is a link named at the
            // modelled upper bound.
            case: "a name at the modelled 128-character bound",
            request: json!({ "name": "n".repeat(128), "subnetIds": [subnets[0]] }),
            settles: AVAILABLE,
            described: AVAILABLE_SENTENCE,
            carries_nothing_of: vec!["n".repeat(128)],
        },
    ];

    let mut fixed = FixedPerStatus::new();
    let links = settled_links(&router, &V2, &rows, &mut fixed).await;

    let one_page = assert_walk(&router, &V2, &rows, &links, 100, &mut fixed).await;
    let page_per_link = assert_walk(&router, &V2, &rows, &links, 1, &mut fixed).await;
    assert_eq!(
        page_per_link, one_page,
        "expected a walk of one link per page to report the pairs one page of the whole \
         collection reported, got {page_per_link:?} against {one_page:?}"
    );

    // A link created and NOT waited for is in the page beside the settled ones, and the
    // walks above hold EVERY item of the collection to the rule: whatever status this one is
    // caught in, it is described from that status, and a link that is not provisioned is not
    // described as ready. Which status a reading catches is deliberately not asserted — that
    // would turn on how fast the host runs.
    let fresh = apigw(
        &router,
        "POST",
        V2.collection,
        Some(json!({ "name": "listing-http-fresh", "subnetIds": [subnets[0]] })),
    )
    .await;
    assert_eq!(
        fresh.status, V2.created_status,
        "expected {} from CreateVpcLink, got {} with {}",
        V2.created_status, fresh.status, fresh.body
    );
    let fresh_id = member("a link just created", V2.label, &fresh.body, V2.id_member);
    let items = walk(&router, &V2, "the collection with a fresh link in it", 100).await;
    for item in &items {
        assert_rule(
            "every link the account holds",
            &V2,
            "GetVpcLinks beside a fresh link",
            item,
            &[],
            &mut fixed,
        );
    }
    reported_item("a link just created", &V2, &items, &fresh_id);

    // Once the clock has settled that link too, the listing and the link's own read are
    // comparable without either having to be caught at an instant: both report the settled
    // status, so they are held to reporting the SAME pair rather than to agreeing only when a
    // reading happens to catch them in step.
    let case = "a link settled after it was listed unsettled";
    let read_fresh = settled(&router, &V2, case, &fresh_id).await;
    let read_pair = assert_rule(case, &V2, "GetVpcLink", &read_fresh, &[], &mut fixed);
    let settled_items = walk(&router, &V2, case, 100).await;
    let listed_fresh = reported_item(case, &V2, &settled_items, &fresh_id);
    let listed_pair = assert_rule(case, &V2, "GetVpcLinks", &listed_fresh, &[], &mut fixed);
    assert_eq!(
        listed_pair, read_pair,
        "case {case:?}: expected the listed link to be reported as its own read reports it, \
         got {listed_fresh} against {read_fresh}"
    );
    assert_eq!(
        (listed_pair.0.as_str(), listed_pair.1.as_str()),
        (AVAILABLE, AVAILABLE_SENTENCE),
        "case {case:?}: expected the settled link to be listed as provisioned and described \
         {AVAILABLE_SENTENCE:?}, got {listed_fresh}"
    );

    // The settled links are all provisioned, so the page pinned the service's sentence for
    // that status; the fresh one proves the page did not simply repeat it everywhere.
    for (status, message) in &one_page {
        assert_eq!(
            (status.as_str(), message.as_str()),
            (AVAILABLE, AVAILABLE_SENTENCE),
            "expected every settled row to be listed as provisioned and described \
             {AVAILABLE_SENTENCE:?}, got ({status:?}, {message:?})"
        );
    }

    // This surface settles a link in one status only, so the other status its listing can
    // report an item in is the one a DELETE moves a link to: a link whose deprovisioning is
    // outstanding is still listed, and the rule holds of every reading of it — it is
    // described from the status it is in, and not as ready. The walk LOOKS AGAIN until the
    // account stops reporting the link, a condition rather than a wait, so no case turns on
    // which readings a run happens to catch.
    let case = "a link whose deprovisioning the clock still owes";
    let (doomed, _) = &links[0];
    let accepted = apigw(
        &router,
        "DELETE",
        &format!("{}/{doomed}", V2.collection),
        None,
    )
    .await;
    assert_eq!(
        accepted.status, 202,
        "case {case:?}: expected 202 from DeleteVpcLink({doomed}), got {} with {}",
        accepted.status, accepted.body
    );
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let items = walk(&router, &V2, case, 100).await;
        for item in &items {
            assert_rule(
                case,
                &V2,
                "GetVpcLinks while a link is being deprovisioned",
                item,
                &[],
                &mut fixed,
            );
        }
        if !items
            .iter()
            .any(|item| item.get(V2.id_member).and_then(Value::as_str) == Some(doomed.as_str()))
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the deprovisioning the clock owes to end the readings of \
             {doomed}, which is still listed"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}
