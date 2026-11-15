// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `CreateVpnConcentrator`, driven over the wire the service actually
//! speaks: `ec2Query`, an `application/x-www-form-urlencoded` POST whose body carries
//! `Action=…`, answered with XML (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rules under test:
//!
//! 1. A CREATE MINTS THE CONCENTRATOR THE REQUEST DESCRIBES, ON THE GATEWAY IT NAMES: a
//!    freshly minted `VpnConcentratorId`, the transit gateway and type the request asked for,
//!    the tags its `TagSpecifications` carried, the non-terminal `pending` state the clock
//!    then owes `available`, and the transit gateway attachment the concentrator aggregates
//!    its VPN connections onto that gateway through — a REAL attachment of the one store every
//!    kind of attachment shares, so `DescribeTransitGatewayAttachments` reports it, under the
//!    `vpn-concentrator` resource type, as attaching the concentrator the create just made.
//! 2. THE CONCENTRATOR A CREATE MINTED IS THE ONE THE REST OF THE API RESOLVES: the id it
//!    answered is a concentrator `CreateVpnConnection` will associate a connection with and
//!    `DeleteVpnConcentrator` will take away, while an id no create minted is refused by both.
//! 3. A PARAMETER THE REQUEST DID NOT GIVE, AND A GATEWAY THE ACCOUNT DOES NOT HOLD, ARE
//!    REFUSED AND NOTHING IS WRITTEN: an absent or empty `@clientOptional` parameter is EC2's
//!    `MissingParameter` naming the parameter, and a `TransitGatewayId` that resolves to no
//!    gateway of the account is that resource's own `Invalid…ID.NotFound`, whatever form the
//!    id takes. The recorded case this issue was filed about — a request carrying no
//!    `TransitGatewayId` — is ONE row of that table.
//! 4. `DryRun` CHANGES NOTHING: a create that would have succeeded answers `DryRunOperation`
//!    and stores neither the concentrator nor its attachment, while a dry run naming a gateway
//!    nothing holds is still refused for the gateway.
//!
//! Each table row is a distinct input along what those rules generalise over: how many tags
//! the request carries, whether the gateway is already in service or still coming up, how many
//! concentrators the account already holds on that gateway, which parameter the request left
//! out, and which form an unresolvable gateway id takes.
//!
//! `DescribeVpnConcentrators` is still a scaffold under its own issue, so what a create wrote
//! is read back through the API by the operations that DO resolve a concentrator
//! (`CreateVpnConnection`, `DeleteVpnConcentrator`) and through
//! `DescribeTransitGatewayAttachments` for the attachment it minted, beside the store itself.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use ec2::ops::types::{VPN_CONCENTRATOR_TYPE_VALUES, VpnConcentrator};
use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, DRY_RUN_OPERATION,
    INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND, INVALID_VPN_CONCENTRATOR_ID_NOT_FOUND, MISSING_PARAMETER,
    TRANSIT_GATEWAY_ATTACHMENT_ID_PREFIX, TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPN_CONCENTRATOR,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE, TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING,
    TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING, TRANSITION_DELAY, VPN_CONCENTRATOR_ID_PREFIX,
    VPN_STATE_AVAILABLE, VPN_STATE_PENDING,
};

use super::ec2_query::{error_code, error_message};
use super::wire::{Param, child, child_text, document, is_long_id, param, set_items};

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The transit gateway state a gateway in service reports
/// (`smithy_query(query="shape",shape="TransitGatewayState")`'s `available` member), which the
/// rows that want a gateway ALREADY in service wait for.
const TRANSIT_GATEWAY_STATE_AVAILABLE: &str = "available";

/// How long a poll below looks for a clock-owed move before giving up: a generous multiple of
/// the service's own transition span, so what a row asserts is the state the service reached
/// and never how fast the host runs.
fn poll_deadline() -> Duration {
    TRANSITION_DELAY * 120
}

