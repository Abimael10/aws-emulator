// SPDX-License-Identifier: CC0-1.0

//! A Direct Connect gateway association is made on a gateway the ACCOUNT holds, and
//! it advertises that gateway's own network.
//!
//! RULE: `CreateDirectConnectGatewayAssociation` associates an existing Direct
//! Connect gateway with the virtual private gateway or transit gateway a request
//! names, resolved against Amazon EC2 — the service that OWNS those gateways and the
//! VPCs they attach — before anything is stored. The association is minted in
//! `associating`, carries the associated gateway's type, owner account and Region,
//! advertises for a virtual private gateway the CIDR blocks of the VPCs it is
//! attached to (and for a transit gateway the prefixes the request names), and is
//! refused with the modelled `DirectConnectClientException` (HTTP 400) — storing
//! nothing — when the Direct Connect gateway does not exist, when EC2 holds no such
//! gateway, when a virtual private gateway is attached to no VPC, or when that
//! gateway is already held by an association that has not fully disassociated.
//!
//! This suite lives beside the router because the gateways and the VPCs are EC2's:
//! every identifier an accepted request names is one EC2 minted on the SAME engine
//! that serves Direct Connect, so each advertised prefix is a block EC2 actually
//! holds rather than one this suite made up, and each refusal is grounded in what EC2
//! answered.
//!
//! Sources: the `CreateDirectConnectGatewayAssociation` documentation ("The virtual
//! private gateway must be attached to a VPC and must not be associated with another
//! Direct Connect gateway"), `DirectConnectGatewayAssociationState` for the four
//! states, `GatewayType` for the two kinds of associated gateway, and
//! `CreateDirectConnectGatewayAssociationRequest$addAllowedPrefixesToDirectConnectGateway`
//! ("This parameter is required when you create an association to a transit
//! gateway").

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_directconnect_overtureservice::state::gateway_associations::{
    ASSOCIATION_STATE_ASSOCIATED, ASSOCIATION_STATE_ASSOCIATING, ASSOCIATION_STATE_DISASSOCIATING,
    MAX_ALLOWED_PREFIXES_PER_ASSOCIATION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for: the Region whose gateways a request
/// is resolved against.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix of this service's `awsJson1_1` operations.
const TARGET_PREFIX: &str = "OvertureService";

/// The HTTP status `DirectConnectClientException` is answered with.
const CLIENT_ERROR: u16 = 400;

/// The bare shape name of the modelled client refusal.
const CLIENT_EXCEPTION: &str = "DirectConnectClientException";

/// How long one poll waits before looking again. Every state move below is owed by
/// the clock, so each loop waits for the CONDITION under a generous deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How many times a poll looks before giving up — a generous deadline for a move the
/// service owes in well under a second.
const POLL_ATTEMPTS: usize = 400;

/// One emulator carrying Amazon EC2 and Direct Connect over ONE shared engine, as the
/// production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_directconnect_overtureservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which
/// is what routes the request here; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One answer: its HTTP status and its body as JSON (`Value::Null` when it is not
/// JSON).
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The bare shape name of the awsJson error code, or `""` on success.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| self.body.get("Message").and_then(Value::as_str))
            .unwrap_or_default()
    }

    /// The `directConnectGatewayAssociation` a create or a delete answered with.
    fn association(&self) -> Value {
        self.body
            .get("directConnectGatewayAssociation")
            .cloned()
            .unwrap_or(Value::Null)
    }
}

