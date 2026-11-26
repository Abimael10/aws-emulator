// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the `VpcEndpointId` an operation takes as the SUBJECT of the call once
//! that endpoint no longer exists, driven over `ec2Query` (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test: an operation whose subject is a `VpcEndpointId` resolves that id
//! against the stored endpoints and refuses a well-formed id the account holds no endpoint for
//! — whether it was never issued or its endpoint has since been deleted — with HTTP 400,
//! `InvalidVpcEndpointId.NotFound` and the sentence "The Vpc Endpoint Id '<id>' does not
//! exist", whose only variable part is the id the request carried; while the same call on a
//! LIVE endpoint succeeds and persists what it changed, so `DescribeVpcEndpoints` reports it.
//!
//! The rows differ along what the rule generalises over: the endpoint's type, which member the
//! modify carries, how the id stopped resolving (never issued, or deleted), the id FORM (EC2's
//! short and long id shapes), and which operation resolves the id.

use std::time::Duration;

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, indexed, unescaped};
use super::wire::{Param, param};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The endpoint service the interface endpoints below are created to — one the Region offers
/// interface endpoints to.
const INTERFACE_SERVICE: &str = "com.amazonaws.us-east-1.logs";

/// The endpoint service the gateway endpoints below are created to — one the Region publishes a
/// prefix list for, which is what a gateway endpoint's route is destined for.
const GATEWAY_SERVICE: &str = "com.amazonaws.us-east-1.s3";

/// How long a poll waits for a move the clock owes, or for a deletion to become visible, before
/// giving up. Far past [`ec2::state::TRANSITION_DELAY`], so a loaded or slow host cannot fail an
/// assertion: each loop waits for the CONDITION and this is only its ceiling.
const POLL_DEADLINE: Duration = Duration::from_secs(30);

/// How long each turn of a poll loop waits before looking again.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A policy a request attaches to an endpoint: full access, as a client writes it.
const FULL_ACCESS_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"*","Resource":"*"}]}"#;

/// A narrower policy, so one row's document is not the other's.
const READ_ONLY_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"}]}"#;

