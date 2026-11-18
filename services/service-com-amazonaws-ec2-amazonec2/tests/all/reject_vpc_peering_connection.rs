// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `RejectVpcPeeringConnection`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=RejectVpcPeeringConnection`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! * A rejection ANSWERS A REQUEST THAT IS STILL WAITING — "Rejects a VPC peering connection
//!   request. The VPC peering connection must be in the pending-acceptance state"
//!   (`RejectVpcPeeringConnection`): such a connection is written into the terminal `rejected`
//!   member of its own state catalog, with the sentence that state carries, no expiry left on
//!   a request nobody can accept any more and nothing owed to the clock, and the answer is
//!   `true`. `DescribeVpcPeeringConnections` reads every one of those facts back.
//! * Over the WHOLE modelled `VpcPeeringConnectionStateReasonCode` catalog, a request that is
//!   still waiting — the one on its way in included, so a rejection issued straight after the
//!   create is served — is refused by the accepter, and every other state is answered with the
//!   code EC2 words the peering lifecycle's refusals in, with nothing written.
//! * A rejection is the ACCEPTER's answer, so a connection whose accepter VPC belongs to
//!   another account, and one rejected outside the accepter's Region, are both refused and the
//!   request is left waiting.
//! * An id that is no peering connection id, one this account holds no connection for, and a
//!   dry run are each refused and write nothing.
//! * A rejected connection no longer peers its two VPCs: every route whose target it was reads
//!   back as a blackhole ("the `blackhole` state indicates that the route's target isn't
//!   available", `Route$State`) and the same pair may be peered again.
//!
//! Every row is a distinct input along what those rules generalise over: which member of the
//! state catalog the connection is in when the rejection arrives, where the accepter is and
//! whose it is, which form an unresolvable id takes, and whether a route points at the
//! rejected connection. Nothing waits a fixed time for a state the clock owes: the state the
//! rejection writes is read off the store it wrote it into, and a state the clock owes is
//! looked for AGAIN until the describe reports it or a deadline far past the service's own
//! transition span passes.

use std::time::{Duration, Instant};

use ec2::ops::types::{
    VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES, VpcPeeringConnectionStateReason,
};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_STATE_TRANSITION,
    INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED, INVALID_VPC_PEERING_CONNECTION_ID_NOT_FOUND,
    ROUTE_STATE_ACTIVE, ROUTE_STATE_BLACKHOLE, TRANSITION_DELAY,
    VPC_PEERING_CONNECTION_ALREADY_EXISTS, VPC_PEERING_CONNECTION_REJECTED_MESSAGE,
    VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE, VPC_PEERING_CONNECTION_STATE_REJECTED,
    vpc_peering_connection_awaits_acceptance,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for unless the row says otherwise: the requester VPC
/// is in the Region the request was made in.
const REGION: &str = "us-east-1";

/// A Region of the same partition that is NOT [`REGION`], for the accepter side of an
/// inter-Region request (`CreateVpcPeeringConnectionRequest$PeerRegion`).
const OTHER_REGION: &str = "us-west-2";

/// An account that is not the one this emulator serves, for a cross-account accepter — twelve
/// digits, as an account id is.
const OTHER_ACCOUNT: &str = "210987654321";

/// A well-formed VPC id of [`OTHER_ACCOUNT`], which this store therefore holds no VPC for.
const OTHER_ACCOUNT_VPC_ID: &str = "vpc-0a1b2c3d4e5f60718";

/// The ec2Query `Action` under test.
const ACTION: &str = "RejectVpcPeeringConnection";

/// The describe a rejection is read back through.
const DESCRIBE: &str = "DescribeVpcPeeringConnections";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The error code EC2 refuses a caller without the standing to answer a request with.
const OPERATION_NOT_PERMITTED: &str = "OperationNotPermitted";

/// The deadline the polls below give up at: a large multiple of the service's own transition
/// span, so an assertion is about the state the clock owes and never about how fast the host
/// runs.
fn move_deadline() -> Duration {
    TRANSITION_DELAY * 60
}

/// How long one look waits before looking again — a fraction of that span, so a poll inside
/// the deadline above costs many looks rather than one.
fn poll_interval() -> Duration {
    TRANSITION_DELAY / 10
}

