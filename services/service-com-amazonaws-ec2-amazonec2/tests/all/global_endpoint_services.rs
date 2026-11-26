// SPDX-License-Identifier: CC0-1.0

//! What `DescribeVpcEndpointServices` reports for the services Amazon Web Services publishes
//! itself: the names a caller can resolve, and the DNS names one service carries.
//!
//! The rules under test:
//!
//! 1. A service name's second component is the SCOPE the service belongs to: the Region for a
//!    service each Region offers of its own, and `global` for one Amazon Web Services answers
//!    from a single estate. A globally scoped name is a name every Region's endpoint resolves
//!    — reported in the catalog, answered when `ServiceNames` names it, and reachable by a
//!    created endpoint — rather than refused as naming no service.
//! 2. `ServiceNames` and `ServiceDetails` report the SAME selection, so a caller resolving a
//!    service by name gets that service in both.
//! 3. `PrivateDnsNames` carries every name the service publishes for its one default public
//!    DNS name: a service answering on `<host>.<region>.amazonaws.com` answers the same API on
//!    its dual-stack `<host>.<region>.api.aws` name, and a service already under that domain
//!    publishes the one name it has.
//! 4. A reported service is available in the zones the Region HAS, by name and by this
//!    account's zone ids.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, indexed};

/// The emulator the tests drive.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// One `DescribeVpcEndpointServices` call, answered as the status and the body.
async fn describe(
    router: &MultiServiceRouter,
    params: &[(String, String)],
) -> (u16, String) {
    call(router, REGION, "DescribeVpcEndpointServices", params).await
}

/// The `<serviceNameSet>` entries of a response.
fn service_names(case: &str, body: &str) -> Vec<String> {
    let parsed = super::wire::document(case, body);
    let Some(list) = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "serviceNameSet")
    else {
        return Vec::new();
    };
    list.children()
        .filter(roxmltree::Node::is_element)
        .map(|item| item.text().unwrap_or_default().to_string())
        .collect()
}

/// The one `<serviceDetailSet>` entry whose `<serviceName>` is `service_name`, as the XML of
/// that entry alone.
fn service_detail(case: &str, body: &str, service_name: &str) -> String {
    let parsed = super::wire::document(case, body);
    let item = parsed
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "item")
        .find(|item| {
            super::wire::child_text(*item, "serviceName").as_deref() == Some(service_name)
        })
        .unwrap_or_else(|| panic!("case {case:?}: no detail for {service_name} in {body}"));
    // The detail is handed back as its own XML so the readers below walk one entry's children.
    let range = item.range();
    body.get(range).unwrap_or_default().to_string()
}

/// Rules 1 and 2: a globally scoped name resolves, in both halves of the answer.
#[tokio::test]
async fn a_globally_scoped_service_name_resolves() {
    let router = emulator();
    let global = "aws.api.global.codecatalyst";
    let (status, body) = describe(&router, &indexed("ServiceName", &[global])).await;
    assert_eq!(
        status, 200,
        "case {global:?}: expected HTTP 200, got {status} {body}"
    );
    assert_eq!(
        service_names(global, &body),
        vec![global.to_string()],
        "case {global:?}: expected the named service back, got {body}"
    );
    let detail = service_detail(global, &body, global);
    assert!(
        detail.contains("<serviceId>vpce-svc-"),
        "case {global:?}: the detail carries the service's id, got {detail}"
    );

    // Rule 1 again: the same name is in the unfiltered catalog of the Region.
    let (status, body) = describe(&router, &[]).await;
    assert_eq!(status, 200, "the catalog must be reported, got {status} {body}");
    assert!(
        service_names("catalog", &body).iter().any(|name| name == global),
        "expected {global} in the Region's catalog, got {body}"
    );
}

/// Rule 1's other half: a name whose scope is neither a Region nor `global` names no service,
/// and is refused rather than answered with an empty set.
#[tokio::test]
async fn a_name_of_no_scope_names_no_service() {
    let router = emulator();
    let cases = [
        ("an unpublished global label", "aws.api.global.not-a-service"),
        ("a scope that is no Region", "com.amazonaws.nowhere.logs"),
        ("another Region's scope", "com.amazonaws.eu-west-1.logs"),
    ];

    for (case, service_name) in cases {
        let (status, body) = describe(&router, &indexed("ServiceName", &[service_name])).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for {service_name}, got {status} {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "InvalidServiceName",
            "case {case:?}: expected InvalidServiceName, got {body}"
        );
    }
}

/// Rules 3 and 4 over the two name families: a service under `com.amazonaws` publishes its
/// `amazonaws.com` name AND the dual-stack `api.aws` one, a service already under `api.aws`
/// publishes the one name it has, and each is available in every zone the Region has.
#[tokio::test]
async fn a_service_publishes_every_private_dns_name_it_answers_on() {
    let router = emulator();
    // (the case, the service name, the private DNS names it publishes)
    let cases: [(&str, &str, &[&str]); 3] = [
        (
            "a com.amazonaws service",
            "com.amazonaws.us-west-2.access-analyzer",
            &[
                "access-analyzer.us-west-2.amazonaws.com",
                "access-analyzer.us-west-2.api.aws",
            ],
        ),
        (
            "another com.amazonaws service",
            "com.amazonaws.us-west-2.logs",
            &["logs.us-west-2.amazonaws.com", "logs.us-west-2.api.aws"],
        ),
        (
            "a globally scoped aws.api service",
            "aws.api.global.codecatalyst",
            &["codecatalyst.us-west-2.api.aws"],
        ),
    ];

    for (case, service_name, names) in cases {
        let (status, body) = describe(&router, &indexed("ServiceName", &[service_name])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {body}"
        );
        let detail = service_detail(case, &body, service_name);
        let set = super::ec2_query::element(&detail, "privateDnsNameSet")
            .unwrap_or_else(|| panic!("case {case:?}: no <privateDnsNameSet> in {detail}"));
        let published: Vec<String> = super::ec2_query::elements(&set, "privateDnsName");
        let expected: Vec<String> = names.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            published, expected,
            "case {case:?}: expected the private DNS names {expected:?}, got {detail}"
        );

        // Rule 4: the zones the Region has, by name and by this account's ids.
        for (member, values) in [
            (
                "availabilityZoneSet",
                vec!["us-west-2a", "us-west-2b", "us-west-2c", "us-west-2d"],
            ),
            (
                "availabilityZoneIdSet",
                vec!["usw2-az2", "usw2-az1", "usw2-az3", "usw2-az4"],
            ),
        ] {
            let set = super::ec2_query::element(&detail, member)
                .unwrap_or_else(|| panic!("case {case:?}: no <{member}> in {detail}"));
            for value in &values {
                assert!(
                    set.contains(value),
                    "case {case:?}: expected {value} in <{member}>, got {set}"
                );
            }
        }
    }
}
