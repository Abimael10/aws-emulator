// SPDX-License-Identifier: CC0-1.0

//! The CONNECTION a consumer's endpoint makes to one of the account's own VPC endpoint
//! services, and the two answers its owner can give it — driven over the wire the service
//! speaks (`ec2Query`: an `application/x-www-form-urlencoded` POST answered with XML,
//! README.md / OPERATIONS.md "Wire protocol").
//!
//! ONE RULE is under test: "when a consumer creates an endpoint to your service, a connection
//! request is sent to you" (the Amazon Web Services PrivateLink User Guide's "Manage endpoint
//! connection requests"). So `CreateVpcEndpoint` against a service the ACCOUNT offers records
//! a connection `DescribeVpcEndpointConnections` reports, in the state the endpoint itself is
//! in: `pendingAcceptance` while a service that requires acceptance waits for its owner, and
//! the ordinary `pending` -> `available` of a service that waived it. The owner's answer then
//! moves BOTH the connection and the endpoint it is of — `AcceptVpcEndpointConnections` to
//! `pending` with the clock owing `available`, `RejectVpcEndpointConnections` to the terminal
//! `rejected` — so a consumer polling its own endpoint sees the verdict, and a connection
//! nobody is waiting on any more cannot be answered twice.
//!
//! The rows span what that generalises over: whether the service requires acceptance, which
//! answer the owner gives, which state the connection is in when the answer arrives, and which
//! member of a reject request is at fault.

use std::time::{Duration, Instant};

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, error_code, filters};
use super::wire::{child, child_text, document, param, Param};

/// The Region every call here is signed for, and so the Region the services are hosted in.
const REGION: &str = "us-east-1";

/// The VPC the consumer endpoints are created in, and the one subnet they are served from.
const VPC_CIDR: &str = "10.88.0.0/16";
const SUBNET_CIDR: &str = "10.88.1.0/24";

/// A well-formed id of a resource no account holds, for the rows that must be refused.
const UNKNOWN_SERVICE: &str = "vpce-svc-00000000000000000";
const UNKNOWN_ENDPOINT: &str = "vpce-00000000000000000";

/// How long a poll keeps looking: far past the span the service moves a record over
/// ([`ec2::state::TRANSITION_DELAY`]), so a loaded host only waits longer.
fn poll_deadline() -> Instant {
    Instant::now() + ec2::state::TRANSITION_DELAY * 40
}

/// How long the poll waits between two looks.
fn poll_interval() -> Duration {
    ec2::state::TRANSITION_DELAY / 20
}

/// One call that must succeed, as its body.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = call(router, REGION, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The ARN of a Network Load Balancer behind one of the services. A load balancer is a
/// resource of another service, so what this crate stores is the ARN the request named.
fn load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// One endpoint service of the account: the id it was minted with and the name it is reached
/// by.
struct Service {
    id: String,
    name: String,
}

/// Configure one endpoint service behind a Network Load Balancer, requiring acceptance or not.
async fn configure_service(
    case: &str,
    router: &MultiServiceRouter,
    name: &str,
    acceptance_required: bool,
) -> Service {
    let body = ok(
        case,
        router,
        "CreateVpcEndpointServiceConfiguration",
        &[
            param("NetworkLoadBalancerArn.1", &load_balancer_arn(name)),
            param("AcceptanceRequired", &acceptance_required.to_string()),
        ],
    )
    .await;
    Service {
        id: element(&body, "serviceId").unwrap_or_default(),
        name: element(&body, "serviceName").unwrap_or_default(),
    }
}

/// Create one interface endpoint to `service` in `subnet_id`, and answer its id.
async fn create_endpoint(
    case: &str,
    router: &MultiServiceRouter,
    vpc_id: &str,
    subnet_id: &str,
    service: &Service,
) -> String {
    let body = ok(
        case,
        router,
        "CreateVpcEndpoint",
        &[
            param("VpcId", vpc_id),
            param("VpcEndpointType", "Interface"),
            param("ServiceName", &service.name),
            param("SubnetId.1", subnet_id),
        ],
    )
    .await;
    element(&body, "vpcEndpointId")
        .unwrap_or_else(|| panic!("case {case:?}: a create reports the endpoint it made: {body}"))
}

