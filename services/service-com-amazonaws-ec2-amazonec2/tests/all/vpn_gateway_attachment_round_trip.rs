// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the virtual private gateway ATTACHMENT a `AttachVpnGateway` records
//! and a `DetachVpnGateway` takes down, driven over the wire the service actually speaks —
//! `ec2Query`, through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. THE ATTACHMENT A CALL RECORDS IS THE ATTACHMENT THE OTHER CALL READS. "Any VPCs attached
//!    to the virtual private gateway" (`VpnGateway$VpcAttachments`) is where an attachment
//!    lives, so the entry `AttachVpnGateway` writes is the one `DescribeVpnGateways` reports
//!    and the one `DetachVpnGateway` acts on: the detach of the pair that was just attached is
//!    served, and the entry then reports the terminal `detached`.
//! 2. A DETACH ASKS FOR A STATE, SO ONE ALREADY REACHED IS AN ACCEPTED NO-OP. A repeat detach
//!    of the same pair is served and changes nothing, where a pair this gateway never attached
//!    is refused 400 `InvalidAttachment.NotFound` with
//!    `The vpnGateway '<vgw-id>' is not attached to the vpc '<vpc-id>'.` — the distinction a
//!    caller tearing a topology down depends on.
//! 3. A DETACHED ATTACHMENT ATTACHES NOTHING, so the same pair may be attached again, and the
//!    gateway then reports it attached once more.
//!
//! Each table row is a distinct input along what the rules generalise over — which pair of the
//! population is named (a pair that is attached, a pair neither end ever attached, a gateway
//! attached to ANOTHER VPC of the account, and a VPC attached by ANOTHER gateway), and how
//! many times the detach is repeated — so no row is the specification. The expected codes and
//! sentences are written out here, independently of the service.

