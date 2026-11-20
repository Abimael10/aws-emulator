// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the ENDS of a Reachability Analyzer path — what
//! `CreateNetworkInsightsPath` does with the `Source` and `Destination` it is given — driven
//! over the wire the service actually speaks — `ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. AN END GIVEN AS A BARE ID NAMES A RESOURCE OF THIS ACCOUNT, SO IT HAS TO RESOLVE. "The
//!    ID or ARN of the source. If the resource is in another account, you must specify an ARN"
//!    (`CreateNetworkInsightsPathRequest$Source`, and the same sentence for `Destination`), so
//!    an end given as an id of this account that names no resource the account holds is
//!    refused with THAT resource kind's own not-found code, and no path is created — a path
//!    over endpoints that do not exist is a path nothing could be analyzed over.
//! 2. THE RULE IS ABOUT THE RESOURCE, NOT ABOUT ONE KIND OF END: a path between ends the
//!    account DOES hold is created and reads back through
//!    `DescribeNetworkInsightsPaths`, for each kind of resource an end may be.
//!
//! Each row is a distinct input along what the rules generalise over — WHICH kind of resource
//! the end names (a network interface, an internet gateway, a virtual private gateway and a
//! transit gateway, four of the kinds the `Source`/`Destination` catalog admits), and WHICH
//! end of the path carries the unresolvable value — so no row is the specification. The
//! expected codes and sentences are written out here, independently of the service.

use ec2::state::{
    INVALID_INTERNET_GATEWAY_ID_NOT_FOUND, INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
    INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND, INVALID_VPN_GATEWAY_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, elements, error_code, error_message};
use super::wire::param;

/// The Region every call here is signed for.
const REGION: &str = "us-east-1";

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// The `Protocol` every path here is created for: a member of the modelled enum
/// (`smithy_query(query="shape",shape="Protocol")`).
const PROTOCOL: &str = "tcp";

/// One kind of resource an end of a path may name: what to call it, the id of one the account
/// holds, a value in that kind's id form the account holds nothing under, and the refusal that
/// value earns.
struct Endpoint {
    case: &'static str,
    held: String,
    unissued: &'static str,
    code: &'static str,
    message: String,
}

/// One `CreateNetworkInsightsPath` call for the two ends, with a token of its own.
async fn create_path(
    router: &MultiServiceRouter,
    token: &str,
    source: &str,
    destination: &str,
) -> (u16, String) {
    call(
        router,
        REGION,
        "CreateNetworkInsightsPath",
        &[
            param("Source", source),
            param("Destination", destination),
            param("Protocol", PROTOCOL),
            param("ClientToken", token),
        ],
    )
    .await
}

/// How many paths the account holds, read back through the describe that reports them.
async fn reported_paths(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let (status, body) = call(router, REGION, "DescribeNetworkInsightsPaths", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 describing the paths, got {status} with body {body}"
    );
    elements(&body, "networkInsightsPathId")
}

