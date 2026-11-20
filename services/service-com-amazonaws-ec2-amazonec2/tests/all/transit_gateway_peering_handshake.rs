// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two-sided transit gateway peering handshake, driven over the wire
//! the service actually speaks — `ec2Query`, through the crate's shared [`super::ec2_query`]
//! harness.
//!
//! The rules under test:
//!
//! 1. A PEERING REQUEST RESOLVES ITS PEER IN THE ACCOUNT AND REGION THE REQUEST NAMES.
//!    `CreateTransitGatewayPeeringAttachment` takes `PeerTransitGatewayId`, `PeerAccountId`
//!    and `PeerRegion` beside the requester's own `TransitGatewayId`
//!    (`smithy_query(query="operation",operation="CreateTransitGatewayPeeringAttachment")`),
//!    and "the peer transit gateway can be in your account or a different Amazon Web Services
//!    account" in "the Region where the peer transit gateway is located" — so the peer is
//!    looked up where the request places it and NOT in the Region the call was signed for. A
//!    peer the request places where this service holds no such gateway is refused with the
//!    transit gateway's own `InvalidTransitGatewayID.NotFound`, and nothing is stored.
//! 2. ACCEPTING IS THE ACCEPTER'S CALL. `AcceptTransitGatewayPeeringAttachment` carries
//!    nothing but the attachment id and `DryRun`, so the side a request speaks for is decided
//!    from the stored attachment's two ends against the account and Region the request was
//!    signed for: a caller on the REQUESTER end is the source of the request and is refused
//!    400 `InvalidParameterValue` with `Cannot accept <attachment-id> as the source of the
//!    peering request.`, leaving the attachment in `pendingAcceptance`, while the accepter
//!    end's own Region moves it on towards `available`.
//! 3. REJECTING WITHDRAWS THE REQUEST ITSELF, so it is not one side's call: a rejection moves
//!    a waiting attachment into the transient state of a rejection with the clock owing it
//!    `rejected`, and an attachment the lifecycle has already moved on from — one that was
//!    accepted, or rejected already — admits neither decision any more, which is the
//!    `IncorrectState` every accepting and rejecting operation of this family answers.
//!
//! Each table row is a distinct input along what the rules generalise over — WHICH Region the
//! peer sits in against the Region the call is signed for, which end of the pair the accept
//! speaks for, and the way a peer fails to resolve (never issued anywhere, issued in another
//! Region than the one named, owned by another account) — so no row is the specification.
//! Every expected code and sentence is written out here, independently of the service.

