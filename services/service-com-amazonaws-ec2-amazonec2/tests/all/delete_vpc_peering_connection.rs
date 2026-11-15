// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `DeleteVpcPeeringConnection`, driven over the wire the service
//! actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose body
//! carries `Action=DeleteVpcPeeringConnection`, answered with XML (README.md /
//! OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a delete ACTS ON A CONNECTION THAT STILL PEERS ITS TWO VPCS — "either
//! the owner of the requester VPC or the owner of the accepter VPC can delete the VPC peering
//! connection if it's in the active state. The owner of the requester VPC can delete a VPC
//! peering connection in the pending-acceptance state. You cannot delete a VPC peering
//! connection that's in the failed or rejected state" (`DeleteVpcPeeringConnection`). Such a
//! connection is written into the transient `deleting` member of its own state catalog, with
//! the account that deleted it named in the status message, the clock owing it the terminal
//! `deleted`, and no expiry left on a connection nothing can accept any more; every route
//! whose target it was reads back as a blackhole ("the `blackhole` state indicates that the
//! route's target isn't available", `Route$State`); and the answer is `true`. Any other state
//! is refused with the code EC2 words the peering lifecycle's refusals in, an id the account
//! holds no connection for with that resource's `NotFound` code, and a value that is no
//! connection id at all with its `Malformed` half.
//!
//! Every table row is a distinct input along what that rule generalises over: which member of
//! the modelled `VpcPeeringConnectionStateReasonCode` catalog the connection is in when the
//! delete arrives (each deletable one, each one that admits no delete), which form an
//! unresolvable id takes, and whether a route table's route points at the deleted connection.
//!
//! Nothing here waits a fixed time for a state the clock owes: the state the delete writes is
//! read off the store the delete wrote it into, and the state the clock owes is looked for
//! AGAIN until the describe reports it or a deadline far past the service's own transition
//! span passes.

use std::time::{Duration, Instant};

use ec2::ops::types::{VPC_PEERING_CONNECTION_STATE_REASON_CODE_VALUES, VpcPeeringConnectionStateReason};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_STATE_TRANSITION,
    INVALID_VPC_PEERING_CONNECTION_ID_MALFORMED, INVALID_VPC_PEERING_CONNECTION_ID_NOT_FOUND,
    ROUTE_STATE_ACTIVE, ROUTE_STATE_BLACKHOLE, TRANSITION_DELAY,
    VPC_PEERING_CONNECTION_ALREADY_EXISTS, VPC_PEERING_CONNECTION_STATE_DELETED,
    VPC_PEERING_CONNECTION_STATE_DELETING, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE,
    vpc_peering_deleted_message,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, error_message, indexed};
use super::wire::{Param, child, child_text, document, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The ec2Query `Action` under test.
const ACTION: &str = "DeleteVpcPeeringConnection";

/// The describe the delete is read back through.
const DESCRIBE: &str = "DescribeVpcPeeringConnections";

/// The HTTP status EC2 answers a refused request with.
const REFUSAL_STATUS: u16 = 400;

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

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

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One successful call of `action`, as its body.
async fn ok_body(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
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
        "CreateVpc",
        &[param("CidrBlock", cidr_block)],
    )
    .await;
    element(&body, "vpcId").unwrap_or_else(|| panic!("CreateVpc answered no vpcId: {body}"))
}

/// One peering request between two VPCs of the account, by the id it was minted with.
async fn create_peering(router: &MultiServiceRouter, vpc_id: &str, peer_vpc_id: &str) -> String {
    let body = ok_body(
        "CreateVpcPeeringConnection",
        router,
        "CreateVpcPeeringConnection",
        &[param("VpcId", vpc_id), param("PeerVpcId", peer_vpc_id)],
    )
    .await;
    element(&body, "vpcPeeringConnectionId")
        .unwrap_or_else(|| panic!("CreateVpcPeeringConnection answered no id: {body}"))
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
/// the delete wrote, so what the delete wrote is asserted without racing the move the clock
/// owes it (a move is applied by the next call that reaches the store, never by a timer).
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
/// it — both facts the modelled shape carries no member for that a delete settles.
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

/// Write `code` into the stored connection `id`, the way the operation that moves a
/// connection into that state would: seeding through the store is what lets the tables below
/// cover every member of the catalog, including the ones no implemented operation reaches yet.
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
    // A connection in any state the tables seed is waiting for nobody, so it carries neither
    // an expiry nor a move the clock owes it.
    record.view.expiration_time = None;
    record.pending = None;
}

