// SPDX-License-Identifier: CC0-1.0

//! What a VPC link reports as the DESCRIPTION of its provisioning status, on both of the
//! API surfaces a VPC link is projected on.
//!
//! RULE: **a VPC link's status description is the service's fixed sentence for the status
//! the link reports at the same instant** — one sentence per status, the same for every link
//! in it, interpolating nothing from the request or from the link's record — and API Gateway
//! states that sentence identically on both of its surfaces and from every operation that
//! projects a link (`CreateVpcLink`, `GetVpcLink`, `GetVpcLinks`, `UpdateVpcLink`), so a
//! readiness gate matching on the description terminates whichever surface and whichever
//! reader it polls. A provisioned link's sentence is pinned verbatim, to the one the service
//! is known to serve; the status a link is still being provisioned in is held to the
//! invariants above instead, because no sentence of the service's has been observed for it
//! to pin.
//!
//! A description of a link's STATE is also never the wording of a REFUSED request: a refusal
//! says something about a request and is stated in that refusal's own answer, so every sentence
//! the service refuses a VPC link call with ([`refusal_sentences`]) is held against every
//! description the readers below serve.
//!
//! The two surfaces name that pair differently — `status`/`statusMessage` on the REST-API
//! surface and `vpcLinkStatus`/`vpcLinkStatusMessage` on the HTTP-API one — and each is
//! served by its OWN service crate, so the sentence they agree on is only observable from
//! here: this suite mounts both control planes on one engine, as `core/router/src/lib.rs`
//! mounts them, and asserts the one description against both.
//!
//! The rows differ along what the rule generalises over: how many subnets a link attaches to
//! and how many load balancers it targets, how many security groups it carries, whether it
//! is described and tagged, and the length of its name. Every subnet a row attaches to is
//! one Amazon EC2 itself minted on the same engine, so the accepted creates are grounded in
//! what the owner of subnets actually holds.

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

/// The sentence a provisioned VPC link reports.
///
/// Spelled out here rather than taken from either crate's own constant: a test that asserts
/// a handler's constant back at it cannot catch the wording drifting away from the
/// service's, and the point of this suite is that the two surfaces state the SAME sentence
/// rather than each its own.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";

/// The status a link carries while it is being provisioned.
const PENDING: &str = "PENDING";

/// The status a link carries once it has been provisioned.
const AVAILABLE: &str = "AVAILABLE";

/// How long a poll waits for the provisioning the clock owes before the run is called a
/// defect, and how long it waits between reads. Generous: the transition is polled for
/// rather than waited out, and the deadline is only what stops a test whose premise never
/// arrives.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// One of the two API surfaces a VPC link is projected on: where its collection hangs, what
/// its status pair and its id are called, the success status its create answers, and the
/// member its listing reports a page under.
struct Surface {
    label: &'static str,
    collection: &'static str,
    status_member: &'static str,
    message_member: &'static str,
    id_member: &'static str,
    created_status: u16,
    page_member: &'static str,
}

/// The REST-API surface (`com.amazonaws.apigateway#VpcLink`), whose pair is
/// `status`/`statusMessage` and whose create is "an asynchronous operation" answered 202.
const REST: Surface = Surface {
    label: "the REST-API surface",
    collection: "/vpclinks",
    status_member: "status",
    message_member: "statusMessage",
    id_member: "id",
    created_status: 202,
    page_member: "item",
};

/// The HTTP-API surface (`com.amazonaws.apigatewayv2#VpcLink`), whose pair is
/// `vpcLinkStatus`/`vpcLinkStatusMessage` and whose create answers 201.
const V2: Surface = Surface {
    label: "the HTTP-API surface",
    collection: "/v2/vpclinks",
    status_member: "vpcLinkStatus",
    message_member: "vpcLinkStatusMessage",
    id_member: "vpcLinkId",
    created_status: 201,
    page_member: "items",
};

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not JSON).
struct Reply {
    status: u16,
    body: Value,
}