/// The value `name` names in the modelled `VpcPeeringConnectionStateReasonCode` catalog
/// (`smithy_query(query="shape",shape="VpcPeeringConnectionStateReasonCode")`, re-exported
/// from `ec2::ops::types`), or a panic — so every state seeded or asserted on here comes from
/// the model rather than from this file.
fn status_code(name: &str) -> &'static str {
    VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES
        .iter()
        .find(|value| **value == name)
        .unwrap_or_else(|| panic!("{name:?} is no VpcPeeringConnectionStateReasonCode member"))
}

/// One successful call of `action` signed for `region`, as its body.
async fn ok_body(
    case: &str,
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    params: &[Param],
) -> String {
    let (status, body) = call(router, region, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// One VPC of the account, by id — so a peering names VPCs the account really has.
async fn create_vpc(router: &MultiServiceRouter, cidr_block: &str) -> String {
    let body = ok_body(
        cidr_block,
        router,
        REGION,
        "CreateVpc",
        &[param("CidrBlock", cidr_block)],
    )
    .await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// One peering request as the create minted it, by id.
async fn create_peering(router: &MultiServiceRouter, params: &[Param]) -> String {
    let body = ok_body(
        "CreateVpcPeeringConnection",
        router,
        REGION,
        "CreateVpcPeeringConnection",
        params,
    )
    .await;
    element(&body, "vpcPeeringConnectionId")
        .unwrap_or_else(|| panic!("CreateVpcPeeringConnection answered no id: {body}"))
}

/// One peering request between two fresh VPCs of this account, by id.
async fn peering_between_fresh_vpcs(
    router: &MultiServiceRouter,
    requester_cidr: &str,
    accepter_cidr: &str,
) -> String {
    let requester = create_vpc(router, requester_cidr).await;
    let accepter = create_vpc(router, accepter_cidr).await;
    create_peering(
        router,
        &[param("VpcId", &requester), param("PeerVpcId", &accepter)],
    )
    .await
}

/// One reported peering connection, read through the XML tree rather than by string surgery.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Reported {
    id: String,
    status_code: String,
    status_message: Option<String>,
    expiration_time: Option<String>,
}

/// The one connection `id` names, as the describe reports it.
async fn described(case: &str, router: &MultiServiceRouter, id: &str) -> Reported {
    let body = ok_body(
        case,
        router,
        REGION,
        DESCRIBE,
        &indexed("VpcPeeringConnectionId", &[id]),
    )
    .await;
    let parsed = document(case, &body);
    let items: Vec<Reported> = parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "vpcPeeringConnectionId").is_some())
        .map(|node| {
            let status = child(node, "status");
            Reported {
                id: child_text(node, "vpcPeeringConnectionId").unwrap_or_default(),
                status_code: status
                    .and_then(|status| child_text(status, "code"))
                    .unwrap_or_default(),
                status_message: status.and_then(|status| child_text(status, "message")),
                expiration_time: child_text(node, "expirationTime"),
            }
        })
        .collect();
    assert_eq!(
        items.len(),
        1,
        "case {case:?}: expected exactly one peering connection for {id}, got {items:?}"
    );
    items.into_iter().next().unwrap_or_default()
}