/// The `state` `DescribeVpcEndpoints` reports for one endpoint.
async fn endpoint_state(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpoints",
        &[param("VpcEndpointId.1", endpoint_id)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "vpcEndpointId").is_some())
        .and_then(|endpoint| child_text(endpoint, "state"))
        .unwrap_or_else(|| panic!("case {case:?}: {endpoint_id} must report a state: {body}"))
}

/// What one connection of a service reports: the endpoint it is of, its state and the account
/// that owns the endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectionView {
    endpoint_id: String,
    state: String,
    owner: String,
    connection_id: String,
}

/// Every connection `DescribeVpcEndpointConnections` reports for `service_id`.
async fn connections(
    case: &str,
    router: &MultiServiceRouter,
    service_id: &str,
) -> Vec<ConnectionView> {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointConnections",
        &filters(&[("service-id", &[service_id])]),
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "vpcEndpointId").is_some())
        .map(|node| ConnectionView {
            endpoint_id: child_text(node, "vpcEndpointId").unwrap_or_default(),
            state: child_text(node, "vpcEndpointState").unwrap_or_default(),
            owner: child_text(node, "vpcEndpointOwner").unwrap_or_default(),
            connection_id: child_text(node, "vpcEndpointConnectionId").unwrap_or_default(),
        })
        .collect()
}

/// The connection of `endpoint_id` among the service's, or `None` when the service holds none
/// for it.
async fn connection_of(
    case: &str,
    router: &MultiServiceRouter,
    service_id: &str,
    endpoint_id: &str,
) -> Option<ConnectionView> {
    connections(case, router, service_id)
        .await
        .into_iter()
        .find(|connection| connection.endpoint_id == endpoint_id)
}

/// Look again until the connection of `endpoint_id` reports `wanted`, or a generous deadline
/// passes.
async fn until_connection_state(
    case: &str,
    router: &MultiServiceRouter,
    service_id: &str,
    endpoint_id: &str,
    wanted: &str,
) {
    let deadline = poll_deadline();
    loop {
        let held = connection_of(case, router, service_id, endpoint_id).await;
        if held.as_ref().is_some_and(|view| view.state == wanted) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the connection of {endpoint_id} must reach {wanted:?}, got {held:?}"
        );
        tokio::time::sleep(poll_interval()).await;
    }
}