/// One `DeleteVpcPeeringConnection` call for `id`, with `DryRun` when asked for.
async fn delete(router: &MultiServiceRouter, id: &str, dry_run: bool) -> (u16, String) {
    let mut params = vec![param("VpcPeeringConnectionId", id)];
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    invoke(router, ACTION, &params).await
}

/// The `return` member of a delete's answer.
fn returned(case: &str, body: &str) -> String {
    element(body, "return")
        .unwrap_or_else(|| panic!("case {case:?}: the delete answered no return: {body}"))
}

/// One row of the success table: the state the connection is in when the delete arrives, and
/// whether the row has to get there through the clock rather than by seeding.
struct DeletableCase {
    name: &'static str,
    state: &'static str,
    /// `true` for the state the create itself answers, which is reached without seeding.
    as_created: bool,
    /// `true` for the state the clock owes a fresh request, which is waited for.
    awaited: bool,
}

/// A delete of a connection in any state that still peers its two VPCs writes the transient
/// `deleting`, names the account that deleted it, leaves no expiry, owes the clock `deleted`
/// and answers `true` — and the describe reports exactly that, then the terminal state.
#[tokio::test]
async fn a_delete_of_a_connection_that_still_peers_writes_deleting_and_is_owed_deleted() {
    let (router, engine) = super::ec2_query::emulator();
    let account = engine::emulator_account_id().to_string();

    let cases = [
        DeletableCase {
            name: "the state the create answers",
            state: status_code("initiating-request"),
            as_created: true,
            awaited: false,
        },
        DeletableCase {
            name: "waiting for the accepter",
            state: status_code("pending-acceptance"),
            as_created: false,
            awaited: true,
        },
        DeletableCase {
            name: "being set up after an acceptance",
            state: status_code("provisioning"),
            as_created: false,
            awaited: false,
        },
        DeletableCase {
            name: "carrying traffic",
            state: status_code("active"),
            as_created: false,
            awaited: false,
        },
    ];


    for (index, case) in cases.iter().enumerate() {
        let requester = create_vpc(&router, &format!("10.{}.0.0/16", 2 * index + 1)).await;
        let accepter = create_vpc(&router, &format!("10.{}.0.0/16", 2 * index + 2)).await;
        let id = create_peering(&router, &requester, &accepter).await;
        if case.awaited {
            poll_until_state(case.name, &router, &id, VPC_PEERING_CONNECTION_STATE_PENDING_ACCEPTANCE)
                .await;
        } else if !case.as_created {
            seed_status(&engine, &id, case.state);
        }
        let before = described(case.name, &router, &id).await;
        assert_eq!(
            before.status_code, case.state,
            "case {name:?}: expected the connection in {state:?} before the delete, got \
             {before:?}",
            name = case.name,
            state = case.state
        );

        let (status, body) = delete(&router, &id, false).await;
        assert_eq!(
            status, 200,
            "case {name:?}: expected HTTP 200 from the delete, got {status} with body {body}",
            name = case.name
        );
        assert_eq!(
            returned(case.name, &body),
            "true",
            "case {name:?}: expected the delete to answer true, got {body}",
            name = case.name
        );

        // What the delete WROTE, read off the store it wrote it into: the transient state, the
        // account that deleted it, no expiry left, and the terminal state owed to the clock.
        let (code, message) = stored_status(&engine, &id);
        assert_eq!(
            code, VPC_PEERING_CONNECTION_STATE_DELETING,
            "case {name:?}: expected the stored connection in {deleting:?}, got {code:?}",
            name = case.name,
            deleting = VPC_PEERING_CONNECTION_STATE_DELETING
        );
        assert_eq!(
            message.as_deref(),
            Some(vpc_peering_deleted_message(&account).as_str()),
            "case {name:?}: expected the status to name the account that deleted it, got \
             {message:?}",
            name = case.name
        );
        let (has_expiry, owed) = stored_expiry_and_owed_move(&engine, &id);
        assert!(
            !has_expiry,
            "case {name:?}: a connection nothing can accept any more must carry no expiry",
            name = case.name
        );
        assert_eq!(
            owed.as_deref(),
            Some(VPC_PEERING_CONNECTION_STATE_DELETED),
            "case {name:?}: expected the clock to owe the connection {deleted:?}, got {owed:?}",
            name = case.name,
            deleted = VPC_PEERING_CONNECTION_STATE_DELETED
        );

        // And what the clock owes, looked for until it holds: the connection is still
        // reported, in the terminal state, with the sentence the delete wrote kept.
        let settled =
            poll_until_state(case.name, &router, &id, VPC_PEERING_CONNECTION_STATE_DELETED).await;
        assert_eq!(
            settled.status_message.as_deref(),
            Some(vpc_peering_deleted_message(&account).as_str()),
            "case {name:?}: expected the terminal state to keep the delete's sentence, got \
             {settled:?}",
            name = case.name
        );
        assert_eq!(
            settled.expiration_time, None,
            "case {name:?}: expected no reported expiry, got {settled:?}",
            name = case.name
        );
        assert_eq!(
            settled.id, id,
            "case {name:?}: expected the same connection to be reported, got {settled:?}",
            name = case.name
        );
    }
}