/// How long a poll waits before looking AGAIN. Inside the loop and far below any span under
/// test: what the loop waits for is the condition, so a turn of it only costs a look.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The only `VpnConcentratorType` the model publishes
/// (`smithy_query(query="shape",shape="VpnConcentratorType")`, as the generated
/// [`VPN_CONCENTRATOR_TYPE_VALUES`] catalog spells it).
fn concentrator_type() -> &'static str {
    VPN_CONCENTRATOR_TYPE_VALUES[0]
}

/// The code the REQUEST BOUNDARY answers a `@required` member the request left out
/// entirely, before this operation's handler runs — as every other operation of this service
/// answers it: EC2's own `MissingParameter`, which is also what the handler answers for a
/// member the request sent EMPTY (in its own sentence, below).
const BOUNDARY_MISSING_MEMBER: &str = "MissingParameter";

/// The code the REQUEST BOUNDARY answers a value outside what a member's modelled enum
/// publishes, before this operation's handler runs — as every other operation of this service
/// answers it.
const BOUNDARY_ENUM_VIOLATION: &str = "ValidationError";

/// One `action` call carrying `params`, as its HTTP status and XML body.
async fn call(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// The text of the first element named `name` anywhere in `body`.
fn first_text(case: &str, body: &str, name: &str) -> String {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == name)
        .and_then(|node| node.text().map(str::to_string))
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// A transit gateway of the account, as `CreateTransitGateway` makes one.
async fn transit_gateway(case: &str, router: &MultiServiceRouter) -> String {
    let (status, body) = call(router, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateTransitGateway, got {status} with body \
         {body}"
    );
    first_text(case, &body, "transitGatewayId")
}

/// The state `DescribeTransitGateways` reports for `transit_gateway_id` right now.
async fn described_gateway_state(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeTransitGateways",
        &[param("TransitGatewayIds.1", transit_gateway_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {transit_gateway_id} must succeed, got {status} with \
         body {body}"
    );
    first_text(case, &body, "state")
}

/// Look again until `DescribeTransitGateways` reports the gateway in service, or a deadline
/// far past the service's own transition span passes.
///
/// The CONDITION is waited for rather than a span slept out, so a row that wants a gateway
/// already in service gets one however fast the host runs.
async fn poll_until_gateway_available(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
) {
    let deadline = Instant::now() + poll_deadline();
    loop {
        let state = described_gateway_state(case, router, transit_gateway_id).await;
        if state == TRANSIT_GATEWAY_STATE_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {transit_gateway_id} to reach \
             {TRANSIT_GATEWAY_STATE_AVAILABLE}, last saw {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The form fields for the tags of the one tag specification a `CreateVpnConcentrator` may
/// carry — "the tags to apply to the VPN concentrator during creation"
/// (`CreateVpnConcentratorRequest$TagSpecifications`).
fn tag_params(tags: &[(&str, &str)]) -> Vec<Param> {
    if tags.is_empty() {
        return Vec::new();
    }
    let mut params = vec![param(
        "TagSpecification.1.ResourceType",
        ec2::state::RESOURCE_TYPE_VPN_CONCENTRATOR,
    )];
    for (index, (key, value)) in tags.iter().enumerate() {
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Key", index + 1),
            key,
        ));
        params.push(param(
            &format!("TagSpecification.1.Tag.{}.Value", index + 1),
            value,
        ));
    }
    params
}

/// One `CreateVpnConcentrator` call carrying `params` exactly as written.
async fn create(router: &MultiServiceRouter, params: &[Param]) -> (u16, String) {
    call(router, "CreateVpnConcentrator", params).await
}

/// One `CreateVpnConcentrator` of `transit_gateway_id` with `tags`, requiring success.
async fn create_ok(
    case: &str,
    router: &MultiServiceRouter,
    transit_gateway_id: &str,
    tags: &[(&str, &str)],
) -> Reported {
    let mut params = vec![
        param("Type", concentrator_type()),
        param("TransitGatewayId", transit_gateway_id),
    ];
    params.extend(tag_params(tags));
    let (status, body) = create(router, &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVpnConcentrator, got {status} with body \
         {body}"
    );
    reported(case, &body)
}

/// The concentrator a response carries, member by member.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Reported {
    /// `VpnConcentrator$VpnConcentratorId`.
    vpn_concentrator_id: String,
    /// `VpnConcentrator$State`.
    state: String,
    /// `VpnConcentrator$TransitGatewayId`.
    transit_gateway_id: Option<String>,
    /// `VpnConcentrator$TransitGatewayAttachmentId`.
    transit_gateway_attachment_id: Option<String>,
    /// `VpnConcentrator$Type`.
    concentrator_type: Option<String>,
    /// `VpnConcentrator$Tags`, as `(key, value)` pairs in the order they were reported.
    tags: Vec<(String, String)>,
}

/// The one concentrator `body` carries: the element that has a `vpnConcentratorId` child.
fn reported(case: &str, body: &str) -> Reported {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "vpnConcentratorId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no VPN concentrator in {body}"));
    Reported {
        vpn_concentrator_id: child_text(node, "vpnConcentratorId").unwrap_or_default(),
        state: child_text(node, "state").unwrap_or_default(),
        transit_gateway_id: child_text(node, "transitGatewayId"),
        transit_gateway_attachment_id: child_text(node, "transitGatewayAttachmentId"),
        concentrator_type: child_text(node, "type"),
        tags: set_items(node, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect(),
    }
}

/// The concentrator the store holds for `vpn_concentrator_id`, read without going through an
/// operation — `None` when the store holds no such concentrator.
fn stored(engine: &engine::InMemoryEngine, vpn_concentrator_id: &str) -> Option<VpnConcentrator> {
    engine
        .state::<Ec2State>()
        .lock()
        .vpn_concentrator(vpn_concentrator_id)
        .cloned()
}

/// How many concentrators the store holds, so a refusal can be shown to have written none.
fn stored_concentrators(engine: &engine::InMemoryEngine) -> usize {
    engine.state::<Ec2State>().lock().vpn_concentrators.len()
}

/// How many transit gateway attachments the store holds, so a refusal can be shown to have
/// minted no attachment either.
fn stored_attachments(engine: &engine::InMemoryEngine) -> usize {
    engine
        .state::<Ec2State>()
        .lock()
        .transit_gateway_attachments
        .len()
}

/// The state the store holds for `vpn_concentrator_id`, or `None` when it holds none.
fn stored_state(engine: &engine::InMemoryEngine, vpn_concentrator_id: &str) -> Option<String> {
    stored(engine, vpn_concentrator_id).and_then(|view| view.state)
}

/// Look again until the store reports `vpn_concentrator_id` in the state `wanted`, or a
/// deadline far past the service's own transition span passes. Answers the last state seen.
///
/// The move the clock owes a `pending` concentrator is recorded rather than slept for, so the
/// CONDITION is polled through a read of the service's own state.
async fn poll_until_state(
    case: &str,
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    vpn_concentrator_id: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + poll_deadline();
    loop {
        // A read of the API is what applies the moves the clock has made due, so the poll
        // drives one rather than reaching into the store alone.
        let _ = call(router, "DescribeTransitGatewayAttachments", &[]).await;
        let state = stored_state(engine, vpn_concentrator_id).unwrap_or_default();
        if state == wanted {
            return state;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {vpn_concentrator_id} to reach {wanted:?}, last saw \
             {state:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The attachment `DescribeTransitGatewayAttachments` reports for `attachment_id`: the
/// members a report of one carries, read through the API rather than off the store.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedAttachment {
    /// `TransitGatewayAttachment$ResourceType`.
    resource_type: Option<String>,
    /// `TransitGatewayAttachment$ResourceId`.
    resource_id: Option<String>,
    /// `TransitGatewayAttachment$TransitGatewayId`.
    transit_gateway_id: Option<String>,
    /// `TransitGatewayAttachment$State`.
    state: Option<String>,
}

/// The one attachment `DescribeTransitGatewayAttachments` reports for `attachment_id`.
async fn described_attachment(
    case: &str,
    router: &MultiServiceRouter,
    attachment_id: &str,
) -> ReportedAttachment {
    let (status, body) = call(
        router,
        "DescribeTransitGatewayAttachments",
        &[param("TransitGatewayAttachmentIds.1", attachment_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the describe of {attachment_id} must succeed, got {status} with body \
         {body}"
    );
    let parsed = document(case, &body);
    let node = parsed
        .descendants()
        .find(|node| {
            node.is_element() && child(*node, "transitGatewayAttachmentId").is_some()
        })
        .unwrap_or_else(|| {
            panic!("case {case:?}: no attachment {attachment_id} reported in {body}")
        });
    assert_eq!(
        child_text(node, "transitGatewayAttachmentId").as_deref(),
        Some(attachment_id),
        "case {case:?}: the report must be of the attachment asked about, got {body}"
    );
    ReportedAttachment {
        resource_type: child_text(node, "resourceType"),
        resource_id: child_text(node, "resourceId"),
        transit_gateway_id: child_text(node, "transitGatewayId"),
        state: child_text(node, "state"),
    }
}

/// The attachment id a reported concentrator carries, required to be a freshly minted one.
fn attachment_id(case: &str, reported: &Reported) -> String {
    let attachment_id = reported
        .transit_gateway_attachment_id
        .clone()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a concentrator attached to a transit gateway reports the \
                 attachment it is reached through, got {reported:?}"
            )
        });
    assert!(
        is_long_id(TRANSIT_GATEWAY_ATTACHMENT_ID_PREFIX, &attachment_id),
        "case {case:?}: expected a {TRANSIT_GATEWAY_ATTACHMENT_ID_PREFIX}- attachment id, got \
         {attachment_id:?}"
    );
    attachment_id
}

// ---------------------------------------------------------------------------
// RULE 1: what a create mints
// ---------------------------------------------------------------------------

/// One row of the create table: what makes it a different input.
struct CreateCase {
    /// What makes this row a different input.
    case: &'static str,
    /// The tags its `TagSpecifications` carries.
    tags: &'static [(&'static str, &'static str)],
    /// Whether the gateway is already in service when the create reaches it.
    gateway_in_service: bool,
    /// How many concentrators the account already holds on that gateway.
    held: usize,
}

/// The rows: how many tags the request carries, whether the gateway is in service already or
/// still coming up, and how many concentrators the account already holds on it.
fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            case: "no tags, on a gateway in service",
            tags: &[],
            gateway_in_service: true,
            held: 0,
        },
        CreateCase {
            case: "one tag, on a gateway still coming up",
            tags: &[("Name", "head-office")],
            gateway_in_service: false,
            held: 0,
        },
        CreateCase {
            case: "several tags, on a gateway in service",
            tags: &[("Name", "branch"), ("team", "network"), ("cost-centre", "42")],
            gateway_in_service: true,
            held: 0,
        },
        CreateCase {
            case: "a further concentrator on a gateway that already carries two",
            tags: &[("Name", "spare")],
            gateway_in_service: true,
            held: 2,
        },
    ]
}