/// The emulator the suite drives.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// One `action` call that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The text of every `<item>` of the first set element named `set`, read through the XML parser
/// so one set's items are never confused with another's.
fn set_items(case: &str, body: &str, set: &str) -> Vec<String> {
    let document = super::wire::document(case, body);
    document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == set)
        .map(|node| {
            node.children()
                .filter(|item| item.is_element() && item.tag_name().name() == "item")
                .map(|item| item.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The VPC, with one subnet per Availability Zone in `zones`, every endpoint is built in.
struct Network {
    vpc_id: String,
    /// The subnets, in the order their zones were given.
    subnets: Vec<String>,
}

/// Create the VPC and its subnets through the API, so every endpoint below is created against
/// resources this emulator actually stores.
async fn network(case: &str, router: &MultiServiceRouter, zones: &[&str]) -> Network {
    let body = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.103.0.0/16")]).await;
    let vpc_id = member(case, &body, "vpcId");
    let mut subnets = Vec::with_capacity(zones.len());
    for (index, zone) in zones.iter().enumerate() {
        let cidr_block = format!("10.103.{}.0/24", index + 1);
        let created = ok(
            case,
            router,
            "CreateSubnet",
            &[
                param("VpcId", &vpc_id),
                param("CidrBlock", &cidr_block),
                param("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(member(case, &created, "subnetId"));
    }
    Network { vpc_id, subnets }
}

/// The body `DescribeVpcEndpoints` answers for the one endpoint `endpoint_id`, with its status.
async fn describe(router: &MultiServiceRouter, endpoint_id: &str) -> (u16, String) {
    call(
        router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await
}

/// Wait until the endpoint reports `available`, and answer the body that reported it.
///
/// A poll for the CONDITION, not a wait for a span, so the assertion cannot depend on how fast
/// the host runs.
async fn available(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> String {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = describe(router, endpoint_id).await;
        if status == 200
            && element(&body, "state").as_deref() == Some(ec2::state::VPC_ENDPOINT_STATE_AVAILABLE)
        {
            return body;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the clock to owe {endpoint_id} its available state, got \
             HTTP {status} with body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Wait until `endpoint_id` is no longer describable as a live endpoint — the way a client
/// learns a deletion has taken effect: the describe refuses the id, or reports it `deleted`.
async fn gone(case: &str, router: &MultiServiceRouter, endpoint_id: &str) {
    let deadline = std::time::Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = describe(router, endpoint_id).await;
        let refused = status != 200;
        let reported_deleted = element(&body, "state").as_deref() == Some("deleted");
        if refused || reported_deleted {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected {endpoint_id} to stop being describable after its \
             deletion, got HTTP {status} with body {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Delete `endpoint_id` through `DeleteVpcEndpoints` and assert nothing came back unsuccessful,
/// then wait until the id has stopped resolving.
async fn delete(case: &str, router: &MultiServiceRouter, endpoint_id: &str) {
    let body = ok(
        case,
        router,
        "DeleteVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    assert!(
        set_items(case, &body, "unsuccessful").is_empty(),
        "case {case:?}: expected the delete of {endpoint_id} to report nothing unsuccessful, \
         got {body}"
    );
    gone(case, router, endpoint_id).await;
}

/// Assert `body` is the refusal AWS answers a `VpcEndpointId` the account holds no endpoint
/// for: HTTP 400, `InvalidVpcEndpointId.NotFound`, and the sentence naming the id the request
/// carried.
fn assert_not_found(case: &str, status: u16, body: &str, endpoint_id: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for {endpoint_id}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(body),
        ec2::state::INVALID_VPC_ENDPOINT_ID_NOT_FOUND,
        "case {case:?}: expected the code {expected} for {endpoint_id}, got {body}",
        expected = ec2::state::INVALID_VPC_ENDPOINT_ID_NOT_FOUND,
    );
    assert_eq!(
        error_message(body),
        format!("The Vpc Endpoint Id '{endpoint_id}' does not exist"),
        "case {case:?}: expected that refusal sentence for {endpoint_id}, got {body}"
    );
}

/// What the live modify of one row must leave visible to `DescribeVpcEndpoints`.
enum Persisted {
    /// `policyDocument` reads back as exactly this document.
    Policy(&'static str),
    /// `policyDocument` reads back as the document the create installed — what `ResetPolicy`
    /// puts back ("Specify true to reset the policy document to the default policy").
    PolicyAsCreated,
    /// `subnetIdSet` reads back as exactly these subnet ids, in this order.
    Subnets(Vec<String>),
}

/// One row: an endpoint to create, the modify to make on it, and what that modify must persist.
struct Row {
    case: &'static str,
    /// Extra `CreateVpcEndpoint` parameters beside the VPC: the type and the service.
    create: Vec<Param>,
    /// The `ModifyVpcEndpoint` parameters beside `VpcEndpointId`.
    modify: Vec<Param>,
    persisted: Persisted,
}

/// A modify is served on a live endpoint and refused once that endpoint has been deleted.
///
/// Every row drives the whole life of one endpoint: the modify succeeds with `Return` `true`
/// and the change is read back through `DescribeVpcEndpoints`; the endpoint is then deleted and
/// the SAME modify is refused with the endpoint-not-found class, so the refusal follows from
/// the id no longer resolving rather than from the request.
#[tokio::test]
async fn a_modify_is_served_live_and_refused_once_the_endpoint_is_deleted() {
    let router = emulator();
    let built = network("the lifecycle table", &router, &["us-east-1a", "us-east-1b"]).await;

    let rows = vec![
        Row {
            case: "a gateway endpoint whose policy the request replaces",
            create: vec![
                param("ServiceName", GATEWAY_SERVICE),
                param("VpcEndpointType", "Gateway"),
            ],
            modify: vec![param("PolicyDocument", FULL_ACCESS_POLICY)],
            persisted: Persisted::Policy(FULL_ACCESS_POLICY),
        },
        Row {
            case: "a gateway endpoint whose policy the request resets",
            create: vec![
                param("ServiceName", GATEWAY_SERVICE),
                param("VpcEndpointType", "Gateway"),
            ],
            modify: vec![param("ResetPolicy", "true")],
            persisted: Persisted::PolicyAsCreated,
        },
        Row {
            case: "an interface endpoint whose policy the request replaces",
            create: vec![
                param("ServiceName", INTERFACE_SERVICE),
                param("VpcEndpointType", "Interface"),
                // No private hosted zone: the rows are about the id and the modify, not about
                // which endpoint of the VPC intercepts the service's private DNS name.
                param("PrivateDnsEnabled", "false"),
                param("SubnetId.1", &built.subnets[0]),
            ],
            modify: vec![param("PolicyDocument", READ_ONLY_POLICY)],
            persisted: Persisted::Policy(READ_ONLY_POLICY),
        },
        Row {
            case: "an interface endpoint the request adds a subnet to",
            create: vec![
                param("ServiceName", INTERFACE_SERVICE),
                param("VpcEndpointType", "Interface"),
                param("PrivateDnsEnabled", "false"),
                param("SubnetId.1", &built.subnets[0]),
            ],
            modify: vec![param("AddSubnetId.1", &built.subnets[1])],
            persisted: Persisted::Subnets(vec![
                built.subnets[0].clone(),
                built.subnets[1].clone(),
            ]),
        },
    ];

    for row in rows {
        let case = row.case;
        let mut create = vec![param("VpcId", &built.vpc_id)];
        create.extend(row.create.iter().cloned());
        let created = ok(case, &router, "CreateVpcEndpoint", &create).await;
        let endpoint_id = member(case, &created, "vpcEndpointId");
        let as_created = available(case, &router, &endpoint_id).await;
        let created_policy = unescaped(&member(case, &as_created, "policyDocument"));

        // The live modify: 200 with `Return` `true` — "Returns true if the request succeeds"
        // (`ModifyVpcEndpointResult$Return`).
        let mut modify = vec![param("VpcEndpointId", &endpoint_id)];
        modify.extend(row.modify.iter().cloned());
        let modified = ok(case, &router, "ModifyVpcEndpoint", &modify).await;
        assert_eq!(
            element(&modified, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected <return>true</return>, got {modified}"
        );

        // What the modify changed is the record the store holds, read back once the endpoint is
        // in service again (adding a subnet stands up an endpoint network interface).
        let described = available(case, &router, &endpoint_id).await;
        match &row.persisted {
            Persisted::Policy(expected) => assert_eq!(
                unescaped(&member(case, &described, "policyDocument")),
                *expected,
                "case {case:?}: expected that policy document, got {described}"
            ),
            Persisted::PolicyAsCreated => assert_eq!(
                unescaped(&member(case, &described, "policyDocument")),
                created_policy,
                "case {case:?}: expected the policy the create installed, got {described}"
            ),
            Persisted::Subnets(expected) => assert_eq!(
                &set_items(case, &described, "subnetIdSet"),
                expected,
                "case {case:?}: expected those subnets, got {described}"
            ),
        }

        // The same call, once the endpoint is gone.
        delete(case, &router, &endpoint_id).await;
        let (status, refusal) = call(&router, "ModifyVpcEndpoint", &modify).await;
        assert_not_found(case, status, &refusal, &endpoint_id);
    }
}

/// The not-found refusal names the id the REQUEST carried, whatever form it takes and however
/// it came to resolve to nothing, and every operation that resolves a `VpcEndpointId` as the
/// subject of the call answers it the same way.
///
/// The id forms are EC2's two published id shapes, `^vpce-([0-9a-f]{8}|[0-9a-f]{17})$`
/// (`crate::state::prefixed_id_is_well_formed`, the reader every `vpce-` id goes through), plus
/// the id of an endpoint this test created and deleted — so the sentence's variable part is
/// shown to come from the request rather than from any one id.
#[tokio::test]
async fn the_not_found_refusal_names_the_id_the_request_carried() {
    let router = emulator();
    let built = network("the id table", &router, &["us-east-1a"]).await;

    // An endpoint created and then deleted: its id was issued by this emulator, so it is the
    // row that no "never issued" screen can answer.
    let deleted_id = {
        let created = ok(
            "a deleted endpoint's own id",
            &router,
            "CreateVpcEndpoint",
            &[
                param("VpcId", &built.vpc_id),
                param("ServiceName", GATEWAY_SERVICE),
                param("VpcEndpointType", "Gateway"),
            ],
        )
        .await;
        let endpoint_id = member("a deleted endpoint's own id", &created, "vpcEndpointId");
        available("a deleted endpoint's own id", &router, &endpoint_id).await;
        delete("a deleted endpoint's own id", &router, &endpoint_id).await;
        endpoint_id
    };

    let ids: Vec<(&str, String)> = vec![
        ("the short id form", "vpce-0a1b2c3d".to_string()),
        ("the long id form", "vpce-00112233445566778".to_string()),
        (
            "the long id form, all digits",
            "vpce-00000000000000000".to_string(),
        ),
        ("a deleted endpoint's own id", deleted_id),
    ];

    for (case, endpoint_id) in ids {
        // Every operation below takes the `VpcEndpointId` as the subject of the call, so each
        // one must resolve it and refuse an id the account holds no endpoint for identically.
        let calls: Vec<(&str, Vec<Param>)> = vec![
            (
                "ModifyVpcEndpoint",
                vec![
                    param("VpcEndpointId", &endpoint_id),
                    param("PolicyDocument", FULL_ACCESS_POLICY),
                ],
            ),
            (
                "ModifyVpcEndpoint",
                vec![
                    param("VpcEndpointId", &endpoint_id),
                    param("ResetPolicy", "true"),
                ],
            ),
            (
                "DescribeVpcEndpoints",
                indexed("VpcEndpointId", &[endpoint_id.as_str()]),
            ),
        ];
        for (action, params) in calls {
            let labelled = format!("{case} through {action}");
            let (status, body) = call(&router, action, &params).await;
            assert_not_found(&labelled, status, &body, &endpoint_id);
        }
    }
}