/// The emulator with the three services this suite needs, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: the two API Gateway control planes, and Amazon EC2,
/// which owns the subnets a link attaches to.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
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

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("ec2.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// Three subnets of ONE VPC, minted through EC2's own operations: a link's interfaces all
/// live in one VPC, so this is the inventory the rows attach to.
async fn ec2_subnets(router: &MultiServiceRouter) -> Vec<String> {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let mut subnets = Vec::new();
    for (index, zone) in ["us-west-2a", "us-west-2b", "us-west-2c"]
        .into_iter()
        .enumerate()
    {
        let cidr = format!("10.90.{}.0/24", index + 1);
        subnets.push(element(
            &ec2(
                router,
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

/// A network-load-balancer ARN of the shape `targetArns` documents ("The ARN of the network
/// load balancer of the VPC targeted by the VPC link"), numbered so a row can name several
/// distinct targets.
fn target_arn(index: usize) -> String {
    format!(
        "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/net/nlb-{index}/\
         abc123def456789{index}"
    )
}

/// One link the rule is asserted over: the create body each surface takes, and the values of
/// the request no fixed sentence may carry.
struct Row {
    case: &'static str,
    /// The `CreateVpcLink` body on [`REST`], whose link names load balancers.
    rest_request: Value,
    /// The `CreateVpcLink` body on [`V2`], whose link attaches to subnets.
    v2_request: Value,
    /// Every request value the sentences must be free of — the rule is that they interpolate
    /// nothing from the request.
    carries_nothing_of: Vec<String>,
}

/// The rows, written against the subnets EC2 holds.
fn rows(subnets: &[String]) -> Vec<Row> {
    let longest_name = "n".repeat(128);
    vec![
        Row {
            case: "the required members only: one target, one subnet",
            rest_request: json!({
                "name": "surface-minimal",
                "targetArns": [target_arn(0)],
            }),
            v2_request: json!({
                "name": "surface-minimal",
                "subnetIds": [subnets[0]],
            }),
            carries_nothing_of: vec![
                "surface-minimal".to_string(),
                target_arn(0),
                subnets[0].clone(),
            ],
        },
        Row {
            case: "two targets, two subnets, one security group, described",
            rest_request: json!({
                "name": "surface-described",
                "description": "the private path to the fleet",
                "targetArns": [target_arn(1), target_arn(2)],
            }),
            v2_request: json!({
                "name": "surface-described",
                "subnetIds": [subnets[0], subnets[1]],
                "securityGroupIds": ["sg-0123456789abcdef0"],
            }),
            carries_nothing_of: vec![
                "surface-described".to_string(),
                "the private path to the fleet".to_string(),
                target_arn(2),
                subnets[1].clone(),
                "sg-0123456789abcdef0".to_string(),
            ],
        },
        Row {
            case: "three targets, three subnets, two security groups, tagged",
            rest_request: json!({
                "name": "surface-multi-az",
                "targetArns": [target_arn(3), target_arn(4), target_arn(5)],
                "tags": { "team": "platform", "env": "canary" },
            }),
            v2_request: json!({
                "name": "surface-multi-az",
                "subnetIds": subnets,
                "securityGroupIds": ["sg-0a1b2c3d", "sg-00112233445566778"],
                "tags": { "team": "platform", "env": "canary" },
            }),
            carries_nothing_of: vec![
                "surface-multi-az".to_string(),
                target_arn(5),
                subnets[2].clone(),
                "sg-00112233445566778".to_string(),
                "platform".to_string(),
                "canary".to_string(),
            ],
        },
        Row {
            // `VpcLink$Name` on the HTTP-API surface targets `StringWithLengthBetween1And128`,
            // so this is a link named at the longest either surface stores: the size of a
            // link's own members changes nothing about the sentence for its status.
            case: "a name at the 128-character bound both surfaces accept",
            rest_request: json!({
                "name": longest_name,
                "targetArns": [target_arn(6)],
            }),
            v2_request: json!({
                "name": longest_name,
                "subnetIds": [subnets[0]],
            }),
            carries_nothing_of: vec![longest_name.clone(), target_arn(6)],
        },
    ]
}

/// The sentence each status was seen reporting, across the rows, the readers AND the two
/// surfaces: the rule says there is exactly one per status, so the first reading of a status
/// is what every later reading of it is held to.
type SentencePerStatus = BTreeMap<String, String>;

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

/// Every sentence the service REFUSED a VPC link call with, on both surfaces: a read and an
/// update of a link id nothing minted.
///
/// A refusal's wording belongs to that refusal's own answer — it describes a REQUEST, not the
/// state of a resource — so no link's status description may state it. The sentences are taken
/// from the service rather than written out here, so a change to either surface's wording keeps
/// being checked.
async fn refusal_sentences(router: &MultiServiceRouter) -> Vec<String> {
    /// A `VpcLinkId` of the form either surface mints that nothing minted.
    const UNKNOWN: &str = "nosuchlink";
    let mut sentences = Vec::new();
    for surface in [&REST, &V2] {
        let unknown = format!("{}/{UNKNOWN}", surface.collection);
        for (label, method, body) in [
            ("a read of a link id nothing minted", "GET", None),
            (
                "an update of a link id nothing minted",
                "PATCH",
                Some(update_request(surface, 0)),
            ),
        ] {
            let answer = apigw(router, method, &unknown, body).await;
            assert!(
                (400..500).contains(&answer.status),
                "{label} ({}): expected the service to refuse it, got {} with {}",
                surface.label,
                answer.status,
                answer.body
            );
            let sentence = answer
                .body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                !sentence.is_empty(),
                "{label} ({}): expected the refusal to state why, got {}",
                surface.label,
                answer.body
            );
            sentences.push(sentence);
        }
    }
    sentences
}

/// Assert what the rule says of the status/description pair `projection` carries, record the
/// sentence that status reports in `seen`, and answer the status it reported.
fn assert_pair(
    case: &str,
    surface: &Surface,
    reader: &str,
    projection: &Value,
    carries_nothing_of: &[String],
    refusals: &[String],
    seen: &mut SentencePerStatus,
) -> String {
    let label = format!("{} / {reader}", surface.label);
    let status = member(case, &label, projection, surface.status_member);
    let message = member(case, &label, projection, surface.message_member);
    assert!(
        !message.is_empty(),
        "case {case:?} ({label}): expected a description of status {status:?}, got {projection}"
    );
    if status == AVAILABLE {
        assert_eq!(
            message, AVAILABLE_SENTENCE,
            "case {case:?} ({label}): expected a provisioned link to report \
             {AVAILABLE_SENTENCE:?}, got {message:?}"
        );
    }
    for value in carries_nothing_of {
        assert!(
            !message.contains(value.as_str()),
            "case {case:?} ({label}): expected the sentence for {status:?} to carry nothing of \
             the request or the record, but {message:?} carries {value:?}"
        );
    }
    // And a description of a link's STATE is never the wording of a refused request.
    for sentence in refusals {
        assert!(
            !message.contains(sentence.as_str()),
            "case {case:?} ({label}): expected the sentence for {status:?} to state nothing of \
             the refusal {sentence:?}, got {message:?}"
        );
    }
    // Neither the Region a request was signed for nor the account it was signed by is part
    // of a fixed sentence either.
    for value in [REGION, "000000000000"] {
        assert!(
            !message.contains(value),
            "case {case:?} ({label}): expected the sentence for {status:?} to name no \
             environment of the call, but {message:?} carries {value:?}"
        );
    }
    match seen.get(&status) {
        Some(first) => assert_eq!(
            &message, first,
            "case {case:?} ({label}): expected every link in {status:?} to be described by the \
             one sentence {first:?}, on either surface and from every reader, got {message:?}"
        ),
        None => {
            seen.insert(status.clone(), message.clone());
        }
    }
    status
}

/// Read `GetVpcLink` on `surface` until it reports a settled status — anything other than
/// [`PENDING`] — asserting the rule of every reading on the way, and answer that reading.
///
/// The provisioning is a transition the CLOCK owes, so this LOOKS AGAIN until the record
/// reports it rather than waiting the span out: nothing is nudged between reads.
async fn settled(
    router: &MultiServiceRouter,
    surface: &Surface,
    case: &str,
    id: &str,
    carries_nothing_of: &[String],
    refusals: &[String],
    seen: &mut SentencePerStatus,
) -> Value {
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
        let status = assert_pair(
            case,
            surface,
            "GetVpcLink",
            &read.body,
            carries_nothing_of,
            refusals,
            seen,
        );
        if status != PENDING {
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

/// The item `GetVpcLinks` reports for `id` on `surface`.
async fn listed_item(
    router: &MultiServiceRouter,
    surface: &Surface,
    case: &str,
    id: &str,
) -> Value {
    let page = apigw(router, "GET", surface.collection, None).await;
    assert_eq!(
        page.status,
        200,
        "case {case:?} ({}): expected 200 from GetVpcLinks, got {} with {}",
        surface.label,
        page.status,
        page.body
    );
    page.body
        .get(surface.page_member)
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get(surface.id_member).and_then(Value::as_str) == Some(id))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!(
                "case {case:?} ({}): expected GetVpcLinks to list {id}, got {}",
                surface.label, page.body
            )
        })
}

/// The `UpdateVpcLink` body each surface takes: the REST surface patches the record with
/// JSON-Patch operations, the HTTP-API surface with the members it replaces.
fn update_request(surface: &Surface, round: usize) -> Value {
    if surface.collection == REST.collection {
        json!({
            "patchOperations": [
                { "op": "replace", "path": "/description", "value": format!("updated-{round}") },
            ],
        })
    } else {
        json!({ "name": format!("updated-{round}") })
    }
}

#[tokio::test]
async fn both_api_gateway_surfaces_describe_one_vpc_link_status_with_one_sentence() {
    let router = build_router();
    let subnets = ec2_subnets(&router).await;
    // What the service says when it REFUSES a call about a VPC link: no description of a
    // link's own status may state any of it.
    let refusals = refusal_sentences(&router).await;

    // Every reading of every row on either surface goes in here, so the sentences are held
    // to being one per status ACROSS the rows, the readers and the two surfaces.
    let mut seen = SentencePerStatus::new();
    for (round, row) in rows(&subnets).into_iter().enumerate() {
        let case = row.case;
        for (surface, request) in [(&REST, &row.rest_request), (&V2, &row.v2_request)] {
            let created = apigw(&router, "POST", surface.collection, Some(request.clone())).await;
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
            // fixed sentence — the minted id included.
            let mut forbidden = row.carries_nothing_of.clone();
            forbidden.push(id.clone());

            // A link is still being provisioned when the create answers, so the create's own
            // projection describes THAT status.
            let created_status = assert_pair(
                case,
                surface,
                "CreateVpcLink",
                &created.body,
                &forbidden,
                &refusals,
                &mut seen,
            );
            assert_eq!(
                created_status, PENDING,
                "case {case:?} ({}): expected CreateVpcLink to report status {PENDING}, got {}",
                surface.label, created.body
            );

            let read = settled(&router, surface, case, &id, &forbidden, &refusals, &mut seen).await;
            let settled_status = member(case, surface.label, &read, surface.status_member);
            assert_eq!(
                settled_status, AVAILABLE,
                "case {case:?} ({}): expected a link over resources the account holds to \
                 provision, got {read}",
                surface.label
            );

            // The collection reader and the updater describe the settled link the same way.
            let item = listed_item(&router, surface, case, &id).await;
            assert_pair(
                case,
                surface,
                "GetVpcLinks",
                &item,
                &forbidden,
                &refusals,
                &mut seen,
            );
            assert_eq!(
                item.get(surface.message_member),
                read.get(surface.message_member),
                "case {case:?} ({}): expected the listed link to describe its status as its own \
                 read does ({read}), got {item}",
                surface.label
            );

            let patched = apigw(
                &router,
                "PATCH",
                &format!("{}/{id}", surface.collection),
                Some(update_request(surface, round)),
            )
            .await;
            assert_eq!(
                patched.status,
                200,
                "case {case:?} ({}): expected 200 from UpdateVpcLink, got {} with {}",
                surface.label,
                patched.status,
                patched.body
            );
            assert_pair(
                case,
                surface,
                "UpdateVpcLink",
                &patched.body,
                &forbidden,
                &refusals,
                &mut seen,
            );
        }
    }

    // Both statuses a provisioning link passes through were observed, and they are described
    // by sentences of their own, so the description MOVED with the status rather than
    // standing still — on both surfaces, which observed the one sentence each time.
    for status in [PENDING, AVAILABLE] {
        assert!(
            seen.contains_key(status),
            "expected the rows to observe status {status:?}, got {seen:?}"
        );
    }
    assert_ne!(
        seen[PENDING], seen[AVAILABLE],
        "expected a link being provisioned and a provisioned one to be described by sentences \
         of their own, got {:?} for both",
        seen[PENDING]
    );
}
