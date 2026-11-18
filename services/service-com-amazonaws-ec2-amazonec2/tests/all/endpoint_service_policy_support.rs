// SPDX-License-Identifier: CC0-1.0

//! Which published endpoint services support an endpoint POLICY, driven over the wire the
//! service actually speaks: `ec2Query` — an `application/x-www-form-urlencoded` POST whose
//! body carries `Action=<Operation>`, answered with XML (README.md / OPERATIONS.md "Wire
//! protocol").
//!
//! The rule under test: **the catalogue a client reads to decide whether it may attach an
//! endpoint policy is the same answer the create is held to.** "Indicates whether the service
//! supports endpoint policies" (`ServiceDetail$VpcEndpointPolicySupported`) is a fact of the
//! service, so `DescribeVpcEndpointServices` reports it and `CreateVpcEndpoint` refuses a
//! NARROWING `PolicyDocument` for a service that reports `false` — a client is never told it
//! has locked an endpoint down to a policy the service would ignore.
//!
//! The rows are the published services whose offering differs: the two data-plane services
//! that support no endpoint policy (the AppStream 2.0 streaming service and the AWS IoT Core
//! data plane), beside services that do — so a row is not passing because every service
//! answers the same way.

use ec2::state::INVALID_PARAMETER;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, error_code, error_message, filters};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for; the service names below carry it.
const REGION: &str = "us-east-1";

/// A policy document that narrows what an endpoint may reach — the kind a service supporting
/// no endpoint policy refuses.
const NARROWING_POLICY: &str = r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"s3:GetObject","Resource":"*"}]}"#;

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

/// The `VpcEndpointPolicySupported` the catalogue reports for the service `service_name`, read
/// through the XML tree off the entry that names that service.
async fn policy_supported(case: &str, router: &MultiServiceRouter, service_name: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointServices",
        &[param("ServiceName.1", service_name)],
    )
    .await;
    let parsed = document(case, &body);
    parsed
        .descendants()
        .find(|node| {
            node.is_element() && child_text(*node, "serviceName").as_deref() == Some(service_name)
        })
        .and_then(|node| child_text(node, "vpcEndpointPolicySupported"))
        .unwrap_or_else(|| panic!("case {case:?}: no entry for {service_name} in {body}"))
}

/// A VPC with one subnet, as `(vpcId, subnetId)` — what an interface endpoint is created in.
async fn network(case: &str, router: &MultiServiceRouter) -> (String, String) {
    let vpc = ok(
        case,
        router,
        "CreateVpc",
        &[param("CidrBlock", "10.90.0.0/16")],
    )
    .await;
    let vpc_id = element(&vpc, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: no vpcId in {vpc}"));
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.90.1.0/24"),
        ],
    )
    .await;
    let subnet_id = element(&subnet, "subnetId")
        .unwrap_or_else(|| panic!("case {case:?}: no subnetId in {subnet}"));
    (vpc_id, subnet_id)
}

/// The rule: what the catalogue reports is what the create is held to, service by service.
#[tokio::test]
async fn the_catalogue_and_the_create_agree_on_endpoint_policy_support() {
    // Rows: the services published with no endpoint-policy support, and ones published with it
    // — an interface service and the API service of the very same product, so the row is about
    // the SERVICE rather than about the product it belongs to.
    let rows: [(&str, bool); 4] = [
        ("appstream.streaming", false),
        ("iot.data", false),
        ("appstream.api", true),
        ("ec2", true),
    ];
    let router = super::ec2_query::emulator().0;
    let (vpc_id, subnet_id) = network("the network the endpoints go in", &router).await;

    for (label, supported) in rows {
        let case = label;
        let service_name = format!("com.amazonaws.{REGION}.{label}");
        assert_eq!(
            policy_supported(case, &router, &service_name).await,
            supported.to_string(),
            "case {case:?}: the catalogue must report whether {service_name} supports endpoint \
             policies"
        );

        let request = vec![
            param("VpcId", &vpc_id),
            param("ServiceName", &service_name),
            param("VpcEndpointType", "Interface"),
            param("SubnetId.1", &subnet_id),
            // The private DNS name of a service is a question of its own — it needs the VPC's
            // DNS attributes — and this suite is about the endpoint POLICY, so the endpoint is
            // created with it off.
            param("PrivateDnsEnabled", "false"),
            param("PolicyDocument", NARROWING_POLICY),
        ];
        let (status, body) = call(&router, "CreateVpcEndpoint", &request).await;
        if supported {
            assert_eq!(
                status, 200,
                "case {case:?}: a service that supports endpoint policies takes one, got \
                 {status} with body {body}"
            );
            assert!(
                element(&body, "vpcEndpointId").is_some(),
                "case {case:?}: expected the created endpoint reported, got {body}"
            );
        } else {
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, INVALID_PARAMETER),
                "case {case:?}: a narrowing policy on a service that supports none is refused, \
                 got {status} with body {body}"
            );
            assert!(
                error_message(&body).contains(&service_name),
                "case {case:?}: expected the refusal to name the service, got {message}",
                message = error_message(&body)
            );
        }
    }
}

/// The same catalogue read without narrowing it to one service reports each of those services
/// exactly once, with the support it has: the facts are the catalogue's own rather than a
/// property of how the request selected it.
#[tokio::test]
async fn the_published_catalogue_carries_the_same_facts_unfiltered() {
    let case = "the whole published catalogue";
    let router = super::ec2_query::emulator().0;
    let body = ok(
        case,
        &router,
        "DescribeVpcEndpointServices",
        &filters(&[("service-type", &["Interface"])]),
    )
    .await;
    let parsed = document(case, &body);
    for (label, supported) in [("appstream.streaming", "false"), ("appstream.api", "true")] {
        let service_name = format!("com.amazonaws.{REGION}.{label}");
        let entries: Vec<String> = parsed
            .descendants()
            .filter(|node| {
                node.is_element()
                    && child_text(*node, "serviceName").as_deref() == Some(service_name.as_str())
            })
            .filter_map(|node| child_text(node, "vpcEndpointPolicySupported"))
            .collect();
        assert_eq!(
            entries,
            vec![supported.to_string()],
            "case {label:?}: expected one entry for {service_name} reporting \
             vpcEndpointPolicySupported={supported}"
        );
    }
}
