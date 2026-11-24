// SPDX-License-Identifier: CC0-1.0

//! What a VPC link's asynchronous provisioning RESOLVES its targets against, end to end
//! over HTTP.
//!
//! RULE: **a VPC link carries traffic through the network load balancers its `targetArns`
//! name, so a link settles `AVAILABLE` only where Elastic Load Balancing holds every one of
//! them, and settles `FAILED` with the service's fixed description "NLB ARN not found" where
//! it holds one of them not** — a question about the RESOURCE, asked of the service that owns
//! load balancers at the instant the link is provisioned, and therefore not answerable from
//! the ARN's text. `targetArns` is "The ARN of the network load balancer of the VPC targeted
//! by the VPC link. The network load balancer must be owned by the same Amazon Web Services
//! account of the API owner" (`VpcLink$targetArns`), and `VpcLink$status` is the asynchronous
//! `AVAILABLE | PENDING | DELETING | FAILED` the create answers `PENDING` out of.
//!
//! The ARN's FORM is a different question, answered synchronously: an entry that is no ARN at
//! all is refused `400 BadRequestException` by the create itself, and the last case pins that
//! this refusal is still that one rather than a link that settles.
//!
//! Only this test crate can assert the rule: API Gateway and Elastic Load Balancing are
//! separate service crates, so the link's targets exist only where both are mounted on one
//! engine — as `core/router/src/lib.rs` mounts them — and every load balancer a row names is
//! created through Elastic Load Balancing's OWN API, at the shared endpoint an ELBv2 client
//! addresses (its API version in the envelope), rather than written into a store by hand.
//!
//! The rows differ along what the rule generalises over: how many targets a link carries,
//! WHICH of them is unresolvable (the only one, the first of several, the last of several),
//! how a target comes to be unresolvable (never created, or created and then deleted — so the
//! resolution is live rather than cached), and whether the target names another Region.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and the one the load balancers are created
/// in.
pub(crate) const REGION: &str = "us-west-2";

/// Another Region, for the row whose target names a load balancer somewhere else.
const OTHER_REGION: &str = "eu-west-1";

/// The API version an ELBv2 client names in its awsQuery envelope, which is what the shared
/// `elasticloadbalancing` endpoint serves the ELBv2 model for.
const ELB_VERSION: &str = "2015-12-01";

/// The statuses a VPC link passes through and settles in (`VpcLinkStatus`).
const PENDING: &str = "PENDING";
const AVAILABLE: &str = "AVAILABLE";
const FAILED: &str = "FAILED";

/// The description a provisioned link reports, and the one a link whose target does not
/// resolve reports. Spelled out here rather than read from the service crate: these are the
/// sentences a client's readiness gate keys on, so the suite pins the wording independently
/// of the constants the handler carries. Both are FIXED text, with no variable part.
const AVAILABLE_SENTENCE: &str = "VPC link is ready to route traffic";
const NOT_FOUND_SENTENCE: &str = "NLB ARN not found";

/// How long a poll waits for the provisioning the clock owes before the run is called a
/// defect, and how long it waits between reads. Generous: the transition is polled for rather
/// than waited out, and the deadline is only what stops a test whose premise never arrives.
const SETTLE_DEADLINE: Duration = Duration::from_secs(30);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not JSON).
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