/// Look at the connection AGAIN until the describe reports it in `wanted`, or a deadline far
/// past the service's own transition span passes — the CONDITION is waited for, never a fixed
/// time, so nothing here depends on how fast the host runs.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    id: &str,
    wanted: &str,
) -> Reported {
    let deadline = Instant::now() + move_deadline();
    loop {
        let seen = described(case, router, id).await;
        if seen.status_code == wanted {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {id} to reach {wanted:?}, got {seen:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The status the STORE holds for `id`, as `(code, message)` — read straight off the record
/// the rejection wrote.
fn stored_status(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    id: &str,
) -> (String, Option<String>) {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    let record = guard
        .vpc_peering_connections
        .get(id)
        .unwrap_or_else(|| panic!("the store holds no peering connection {id}"));
    let status = record.view.status.clone().unwrap_or_default();
    (status.code.unwrap_or_default(), status.message)
}

/// Whether the store's record for `id` still carries an expiry, and the state the clock owes
/// it — both facts the modelled shape carries no member for that an answer to the request
/// settles.
fn stored_expiry_and_owed_move(
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    id: &str,
) -> (bool, Option<String>) {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    let record = guard
        .vpc_peering_connections
        .get(id)
        .unwrap_or_else(|| panic!("the store holds no peering connection {id}"));
    (
        record.view.expiration_time.is_some(),
        record.pending.as_ref().map(|owed| owed.to.clone()),
    )
}

/// Write `code` into the stored connection `id`, the way the operation that moves a connection
/// into that state would: seeding through the store is what lets the catalog table below cover
/// every member of the catalog, including the ones no implemented operation reaches.
fn seed_status(engine: &std::sync::Arc<engine::InMemoryEngine>, id: &str, code: &str) {
    let store = engine.state::<Ec2State>();
    let mut guard = store.lock();
    let record = guard
        .vpc_peering_connections
        .get_mut(id)
        .unwrap_or_else(|| panic!("the store holds no peering connection {id}"));
    record.view.status = Some(VpcPeeringConnectionStateReason {
        code: Some(code.to_string()),
        message: None,
    });
    record.pending = None;
}

/// One `RejectVpcPeeringConnection` call for `id`, signed for `region`, with `DryRun` when
/// asked for.
async fn reject_in(
    router: &MultiServiceRouter,
    region: &str,
    id: &str,
    dry_run: bool,
) -> (u16, String) {
    let mut params = vec![param("VpcPeeringConnectionId", id)];
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    call(router, region, ACTION, &params).await
}

/// One `RejectVpcPeeringConnection` call for `id`, signed for [`REGION`].
async fn reject(router: &MultiServiceRouter, id: &str) -> (u16, String) {
    reject_in(router, REGION, id, false).await
}

/// The `return` member of a rejection's answer.
fn returned(case: &str, body: &str) -> String {
    element(body, "return")
        .unwrap_or_else(|| panic!("case {case:?}: the rejection answered no return: {body}"))
}

// ---------------------------------------------------------------------------
// The rejection answers the stored request
// ---------------------------------------------------------------------------

/// One row of the rejection table: which pair of VPCs, where the accepter is, and how the
/// connection reaches the state the rejection arrives in.
struct RejectCase {
    name: &'static str,
    requester_cidr: &'static str,
    accepter_cidr: &'static str,
    /// The Region the request names for the accepter VPC, if any — and therefore the Region
    /// the rejection has to be made in.
    peer_region: Option<&'static str>,
    /// Whether the row waits for the state the clock owes a fresh request; `false` rejects the
    /// state the create itself answers, with nothing in between.
    awaited: bool,
}

/// Each rejection answers the request the store holds: the connection is reported `rejected`
/// with the sentence that state carries, it keeps no expiry — "the time that an UNACCEPTED VPC
/// peering connection will expire" (`VpcPeeringConnection$ExpirationTime`) — the clock is owed
/// nothing further, the answer is `true`, and `DescribeVpcPeeringConnections` reads all of it
/// back. The rows differ in which of the two waiting states the rejection arrives in and
/// whether the two sides are in one Region.
#[tokio::test]
async fn a_rejection_of_a_waiting_request_records_rejected_and_round_trips() {
    let (router, engine) = super::ec2_query::emulator();

    let cases = [
        RejectCase {
            name: "the state the create answers",
            requester_cidr: "10.1.0.0/16",
            accepter_cidr: "10.2.0.0/16",
            peer_region: None,
            awaited: false,
        },
        RejectCase {
            name: "a request waiting for the accepter",
            requester_cidr: "10.3.0.0/16",
            accepter_cidr: "10.4.0.0/24",
            peer_region: None,
            awaited: true,
        },
        RejectCase {
            name: "an inter-Region request, rejected in the accepter's Region",
            requester_cidr: "10.5.0.0/16",
            accepter_cidr: "10.6.0.0/16",
            peer_region: Some(OTHER_REGION),
            awaited: true,
        },
    ];

    for case in &cases {
        let name = case.name;
        let requester = create_vpc(&router, case.requester_cidr).await;
        let accepter = create_vpc(&router, case.accepter_cidr).await;
        let mut params = vec![
            param("VpcId", &requester),
            param("PeerVpcId", &accepter),
        ];
        if let Some(peer_region) = case.peer_region {
            params.push(param("PeerRegion", peer_region));
        }
        let id = create_peering(&router, &params).await;
        if case.awaited {
            poll_until_state(
                name,
                &router,
                &id,
                VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
            )
            .await;
        }

        let reject_region = case.peer_region.unwrap_or(REGION);
        let (status, body) = reject_in(&router, reject_region, &id, false).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the rejection, got {status} with body {body}"
        );
        assert_eq!(
            returned(name, &body),
            "true",
            "case {name:?}: expected the rejection to answer true, got {body}"
        );

        // What the rejection WROTE, read off the store it wrote it into.
        let (code, message) = stored_status(&engine, &id);
        assert_eq!(
            (code.as_str(), message.as_deref()),
            (
                VPC_PEERING_CONNECTION_STATE_REJECTED,
                Some(VPC_PEERING_CONNECTION_REJECTED_MESSAGE)
            ),
            "case {name:?}: expected the stored request refused, got {code:?} / {message:?}"
        );
        let (has_expiry, owed) = stored_expiry_and_owed_move(&engine, &id);
        assert!(
            !has_expiry,
            "case {name:?}: a request nothing can accept any more must carry no expiry"
        );
        assert_eq!(
            owed, None,
            "case {name:?}: a refused request is answered, so the clock owes it nothing, got \
             {owed:?}"
        );

        // The round trip: the describe reports exactly what the rejection recorded.
        let read_back = described(name, &router, &id).await;
        assert_eq!(
            read_back,
            Reported {
                id: id.clone(),
                status_code: VPC_PEERING_CONNECTION_STATE_REJECTED.to_string(),
                status_message: Some(VPC_PEERING_CONNECTION_REJECTED_MESSAGE.to_string()),
                expiration_time: None,
            },
            "case {name:?}: expected the describe to report the refused request, got \
             {read_back:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Which states admit a rejection, over the whole catalog
// ---------------------------------------------------------------------------

/// "The VPC peering connection must be in the pending-acceptance state"
/// (`RejectVpcPeeringConnection`). Every member of the modelled
/// `VpcPeeringConnectionStateReasonCode` catalog is a row: a request still WAITING — the one on
/// its way in included, which is the state the create's own answer reports — is refused by the
/// accepter, and every other state is answered with the code EC2 words the peering lifecycle's
/// refusals in, naming the connection and the state it IS in, with nothing written.
#[tokio::test]
async fn a_rejection_is_served_for_a_waiting_request_and_refused_for_every_other_state() {
    let (router, engine) = super::ec2_query::emulator();

    let mut served = 0;
    let mut refused = 0;
    for (index, code) in VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES
        .iter()
        .copied()
        .enumerate()
    {
        // One connection per row, so a row never sees what another row wrote.
        let id = peering_between_fresh_vpcs(
            &router,
            &format!("10.{}.0.0/16", 2 * index + 140),
            &format!("10.{}.0.0/16", 2 * index + 141),
        )
        .await;
        seed_status(&engine, &id, code);
        let before = described(code, &router, &id).await;
        let (status, body) = reject(&router, &id).await;

        if vpc_peering_connection_awaits_acceptance(Some(code)) {
            served += 1;
            assert_eq!(
                status, 200,
                "case {code:?}: expected HTTP 200, got {status} with body {body}"
            );
            assert_eq!(
                returned(code, &body),
                "true",
                "case {code:?}: expected the rejection to answer true, got {body}"
            );
            let (stored, _message) = stored_status(&engine, &id);
            assert_eq!(
                stored, VPC_PEERING_CONNECTION_STATE_REJECTED,
                "case {code:?}: a waiting request is recorded as refused, got {stored:?}"
            );
            continue;
        }

        refused += 1;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {code:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            INVALID_STATE_TRANSITION,
            "case {code:?}: expected the peering lifecycle's refusal code, got {body}"
        );
        let message = error_message(&body);
        assert!(
            message.contains(&id) && message.contains(code),
            "case {code:?}: expected the refusal to name {id} and the state, got {message:?}"
        );
        let after = described(code, &router, &id).await;
        assert_eq!(
            after, before,
            "case {code:?}: a refused rejection must leave the connection it found, got \
             {after:?}"
        );
    }
    assert!(
        served > 0 && refused > 0,
        "both halves of the rule need rows, got {served} served and {refused} refused"
    );
    assert_eq!(
        served + refused,
        VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES.len(),
        "every state of the catalog is a row"
    );
}

// ---------------------------------------------------------------------------
// Who may reject, and where
// ---------------------------------------------------------------------------

/// One row of the standing table: where the accepter VPC is and whose it is, where the
/// rejection is made from, and what the refusal has to name — `None` when the rejection is one
/// the operation permits.
struct StandingCase {
    name: &'static str,
    requester_cidr: &'static str,
    /// The accepter VPC's block when it is a VPC of this account; `None` for a VPC of
    /// [`OTHER_ACCOUNT`], which this store holds none of.
    accepter_cidr: Option<&'static str>,
    /// The Region the request names for the accepter VPC, if any.
    peer_region: Option<&'static str>,
    /// The Region the rejection is signed for.
    reject_region: &'static str,
    refused_naming: Option<&'static str>,
}

/// A rejection is the ANSWER the owner of the accepter VPC gives the request, so it takes the
/// same standing an acceptance takes: a connection whose accepter belongs to another account,
/// and one rejected outside the accepter's Region, are both refused with
/// `OperationNotPermitted` and left waiting, while the same request rejected where the accepter
/// is is answered.
#[tokio::test]
async fn a_rejection_from_the_wrong_account_or_region_is_refused() {
    assert_ne!(
        OTHER_ACCOUNT,
        engine::emulator_account_id(),
        "the cross-account row needs an account id this emulator does not serve"
    );
    let (router, engine) = super::ec2_query::emulator();

    for case in [
        // The accepter VPC belongs to another account, so no caller of this emulator owns it —
        // and the two sides are in one Region, so the Region is not what is being refused.
        StandingCase {
            name: "another account's accepter",
            requester_cidr: "10.30.0.0/16",
            accepter_cidr: None,
            peer_region: None,
            reject_region: REGION,
            refused_naming: Some(OTHER_ACCOUNT),
        },
        // The accepter VPC is in another Region, so a rejection made where the REQUESTER is
        // cannot answer it.
        StandingCase {
            name: "an inter-Region request rejected in the requester's Region",
            requester_cidr: "10.31.0.0/16",
            accepter_cidr: Some("10.32.0.0/16"),
            peer_region: Some(OTHER_REGION),
            reject_region: REGION,
            refused_naming: Some(OTHER_REGION),
        },
        // The same request, rejected where the accepter is: permitted.
        StandingCase {
            name: "an inter-Region request rejected in the accepter's Region",
            requester_cidr: "10.33.0.0/16",
            accepter_cidr: Some("10.34.0.0/16"),
            peer_region: Some(OTHER_REGION),
            reject_region: OTHER_REGION,
            refused_naming: None,
        },
        // A request whose two sides are in one Region, rejected somewhere else: the rule holds
        // in that direction too.
        StandingCase {
            name: "a same-Region request rejected in another Region",
            requester_cidr: "10.35.0.0/16",
            accepter_cidr: Some("10.36.0.0/16"),
            peer_region: None,
            reject_region: OTHER_REGION,
            refused_naming: Some(REGION),
        },
    ] {
        let name = case.name;
        let requester = create_vpc(&router, case.requester_cidr).await;
        let mut params = vec![param("VpcId", &requester)];
        match case.accepter_cidr {
            Some(accepter_cidr) => {
                let accepter = create_vpc(&router, accepter_cidr).await;
                params.push(param("PeerVpcId", &accepter));
            }
            None => {
                params.push(param("PeerVpcId", OTHER_ACCOUNT_VPC_ID));
                params.push(param("PeerOwnerId", OTHER_ACCOUNT));
            }
        }
        if let Some(peer_region) = case.peer_region {
            params.push(param("PeerRegion", peer_region));
        }
        let id = create_peering(&router, &params).await;
        poll_until_state(
            name,
            &router,
            &id,
            VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
        )
        .await;

        let (status, body) = reject_in(&router, case.reject_region, &id, false).await;
        let Some(refused_naming) = case.refused_naming else {
            assert_eq!(
                status, 200,
                "case {name:?}: expected HTTP 200, got {status} with body {body}"
            );
            let (stored, _message) = stored_status(&engine, &id);
            assert_eq!(
                stored, VPC_PEERING_CONNECTION_STATE_REJECTED,
                "case {name:?}: expected the request refused, got {stored:?}"
            );
            continue;
        };
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            OPERATION_NOT_PERMITTED,
            "case {name:?}: expected the standing refusal, got {body}"
        );
        let message = error_message(&body);
        assert!(
            message.contains(refused_naming) && message.contains(&id),
            "case {name:?}: expected the refusal to name {refused_naming} and the connection, \
             got {body}"
        );
        let (stored, _message) = stored_status(&engine, &id);
        assert_eq!(
            stored, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
            "case {name:?}: a refused rejection leaves the request waiting"
        );
    }
}

// ---------------------------------------------------------------------------
// What it refuses without writing anything
// ---------------------------------------------------------------------------

/// One row of the unresolvable-id table: the value the request names, and the code EC2 answers
/// it with.
struct UnresolvableCase {
    name: &'static str,
    id: &'static str,
    code: &'static str,
}

/// A rejection reports no refusal it did not record: a value that is no peering connection id
/// at all is answered as malformed BEFORE anything is looked up, and a well-formed id this
/// account holds no connection for with that resource's not-found code — and a dry run reports
/// PERMISSION and answers nothing: "if you have the required permissions, the error response is
/// DryRunOperation" (`$DryRun`).
#[tokio::test]
async fn an_unresolvable_id_or_a_dry_run_writes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    // One real waiting request, so a refusal is never just "the store is empty".
    let held = peering_between_fresh_vpcs(&router, "192.168.0.0/16", "192.169.0.0/16").await;
    let waiting = poll_until_state(
        "the held request",
        &router,
        &held,
        VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
    )
    .await;

    for case in [
        UnresolvableCase {
            name: "no id at all",
            id: "not-an-id",
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "the prefix alone",
            id: "pcx-",
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "digits that are not hexadecimal",
            id: "pcx-zzzzzzzz",
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "neither published digit count",
            id: "pcx-0a1b2c3",
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "upper case digits",
            id: "pcx-0A1B2C3D4E5F60718",
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "an id of another resource",
            id: OTHER_ACCOUNT_VPC_ID,
            code: INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED,
        },
        UnresolvableCase {
            name: "a well-formed short id nothing minted",
            id: "pcx-0a1b2c3d",
            code: INVALID_VPC_PEERING_CONNECTION_ID_NOT_FOUND,
        },
        UnresolvableCase {
            name: "a well-formed long id nothing minted",
            id: "pcx-00000000000000000",
            code: INVALID_VPC_PEERING_CONNECTION_ID_NOT_FOUND,
        },
    ] {
        let name = case.name;
        let (status, body) = reject(&router, case.id).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            case.code,
            "case {name:?}: expected {code}, got {body}",
            code = case.code
        );
        let message = error_message(&body);
        assert!(
            message.contains(case.id),
            "case {name:?}: expected the refusal to name {id:?}, got {message:?}",
            id = case.id
        );
    }

    // A dry run over a request that is waiting: reported, and the request still waiting
    // afterwards.
    let (status, body) = reject_in(&router, REGION, &held, true).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "the dry run: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "the dry run: expected the permitted-request code, got {body}"
    );
    let after = described("after the dry run", &router, &held).await;
    assert_eq!(
        after, waiting,
        "the dry run: a dry run answers nothing, so the request is as it was: {waiting:?}"
    );
    let (stored, _message) = stored_status(&engine, &held);
    assert_eq!(
        stored, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
        "the dry run: the stored request must be left waiting"
    );
}

// ---------------------------------------------------------------------------
// What follows a rejection
// ---------------------------------------------------------------------------

/// One row of the route table: whether the route points at the connection the rejection
/// answers.
struct RouteCase {
    name: &'static str,
    destination: &'static str,
    /// `true` for the route whose target is the rejected connection.
    targets_rejected: bool,
}

/// A route whose target is a rejected peering connection reads back as a blackhole — "the
/// `blackhole` state indicates that the route's target isn't available" (`Route$State`) — while
/// a route to a connection that is still waiting, and the table's own local route, are left
/// active.
#[tokio::test]
async fn a_rejected_connection_blackholes_the_routes_that_targeted_it() {
    let (router, _engine) = super::ec2_query::emulator();
    let requester = create_vpc(&router, "10.90.0.0/16").await;
    let accepter = create_vpc(&router, "10.91.0.0/16").await;
    let other = create_vpc(&router, "10.92.0.0/16").await;
    let rejected = create_peering(
        &router,
        &[param("VpcId", &requester), param("PeerVpcId", &accepter)],
    )
    .await;
    let kept = create_peering(
        &router,
        &[param("VpcId", &requester), param("PeerVpcId", &other)],
    )
    .await;

    let body = ok_body(
        "CreateRouteTable",
        &router,
        REGION,
        "CreateRouteTable",
        &[param("VpcId", &requester)],
    )
    .await;
    let route_table_id = element(&body, "routeTableId")
        .unwrap_or_else(|| panic!("CreateRouteTable answered no routeTableId: {body}"));

    let cases = [
        RouteCase {
            name: "the route to the rejected connection",
            destination: "10.91.0.0/16",
            targets_rejected: true,
        },
        RouteCase {
            name: "the route to the connection that stays",
            destination: "10.92.0.0/16",
            targets_rejected: false,
        },
    ];
    for case in &cases {
        let target = if case.targets_rejected {
            &rejected
        } else {
            &kept
        };
        ok_body(
            case.name,
            &router,
            REGION,
            "CreateRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", case.destination),
                param("VpcPeeringConnectionId", target),
            ],
        )
        .await;
        let before = route_state(
            "before the rejection",
            &router,
            &route_table_id,
            case.destination,
        )
        .await;
        assert_eq!(
            before.as_deref(),
            Some(ROUTE_STATE_ACTIVE),
            "case {name:?}: expected the route active before the rejection, got {before:?}",
            name = case.name
        );
    }

    let (status, body) = reject(&router, &rejected).await;
    assert_eq!(
        status, 200,
        "the rejection: expected HTTP 200, got {status} with body {body}"
    );

    for case in &cases {
        let expected = if case.targets_rejected {
            ROUTE_STATE_BLACKHOLE
        } else {
            ROUTE_STATE_ACTIVE
        };
        let seen = route_state(
            "after the rejection",
            &router,
            &route_table_id,
            case.destination,
        )
        .await;
        assert_eq!(
            seen.as_deref(),
            Some(expected),
            "case {name:?}: expected the route in {expected:?} after the rejection, got \
             {seen:?}",
            name = case.name
        );
    }

    // The table's own local route is about the VPC's address space rather than about any
    // peering, so a rejection leaves it alone.
    let local = route_state(
        "after the rejection",
        &router,
        &route_table_id,
        "10.90.0.0/16",
    )
    .await;
    assert_eq!(
        local.as_deref(),
        Some(ROUTE_STATE_ACTIVE),
        "the local route must be untouched by a peering rejection, got {local:?}"
    );
}