/// Call one Direct Connect operation over its `awsJson1_1` wire.
async fn dx(router: &MultiServiceRouter, operation: &str, body: Value) -> Reply {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("directconnect.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("directconnect"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
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

/// A VPC of the account, with the IPv4 blocks EC2 reports for it in the order they
/// were associated.
struct Network {
    vpc_id: String,
    cidrs: Vec<String>,
}

/// Mint a VPC through EC2's own `CreateVpc`, plus one additional IPv4 block per entry
/// of `extra_cidrs` through `AssociateVpcCidrBlock`.
///
/// Every block comes back out of EC2's OWN answer, so the prefixes this suite expects
/// an association to advertise are the ones its owner reports.
async fn network(router: &MultiServiceRouter, cidr: &str, extra_cidrs: &[&str]) -> Network {
    let created = ec2(router, "CreateVpc", &[("CidrBlock", cidr)]).await;
    let vpc_id = element(&created, "vpcId");
    let mut cidrs = vec![element(&created, "cidrBlock")];
    for extra in extra_cidrs {
        let associated = ec2(
            router,
            "AssociateVpcCidrBlock",
            &[("VpcId", vpc_id.as_str()), ("CidrBlock", extra)],
        )
        .await;
        cidrs.push(element(&associated, "cidrBlock"));
    }
    Network { vpc_id, cidrs }
}

/// Mint a virtual private gateway through EC2 and attach it to `vpc_id`.
async fn attached_virtual_gateway(router: &MultiServiceRouter, vpc_id: &str) -> String {
    let gateway_id = element(
        &ec2(router, "CreateVpnGateway", &[("Type", "ipsec.1")]).await,
        "vpnGatewayId",
    );
    ec2(
        router,
        "AttachVpnGateway",
        &[("VpnGatewayId", gateway_id.as_str()), ("VpcId", vpc_id)],
    )
    .await;
    gateway_id
}

/// Mint a Direct Connect gateway and answer its id.
async fn direct_connect_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let created = dx(
        router,
        "CreateDirectConnectGateway",
        json!({ "directConnectGatewayName": name }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateDirectConnectGateway({name}) must succeed, got {} with {}",
        created.status, created.body
    );
    created
        .body
        .get("directConnectGateway")
        .and_then(|gateway| gateway.get("directConnectGatewayId"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup: the create reported no gateway id: {}", created.body))
        .to_string()
}

/// The `allowedPrefixesToDirectConnectGateway` an association reports, as CIDR
/// strings in the order they are reported.
fn advertised(association: &Value) -> Vec<String> {
    association
        .get("allowedPrefixesToDirectConnectGateway")
        .and_then(Value::as_array)
        .map(|prefixes| {
            prefixes
                .iter()
                .map(|prefix| {
                    prefix
                        .get("cidr")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every association `DescribeDirectConnectGatewayAssociations` answers for `body`.
async fn described(router: &MultiServiceRouter, case: &str, body: Value) -> Vec<Value> {
    let read = dx(router, "DescribeDirectConnectGatewayAssociations", body).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: the describe must be served, got {} with {}",
        read.status, read.body
    );
    read.body
        .get("directConnectGatewayAssociations")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Poll the describe until `association_id` reports `state`, answering the record.
///
/// Waits for the CONDITION, never for a fixed time: each move is owed by the clock.
async fn await_state(
    router: &MultiServiceRouter,
    case: &str,
    association_id: &str,
    state: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLL_ATTEMPTS {
        let found = described(
            router,
            case,
            json!({ "associationId": association_id }),
        )
        .await;
        last = found.first().cloned().unwrap_or(Value::Null);
        if last.get("associationState").and_then(Value::as_str) == Some(state) {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the association {association_id} never reached {state}: {last}");
}

/// Poll the describe until `association_id` is reported no longer, which is what
/// ENDS an association: reaching the terminal `disassociated` state stops the traffic
/// and the association with it.
///
/// Waits for the CONDITION, never for a fixed time.
async fn await_gone(router: &MultiServiceRouter, case: &str, association_id: &str) {
    for _ in 0..POLL_ATTEMPTS {
        let found = described(router, case, json!({ "associationId": association_id })).await;
        if found.is_empty() {
            return;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the association {association_id} was still reported");
}

/// Poll the describe until `association_id` advertises exactly `expected`, answering
/// the record.
///
/// Waits for the CONDITION, never for a fixed time: what a virtual private gateway
/// association advertises is its attached VPCs' blocks as their owner reports them.
async fn await_advertised(
    router: &MultiServiceRouter,
    case: &str,
    association_id: &str,
    expected: &[String],
) -> Value {
    let mut last = Value::Null;
    for _ in 0..POLL_ATTEMPTS {
        let found = described(router, case, json!({ "associationId": association_id })).await;
        last = found.first().cloned().unwrap_or(Value::Null);
        if advertised(&last) == expected {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    panic!("case {case:?}: the association {association_id} never advertised {expected:?}: {last}");
}

// ---------------------------------------------------------------------------
// The accepted side of the rule, for a virtual private gateway: the association
// advertises the CIDR blocks of the VPCs its gateway attaches, reports that gateway's
// type, owner and Region, and reads back field for field.
//
// Rows differ along what the rule generalises over: the VALUE and SIZE of the VPC's
// block (each of the three private ranges, and a block narrower than a /16), and HOW
// MANY blocks the VPC carries.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_virtual_private_gateway_association_advertises_its_attached_vpc_blocks() {
    let router = emulator();

    // (case, the VPC's primary block, any further blocks associated with it)
    let rows: [(&str, &str, &[&str]); 5] = [
        ("a /16 of the 10.0.0.0/8 private range", "10.7.0.0/16", &[]),
        (
            "a /16 of the 172.16.0.0/12 private range",
            "172.20.0.0/16",
            &[],
        ),
        (
            "a /16 of the 192.168.0.0/16 private range",
            "192.168.0.0/16",
            &[],
        ),
        ("a block narrower than a /16", "10.8.4.0/24", &[]),
        (
            "a VPC carrying a second IPv4 block",
            "10.9.0.0/16",
            &["10.10.0.0/16"],
        ),
    ];

    let mut minted: Vec<(String, String)> = Vec::new();
    for (index, (case, cidr, extra)) in rows.iter().enumerate() {
        let vpc = network(&router, cidr, extra).await;
        let virtual_gateway_id = attached_virtual_gateway(&router, &vpc.vpc_id).await;
        let gateway_id = direct_connect_gateway(&router, &format!("dxgw-{index}")).await;

        let created = dx(
            &router,
            "CreateDirectConnectGatewayAssociation",
            json!({
                "directConnectGatewayId": gateway_id,
                "gatewayId": virtual_gateway_id,
            }),
        )
        .await;
        assert_eq!(
            (created.status, created.error_code().as_str()),
            (200, ""),
            "case {case:?}: an association on a gateway EC2 holds must be accepted, got {} with {}",
            created.status,
            created.body
        );
        let association = created.association();

        // "associating: The initial state after calling
        // CreateDirectConnectGatewayAssociation" — the span is never zero, so a
        // caller reading straight back still observes it.
        assert_eq!(
            association.get("associationState").and_then(Value::as_str),
            Some(ASSOCIATION_STATE_ASSOCIATING),
            "case {case:?}: expected {ASSOCIATION_STATE_ASSOCIATING}, got {association}"
        );
        // The prefixes advertised are the blocks EC2 itself reported for the VPC.
        assert_eq!(
            advertised(&association),
            vpc.cidrs,
            "case {case:?}: expected the attached VPC's own blocks {:?}, got {association}",
            vpc.cidrs
        );
        assert_eq!(
            association.get("associatedGateway").cloned(),
            Some(json!({
                "id": virtual_gateway_id,
                "type": "virtualPrivateGateway",
                "ownerAccount": engine::EMULATOR_ACCOUNT_ID,
                "region": REGION,
            })),
            "case {case:?}: expected the gateway EC2 holds, got {association}"
        );
        assert_eq!(
            association.get("virtualGatewayId").and_then(Value::as_str),
            Some(virtual_gateway_id.as_str()),
            "case {case:?}: expected the virtual private gateway spelling too, got {association}"
        );
        assert_eq!(
            association.get("virtualGatewayRegion").and_then(Value::as_str),
            Some(REGION),
            "case {case:?}: expected the gateway's Region, got {association}"
        );
        assert_eq!(
            association
                .get("virtualGatewayOwnerAccount")
                .and_then(Value::as_str),
            Some(engine::EMULATOR_ACCOUNT_ID),
            "case {case:?}: expected the gateway's owner, got {association}"
        );
        assert_eq!(
            association
                .get("directConnectGatewayId")
                .and_then(Value::as_str),
            Some(gateway_id.as_str()),
            "case {case:?}: expected the Direct Connect gateway named, got {association}"
        );
        assert_eq!(
            association
                .get("directConnectGatewayOwnerAccount")
                .and_then(Value::as_str),
            Some(engine::EMULATOR_ACCOUNT_ID),
            "case {case:?}: expected the Direct Connect gateway's owner, got {association}"
        );
        let association_id = association
            .get("associationId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: no associationId in {association}"))
            .to_string();

        // The round trip: the stored record reads back as created, bar the state the
        // clock owes it.
        let settled = await_state(&router, case, &association_id, ASSOCIATION_STATE_ASSOCIATED)
            .await;
        let mut expected = association.clone();
        expected
            .as_object_mut()
            .expect("an association is a JSON object")
            .insert(
                "associationState".to_string(),
                json!(ASSOCIATION_STATE_ASSOCIATED),
            );
        assert_eq!(
            settled, expected,
            "case {case:?}: the describe must render the stored record, got {settled}"
        );

        // Every selector the request models finds it, and each is an AND.
        for (selector, body) in [
            (
                "directConnectGatewayId",
                json!({ "directConnectGatewayId": gateway_id }),
            ),
            (
                "associatedGatewayId",
                json!({ "associatedGatewayId": virtual_gateway_id }),
            ),
            (
                "virtualGatewayId",
                json!({ "virtualGatewayId": virtual_gateway_id }),
            ),
            (
                "both the Direct Connect gateway and the gateway",
                json!({
                    "directConnectGatewayId": gateway_id,
                    "virtualGatewayId": virtual_gateway_id,
                }),
            ),
        ] {
            let found = described(&router, case, body).await;
            assert_eq!(
                found,
                vec![settled.clone()],
                "case {case:?}: selecting on {selector} must answer exactly this association, \
                 got {found:?}"
            );
        }

        minted.push((gateway_id.clone(), association_id));
    }

    // Every association is still held, each exactly once, read back on the Direct
    // Connect gateway it was made on — this listing publishes no "every association"
    // form, so each is selected by one of the selectors it does publish.
    let listed: Vec<String> = {
        let mut found_ids = Vec::new();
        for (gateway_id, association_id) in &minted {
            let found = described(
                &router,
                "every association",
                json!({ "directConnectGatewayId": gateway_id }),
            )
            .await;
            let ids: Vec<String> = found
                .iter()
                .map(|association| {
                    association
                        .get("associationId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string()
                })
                .collect();
            assert_eq!(
                ids,
                vec![association_id.clone()],
                "case \"every association\": the gateway {gateway_id} must hold exactly its own \
                 association, got {ids:?}"
            );
            found_ids.extend(ids);
        }
        found_ids
    };
    let expected: Vec<String> = minted
        .iter()
        .map(|(_, association_id)| association_id.clone())
        .collect();
    assert_eq!(
        listed, expected,
        "case \"every association\": expected each stored association once, got {listed:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule: what a virtual private gateway association advertises is its attached VPCs'
// CIDR blocks as the owner of those VPCs reports them NOW — not a snapshot taken when
// the association was made — so a block associated with the VPC afterwards is
// advertised too, and one disassociated from it stops being advertised.
//
// Rows differ along which block is added, and how many the VPC then carries.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_association_reflects_a_block_added_to_its_attached_vpc() {
    let router = emulator();
    let vpc = network(&router, "10.20.0.0/16", &[]).await;
    let virtual_gateway_id = attached_virtual_gateway(&router, &vpc.vpc_id).await;
    let gateway_id = direct_connect_gateway(&router, "reflects").await;

    let created = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": gateway_id,
            "virtualGatewayId": virtual_gateway_id,
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the association must be accepted, got {} with {}",
        created.status, created.body
    );
    let association = created.association();
    let association_id = association
        .get("associationId")
        .and_then(Value::as_str)
        .expect("setup: the create reports an associationId")
        .to_string();
    assert_eq!(
        advertised(&association),
        vpc.cidrs,
        "case \"at the create\": expected the VPC's only block, got {association}"
    );

    // Each row adds one more IPv4 block to the attached VPC through EC2's own
    // `AssociateVpcCidrBlock`, and the association must advertise every block the
    // owner then reports for it.
    let mut expected = vpc.cidrs.clone();
    for added in ["10.21.0.0/16", "10.22.4.0/24"] {
        let case = format!("after associating {added} with the attached VPC");
        let associated = ec2(
            &router,
            "AssociateVpcCidrBlock",
            &[("VpcId", vpc.vpc_id.as_str()), ("CidrBlock", added)],
        )
        .await;
        expected.push(element(&associated, "cidrBlock"));
        let reported = await_advertised(&router, &case, &association_id, &expected).await;
        assert_eq!(
            advertised(&reported),
            expected,
            "case {case:?}: expected every block the owner reports, got {reported}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule: the associated gateway "must not be associated with another Direct Connect
// gateway" — one association holds it until that association has fully
// disassociated, and only then may another be made.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_held_gateway_is_refused_until_its_association_has_disassociated() {
    let router = emulator();
    let vpc = network(&router, "10.11.0.0/16", &[]).await;
    let virtual_gateway_id = attached_virtual_gateway(&router, &vpc.vpc_id).await;
    let first_gateway = direct_connect_gateway(&router, "holder").await;
    let second_gateway = direct_connect_gateway(&router, "contender").await;

    let created = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": first_gateway,
            "gatewayId": virtual_gateway_id,
        }),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: the first association must be accepted, got {} with {}",
        created.status, created.body
    );
    let association_id = created
        .association()
        .get("associationId")
        .and_then(Value::as_str)
        .expect("setup: the create reports an associationId")
        .to_string();

    // Rows differ in WHICH Direct Connect gateway the second attempt is made on and
    // in WHICH member names the held gateway: both are refused while it is held.
    //
    // (case, the Direct Connect gateway, the request body)
    let attempts: [(&str, &str, Value); 3] = [
        (
            "another Direct Connect gateway, by gatewayId",
            &second_gateway,
            json!({
                "directConnectGatewayId": second_gateway,
                "gatewayId": virtual_gateway_id,
            }),
        ),
        (
            "another Direct Connect gateway, by virtualGatewayId",
            &second_gateway,
            json!({
                "directConnectGatewayId": second_gateway,
                "virtualGatewayId": virtual_gateway_id,
            }),
        ),
        (
            "the SAME Direct Connect gateway again",
            &first_gateway,
            json!({
                "directConnectGatewayId": first_gateway,
                "gatewayId": virtual_gateway_id,
            }),
        ),
    ];

    for (case, _gateway, body) in &attempts {
        let refused = dx(
            &router,
            "CreateDirectConnectGatewayAssociation",
            body.clone(),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (CLIENT_ERROR, CLIENT_EXCEPTION),
            "case {case:?}: expected {CLIENT_ERROR} {CLIENT_EXCEPTION}, got {} with {}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(virtual_gateway_id.as_str()),
            "case {case:?}: the refusal must name the gateway that is already held, got {}",
            refused.body
        );
        // Nothing was stored: the gateway still holds exactly the one association.
        // This listing publishes no "every association" form, so the probe selects on
        // the gateway the attempts name.
        let held = described(
            &router,
            case,
            json!({ "associatedGatewayId": virtual_gateway_id }),
        )
        .await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: a refused create must store no association, got {held:?}"
        );
    }

    // The association is torn down. "disassociating: The initial state after calling
    // DeleteDirectConnectGatewayAssociation".
    let deleted = dx(
        &router,
        "DeleteDirectConnectGatewayAssociation",
        json!({ "associationId": association_id }),
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "case \"the delete\": expected 200, got {} with {}",
        deleted.status, deleted.body
    );
    assert_eq!(
        deleted
            .association()
            .get("associationState")
            .and_then(Value::as_str),
        Some(ASSOCIATION_STATE_DISASSOCIATING),
        "case \"the delete\": expected {ASSOCIATION_STATE_DISASSOCIATING}, got {}",
        deleted.body
    );

    // While it is still disassociating the gateway is STILL held: traffic stops only
    // once the disassociation has completed.
    let too_early = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": second_gateway,
            "gatewayId": virtual_gateway_id,
        }),
    )
    .await;
    assert_eq!(
        (too_early.status, too_early.error_code().as_str()),
        (CLIENT_ERROR, CLIENT_EXCEPTION),
        "case \"while disassociating\": expected {CLIENT_ERROR} {CLIENT_EXCEPTION}, got {} with {}",
        too_early.status,
        too_early.body
    );

    // Once the disassociation has completed the association is gone and the gateway
    // is free again.
    await_gone(
        &router,
        "the disassociation completes",
        &association_id,
    )
    .await;
    let again = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": second_gateway,
            "gatewayId": virtual_gateway_id,
        }),
    )
    .await;
    assert_eq!(
        (again.status, again.error_code().as_str()),
        (200, ""),
        "case \"after the disassociation\": the freed gateway must be associable, got {} with {}",
        again.status,
        again.body
    );
    let second_association = again.association();
    assert_eq!(
        advertised(&second_association),
        vpc.cidrs,
        "case \"after the disassociation\": expected the attached VPC's own blocks {:?}, got \
         {second_association}",
        vpc.cidrs
    );
    assert_eq!(
        second_association
            .get("directConnectGatewayId")
            .and_then(Value::as_str),
        Some(second_gateway.as_str()),
        "case \"after the disassociation\": expected the second Direct Connect gateway, got \
         {second_association}"
    );
    // The delete is the one thing that frees it: once the association has ended, its
    // id names nothing to delete any more.
    let twice = dx(
        &router,
        "DeleteDirectConnectGatewayAssociation",
        json!({ "associationId": association_id }),
    )
    .await;
    assert_eq!(
        (twice.status, twice.error_code().as_str()),
        (CLIENT_ERROR, CLIENT_EXCEPTION),
        "case \"deleting an association that has ended\": expected {CLIENT_ERROR} \
         {CLIENT_EXCEPTION}, got {} with {}",
        twice.status,
        twice.body
    );
}

// ---------------------------------------------------------------------------
// Rule: a transit gateway association advertises the prefixes the request names, and
// `addAllowedPrefixesToDirectConnectGateway` "is required when you create an
// association to a transit gateway".
//
// Rows differ along how many prefixes are named, up to and one past the ceiling this
// emulator stores per association.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_transit_gateway_association_advertises_the_prefixes_the_request_names() {
    let router = emulator();
    let transit_gateway_id = element(
        &ec2(&router, "CreateTransitGateway", &[]).await,
        "transitGatewayId",
    );
    let gateway_id = direct_connect_gateway(&router, "transit-holder").await;

    // A transit gateway association with no prefixes names nothing to advertise.
    let refused = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": gateway_id,
            "gatewayId": transit_gateway_id,
        }),
    )
    .await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (CLIENT_ERROR, CLIENT_EXCEPTION),
        "case \"no allowed prefixes\": expected {CLIENT_ERROR} {CLIENT_EXCEPTION}, got {} with {}",
        refused.status,
        refused.body
    );

    // One past the ceiling this emulator stores per association.
    let too_many: Vec<Value> = (0..=MAX_ALLOWED_PREFIXES_PER_ASSOCIATION)
        .map(|n| json!({ "cidr": format!("10.{}.{}.0/24", n / 256, n % 256) }))
        .collect();
    let over = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": gateway_id,
            "gatewayId": transit_gateway_id,
            "addAllowedPrefixesToDirectConnectGateway": too_many,
        }),
    )
    .await;
    assert_eq!(
        (over.status, over.error_code().as_str()),
        (CLIENT_ERROR, CLIENT_EXCEPTION),
        "case \"one prefix past the ceiling\": expected {CLIENT_ERROR} {CLIENT_EXCEPTION}, got {} \
         with {}",
        over.status,
        over.body
    );

    // Exactly the ceiling, which is accepted and advertised in full.
    let prefixes: Vec<String> = (0..MAX_ALLOWED_PREFIXES_PER_ASSOCIATION)
        .map(|n| format!("10.{}.{}.0/24", n / 256, n % 256))
        .collect();
    let created = dx(
        &router,
        "CreateDirectConnectGatewayAssociation",
        json!({
            "directConnectGatewayId": gateway_id,
            "gatewayId": transit_gateway_id,
            "addAllowedPrefixesToDirectConnectGateway": prefixes
                .iter()
                .map(|cidr| json!({ "cidr": cidr }))
                .collect::<Vec<Value>>(),
        }),
    )
    .await;
    assert_eq!(
        (created.status, created.error_code().as_str()),
        (200, ""),
        "case \"exactly the ceiling\": expected 200, got {} with {}",
        created.status,
        created.body
    );
    let association = created.association();
    assert_eq!(
        advertised(&association),
        prefixes,
        "case \"exactly the ceiling\": expected the prefixes the request named, got {association}"
    );
    assert_eq!(
        association.get("associatedGateway").cloned(),
        Some(json!({
            "id": transit_gateway_id,
            "type": "transitGateway",
            "ownerAccount": engine::EMULATOR_ACCOUNT_ID,
            "region": REGION,
        })),
        "case \"exactly the ceiling\": expected the transit gateway EC2 holds, got {association}"
    );
    // `virtualGatewayId` is the virtual-private-gateway spelling, so a transit
    // gateway association carries none.
    assert!(
        association
            .get("virtualGatewayId")
            .is_none_or(Value::is_null),
        "case \"exactly the ceiling\": a transit gateway association names no virtual private \
         gateway, got {association}"
    );
    let association_id = association
        .get("associationId")
        .and_then(Value::as_str)
        .expect("the create reports an associationId");
    await_state(
        &router,
        "exactly the ceiling",
        association_id,
        ASSOCIATION_STATE_ASSOCIATED,
    )
    .await;
}