/// One row of the refusal table: the state the connection is in, which admits no delete.
struct UndeletableCase {
    name: &'static str,
    state: &'static str,
}

/// A delete is refused for every state a connection no longer peers its two VPCs in — the
/// failed and rejected states the operation names, a request nobody answered in time, and a
/// connection a delete has already acted on — and the connection is left exactly as it was.
#[tokio::test]
async fn a_delete_of_a_connection_that_no_longer_peers_is_refused_and_writes_nothing() {
    let (router, engine) = super::ec2_query::emulator();

    let cases = [
        UndeletableCase {
            name: "a request that could not be set up",
            state: status_code("failed"),
        },
        UndeletableCase {
            name: "a request the accepter rejected",
            state: status_code("rejected"),
        },
        UndeletableCase {
            name: "a request nobody answered in time",
            state: status_code("expired"),
        },
        UndeletableCase {
            name: "a connection already on its way out",
            state: status_code("deleting"),
        },
        UndeletableCase {
            name: "a connection already deleted",
            state: status_code("deleted"),
        },
    ];


    for (index, case) in cases.iter().enumerate() {
        let requester = create_vpc(&router, &format!("172.{}.0.0/16", 2 * index + 16)).await;
        let accepter = create_vpc(&router, &format!("172.{}.0.0/16", 2 * index + 17)).await;
        let id = create_peering(&router, &requester, &accepter).await;
        seed_status(&engine, &id, case.state);

        let (status, body) = delete(&router, &id, false).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name:?}: expected HTTP {REFUSAL_STATUS} from the delete, got {status} with \
             body {body}",
            name = case.name
        );
        assert_eq!(
            error_code(&body),
            INVALID_STATE_TRANSITION,
            "case {name:?}: expected the peering lifecycle's refusal code, got {body}",
            name = case.name
        );
        let message = error_message(&body);
        assert!(
            message.contains(&id) && message.contains(case.state),
            "case {name:?}: expected the refusal to name {id} and {state:?}, got {message:?}",
            name = case.name,
            state = case.state
        );

        // Nothing was written: the connection is still in the state it was in.
        let (code, _message) = stored_status(&engine, &id);
        assert_eq!(
            code, case.state,
            "case {name:?}: a refused delete must write nothing, got {code:?}",
            name = case.name
        );
        let (_has_expiry, owed) = stored_expiry_and_owed_move(&engine, &id);
        assert_eq!(
            owed, None,
            "case {name:?}: a refused delete must owe the clock nothing, got {owed:?}",
            name = case.name
        );
    }
}