/// One resource of each kind an end may name, created over the wire.
async fn endpoints(case: &str, router: &MultiServiceRouter) -> Vec<Endpoint> {
    let (status, body) = call(router, REGION, "CreateVpc", &[param("CidrBlock", "10.80.0.0/16")]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a VPC, got {status} with body {body}"
    );
    let vpc_id = element(&body, "vpcId").unwrap_or_else(|| panic!("no vpcId in {body}"));

    let (status, body) = call(
        router,
        REGION,
        "CreateSubnet",
        &[param("VpcId", &vpc_id), param("CidrBlock", "10.80.1.0/24")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a subnet, got {status} with body {body}"
    );
    let subnet_id = element(&body, "subnetId").unwrap_or_else(|| panic!("no subnetId in {body}"));

    let (status, body) = call(
        router,
        REGION,
        "CreateNetworkInterface",
        &[param("SubnetId", &subnet_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a network interface, got {status} with body \
         {body}"
    );
    let interface_id = element(&body, "networkInterfaceId")
        .unwrap_or_else(|| panic!("no networkInterfaceId in {body}"));

    let (status, body) = call(router, REGION, "CreateInternetGateway", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating an internet gateway, got {status} with body \
         {body}"
    );
    let internet_gateway_id = element(&body, "internetGatewayId")
        .unwrap_or_else(|| panic!("no internetGatewayId in {body}"));

    let (status, body) = call(
        router,
        REGION,
        "CreateVpnGateway",
        &[param("Type", "ipsec.1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a virtual private gateway, got {status} with \
         body {body}"
    );
    let vpn_gateway_id =
        element(&body, "vpnGatewayId").unwrap_or_else(|| panic!("no vpnGatewayId in {body}"));

    let (status, body) = call(router, REGION, "CreateTransitGateway", &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 creating a transit gateway, got {status} with body \
         {body}"
    );
    let transit_gateway_id = element(&body, "transitGatewayId")
        .unwrap_or_else(|| panic!("no transitGatewayId in {body}"));

    vec![
        Endpoint {
            case: "a network interface",
            held: interface_id,
            // The documentation-shaped id a client reaches for, which no account was ever
            // issued.
            unissued: "eni-0123456789abcdef0",
            code: INVALID_NETWORK_INTERFACE_ID_NOT_FOUND,
            message: "The networkInterface ID 'eni-0123456789abcdef0' does not exist".to_string(),
        },
        Endpoint {
            case: "an internet gateway",
            held: internet_gateway_id,
            unissued: "igw-0123456789abcdef0",
            code: INVALID_INTERNET_GATEWAY_ID_NOT_FOUND,
            message: "The internetGateway ID 'igw-0123456789abcdef0' does not exist".to_string(),
        },
        Endpoint {
            case: "a virtual private gateway",
            held: vpn_gateway_id,
            unissued: "vgw-0123456789abcdef0",
            code: INVALID_VPN_GATEWAY_ID_NOT_FOUND,
            message: "The vpnGateway ID 'vgw-0123456789abcdef0' does not exist".to_string(),
        },
        Endpoint {
            case: "a transit gateway",
            held: transit_gateway_id,
            unissued: "tgw-0123456789abcdef0",
            code: INVALID_TRANSIT_GATEWAY_ID_NOT_FOUND,
            message: "Transit Gateway tgw-0123456789abcdef0 was deleted or does not exist."
                .to_string(),
        },
    ]
}

/// RULE 1: an end of either side that names no resource of the account is refused with that
/// kind's own not-found code, and nothing is created.
#[tokio::test]
async fn a_path_over_an_end_the_account_does_not_hold_is_refused() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "an end the account does not hold";
    let ends = endpoints(case, &router).await;

    let mut token = 0;
    for end in &ends {
        // Each row is driven on BOTH sides of the path, so the rule is not read off the
        // `Source` member alone.
        for (side, source, destination) in [
            ("the source", end.unissued, end.held.as_str()),
            ("the destination", end.held.as_str(), end.unissued),
            ("both ends", end.unissued, end.unissued),
        ] {
            token += 1;
            let row = format!("{} as {side}", end.case);
            let (status, body) =
                create_path(&router, &format!("unresolvable-{token}"), source, destination).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {row:?}: expected HTTP {REFUSAL_STATUS}, got {status} with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, end.code,
                "case {row:?}: expected the code {:?}, got {code:?} in {body}",
                end.code
            );
            let message = error_message(&body);
            assert_eq!(
                message, end.message,
                "case {row:?}: expected the message {:?}, got {message:?}",
                end.message
            );
            assert!(
                reported_paths(&row, &router).await.is_empty(),
                "case {row:?}: a refused request creates no path"
            );
        }
    }
}

/// RULE 2: a path between ends the account DOES hold is created for each kind of end and reads
/// back through the describe.
#[tokio::test]
async fn a_path_between_ends_the_account_holds_is_created() {
    let (router, _engine) = super::ec2_query::emulator();
    let case = "ends the account holds";
    let ends = endpoints(case, &router).await;

    let mut created: Vec<String> = Vec::new();
    for (index, end) in ends.iter().enumerate() {
        // The other end is the NEXT kind of the table, so each row is a path between two
        // different kinds of resource rather than one kind peered with itself.
        let other = &ends[(index + 1) % ends.len()];
        let row = format!("{} to {}", end.case, other.case);
        let (status, body) = create_path(
            &router,
            &format!("held-{index}"),
            &end.held,
            &other.held,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {row:?}: expected HTTP 200, got {status} with body {body}"
        );
        let path_id = element(&body, "networkInsightsPathId")
            .unwrap_or_else(|| panic!("case {row:?}: no networkInsightsPathId in {body}"));
        let reported_source = element(&body, "source").unwrap_or_default();
        assert_eq!(
            reported_source, end.held,
            "case {row:?}: expected the source {:?}, got {reported_source:?} in {body}",
            end.held
        );
        created.push(path_id);
    }

    let reported = reported_paths(case, &router).await;
    for path_id in &created {
        assert!(
            reported.contains(path_id),
            "case {case:?}: expected {path_id} among the reported paths, got {reported:?}"
        );
    }
    assert_eq!(
        reported.len(),
        created.len(),
        "case {case:?}: expected one stored path per served request, got {reported:?}"
    );
}
