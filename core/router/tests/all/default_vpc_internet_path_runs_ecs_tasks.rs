// SPDX-License-Identifier: CC0-1.0

//! The account's DEFAULT VPC comes with the internet path AWS gives it, through the assembled
//! router: an internet gateway attached to that VPC, and a main route table that sends
//! internet-bound traffic to that gateway.
//!
//! RULE: "when you create an AWS account, we create a default VPC for you in each AWS Region"
//! and the resources AWS creates with it include "an internet gateway and connect it to your
//! default VPC" and "a main route table for your default VPC with a route that sends all
//! traffic destined for the internet to the internet gateway" (Amazon VPC User Guide, "Default
//! VPCs"), so an account that created nothing already holds that gateway and that route, while
//! a VPC the caller created holds neither until the caller makes them.
//!
//! It lives in this cross-service target because the path is what other services READ before
//! they place a workload in a default subnet: Amazon ECS decides a task's image fetch against
//! these very records, which is why the whole assembled router is driven here rather than one
//! service crate's own harness. What the fetch then decides, and what a task placed in a
//! default subnet does, is pinned by that service's own suites
//! (`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/
//! image_pull_placement.rs` and `.../default_network_alb_service.rs`).
//!
//! The rows differ in how the VPC came to be: the ambient default VPC of the Region the
//! request is signed for, and a VPC the caller created in the same account. The suite is
//! named for what that path is FOR: an Amazon ECS task placed in a default subnet fetches the
//! image its container runs only because the path is there.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode, items_of};

/// The region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API version an ec2Query request carries.
const EC2_API_VERSION: &str = "2016-11-15";

/// The destination of the route that carries traffic off a VPC.
const INTERNET_DESTINATION: &str = "0.0.0.0/0";

/// The CIDR block of the VPC the second row creates.
const OWN_VPC_CIDR: &str = "10.75.0.0/16";

/// One emulator carrying Amazon EC2, built the way the production router builds it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(engine));
    router
}

/// Drive one ec2Query action that must succeed, and answer its XML body.
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
        .uri(format!("https://ec2.{REGION}.amazonaws.com/"))
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with body {answered}"
    );
    answered
}

#[tokio::test]
async fn the_default_vpc_holds_an_internet_gateway_and_a_route_to_it() {
    let router = emulator();

    // The ambient default VPC of the Region, which this suite creates nothing for.
    let default_vpc = element(
        &ec2(
            &router,
            "DescribeVpcs",
            &[("Filter.1.Name", "isDefault"), ("Filter.1.Value.1", "true")],
        )
        .await,
        "vpcId",
    );
    // And a VPC of the caller's own, which comes with neither.
    let own_vpc = element(
        &ec2(&router, "CreateVpc", &[("CidrBlock", OWN_VPC_CIDR)]).await,
        "vpcId",
    );

    // (case, the VPC, whether it comes with an attached gateway and a default route to it)
    let cases: [(&str, &str, bool); 2] = [
        ("the account's default VPC", default_vpc.as_str(), true),
        ("a VPC the caller created", own_vpc.as_str(), false),
    ];

    for (case, vpc, comes_with_a_path) in cases {
        let gateways = ec2(
            &router,
            "DescribeInternetGateways",
            &[
                ("Filter.1.Name", "attachment.vpc-id"),
                ("Filter.1.Value.1", vpc),
            ],
        )
        .await;
        let attached = elements(&gateways, "internetGatewayId");
        let tables = ec2(
            &router,
            "DescribeRouteTables",
            &[("Filter.1.Name", "vpc-id"), ("Filter.1.Value.1", vpc)],
        )
        .await;
        // The default route of the VPC's route tables, as the gateway it points at.
        let routed_to: Option<String> = items_of(&tables)
            .into_iter()
            .find(|item| {
                elements(item, "destinationCidrBlock")
                    .iter()
                    .any(|block| block == INTERNET_DESTINATION)
            })
            .and_then(|item| elements(item, "gatewayId").into_iter().next());

        if comes_with_a_path {
            assert_eq!(
                attached.len(),
                1,
                "case {case:?}: expected one internet gateway attached to {vpc}, got {gateways}"
            );
            assert_eq!(
                elements(&gateways, "state"),
                vec!["available".to_string()],
                "case {case:?}: expected the attachment reported available, got {gateways}"
            );
            assert_eq!(
                routed_to.as_deref(),
                attached.first().map(String::as_str),
                "case {case:?}: expected {INTERNET_DESTINATION} routed to the VPC's gateway, \
                 got {tables}"
            );
        } else {
            assert!(
                attached.is_empty(),
                "case {case:?}: expected no gateway attached to {vpc}, got {gateways}"
            );
            assert_eq!(
                routed_to, None,
                "case {case:?}: expected no route off the VPC, got {tables}"
            );
        }
    }
}