use ec2::state::{
    INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_REJECTED, TRANSIT_GATEWAY_ATTACHMENT_STATE_REJECTING,
    TRANSIT_GATEWAY_STATE_AVAILABLE,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::rbin_retention_rules::poll_until;

/// The Region the requester end of every peering here is created in and signed for.
const HOME_REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// EC2's code for a parameter whose value the service cannot act on.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// EC2's code for a request the state of the named resource does not admit.
const INCORRECT_STATE: &str = "IncorrectState";

/// A well-formed transit gateway id no gateway of this account holds, in any Region: the mint
/// counts from one, so the all-zero id is never issued.
const UNISSUED_GATEWAY_ID: &str = "tgw-00000000000000000";

/// An account id that is not the one this emulator serves, so a peer placed in it names a
/// gateway this service holds none of.
const ANOTHER_ACCOUNT_ID: &str = "210987654321";

/// One `CreateTransitGateway` signed for `region`, polled until the clock has moved it to
/// `available` — which is the state a peering attachment can be created between gateways in.
async fn available_transit_gateway(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
) -> String {
    let (status, body) = call(router, region, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a transit gateway in {region}, got {status} \
         with body {body}"
    );
    let transit_gateway_id = element(&body, "transitGatewayId")
        .unwrap_or_else(|| panic!("case {case:?}: no transitGatewayId in {body}"));
    poll_until(case, "the transit gateway to become available", || {
        let transit_gateway_id = transit_gateway_id.clone();
        async move {
            let (status, body) = call(
                router,
                region,
                "DescribeTransitGateways",
                &indexed("TransitGatewayIds", &[transit_gateway_id.as_str()]),
            )
            .await;
            status == 200 && element(&body, "state").as_deref() == Some(TRANSIT_GATEWAY_STATE_AVAILABLE)
        }
    })
    .await;
    transit_gateway_id
}

/// The `CreateTransitGatewayPeeringAttachment` form fields for one peering request.
fn peering_params(
    transit_gateway_id: &str,
    peer_transit_gateway_id: &str,
    peer_account_id: &str,
    peer_region: &str,
) -> Vec<(String, String)> {
    vec![
        ("TransitGatewayId".to_string(), transit_gateway_id.to_string()),
        (
            "PeerTransitGatewayId".to_string(),
            peer_transit_gateway_id.to_string(),
        ),
        ("PeerAccountId".to_string(), peer_account_id.to_string()),
        ("PeerRegion".to_string(), peer_region.to_string()),
    ]
}

/// One peering request signed for `region`.
async fn request_peering(
    router: &MultiServiceRouter,
    region: &str,
    params: &[(String, String)],
) -> (u16, String) {
    call(
        router,
        region,
        "CreateTransitGatewayPeeringAttachment",
        params,
    )
    .await
}

/// How many peering attachments the store holds, read back through the describe that reports
/// them rather than out of the store.
async fn reported_peerings(case: &str, router: &MultiServiceRouter) -> usize {
    let (status, body) = call(
        router,
        HOME_REGION,
        "DescribeTransitGatewayPeeringAttachments",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 describing the peering attachments, got {status} with \
         body {body}"
    );
    super::ec2_query::elements(&body, "transitGatewayAttachmentId").len()
}

/// The state `DescribeTransitGatewayPeeringAttachments` reports for `attachment_id`.
async fn reported_state(case: &str, router: &MultiServiceRouter, attachment_id: &str) -> String {
    let (status, body) = call(
        router,
        HOME_REGION,
        "DescribeTransitGatewayPeeringAttachments",
        &indexed("TransitGatewayAttachmentIds", &[attachment_id]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 describing {attachment_id}, got {status} with body \
         {body}"
    );
    element(&body, "state").unwrap_or_else(|| panic!("case {case:?}: no state in {body}"))
}

/// Look again until the attachment `attachment_id` reports `wanted`.
async fn poll_until_attachment_state(
    case: &str,
    router: &MultiServiceRouter,
    attachment_id: &str,
    wanted: &str,
) {
    poll_until(
        case,
        "the attachment to reach the state the clock owes it",
        || async move { reported_state(case, router, attachment_id).await == wanted },
    )
    .await;
}

/// RULE 1: a peer the request places in ANOTHER Region, in this account, is peered with — and
/// the attachment reports that Region as the accepter end's own.
#[tokio::test]
async fn a_peer_in_the_region_the_request_names_is_peered_with() {
    let (router, _engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id().to_string();

    // One row per Region the peer sits in, against the one Region every call is signed for:
    // the caller's own Region, another Region of the partition, and a third one, so what the
    // rule generalises over — WHERE the peer is — is what differs between rows.
    let peer_regions = ["us-east-1", "us-west-2", "eu-central-1"];

    let requester = available_transit_gateway("the requester", &router, HOME_REGION).await;
    let mut served = 0;
    for peer_region in peer_regions {
        let case = format!("a peer in {peer_region}");
        let peer = available_transit_gateway(&case, &router, peer_region).await;

        let (status, body) = request_peering(
            &router,
            HOME_REGION,
            &peering_params(&requester, &peer, &account, peer_region),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let attachment_id = element(&body, "transitGatewayAttachmentId")
            .unwrap_or_else(|| panic!("case {case:?}: no transitGatewayAttachmentId in {body}"));
        served += 1;

        // The ends the attachment reports are the ones the request named: the accepter's
        // Region is `PeerRegion`, not the Region the call was signed for.
        let reported_regions = super::ec2_query::elements(&body, "region");
        assert!(
            reported_regions.contains(&peer_region.to_string()),
            "case {case:?}: expected {peer_region:?} among the ends' Regions, got \
             {reported_regions:?} in {body}"
        );
        assert!(
            reported_regions.contains(&HOME_REGION.to_string()),
            "case {case:?}: expected {HOME_REGION:?} among the ends' Regions, got \
             {reported_regions:?} in {body}"
        );
        let reported_peers = super::ec2_query::elements(&body, "transitGatewayId");
        assert!(
            reported_peers.contains(&peer) && reported_peers.contains(&requester),
            "case {case:?}: expected both {requester:?} and {peer:?} among the ends, got \
             {reported_peers:?} in {body}"
        );

        // The attachment the create stored is the attachment the describe reports, and the
        // clock owes it `pendingAcceptance`.
        poll_until_attachment_state(
            &case,
            &router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
        )
        .await;
        assert_eq!(
            reported_peerings(&case, &router).await,
            served,
            "case {case:?}: expected {served} stored peering attachments after the serves"
        );
    }
}

/// RULE 1: a peer the request places where this service holds no such gateway is refused with
/// the transit gateway's own not-found code, whichever way it fails to resolve, and stores
/// nothing.
#[tokio::test]
async fn a_peer_the_request_places_nowhere_is_refused_for_the_peer() {
    let (router, _engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id().to_string();

    let requester = available_transit_gateway("the requester", &router, HOME_REGION).await;
    let peer_elsewhere = available_transit_gateway("the peer", &router, "us-west-2").await;

    // `(case, the peer id, the account the request places it in, the Region it places it in)`:
    // each row fails to resolve for a DIFFERENT reason.
    let rows: [(&str, &str, &str, &str); 4] = [
        (
            "an id no gateway of any Region holds",
            UNISSUED_GATEWAY_ID,
            account.as_str(),
            HOME_REGION,
        ),
        (
            "a gateway placed in a Region other than the one it was created in",
            peer_elsewhere.as_str(),
            account.as_str(),
            "eu-central-1",
        ),
        (
            "a gateway placed in the caller's own Region, which it is not in",
            peer_elsewhere.as_str(),
            account.as_str(),
            HOME_REGION,
        ),
        (
            "a gateway placed in another account",
            peer_elsewhere.as_str(),
            ANOTHER_ACCOUNT_ID,
            "us-west-2",
        ),
    ];

    for (case, peer, peer_account_id, peer_region) in rows {
        let (status, body) = request_peering(
            &router,
            HOME_REGION,
            &peering_params(&requester, peer, peer_account_id, peer_region),
        )
        .await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {case:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            "case {case:?}: expected the code {INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND:?}, got \
             {code:?} in {body}"
        );
        let expected_message = format!("Transit Gateway {peer} was deleted or does not exist.");
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {case:?}: expected the message {expected_message:?}, got {message:?}"
        );
        assert_eq!(
            reported_peerings(case, &router).await,
            0,
            "case {case:?}: a refused request stores no peering attachment"
        );
    }
}

/// RULE 2: the requester end cannot accept its own peering request — in the same Region as the
/// peer and across Regions alike — and the attachment is left where it was; the accepter end's
/// Region accepts it.
#[tokio::test]
async fn only_the_accepter_end_accepts_a_peering_request() {
    let account = engine::emulator_account_id().to_string();

    // One row per placement of the accepter end against the requester's: the same Region as
    // the requester (where both ends are the caller's own side) and another Region (where only
    // one of them is).
    for peer_region in ["us-east-1", "us-west-2"] {
        let case = format!("an accepter in {peer_region}");
        let (router, _engine) = super::ec2_query::emulator();
        let requester = available_transit_gateway(&case, &router, HOME_REGION).await;
        let peer = available_transit_gateway(&case, &router, peer_region).await;

        let (status, body) = request_peering(
            &router,
            HOME_REGION,
            &peering_params(&requester, &peer, &account, peer_region),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 requesting the peering, got {status} with body \
             {body}"
        );
        let attachment_id = element(&body, "transitGatewayAttachmentId")
            .unwrap_or_else(|| panic!("case {case:?}: no transitGatewayAttachmentId in {body}"));
        poll_until_attachment_state(
            &case,
            &router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
        )
        .await;

        // The requester's own accept: refused as the SOURCE of the request, twice over, and
        // the attachment is still waiting afterwards.
        for attempt in 0..2 {
            let attempted = format!("{case}, attempt {attempt}");
            let (status, body) = call(
                &router,
                HOME_REGION,
                "AcceptTransitGatewayPeeringAttachment",
                &[(
                    "TransitGatewayAttachmentId".to_string(),
                    attachment_id.clone(),
                )],
            )
            .await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {attempted:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body \
                 {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, INVALID_PARAMETER_VALUE,
                "case {attempted:?}: expected the code {INVALID_PARAMETER_VALUE:?}, got {code:?} \
                 in {body}"
            );
            let expected_message =
                format!("Cannot accept {attachment_id} as the source of the peering request.");
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {attempted:?}: expected the message {expected_message:?}, got {message:?}"
            );
            let state = reported_state(&attempted, &router, &attachment_id).await;
            assert_eq!(
                state, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
                "case {attempted:?}: expected the refused accept to leave the attachment \
                 {TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE:?}, got {state:?}"
            );
        }

        // The accepter end's own Region: for the cross-Region row that side is NOT the
        // requester's, so the acceptance is served and the attachment moves into the
        // transient state of an acceptance. The same-Region row has both ends on the
        // requester's side, so every accept of it is the source's.
        let (status, body) = call(
            &router,
            peer_region,
            "AcceptTransitGatewayPeeringAttachment",
            &[(
                "TransitGatewayAttachmentId".to_string(),
                attachment_id.clone(),
            )],
        )
        .await;
        if peer_region == HOME_REGION {
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {case:?}: a same-Region peering has both ends on the caller's side, so \
                 the accept is the source's: expected HTTP {REFUSAL_STATUS}, got {status} with \
                 body {body}"
            );
            let state = reported_state(&case, &router, &attachment_id).await;
            assert_eq!(
                state, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
                "case {case:?}: expected the attachment still waiting, got {state:?}"
            );
        } else {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the accepter end's accept to be served, got {status} \
                 with body {body}"
            );
            let reported = element(&body, "state").unwrap_or_default();
            assert_eq!(
                reported, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
                "case {case:?}: expected the transient state of an acceptance \
                 {TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING:?}, got {reported:?} in {body}"
            );
            let state = reported_state(&case, &router, &attachment_id).await;
            assert_eq!(
                state, TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING,
                "case {case:?}: the store must hold what the acceptance reported, got {state:?}"
            );
        }
    }
}

/// RULE 3: a rejection withdraws the request — from the requester's own endpoint, because a
/// rejection is not one side's call — and an attachment the lifecycle has moved on from admits
/// no further decision, whichever decision it was moved by.
#[tokio::test]
async fn a_rejection_withdraws_a_waiting_peering_request() {
    let account = engine::emulator_account_id().to_string();

    // `(case, the Region the peer sits in, what decides the attachment first)`: the two
    // placements of the accepter end against the requester's, each driven through the decision
    // the row names, so neither the Region nor the decision is the specification.
    let rows: [(&str, &str, &str); 3] = [
        (
            "a same-Region peering, rejected",
            HOME_REGION,
            "RejectTransitGatewayPeeringAttachment",
        ),
        (
            "a cross-Region peering, rejected",
            "us-west-2",
            "RejectTransitGatewayPeeringAttachment",
        ),
        (
            "a cross-Region peering, accepted first",
            "us-west-2",
            "AcceptTransitGatewayPeeringAttachment",
        ),
    ];

    for (case, peer_region, first_decision) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let requester = available_transit_gateway(case, &router, HOME_REGION).await;
        let peer = available_transit_gateway(case, &router, peer_region).await;
        let (status, body) = request_peering(
            &router,
            HOME_REGION,
            &peering_params(&requester, &peer, &account, peer_region),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 requesting the peering, got {status} with body \
             {body}"
        );
        let attachment_id = element(&body, "transitGatewayAttachmentId")
            .unwrap_or_else(|| panic!("case {case:?}: no transitGatewayAttachmentId in {body}"));
        poll_until_attachment_state(
            case,
            &router,
            &attachment_id,
            TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING_ACCEPTANCE,
        )
        .await;

        // The first decision: a rejection is made from the REQUESTER's endpoint, an
        // acceptance from the accepter's.
        let deciding_region = if first_decision == "AcceptTransitGatewayPeeringAttachment" {
            peer_region
        } else {
            HOME_REGION
        };
        let (status, body) = call(
            &router,
            deciding_region,
            first_decision,
            &[(
                "TransitGatewayAttachmentId".to_string(),
                attachment_id.clone(),
            )],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {first_decision}, got {status} with body \
             {body}"
        );
        if first_decision == "RejectTransitGatewayPeeringAttachment" {
            let reported = element(&body, "state").unwrap_or_default();
            assert_eq!(
                reported, TRANSIT_GATEWAY_ATTACHMENT_STATE_REJECTING,
                "case {case:?}: expected the transient state of a rejection \
                 {TRANSIT_GATEWAY_ATTACHMENT_STATE_REJECTING:?}, got {reported:?} in {body}"
            );
            // The move the clock owes it, observed through the describe rather than slept for.
            poll_until_attachment_state(
                case,
                &router,
                &attachment_id,
                TRANSIT_GATEWAY_ATTACHMENT_STATE_REJECTED,
            )
            .await;
        }

        // Neither decision is admitted a second time: the attachment is no longer a request
        // waiting on one.
        for action in [
            "RejectTransitGatewayPeeringAttachment",
            "AcceptTransitGatewayPeeringAttachment",
        ] {
            let attempted = format!("{case}, then {action}");
            let (status, body) = call(
                &router,
                deciding_region,
                action,
                &[(
                    "TransitGatewayAttachmentId".to_string(),
                    attachment_id.clone(),
                )],
            )
            .await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {attempted:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body \
                 {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, INCORRECT_STATE,
                "case {attempted:?}: expected the code {INCORRECT_STATE:?}, got {code:?} in \
                 {body}"
            );
            let expected_message = format!("{attachment_id} is in invalid state");
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {attempted:?}: expected the message {expected_message:?}, got {message:?}"
            );
        }
    }
}