use ec2::state::INVALID_ATTACHMENT_NOT_FOUND;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{child_text, document, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The `GatewayType` a virtual private gateway is created as — the one member of that enum
/// (`smithy_query(query="shape",shape="GatewayType")`).
const GATEWAY_TYPE: &str = "ipsec.1";

/// An attachment that is in force: the `AttachmentStatus` enum's `attached` member
/// (`smithy_query(query="shape",shape="AttachmentStatus")`).
const ATTACHED: &str = "attached";

/// An attachment a detach has taken down: that enum's `detached` member.
const DETACHED: &str = "detached";

/// A well-formed VPC id the account holds no VPC for: the mint counts from one, so the
/// all-zero id is never issued.
const UNISSUED_VPC_ID: &str = "vpc-00000000000000000";

/// One VPC created over the wire, by id.
async fn create_vpc(case: &str, router: &MultiServiceRouter, cidr: &str) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateVpc",
        &[("CidrBlock".to_string(), cidr.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a VPC, got {status} with body {body}"
    );
    element(&body, "vpcId").unwrap_or_else(|| panic!("case {case:?}: no vpcId in {body}"))
}

/// One virtual private gateway created over the wire, by id.
async fn create_vpn_gateway(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = call(
        router,
        REGION,
        "CreateVpnGateway",
        &[("Type".to_string(), GATEWAY_TYPE.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a virtual private gateway, got {status} with \
         body {body}"
    );
    element(&body, "vpnGatewayId").unwrap_or_else(|| panic!("case {case:?}: no vpnGatewayId in {body}"))
}

/// The `VpnGatewayId`/`VpcId` pair every attach and detach is called with.
fn pair(vpn_gateway_id: &str, vpc_id: &str) -> Vec<(String, String)> {
    vec![
        ("VpnGatewayId".to_string(), vpn_gateway_id.to_string()),
        ("VpcId".to_string(), vpc_id.to_string()),
    ]
}

/// One `AttachVpnGateway` call for the pair.
async fn attach(
    router: &MultiServiceRouter,
    vpn_gateway_id: &str,
    vpc_id: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        "AttachVpnGateway",
        &pair(vpn_gateway_id, vpc_id),
    )
    .await
}

/// One `DetachVpnGateway` call for the pair.
async fn detach(
    router: &MultiServiceRouter,
    vpn_gateway_id: &str,
    vpc_id: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        "DetachVpnGateway",
        &pair(vpn_gateway_id, vpc_id),
    )
    .await
}

/// What `DescribeVpnGateways` reports as `vpn_gateway_id`'s attachments: `(vpcId, state)` per
/// entry, in the order the gateway reports them.
async fn reported_attachments(
    case: &str,
    router: &MultiServiceRouter,
    vpn_gateway_id: &str,
) -> Vec<(String, String)> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeVpnGateways",
        &indexed("VpnGatewayId", &[vpn_gateway_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 describing {vpn_gateway_id}, got {status} with body \
         {body}"
    );
    let parsed = document(case, &body);
    let gateway = set_items(parsed.root_element(), "vpnGatewaySet")
        .into_iter()
        .find(|item| child_text(*item, "vpnGatewayId").as_deref() == Some(vpn_gateway_id))
        .unwrap_or_else(|| panic!("case {case:?}: {vpn_gateway_id} is not reported in {body}"));
    set_items(gateway, "attachments")
        .into_iter()
        .map(|item| {
            (
                child_text(item, "vpcId").unwrap_or_default(),
                child_text(item, "state").unwrap_or_default(),
            )
        })
        .collect()
}

/// RULES 1, 2 and 3: the attachment round trip, driven over a population of two gateways and
/// two VPCs so every row names a different pair.
#[tokio::test]
async fn a_vpn_gateway_attachment_round_trips_through_the_detach() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "the attachment round trip";

    let first_vpc = create_vpc(case, &router, "10.70.0.0/16").await;
    let second_vpc = create_vpc(case, &router, "10.71.0.0/16").await;
    let first_gateway = create_vpn_gateway(case, &router).await;
    let second_gateway = create_vpn_gateway(case, &router).await;

    // RULE 1: the attach of each pair is served, and the entry it wrote is what the describe
    // reports — one row per gateway, so the rule is not read off a single pair.
    for (gateway, vpc) in [
        (&first_gateway, &first_vpc),
        (&second_gateway, &second_vpc),
    ] {
        let (status, body) = attach(&router, gateway, vpc).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 attaching {gateway} to {vpc}, got {status} with \
             body {body}"
        );
        let reported = element(&body, "state").unwrap_or_default();
        assert_eq!(
            reported, ATTACHED,
            "case {case:?}: expected the attachment of {gateway} to {vpc} to report \
             {ATTACHED:?}, got {reported:?} in {body}"
        );
        let attachments = reported_attachments(case, &router, gateway).await;
        assert_eq!(
            attachments,
            vec![(vpc.to_string(), ATTACHED.to_string())],
            "case {case:?}: expected {gateway} to report its attachment of {vpc}, got \
             {attachments:?}"
        );
    }

    // RULE 1: the detach of the pair that was just attached is served, and the entry turns
    // terminal rather than vanishing, so a caller polling the gateway sees it is gone.
    let (status, body) = detach(&router, &first_gateway, &first_vpc).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 detaching {first_gateway} from {first_vpc}, got \
         {status} with body {body}"
    );
    let attachments = reported_attachments(case, &router, &first_gateway).await;
    assert_eq!(
        attachments,
        vec![(first_vpc.clone(), DETACHED.to_string())],
        "case {case:?}: expected the detached entry to be reported, got {attachments:?}"
    );

    // RULE 2: the repeat is an accepted no-op — twice over — and it changes nothing, where
    // the other gateway's own attachment is untouched by either call.
    for attempt in 0..2 {
        let attempted = format!("{case}, repeat {attempt}");
        let (status, body) = detach(&router, &first_gateway, &first_vpc).await;
        assert_eq!(
            status, 200,
            "case {attempted:?}: expected a repeat detach to be served, got {status} with body \
             {body}"
        );
        let attachments = reported_attachments(&attempted, &router, &first_gateway).await;
        assert_eq!(
            attachments,
            vec![(first_vpc.clone(), DETACHED.to_string())],
            "case {attempted:?}: expected the repeat to change nothing, got {attachments:?}"
        );
    }
    let untouched = reported_attachments(case, &router, &second_gateway).await;
    assert_eq!(
        untouched,
        vec![(second_vpc.clone(), ATTACHED.to_string())],
        "case {case:?}: expected {second_gateway} to keep its own attachment, got {untouched:?}"
    );

    // RULE 3: a detached attachment attaches nothing, so the same pair attaches again and the
    // gateway reports it in force once more.
    let (status, body) = attach(&router, &first_gateway, &first_vpc).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the pair to attach again, got {status} with body {body}"
    );
    let attachments = reported_attachments(case, &router, &first_gateway).await;
    assert!(
        attachments.contains(&(first_vpc.clone(), ATTACHED.to_string())),
        "case {case:?}: expected {first_vpc} attached again, got {attachments:?}"
    );
    let (status, body) = detach(&router, &first_gateway, &first_vpc).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the re-attached pair to detach, got {status} with body {body}"
    );
    let attachments = reported_attachments(case, &router, &first_gateway).await;
    assert!(
        attachments
            .iter()
            .all(|(vpc, state)| vpc != &first_vpc || state == DETACHED),
        "case {case:?}: expected no attachment of {first_vpc} in force, got {attachments:?}"
    );
}