// ---------------------------------------------------------------------------
// The refusal side: every way a request names a gateway the account cannot associate
// is HTTP 400 `DirectConnectClientException` and stores nothing.
//
// Rows differ in WHY: the Direct Connect gateway does not exist; the identifier names
// neither kind of gateway; EC2 holds no gateway of that id (each kind, in both of
// EC2's published id forms); EC2 holds the gateway but it attaches no VPC (never
// attached, detached again, or deleted); and the request names no gateway at all.
// One row names NEITHER an existing Direct Connect gateway nor a gateway EC2 holds,
// which pins WHICH of the two refusals is reported: the gateway to associate is
// resolved first, so that is the one the message names.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_gateway_the_account_cannot_associate_is_refused_and_stores_nothing() {
    let router = emulator();
    let vpc = network(&router, "10.12.0.0/16", &[]).await;
    let gateway_id = direct_connect_gateway(&router, "refusals").await;

    // A gateway EC2 holds that attaches no VPC at all.
    let unattached = element(
        &ec2(&router, "CreateVpnGateway", &[("Type", "ipsec.1")]).await,
        "vpnGatewayId",
    );
    // A gateway that WAS attached and has been detached again.
    let detached = attached_virtual_gateway(&router, &vpc.vpc_id).await;
    ec2(
        &router,
        "DetachVpnGateway",
        &[
            ("VpnGatewayId", detached.as_str()),
            ("VpcId", vpc.vpc_id.as_str()),
        ],
    )
    .await;
    // A gateway EC2 held and then deleted.
    let deleted = element(
        &ec2(&router, "CreateVpnGateway", &[("Type", "ipsec.1")]).await,
        "vpnGatewayId",
    );
    ec2(
        &router,
        "DeleteVpnGateway",
        &[("VpnGatewayId", deleted.as_str())],
    )
    .await;
    // A gateway EC2 holds, attached to a VPC of its own: associable, so the only thing
    // a request naming it can be refused for is what the request itself names.
    let second = network(&router, "10.13.0.0/16", &[]).await;
    let attached = attached_virtual_gateway(&router, &second.vpc_id).await;

    // (case, the request body, the text the refusal must name)
    let rows: Vec<(String, Value, String)> = vec![
        (
            "a Direct Connect gateway id naming nothing, with a gateway that IS \
             associable"
                .to_string(),
            json!({
                "directConnectGatewayId": "00000000-0000-4000-8000-000000000000",
                "gatewayId": attached.clone(),
            }),
            "00000000-0000-4000-8000-000000000000".to_string(),
        ),
        (
            "a Direct Connect gateway id naming nothing, beside a gateway EC2 does not \
             hold: the gateway to associate is resolved first"
                .to_string(),
            json!({
                "directConnectGatewayId": "00000000-0000-4000-8000-000000000000",
                "gatewayId": "vgw-0123456789abcdef0",
            }),
            "vgw-0123456789abcdef0".to_string(),
        ),
        (
            "an identifier that names neither kind of gateway".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "gatewayId": "potato" }),
            "potato".to_string(),
        ),
        (
            "a virtual private gateway id EC2 does not hold, in its 17-hex form".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "gatewayId": "vgw-0f1e2d3c4b5a69780" }),
            "vgw-0f1e2d3c4b5a69780".to_string(),
        ),
        (
            "a virtual private gateway id EC2 does not hold, in its 8-hex form".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "virtualGatewayId": "vgw-1a2b3c4d" }),
            "vgw-1a2b3c4d".to_string(),
        ),
        (
            "a transit gateway id EC2 does not hold".to_string(),
            json!({
                "directConnectGatewayId": gateway_id,
                "gatewayId": "tgw-0f1e2d3c4b5a69780",
                "addAllowedPrefixesToDirectConnectGateway": [{ "cidr": "10.0.0.0/16" }],
            }),
            "tgw-0f1e2d3c4b5a69780".to_string(),
        ),
        (
            "a gateway EC2 holds that attaches no VPC".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "gatewayId": unattached.clone() }),
            unattached.clone(),
        ),
        (
            "a gateway whose attachment has been detached".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "gatewayId": detached.clone() }),
            detached.clone(),
        ),
        (
            "a gateway EC2 has deleted".to_string(),
            json!({ "directConnectGatewayId": gateway_id, "gatewayId": deleted.clone() }),
            deleted.clone(),
        ),
        (
            "no gateway at all".to_string(),
            json!({ "directConnectGatewayId": gateway_id }),
            "virtualGatewayId".to_string(),
        ),
        (
            "gatewayId and virtualGatewayId naming different gateways".to_string(),
            json!({
                "directConnectGatewayId": gateway_id,
                "gatewayId": "vgw-1a2b3c4d",
                "virtualGatewayId": "vgw-0f1e2d3c4b5a69780",
            }),
            "virtualGatewayId".to_string(),
        ),
        (
            "prefixes named for a gateway whose prefixes are its attached VPCs' blocks"
                .to_string(),
            json!({
                "directConnectGatewayId": gateway_id,
                "gatewayId": attached.clone(),
                "addAllowedPrefixesToDirectConnectGateway": [{ "cidr": "10.0.0.0/16" }],
            }),
            "addAllowedPrefixesToDirectConnectGateway".to_string(),
        ),
    ];

    for (case, body, named) in &rows {
        let refused = dx(
            &router,
            "CreateDirectConnectGatewayAssociation",
            body.clone(),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (CLIENT_ERROR, CLIENT_EXCEPTION),
            "case {case:?}: expected {CLIENT_ERROR} {CLIENT_EXCEPTION}, got {} with {}",
            refused.status,
            refused.body
        );
        assert!(
            refused.message().contains(named.as_str()),
            "case {case:?}: expected the refusal to name {named:?}, got {}",
            refused.body
        );
        // A refused create leaves the account exactly as it was. This listing
        // publishes no "every association" form, so the probe selects on the Direct
        // Connect gateway the rows name.
        let held = described(
            &router,
            case,
            json!({ "directConnectGatewayId": gateway_id }),
        )
        .await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused create must store no association, got {held:?}"
        );
    }
}