/// One row of the unresolvable-id table: the value the request names, and the code EC2
/// answers it with.
struct UnresolvableCase {
    name: &'static str,
    id: &'static str,
    code: &'static str,
}

/// A delete reports no deletion it did not perform: a value that is no peering connection id
/// at all is refused as malformed BEFORE anything is looked up, and a well-formed id this
/// account holds no connection for with that resource's not-found code.
#[tokio::test]
async fn a_delete_refuses_an_id_it_cannot_resolve() {
    let (router, engine) = super::ec2_query::emulator();
    // One real connection, so a refusal is never just "the store is empty".
    let requester = create_vpc(&router, "192.168.0.0/16").await;
    let accepter = create_vpc(&router, "192.169.0.0/16").await;
    let held = create_peering(&router, &requester, &accepter).await;

    let cases = vec![
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
            name: "an id of another resource",
            id: "vpc-0a1b2c3d4e5f60718",
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
    ];

    for case in &cases {
        let (status, body) = delete(&router, case.id, false).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {name:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}",
            name = case.name
        );
        assert_eq!(
            error_code(&body),
            case.code,
            "case {name:?}: expected {code}, got {body}",
            name = case.name,
            code = case.code
        );
        let message = error_message(&body);
        assert!(
            message.contains(case.id),
            "case {name:?}: expected the refusal to name {id:?}, got {message:?}",
            name = case.name,
            id = case.id
        );
    }

    // The connection the account does hold was not touched by any of those refusals.
    let (code, _message) = stored_status(&engine, &held);
    assert_ne!(
        code, VPC_PEERING_CONNECTION_STATE_DELETING,
        "a refused delete must not act on another connection: {held} is {code:?}"
    );
}

/// `DryRun` reports PERMISSION and deletes nothing: "if you have the required permissions,
/// the error response is DryRunOperation" (`$DryRun`).
#[tokio::test]
async fn a_dry_run_reports_permission_and_deletes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    let requester = create_vpc(&router, "10.80.0.0/16").await;
    let accepter = create_vpc(&router, "10.81.0.0/16").await;
    let id = create_peering(&router, &requester, &accepter).await;
    let active = status_code("active");
    seed_status(&engine, &id, active);

    let (status, body) = delete(&router, &id, true).await;
    assert_eq!(
        status, DRY_RUN_STATUS,
        "dry run: expected HTTP {DRY_RUN_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "DryRunOperation",
        "dry run: expected the permitted-request code, got {body}"
    );
    let (code, _message) = stored_status(&engine, &id);
    assert_eq!(
        code, active,
        "dry run: expected the connection left as it was, got {code:?}"
    );

    // A dry run of a request that would be REFUSED answers the refusal instead, so the flag
    // never reports permission for a connection no delete may act on.
    seed_status(&engine, &id, status_code("rejected"));
    let (status, body) = delete(&router, &id, true).await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "dry run of a rejected connection: expected HTTP {REFUSAL_STATUS}, got {status} with \
         body {body}"
    );
    assert_eq!(
        error_code(&body),
        INVALID_STATE_TRANSITION,
        "dry run of a rejected connection: expected the lifecycle refusal, got {body}"
    );
}

/// One row of the route table: whether the route points at the connection the delete takes.
struct RouteCase {
    name: &'static str,
    destination: &'static str,
    /// `true` for the route whose target is the deleted connection.
    targets_deleted: bool,
}