/// The `Unsuccessful` items a reject or an accept answered: `(resourceId, code)`.
fn unsuccessful(case: &str, body: &str) -> Vec<(String, String)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .filter(|node| node.is_element() && child(*node, "resourceId").is_some())
        .map(|node| {
            (
                child_text(node, "resourceId").unwrap_or_default(),
                child(node, "error")
                    .and_then(|error| child_text(error, "code"))
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// One `RejectVpcEndpointConnections` call.
async fn reject(
    router: &MultiServiceRouter,
    service_id: &str,
    endpoint_ids: &[&str],
    dry_run: bool,
) -> (u16, String) {
    let mut params = vec![param("ServiceId", service_id)];
    for (position, endpoint_id) in endpoint_ids.iter().enumerate() {
        params.push(param(
            &format!("VpcEndpointId.{}", position + 1),
            endpoint_id,
        ));
    }
    if dry_run {
        params.push(param("DryRun", "true"));
    }
    call(router, REGION, "RejectVpcEndpointConnections", &params).await
}

/// The network and the services every test here drives.
struct Fixture {
    router: MultiServiceRouter,
    vpc_id: String,
    subnet_id: String,
    /// A service whose owner must answer every connection request.
    gated: Service,
    /// A service that waived acceptance, so a connection to it is accepted for the owner.
    open: Service,
}

async fn fixture() -> Fixture {
    let (router, _engine) = super::ec2_query::emulator();
    let vpc_id = {
        let body = ok("the VPC", &router, "CreateVpc", &[param("CidrBlock", VPC_CIDR)]).await;
        element(&body, "vpcId").unwrap_or_default()
    };
    let subnet_id = {
        let body = ok(
            "the subnet",
            &router,
            "CreateSubnet",
            &[param("VpcId", &vpc_id), param("CidrBlock", SUBNET_CIDR)],
        )
        .await;
        element(&body, "subnetId").unwrap_or_default()
    };
    let gated = configure_service("the gated service", &router, "gated", true).await;
    let open = configure_service("the open service", &router, "open", false).await;
    Fixture {
        router,
        vpc_id,
        subnet_id,
        gated,
        open,
    }
}

/// RULE: an endpoint to a service the account offers records the connection its owner acts on,
/// in the state the endpoint itself is in — whether or not that service requires acceptance.
#[tokio::test]
async fn an_endpoint_to_an_own_service_records_the_connection_its_owner_acts_on() {
    let fixture = fixture().await;
    let router = &fixture.router;

    // (case, the service, the state both the endpoint and its connection start in)
    let rows: Vec<(&str, &Service, &str)> = vec![
        (
            "a service whose owner must accept the connection",
            &fixture.gated,
            ec2::state::VPC_ENDPOINT_STATE_PENDING_ACCEPTANCE,
        ),
        (
            "a service that waived acceptance",
            &fixture.open,
            ec2::state::VPC_ENDPOINT_STATE_PENDING,
        ),
    ];

    // The endpoints the rows made, so the wait below names the one its row created.
    let mut made: Vec<(&str, String, String)> = Vec::new();
    for (case, service, expected) in rows {
        let endpoint_id =
            create_endpoint(case, router, &fixture.vpc_id, &fixture.subnet_id, service).await;
        let state = endpoint_state(case, router, &endpoint_id).await;
        assert_eq!(
            state, expected,
            "case {case:?}: the endpoint must be created in {expected:?}, got {state:?}"
        );
        let connection = connection_of(case, router, &service.id, &endpoint_id).await;
        assert_eq!(
            connection.as_ref().map(|view| view.state.as_str()),
            Some(expected),
            "case {case:?}: the service must hold a connection for {endpoint_id} in \
             {expected:?}, got {connection:?}"
        );
        let connection = connection.expect("asserted just above");
        assert_eq!(
            connection.owner,
            engine::emulator_account_id(),
            "case {case:?}: the connection names the account that owns the endpoint, got \
             {connection:?}"
        );
        assert!(
            connection
                .connection_id
                .starts_with(&format!("{}-", ec2::state::VPC_ENDPOINT_CONNECTION_ID_PREFIX)),
            "case {case:?}: the store mints the connection its own id, got {connection:?}"
        );
        made.push((case, service.id.clone(), endpoint_id));
    }

    // A service that waived acceptance accepts the connection for its owner, so the clock
    // takes both the connection and the endpoint into service without anybody calling again.
    let (case, open_service_id, open_endpoint) = made
        .last()
        .cloned()
        .expect("the rows above made an endpoint to the open service");
    until_connection_state(
        case,
        router,
        &open_service_id,
        &open_endpoint,
        ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
    )
    .await;
    let state = endpoint_state(case, router, &open_endpoint).await;
    assert_eq!(
        state,
        ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
        "a connection accepted for its owner takes its endpoint into service too, got {state:?}"
    );
}

/// RULE: the owner's answer moves the connection AND the endpoint it is of — a rejection to
/// the terminal `rejected`, an acceptance through `pending` to `available` — and a connection
/// that is not waiting for an answer cannot be answered again.
#[tokio::test]
async fn the_owners_answer_moves_the_connection_and_its_endpoint() {
    let fixture = fixture().await;
    let router = &fixture.router;
    let service = &fixture.gated;

    let refused = create_endpoint(
        "the rejected endpoint",
        router,
        &fixture.vpc_id,
        &fixture.subnet_id,
        service,
    )
    .await;
    let accepted = create_endpoint(
        "the accepted endpoint",
        router,
        &fixture.vpc_id,
        &fixture.subnet_id,
        service,
    )
    .await;

    // The rejection: both the connection and the endpoint reach `rejected`, and nothing is
    // reported unsuccessful.
    let (status, body) = reject(router, &service.id, &[&refused], false).await;
    assert_eq!(
        status, 200,
        "the rejection must succeed, got {status} with body {body}"
    );
    assert_eq!(
        unsuccessful("the rejection", &body),
        Vec::<(String, String)>::new(),
        "the rejection of a pending connection reports nothing unsuccessful, got {body}"
    );
    let connection = connection_of("the rejection", router, &service.id, &refused).await;
    assert_eq!(
        connection.as_ref().map(|view| view.state.as_str()),
        Some(ec2::state::VPC_ENDPOINT_STATE_REJECTED),
        "the refused connection must report rejected, got {connection:?}"
    );
    let state = endpoint_state("the rejection", router, &refused).await;
    assert_eq!(
        state,
        ec2::state::VPC_ENDPOINT_STATE_REJECTED,
        "the endpoint of a refused connection must report rejected too, got {state:?}"
    );

    // The acceptance: the connection leaves `pendingAcceptance` for the non-terminal
    // `pending`, so a caller reading straight after it still sees the transient state.
    let (status, body) = call(
        router,
        REGION,
        "AcceptVpcEndpointConnections",
        &[
            param("ServiceId", &service.id),
            param("VpcEndpointId.1", &accepted),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the acceptance must succeed, got {status} with body {body}"
    );
    let connection = connection_of("the acceptance", router, &service.id, &accepted).await;
    assert_eq!(
        connection.as_ref().map(|view| view.state.as_str()),
        Some(ec2::state::VPC_ENDPOINT_STATE_PENDING),
        "an accepted connection is pending before it is in service, got {connection:?}"
    );
    until_connection_state(
        "the acceptance",
        router,
        &service.id,
        &accepted,
        ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
    )
    .await;
    let state = endpoint_state("the acceptance", router, &accepted).await;
    assert_eq!(
        state,
        ec2::state::VPC_ENDPOINT_STATE_AVAILABLE,
        "the endpoint of an accepted connection is in service too, got {state:?}"
    );

    // The rejection is TERMINAL: the span the acceptance above waited out has passed, and the
    // refused connection and its endpoint are still exactly where the refusal left them.
    let connection = connection_of("the terminal rejection", router, &service.id, &refused).await;
    assert_eq!(
        connection.as_ref().map(|view| view.state.as_str()),
        Some(ec2::state::VPC_ENDPOINT_STATE_REJECTED),
        "nothing is owed on a rejected connection, got {connection:?}"
    );
    let state = endpoint_state("the terminal rejection", router, &refused).await;
    assert_eq!(
        state,
        ec2::state::VPC_ENDPOINT_STATE_REJECTED,
        "nothing is owed on a rejected endpoint either, got {state:?}"
    );

    // A connection that is no longer waiting for an answer is reported unsuccessful rather
    // than answered twice — whichever answer it already had.
    for (case, endpoint_id) in [
        ("a connection already in service", &accepted),
        ("a connection already rejected", &refused),
    ] {
        let before = connection_of(case, router, &service.id, endpoint_id)
            .await
            .map(|view| view.state);
        let (status, body) = reject(router, &service.id, &[endpoint_id], false).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the call itself succeeds, got {status} with body {body}"
        );
        assert_eq!(
            unsuccessful(case, &body),
            vec![(
                (*endpoint_id).clone(),
                ec2::state::INCORRECT_STATE.to_string()
            )],
            "case {case:?}: expected the endpoint reported unsuccessful, got {body}"
        );
        let after = connection_of(case, router, &service.id, endpoint_id)
            .await
            .map(|view| view.state);
        assert_eq!(
            after, before,
            "case {case:?}: a refused reject moves nothing, got {after:?} after {before:?}"
        );
    }
}

/// RULE: a reject request the service cannot carry out is refused with the EC2 code for its
/// cause — the whole call for the service it is scoped to, one item of `Unsuccessful` for an
/// endpoint — and moves nothing.
#[tokio::test]
async fn a_reject_that_cannot_be_carried_out_moves_nothing() {
    let fixture = fixture().await;
    let router = &fixture.router;
    let service = &fixture.gated;
    let endpoint_id = create_endpoint(
        "the waiting endpoint",
        router,
        &fixture.vpc_id,
        &fixture.subnet_id,
        service,
    )
    .await;

    // (case, the service id the request names, the endpoint ids it names, DryRun, the code the
    // CALL is refused with, the items it reports unsuccessful)
    struct Row {
        case: &'static str,
        service_id: String,
        endpoint_ids: Vec<String>,
        dry_run: bool,
        refused_with: Option<&'static str>,
        unsuccessful: Vec<&'static str>,
    }
    let rows: Vec<Row> = vec![
        Row {
            case: "a service id of no service the account holds",
            service_id: UNKNOWN_SERVICE.to_string(),
            endpoint_ids: vec![endpoint_id.clone()],
            dry_run: false,
            refused_with: Some(ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND),
            unsuccessful: Vec::new(),
        },
        Row {
            case: "a value that is not a service id at all",
            service_id: "not-a-service-id".to_string(),
            endpoint_ids: vec![endpoint_id.clone()],
            dry_run: false,
            refused_with: Some(ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED),
            unsuccessful: Vec::new(),
        },
        Row {
            case: "no endpoint at all",
            service_id: service.id.clone(),
            endpoint_ids: Vec::new(),
            dry_run: false,
            refused_with: Some(ec2::state::MISSING_PARAMETER),
            unsuccessful: Vec::new(),
        },
        Row {
            case: "a request that would have succeeded, probed with DryRun",
            service_id: service.id.clone(),
            endpoint_ids: vec![endpoint_id.clone()],
            dry_run: true,
            refused_with: Some(ec2::state::DRY_RUN_OPERATION),
            unsuccessful: Vec::new(),
        },
        Row {
            case: "an endpoint id of no connection to this service",
            service_id: service.id.clone(),
            endpoint_ids: vec![UNKNOWN_ENDPOINT.to_string()],
            dry_run: false,
            refused_with: None,
            unsuccessful: vec![ec2::state::INVALID_VPC_ENDPOINT_ID_NOT_FOUND],
        },
        Row {
            case: "a value that is not an endpoint id at all",
            service_id: service.id.clone(),
            endpoint_ids: vec!["not-an-endpoint-id".to_string()],
            dry_run: false,
            refused_with: None,
            unsuccessful: vec![ec2::state::INVALID_VPC_ENDPOINT_ID_MALFORMED],
        },
    ];

    for row in rows {
        let case = row.case;
        let named: Vec<&str> = row.endpoint_ids.iter().map(String::as_str).collect();
        let (status, body) = reject(router, &row.service_id, &named, row.dry_run).await;
        match row.refused_with {
            Some(code) => {
                // A dry run of a request that would have succeeded is answered with EC2's own
                // status for the probe rather than with a bad-request one.
                let expected_status = if code == ec2::state::DRY_RUN_OPERATION {
                    ec2::state::DRY_RUN_OPERATION_STATUS
                } else {
                    400
                };
                assert_eq!(
                    status, expected_status,
                    "case {case:?}: expected HTTP {expected_status}, got {status} with body \
                     {body}"
                );
                assert_eq!(
                    error_code(&body),
                    code,
                    "case {case:?}: expected {code}, got {body}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the call to succeed, got {status} with body {body}"
                );
                let reported: Vec<String> = unsuccessful(case, &body)
                    .into_iter()
                    .map(|(_, code)| code)
                    .collect();
                assert_eq!(
                    reported,
                    row.unsuccessful
                        .iter()
                        .map(|code| (*code).to_string())
                        .collect::<Vec<String>>(),
                    "case {case:?}: expected the causes reported per item, got {body}"
                );
            }
        }
        // Whatever the row asked for, the connection nobody could reject is still waiting.
        let held = connection_of(case, router, &service.id, &endpoint_id).await;
        assert_eq!(
            held.as_ref().map(|view| view.state.as_str()),
            Some(ec2::state::VPC_ENDPOINT_STATE_PENDING_ACCEPTANCE),
            "case {case:?}: the waiting connection must be left exactly where it was, got \
             {held:?}"
        );
    }
}

/// RULE: the connection goes with the endpoint it is of — a service holds no connection from an
/// endpoint the account has deleted.
#[tokio::test]
async fn the_connection_goes_with_the_endpoint_it_is_of() {
    let fixture = fixture().await;
    let router = &fixture.router;
    let service = &fixture.gated;
    let endpoint_id = create_endpoint(
        "the deleted endpoint",
        router,
        &fixture.vpc_id,
        &fixture.subnet_id,
        service,
    )
    .await;
    assert!(
        connection_of("the deleted endpoint", router, &service.id, &endpoint_id)
            .await
            .is_some(),
        "the service must hold the connection before the endpoint is deleted"
    );

    ok(
        "the deleted endpoint",
        router,
        "DeleteVpcEndpoints",
        &[param("VpcEndpointId.1", &endpoint_id)],
    )
    .await;

    let held = connection_of("the deleted endpoint", router, &service.id, &endpoint_id).await;
    assert_eq!(
        held, None,
        "a connection from an endpoint that no longer exists must not be reported, got {held:?}"
    );
}