/// RULE 2: a pair this gateway never attached is refused, whichever way the pair fails to name
/// an attachment of that gateway.
#[tokio::test]
async fn a_pair_that_was_never_attached_is_refused_for_the_attachment() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "a pair that was never attached";

    let attached_vpc = create_vpc(case, &router, "10.72.0.0/16").await;
    let other_vpc = create_vpc(case, &router, "10.73.0.0/16").await;
    let holder = create_vpn_gateway(case, &router).await;
    let bystander = create_vpn_gateway(case, &router).await;
    let (status, body) = attach(&router, &holder, &attached_vpc).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture's attachment must be made, got {status} with body {body}"
    );

    // `(case, the gateway, the VPC)`: each row names a pair that is no attachment of that
    // gateway, for a different reason.
    let rows: [(&str, &str, &str); 3] = [
        (
            "a gateway that attached another VPC of the account",
            holder.as_str(),
            other_vpc.as_str(),
        ),
        (
            "a gateway that attached nothing, named with the attached VPC",
            bystander.as_str(),
            attached_vpc.as_str(),
        ),
        (
            "a gateway that attached nothing, named with a VPC of the account",
            bystander.as_str(),
            other_vpc.as_str(),
        ),
    ];

    for (row, gateway, vpc) in rows {
        let (status, body) = detach(&router, gateway, vpc).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {row:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_ATTACHMENT_NOT_FOUND,
            "case {row:?}: expected the code {INVALID_ATTACHMENT_NOT_FOUND:?}, got {code:?} in \
             {body}"
        );
        let expected_message =
            format!("The vpnGateway '{gateway}' is not attached to the vpc '{vpc}'.");
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {row:?}: expected the message {expected_message:?}, got {message:?}"
        );
    }

    // A VPC the account does not hold at all is refused for the VPC rather than for the
    // attachment: the end that cannot be resolved is named first.
    let (status, body) = detach(&router, &holder, UNISSUED_VPC_ID).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "case {case:?}: expected HTTP {REFUSAL_STATUS} for a VPC of no account, got {status} \
         with body {body}"
    );
    let code = error_code(&body);
    assert_eq!(
        code, "InvalidVpcID.NotFound",
        "case {case:?}: expected the VPC's own not-found code, got {code:?} in {body}"
    );

    // And the fixture's attachment survived every refusal.
    let attachments = reported_attachments(case, &router, &holder).await;
    assert_eq!(
        attachments,
        vec![(attached_vpc.clone(), ATTACHED.to_string())],
        "case {case:?}: expected the refusals to change nothing, got {attachments:?}"
    );
}