/// A route whose target is a deleted peering connection reads back as a blackhole — "the
/// `blackhole` state indicates that the route's target isn't available" (`Route$State`) —
/// while a route to a connection that is still there, and the table's own local route, are
/// left active.
#[tokio::test]
async fn a_deleted_connection_blackholes_the_routes_that_targeted_it() {
    let (router, _engine) = super::ec2_query::emulator();
    let requester = create_vpc(&router, "10.90.0.0/16").await;
    let accepter = create_vpc(&router, "10.91.0.0/16").await;
    let other = create_vpc(&router, "10.92.0.0/16").await;
    let deleted = create_peering(&router, &requester, &accepter).await;
    let kept = create_peering(&router, &requester, &other).await;

    let body = ok_body(
        "CreateRouteTable",
        &router,
        "CreateRouteTable",
        &[param("VpcId", &requester)],
    )
    .await;
    let route_table_id = element(&body, "routeTableId")
        .unwrap_or_else(|| panic!("CreateRouteTable answered no routeTableId: {body}"));

    let cases = vec![
        RouteCase {
            name: "the route to the deleted connection",
            destination: "10.91.0.0/16",
            targets_deleted: true,
        },
        RouteCase {
            name: "the route to the connection that stays",
            destination: "10.92.0.0/16",
            targets_deleted: false,
        },
    ];
    for case in &cases {
        let target = if case.targets_deleted { &deleted } else { &kept };
        ok_body(
            case.name,
            &router,
            "CreateRoute",
            &[
                param("RouteTableId", &route_table_id),
                param("DestinationCidrBlock", case.destination),
                param("VpcPeeringConnectionId", target),
            ],
        )
        .await;
    }

    for case in &cases {
        let before = route_state("before the delete", &router, &route_table_id, case.destination)
            .await;
        assert_eq!(
            before.as_deref(),
            Some(ROUTE_STATE_ACTIVE),
            "case {name:?}: expected the route active before the delete, got {before:?}",
            name = case.name
        );
    }

    let (status, body) = delete(&router, &deleted, false).await;
    assert_eq!(
        status, 200,
        "the delete: expected HTTP 200, got {status} with body {body}"
    );

    for case in &cases {
        let expected = if case.targets_deleted {
            ROUTE_STATE_BLACKHOLE
        } else {
            ROUTE_STATE_ACTIVE
        };
        let seen = route_state("after the delete", &router, &route_table_id, case.destination).await;
        assert_eq!(
            seen.as_deref(),
            Some(expected),
            "case {name:?}: expected the route in {expected:?} after the delete, got {seen:?}",
            name = case.name
        );
    }

    // The table's own local route is about the VPC's address space rather than about any
    // peering, so the delete leaves it alone.
    let local = route_state("after the delete", &router, &route_table_id, "10.90.0.0/16").await;
    assert_eq!(
        local.as_deref(),
        Some(ROUTE_STATE_ACTIVE),
        "the local route must be untouched by a peering delete, got {local:?}"
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

/// Two VPCs a deleted connection no longer peers MAY BE PEERED AGAIN: the duplicate refusal
/// is about a connection that still holds the pair, so a delete is what frees it.
#[tokio::test]
async fn the_pair_a_deleted_connection_held_may_be_peered_again() {
    let (router, _engine) = super::ec2_query::emulator();
    let requester = create_vpc(&router, "10.100.0.0/16").await;
    let accepter = create_vpc(&router, "10.101.0.0/16").await;
    let first = create_peering(&router, &requester, &accepter).await;

    // While the first connection holds the pair, a second request for it is a duplicate.
    let (status, body) = invoke(
        &router,
        "CreateVpcPeeringConnection",
        &[param("VpcId", &requester), param("PeerVpcId", &accepter)],
    )
    .await;
    assert_eq!(
        status, REFUSAL_STATUS,
        "a duplicate request: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        VPC_PEERING_CONNECTION_ALREADY_EXISTS,
        "a duplicate request: expected the already-exists code, got {body}"
    );

    let (status, body) = delete(&router, &first, false).await;
    assert_eq!(
        status, 200,
        "the delete: expected HTTP 200, got {status} with body {body}"
    );

    let second = create_peering(&router, &requester, &accepter).await;
    assert_ne!(
        second, first,
        "the pair must be peered by a NEW connection, got {second} again"
    );
    let reported = described("the new connection", &router, &second).await;
    assert_ne!(
        reported.status_code, VPC_PEERING_CONNECTION_STATE_DELETING,
        "the new connection must not inherit the deleted one's state, got {reported:?}"
    );
}