/// The state the table `route_table_id` reports for the route matching `destination`.
async fn route_state(
    case: &str,
    router: &MultiServiceRouter,
    route_table_id: &str,
    destination: &str,
) -> Option<String> {
    let body = ok_body(
        case,
        router,
        REGION,
        "DescribeRouteTables",
        &indexed("RouteTableId", &[route_table_id]),
    )
    .await;
    let parsed = document(case, &body);
    set_items(parsed.root_element(), "routeTableSet")
        .into_iter()
        .flat_map(|table| set_items(table, "routeSet"))
        .find(|route| child_text(*route, "destinationCidrBlock").as_deref() == Some(destination))
        .and_then(|route| child_text(route, "state"))
}

/// Two VPCs a rejected request no longer peers MAY BE PEERED AGAIN: the duplicate refusal is
/// about a connection that still holds the pair, so a rejection is what frees it.
#[tokio::test]
async fn the_pair_a_rejected_request_held_may_be_peered_again() {
    let (router, _engine) = super::ec2_query::emulator();
    let requester = create_vpc(&router, "10.100.0.0/16").await;
    let accepter = create_vpc(&router, "10.101.0.0/16").await;
    let pair = [param("VpcId", &requester), param("PeerVpcId", &accepter)];
    let first = create_peering(&router, &pair).await;

    // While the first request holds the pair, a second request for it is a duplicate.
    let (status, body) = call(&router, REGION, "CreateVpcPeeringConnection", &pair).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "a duplicate request: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        VPC_PEERING_CONNECTION_ALREADY_EXISTS,
        "a duplicate request: expected the already-exists code, got {body}"
    );

    let (status, body) = reject(&router, &first).await;
    assert_eq!(
        status, 200,
        "the rejection: expected HTTP 200, got {status} with body {body}"
    );

    let second = create_peering(&router, &pair).await;
    assert_ne!(
        second, first,
        "the pair must be peered by a NEW connection, got {second} again"
    );
    let reported = described("the new connection", &router, &second).await;
    assert_ne!(
        reported.status_code,
        status_code("rejected"),
        "the new request must not inherit the refused one's state, got {reported:?}"
    );
}