/// The emulator with the three services this suite needs, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: API Gateway's control plane, and both Elastic Load
/// Balancing models — the Classic one, which owns the `elasticloadbalancing` signing name the
/// two APIs share, and the v2 one, whose load balancers a VPC link targets.
pub(crate) fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
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
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` in `region`,
/// which is what routes the request; the signature is never checked.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Elastic Load Balancing action at the endpoint the two models SHARE, in `region`, with
/// the ELBv2 API version in the envelope — exactly how an ELBv2 SDK client addresses it.
async fn elb(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> Reply {
    let mut body = format!("Action={action}&Version={ELB_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("elasticloadbalancing.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("elasticloadbalancing", region))
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: Value::String(String::from_utf8_lossy(response.body()).to_string()),
    }
}

/// One Elastic Load Balancing action that must succeed, answered with its XML body.
async fn elb_ok(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let answer = elb(router, region, action, params).await;
    let text = answer.body.as_str().unwrap_or_default().to_string();
    assert_eq!(
        answer.status, 200,
        "setup: Elastic Load Balancing {action} must succeed, got {} with {text}",
        answer.status
    );
    text
}

/// A NETWORK load balancer of this account, created through Elastic Load Balancing's own
/// `CreateLoadBalancer`, answering the ARN that service assigned it.
pub(crate) async fn create_network_load_balancer(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) -> String {
    let body = elb_ok(
        router,
        region,
        "CreateLoadBalancer",
        &[
            ("Name", name),
            ("Type", "network"),
            ("Scheme", "internal"),
            ("Subnets.member.1", "subnet-0123456789abcdef0"),
        ],
    )
    .await;
    element(&body, "LoadBalancerArn")
}

/// Remove one load balancer through Elastic Load Balancing's own `DeleteLoadBalancer`, so the
/// account no longer holds it.
pub(crate) async fn delete_load_balancer(router: &MultiServiceRouter, region: &str, arn: &str) {
    elb_ok(
        router,
        region,
        "DeleteLoadBalancer",
        &[("LoadBalancerArn", arn)],
    )
    .await;
}

/// One API Gateway control-plane call over its restJson1 wire.
pub(crate) async fn apigw(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
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
        .header("authorization", authorization("apigateway", REGION))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Reply {
        status: response.status().as_u16(),
        body: serde_json::from_slice(response.body()).unwrap_or(Value::Null),
    }
}

/// One string member of a JSON body.
pub(crate) fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a string {name:?}, got {body}"))
        .to_string()
}

/// A well-formed ARN of a network load balancer in `region` that NOBODY created: the form
/// Elastic Load Balancing writes (`loadbalancer/net/<name>/<id>`) over this emulator's own
/// account, so only the resource it names is missing.
pub(crate) fn unheld_target(region: &str, name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{region}:{}:loadbalancer/net/{name}/0123456789abcdef",
        engine::emulator_account_id()
    )
}

/// Read `GetVpcLink` until it reports a settled status — anything other than [`PENDING`] —
/// and answer that reading.
///
/// The provisioning is a transition the CLOCK owes, so this LOOKS AGAIN until the record
/// reports it rather than waiting the span out: nothing is nudged between reads.
pub(crate) async fn settled(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let path = format!("/vpclinks/{id}");
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let read = apigw(router, "GET", &path, None).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected 200 from GetVpcLink({id}), got {} with {}",
            read.status, read.body
        );
        if member(case, &read.body, "status") != PENDING {
            return read.body;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the clock to settle {id} out of {PENDING}, last read {}",
            read.body
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// The link `GetVpcLinks` reports for `id`.
async fn listed_link(router: &MultiServiceRouter, case: &str, id: &str) -> Value {
    let page = apigw(router, "GET", "/vpclinks?limit=500", None).await;
    assert_eq!(
        page.status, 200,
        "case {case:?}: expected 200 from GetVpcLinks, got {} with {}",
        page.status, page.body
    );
    page.body
        .get("item")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
                .cloned()
        })
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected GetVpcLinks to list {id}, got {}",
                page.body
            )
        })
}

/// One link the rule is asserted over.
struct Row {
    case: String,
    /// The `targetArns` the create carries, in request order.
    targets: Vec<String>,
    /// The status the link must settle in.
    settles: &'static str,
}

#[tokio::test]
async fn a_vpc_link_settles_failed_unless_elastic_load_balancing_holds_every_target() {
    let router = build_router();

    // The load balancers the rows resolve against, each created through Elastic Load
    // Balancing's own API: three this account holds, and one it held and then deleted.
    let held_one = create_network_load_balancer(&router, REGION, "vpclink-held-one").await;
    let held_two = create_network_load_balancer(&router, REGION, "vpclink-held-two").await;
    let held_three = create_network_load_balancer(&router, REGION, "vpclink-held-three").await;
    let removed = create_network_load_balancer(&router, REGION, "vpclink-removed").await;
    delete_load_balancer(&router, REGION, &removed).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "one target, which this account holds".to_string(),
            targets: vec![held_one.clone()],
            settles: AVAILABLE,
        },
        Row {
            case: "three targets, every one of which this account holds".to_string(),
            targets: vec![held_one.clone(), held_two.clone(), held_three.clone()],
            settles: AVAILABLE,
        },
        Row {
            case: "one target, which no load balancer answers to".to_string(),
            targets: vec![unheld_target(REGION, "vpclink-absent")],
            settles: FAILED,
        },
        Row {
            case: "three targets, the LAST of which is unheld".to_string(),
            targets: vec![
                held_one.clone(),
                held_two.clone(),
                unheld_target(REGION, "vpclink-absent-last"),
            ],
            settles: FAILED,
        },
        Row {
            case: "three targets, the FIRST of which is unheld".to_string(),
            targets: vec![
                unheld_target(REGION, "vpclink-absent-first"),
                held_two.clone(),
                held_three.clone(),
            ],
            settles: FAILED,
        },
        Row {
            case: "one target, a load balancer the account has deleted".to_string(),
            targets: vec![removed.clone()],
            settles: FAILED,
        },
        Row {
            case: "one target naming another Region, where nothing holds it".to_string(),
            targets: vec![unheld_target(OTHER_REGION, "vpclink-elsewhere")],
            settles: FAILED,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case.as_str();
        let name = format!("resolution-link-{index}");
        let created = apigw(
            &router,
            "POST",
            "/vpclinks",
            Some(json!({ "name": name, "targetArns": row.targets })),
        )
        .await;
        assert_eq!(
            created.status, 202,
            "case {case:?}: expected 202 from CreateVpcLink, got {} with {}",
            created.status, created.body
        );
        // A create answers while the link is still being provisioned, whatever its targets:
        // which state it settles in is not a refusal of the call.
        assert_eq!(
            member(case, &created.body, "status"),
            PENDING,
            "case {case:?}: expected CreateVpcLink to report {PENDING}, got {}",
            created.body
        );
        let id = member(case, &created.body, "id");

        let read = settled(&router, case, &id).await;
        let status = member(case, &read, "status");
        assert_eq!(
            status, row.settles,
            "case {case:?}: expected the link to settle {}, got {read}",
            row.settles
        );
        let message = member(case, &read, "statusMessage");
        let expected = if row.settles == AVAILABLE {
            AVAILABLE_SENTENCE
        } else {
            NOT_FOUND_SENTENCE
        };
        assert_eq!(
            message, expected,
            "case {case:?}: expected the description {expected:?}, got {message:?}"
        );
        // The description is FIXED text: it carries nothing of the request or of the record.
        let mut carries_nothing_of = row.targets.clone();
        carries_nothing_of.push(name.clone());
        carries_nothing_of.push(id.clone());
        carries_nothing_of.push(REGION.to_string());
        carries_nothing_of.push(engine::emulator_account_id().to_string());
        for value in carries_nothing_of {
            assert!(
                !message.contains(value.as_str()),
                "case {case:?}: expected a fixed description, but {message:?} carries {value:?}"
            );
        }
        // The collection reader reports the same pair, so a client polling either is told
        // the same thing about the link.
        let listed = listed_link(&router, case, &id).await;
        assert_eq!(
            (
                listed.get("status").cloned(),
                listed.get("statusMessage").cloned()
            ),
            (read.get("status").cloned(), read.get("statusMessage").cloned()),
            "case {case:?}: expected GetVpcLinks to report the read's pair ({read}), got {listed}"
        );
        // The targets are stored as the request listed them, whichever way the link settled.
        assert_eq!(
            listed.get("targetArns").and_then(Value::as_array).cloned(),
            Some(
                row.targets
                    .iter()
                    .map(|target| Value::String(target.clone()))
                    .collect::<Vec<Value>>()
            ),
            "case {case:?}: expected the targets it was created with, got {listed}"
        );
    }
}

#[tokio::test]
async fn an_entry_that_is_no_arn_is_still_refused_by_the_create_itself() {
    let router = build_router();
    let held = create_network_load_balancer(&router, REGION, "vpclink-form-held").await;

    // The FORM question is answered synchronously, before anything is stored: an entry that
    // is no ARN names nothing at all, so the create is refused and no link is left to settle.
    // The rows span where the malformed entry sits among well-formed ones.
    let rows: Vec<(&str, Vec<String>, &str)> = vec![
        (
            "the only entry is no ARN",
            vec!["not-a-valid-nlb-arn".to_string()],
            "not-a-valid-nlb-arn",
        ),
        (
            "the second entry is no ARN",
            vec![held.clone(), "also-not-an-arn".to_string()],
            "also-not-an-arn",
        ),
    ];
    for (index, (case, targets, offending)) in rows.into_iter().enumerate() {
        let name = format!("form-link-{index}");
        let answer = apigw(
            &router,
            "POST",
            "/vpclinks",
            Some(json!({ "name": name, "targetArns": targets })),
        )
        .await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected 400 from CreateVpcLink, got {} with {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.body.get("__type").and_then(Value::as_str),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {}",
            answer.body
        );
        let message = member(case, &answer.body, "message");
        let expected = format!("Invalid ARN. ARN is not well formed {offending}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // Nothing was stored, so the account holds no link under that name.
        let page = apigw(&router, "GET", "/vpclinks?limit=500", None).await;
        let names: Vec<String> = page
            .body
            .get("item")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get("name").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            !names.contains(&name),
            "case {case:?}: a refused create must leave no link, got {names:?}"
        );
    }
}