#[tokio::test]
async fn create_mints_the_concentrator_the_request_describes() {
    for CreateCase {
        case,
        tags,
        gateway_in_service,
        held,
    } in create_cases()
    {
        let (router, engine) = super::ec2_query::emulator();
        let transit_gateway_id = transit_gateway(case, &router).await;
        if gateway_in_service {
            poll_until_gateway_available(case, &router, &transit_gateway_id).await;
        }
        // The concentrators the account already holds on that gateway, which this create must
        // neither replace nor rename.
        let mut already: Vec<String> = Vec::new();
        for _ in 0..held {
            already.push(
                create_ok(case, &router, &transit_gateway_id, &[])
                    .await
                    .vpn_concentrator_id,
            );
        }

        let reported = create_ok(case, &router, &transit_gateway_id, tags).await;

        // A freshly minted id of this resource's own form, which no earlier create was given.
        assert!(
            is_long_id(VPN_CONCENTRATOR_ID_PREFIX, &reported.vpn_concentrator_id),
            "case {case:?}: expected a {VPN_CONCENTRATOR_ID_PREFIX}- concentrator id, got \
             {reported:?}"
        );
        assert!(
            !already.contains(&reported.vpn_concentrator_id),
            "case {case:?}: expected an id no earlier create was given, got {reported:?}"
        );
        // The members the request decided, answered back as they were stored.
        assert_eq!(
            reported.transit_gateway_id.as_deref(),
            Some(transit_gateway_id.as_str()),
            "case {case:?}: expected the concentrator on {transit_gateway_id}, got {reported:?}"
        );
        assert_eq!(
            reported.concentrator_type.as_deref(),
            Some(concentrator_type()),
            "case {case:?}: expected the type {:?}, got {reported:?}",
            concentrator_type()
        );
        let expected: Vec<(String, String)> = tags
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect();
        assert_eq!(
            reported.tags, expected,
            "case {case:?}: expected the tags {expected:?}, got {:?}",
            reported.tags
        );
        // "The current state of the VPN concentrator": a create answers the non-terminal
        // state, so a caller acting straight after it sees the concentrator coming up.
        assert_eq!(
            reported.state, VPN_STATE_PENDING,
            "case {case:?}: expected a create to answer {VPN_STATE_PENDING:?}, got {reported:?}"
        );

        // The record is in the store, with the very members the response reported.
        let view = stored(&engine, &reported.vpn_concentrator_id).unwrap_or_else(|| {
            panic!("case {case:?}: expected the store to hold {reported:?}")
        });
        assert_eq!(
            (
                view.vpn_concentrator_id.clone(),
                view.state.clone(),
                view.transit_gateway_id.clone(),
                view.transit_gateway_attachment_id.clone(),
                view.r#type.clone()
            ),
            (
                Some(reported.vpn_concentrator_id.clone()),
                Some(reported.state.clone()),
                reported.transit_gateway_id.clone(),
                reported.transit_gateway_attachment_id.clone(),
                reported.concentrator_type.clone()
            ),
            "case {case:?}: expected the stored record to be what was reported, got {view:?}"
        );
        let actual = stored_concentrators(&engine);
        assert_eq!(
            actual,
            held + 1,
            "case {case:?}: expected the store to hold the {held} concentrators it already \
             had plus this one, got {actual}"
        );

        // The attachment the concentrator is reached through is a REAL attachment of the
        // account: `DescribeTransitGatewayAttachments` reports it, as attaching this
        // concentrator to this gateway under the `vpn-concentrator` resource type.
        let attachment_id = attachment_id(case, &reported);
        let attachment = described_attachment(case, &router, &attachment_id).await;
        assert_eq!(
            (
                attachment.resource_type.as_deref(),
                attachment.resource_id.as_deref(),
                attachment.transit_gateway_id.as_deref()
            ),
            (
                Some(TRANSIT_GATEWAY_ATTACHMENT_RESOURCE_TYPE_VPN_CONCENTRATOR),
                Some(reported.vpn_concentrator_id.as_str()),
                Some(transit_gateway_id.as_str())
            ),
            "case {case:?}: expected the attachment of {} on {transit_gateway_id}, got \
             {attachment:?}",
            reported.vpn_concentrator_id
        );
        assert!(
            attachment.state.as_deref() == Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_PENDING)
                || attachment.state.as_deref()
                    == Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE),
            "case {case:?}: expected the attachment coming up or in service, got {attachment:?}"
        );

        // The move the clock owes: the concentrator reaches `available` on its own, with
        // nobody asking it to, and the attachment it is reached through is in service by then.
        let settled = poll_until_state(
            case,
            &router,
            &engine,
            &reported.vpn_concentrator_id,
            VPN_STATE_AVAILABLE,
        )
        .await;
        assert_eq!(
            settled, VPN_STATE_AVAILABLE,
            "case {case:?}: expected the clock to owe {VPN_STATE_AVAILABLE:?}, got {settled:?}"
        );
        let attachment = described_attachment(case, &router, &attachment_id).await;
        assert_eq!(
            attachment.state.as_deref(),
            Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_AVAILABLE),
            "case {case:?}: a concentrator in service aggregates through an attachment in \
             service, got {attachment:?}"
        );

        // The concentrators the request said nothing about are untouched.
        for other in &already {
            let actual = stored_state(&engine, other);
            assert!(
                actual.is_some(),
                "case {case:?}: expected {other} to be left in the store, got {actual:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// RULE 2: the id a create answers is the one the rest of the API resolves
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_concentrator_a_create_minted_is_the_one_the_api_resolves() {
    let case = "a concentrator a create minted";
    let (router, engine) = super::ec2_query::emulator();
    let transit_gateway_id = transit_gateway(case, &router).await;
    let reported = create_ok(case, &router, &transit_gateway_id, &[("Name", "hq")]).await;
    let vpn_concentrator_id = reported.vpn_concentrator_id.clone();

    // A customer gateway for the connection to run to, as `CreateCustomerGateway` makes one.
    let (status, body) = call(
        &router,
        "CreateCustomerGateway",
        &[
            param("Type", ec2::state::GATEWAY_TYPE_IPSEC_1),
            param("IpAddress", "203.0.113.12"),
            param("BgpAsn", "65001"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the customer gateway must be created, got {status} with body {body}"
    );
    let customer_gateway_id = first_text(case, &body, "customerGatewayId");

    // `CreateVpnConnection` resolves "the ID of the VPN concentrator to associate with the
    // VPN connection" against this account's concentrators, so a connection it accepts for
    // this id is the API itself reporting that the create's id names a concentrator.
    let (status, body) = call(
        &router,
        "CreateVpnConnection",
        &[
            param("Type", ec2::state::GATEWAY_TYPE_IPSEC_1),
            param("CustomerGatewayId", &customer_gateway_id),
            param("TransitGatewayId", &transit_gateway_id),
            param("VpnConcentratorId", &vpn_concentrator_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateVpnConnection, got {status} with body \
         {body}"
    );
    let actual = first_text(case, &body, "vpnConcentratorId");
    assert_eq!(
        actual, vpn_concentrator_id,
        "case {case:?}: expected the connection to report the concentrator \
         {vpn_concentrator_id}, got {actual:?}"
    );

    // An id NO create minted is refused by the same operation, so what the create wrote is
    // what makes the id resolve.
    let unminted = "vcn-0ffffffffffffffff";
    let (status, body) = call(
        &router,
        "CreateVpnConnection",
        &[
            param("Type", ec2::state::GATEWAY_TYPE_IPSEC_1),
            param("CustomerGatewayId", &customer_gateway_id),
            param("TransitGatewayId", &transit_gateway_id),
            param("VpnConcentratorId", unminted),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400 for an id no create minted, got {status} with body \
         {body}"
    );
    let actual = error_code(&body);
    assert_eq!(
        actual, INVALID_VPN_CONCENTRATOR_ID_NOT_FOUND,
        "case {case:?}: expected the code {INVALID_VPN_CONCENTRATOR_ID_NOT_FOUND}, got {actual}"
    );

    // And `DeleteVpnConcentrator` takes away the very concentrator the create minted: the
    // attachment it was reached through stops being in service with it, and the id stops
    // resolving.
    let attachment_id = attachment_id(case, &reported);
    let (status, body) = call(
        &router,
        "DeleteVpnConcentrator",
        &[param("VpnConcentratorId", &vpn_concentrator_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteVpnConcentrator, got {status} with body \
         {body}"
    );
    let actual = stored_state(&engine, &vpn_concentrator_id);
    assert_eq!(
        actual, None,
        "case {case:?}: expected the delete to take the concentrator away, got {actual:?}"
    );
    let attachment = described_attachment(case, &router, &attachment_id).await;
    assert_eq!(
        attachment.state.as_deref(),
        Some(TRANSIT_GATEWAY_ATTACHMENT_STATE_DELETING),
        "case {case:?}: the attachment of a deleted concentrator attaches nothing any more, \
         got {attachment:?}"
    );
    let (status, body) = call(
        &router,
        "DeleteVpnConcentrator",
        &[param("VpnConcentratorId", &vpn_concentrator_id)],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the repeat to be refused, got {status} with body {body}"
    );
    let actual = error_code(&body);
    assert_eq!(
        actual, INVALID_VPN_CONCENTRATOR_ID_NOT_FOUND,
        "case {case:?}: expected the code {INVALID_VPN_CONCENTRATOR_ID_NOT_FOUND}, got {actual}"
    );
}

// ---------------------------------------------------------------------------
// RULE 3: the requests a create refuses
// ---------------------------------------------------------------------------

/// One row of the refusal table: the fields the request carries beside `Action`, and the
/// refusal EC2 answers.
struct RefusalCase {
    /// What makes this row a different input.
    case: &'static str,
    /// `Type` as the request sends it, or `None` when it names none at all.
    concentrator_type: Option<&'static str>,
    /// `TransitGatewayId` as the request sends it, or `None` when it names none at all.
    transit_gateway_id: Option<&'static str>,
    /// The `<Code>` EC2 answers.
    code: &'static str,
    /// The `<Message>` EC2 answers.
    message: &'static str,
}

/// The rows: the parameter the request left out or sent empty, and every form a
/// `TransitGatewayId` that resolves to no gateway of the account can take.
fn refusal_cases() -> Vec<RefusalCase> {
    vec![
        // The recorded request this issue was filed about: a create naming no transit
        // gateway at all.
        RefusalCase {
            case: "no transitGatewayId at all",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: None,
            code: MISSING_PARAMETER,
            message: "The request must contain the parameter transitGatewayId",
        },
        RefusalCase {
            case: "an empty transitGatewayId",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: Some(""),
            code: MISSING_PARAMETER,
            message: "The request must contain the parameter transitGatewayId",
        },
        RefusalCase {
            case: "no type at all",
            concentrator_type: None,
            transit_gateway_id: Some("tgw-0123456789abcdef0"),
            code: BOUNDARY_MISSING_MEMBER,
            message: "The request must include the Type parameter. Add the required parameter \
                      and retry the request.",
        },
        RefusalCase {
            case: "an empty type",
            concentrator_type: Some(""),
            transit_gateway_id: Some("tgw-0123456789abcdef0"),
            code: BOUNDARY_ENUM_VIOLATION,
            message: "1 validation error detected: Value '' at 'type' failed to satisfy \
                      constraint: Member must satisfy enum value set: [ipsec.1]",
        },
        // A type outside the ONE member `VpnConcentratorType` publishes
        // (`smithy_query(query="shape",shape="VpnConcentratorType")`): refused rather than
        // stored, so no concentrator of this account reports a type the model does not have.
        RefusalCase {
            case: "a type outside the published catalog",
            concentrator_type: Some("ipsec.2"),
            transit_gateway_id: Some("tgw-0123456789abcdef0"),
            code: BOUNDARY_ENUM_VIOLATION,
            message: "1 validation error detected: Value 'ipsec.2' at 'type' failed to \
                      satisfy constraint: Member must satisfy enum value set: [ipsec.1]",
        },
        RefusalCase {
            case: "a long-form gateway id nothing holds",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: Some("tgw-0ffffffffffffffff"),
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            message: "Transit Gateway tgw-0ffffffffffffffff was deleted or does not exist.",
        },
        RefusalCase {
            case: "a short-form gateway id nothing holds",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: Some("tgw-0abcd123"),
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            message: "Transit Gateway tgw-0abcd123 was deleted or does not exist.",
        },
        RefusalCase {
            case: "an id of another resource kind",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: Some("vpc-0123456789abcdef0"),
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            message: "Transit Gateway vpc-0123456789abcdef0 was deleted or does not exist.",
        },
        RefusalCase {
            case: "not a resource id at all",
            concentrator_type: Some("ipsec.1"),
            transit_gateway_id: Some("not-an-id"),
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            message: "Transit Gateway not-an-id was deleted or does not exist.",
        },
    ]
}

#[tokio::test]
async fn a_create_that_cannot_be_served_is_refused_and_writes_nothing() {
    let (router, engine) = super::ec2_query::emulator();
    // One gateway and one concentrator the account DOES hold, so a refusal below is about
    // what the request named rather than about an empty account — and so the create can be
    // shown to have left them alone.
    let transit_gateway_id = transit_gateway("the held gateway", &router).await;
    let held = create_ok("the held gateway", &router, &transit_gateway_id, &[])
        .await
        .vpn_concentrator_id;
    let concentrators_before = stored_concentrators(&engine);
    let attachments_before = stored_attachments(&engine);

    for RefusalCase {
        case,
        concentrator_type,
        transit_gateway_id,
        code,
        message,
    } in refusal_cases()
    {
        let mut params = Vec::new();
        if let Some(value) = concentrator_type {
            params.push(param("Type", value));
        }
        if let Some(value) = transit_gateway_id {
            params.push(param("TransitGatewayId", value));
        }
        let (status, body) = create(&router, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 from CreateVpnConcentrator, got {status} with \
             body {body}"
        );
        let actual = error_code(&body);
        assert_eq!(
            actual, code,
            "case {case:?}: expected the code {code}, got {actual} in {body}"
        );
        let actual = error_message(&body);
        assert_eq!(
            actual, message,
            "case {case:?}: expected the message {message:?}, got {actual:?}"
        );

        // Nothing was written: no concentrator and no attachment was added, and the
        // concentrator the account DOES hold is untouched.
        let actual = stored_concentrators(&engine);
        assert_eq!(
            actual, concentrators_before,
            "case {case:?}: expected the store to hold {concentrators_before} concentrators, \
             got {actual}"
        );
        let actual = stored_attachments(&engine);
        assert_eq!(
            actual, attachments_before,
            "case {case:?}: expected the store to hold {attachments_before} attachments, got \
             {actual}"
        );
        assert!(
            stored(&engine, &held).is_some(),
            "case {case:?}: the held concentrator must be untouched"
        );
    }
}

// ---------------------------------------------------------------------------
// RULE 4: DryRun changes nothing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_dry_run_reports_the_outcome_and_writes_nothing() {
    let case = "a dry run of a create that would have succeeded";
    let (router, engine) = super::ec2_query::emulator();
    let transit_gateway_id = transit_gateway(case, &router).await;
    let concentrators_before = stored_concentrators(&engine);
    let attachments_before = stored_attachments(&engine);

    let (status, body) = create(
        &router,
        &[
            param("Type", concentrator_type()),
            param("TransitGatewayId", &transit_gateway_id),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 412,
        "case {case:?}: expected HTTP 412, got {status} with body {body}"
    );
    let actual = error_code(&body);
    assert_eq!(
        actual, DRY_RUN_OPERATION,
        "case {case:?}: expected the code {DRY_RUN_OPERATION}, got {actual} in {body}"
    );
    let actual = stored_concentrators(&engine);
    assert_eq!(
        actual, concentrators_before,
        "case {case:?}: a dry run stores no concentrator, got {actual}"
    );
    let actual = stored_attachments(&engine);
    assert_eq!(
        actual, attachments_before,
        "case {case:?}: a dry run mints no attachment, got {actual}"
    );

    // A dry run naming a gateway nothing holds is refused for the GATEWAY: the flag asks
    // whether the caller is permitted, not whether the request is about something that
    // exists.
    let case = "a dry run naming a gateway nothing holds";
    let (status, body) = create(
        &router,
        &[
            param("Type", concentrator_type()),
            param("TransitGatewayId", "tgw-0ffffffffffffffff"),
            param("DryRun", "true"),
        ],
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    let actual = error_code(&body);
    assert_eq!(
        actual, INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
        "case {case:?}: expected the code {INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND}, got {actual}"
    );

    // And the create the dry run asked about still works.
    let case = "the create the dry run asked about";
    let reported = create_ok(case, &router, &transit_gateway_id, &[]).await;
    assert!(
        stored(&engine, &reported.vpn_concentrator_id).is_some(),
        "case {case:?}: expected the create to store {reported:?}"
    );
}
