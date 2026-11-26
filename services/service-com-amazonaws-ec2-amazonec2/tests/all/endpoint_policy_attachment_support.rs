// SPDX-License-Identifier: CC0-1.0

//! Which endpoint policy documents may be ATTACHED to a VPC endpoint, driven over `ec2Query`
//! (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a `PolicyDocument` that grants LESS than full access is refused with
//! HTTP 400 `InvalidParameter` whenever the endpoint service it would be attached to is one
//! `DescribeVpcEndpointServices` reports `VpcEndpointPolicySupported` `false` for — such an
//! endpoint allows full access to the service whatever document were attached to it
//! ("indicates whether the service supports endpoint policies",
//! `smithy_query(query="doc",shape="ServiceDetail",member="VpcEndpointPolicySupported")`), so a
//! narrowing document is refused rather than stored as a restriction nothing applies — and the
//! store is left exactly as it was. A document that GRANTS full access, however it is spelled,
//! asks for what such an endpoint already has and is served, as is a reset back to the default.
//!
//! The rows differ along what the rule generalises over: which call attaches the document
//! (`CreateVpcEndpoint` on an endpoint being created, `ModifyVpcEndpoint` on one that already
//! exists), whether the service is one Amazon Web Services publishes or one this account
//! configured, and the FORM the document is written in — a narrowing grant, a grant narrowed
//! only by a `Condition`, a grant to a named principal, a deny, and the full-access grant
//! written three ways. Whether a row is served is read off the CATALOGUE rather than written
//! down here, so the answer a client reads and the answer an attachment is held to cannot
//! disagree.

use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{element, elements, error_code, error_message, indexed, unescaped};
use super::wire::{child_text, document, param, Param};

/// The Region every call here is signed for; the service names below carry it.
const REGION: &str = "us-west-2";

/// The emulator the tests drive. The store is reached only over the wire here, so the engine
/// handle is not needed.
fn emulator() -> MultiServiceRouter {
    super::ec2_query::emulator().0
}

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

/// One element of a response body, or a panic naming the case and showing the body.
fn member(case: &str, body: &str, name: &str) -> String {
    element(body, name).unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The one VPC and subnet every endpoint below is created in.
struct Network {
    vpc_id: String,
    subnet_id: String,
}

/// Create that network through the API, so every endpoint is created against resources this
/// emulator really holds.
async fn network(case: &str, router: &MultiServiceRouter) -> Network {
    let vpc = ok(case, router, "CreateVpc", &[param("CidrBlock", "10.70.0.0/16")]).await;
    let vpc_id = member(case, &vpc, "vpcId");
    let subnet = ok(
        case,
        router,
        "CreateSubnet",
        &[
            param("VpcId", &vpc_id),
            param("CidrBlock", "10.70.1.0/24"),
            param("AvailabilityZone", &format!("{REGION}a")),
        ],
    )
    .await;
    let subnet_id = member(case, &subnet, "subnetId");
    Network { vpc_id, subnet_id }
}

/// An endpoint service this ACCOUNT configures, as the name a consumer creates an endpoint to.
///
/// A load balancer is a resource of another service, so the ARN is only ever a well-formed ARN
/// here: what this crate stores is the ARN the request named.
async fn account_endpoint_service(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(
        case,
        router,
        "CreateVpcEndpointServiceConfiguration",
        &[
            param(
                "NetworkLoadBalancerArn.1",
                &format!(
                    "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/\
                     policy-support/0123456789abcdef",
                    account = engine::emulator_account_id(),
                ),
            ),
            // Acceptance is waived so the endpoints below are created without a second call,
            // which is a question of its own and not this suite's.
            param("AcceptanceRequired", "false"),
        ],
    )
    .await;
    member(case, &body, "serviceName")
}

/// The `VpcEndpointPolicySupported` the catalogue reports for `service_name`, read through the
/// XML tree off the entry that names that service — the one answer an attachment is held to.
async fn policy_supported(case: &str, router: &MultiServiceRouter, service_name: &str) -> bool {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointServices",
        &indexed("ServiceName", &[service_name]),
    )
    .await;
    let parsed = document(case, &body);
    let reported = parsed
        .descendants()
        .find(|node| {
            node.is_element() && child_text(*node, "serviceName").as_deref() == Some(service_name)
        })
        .and_then(|node| child_text(node, "vpcEndpointPolicySupported"))
        .unwrap_or_else(|| panic!("case {case:?}: no entry for {service_name} in {body}"));
    match reported.as_str() {
        "true" => true,
        "false" => false,
        other => panic!("case {case:?}: expected a boolean support flag, got {other:?}"),
    }
}

/// Every VPC endpoint the account holds, by id, in the order the describe reports them — what a
/// refusal must leave exactly as it found it.
async fn endpoint_ids(case: &str, router: &MultiServiceRouter) -> Vec<String> {
    let body = ok(case, router, "DescribeVpcEndpoints", &[]).await;
    elements(&body, "vpcEndpointId")
}

/// The document `DescribeVpcEndpoints` reports for `endpoint_id`, as the bytes it was stored as.
async fn stored_policy(case: &str, router: &MultiServiceRouter, endpoint_id: &str) -> String {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpoints",
        &indexed("VpcEndpointId", &[endpoint_id]),
    )
    .await;
    unescaped(&member(case, &body, "policyDocument"))
}

/// The refusal a narrowing document on such a service is answered with, asserted whole: the
/// status, the EC2 code and the message naming the service the request asked for.
fn assert_refused(case: &str, status: u16, body: &str, service_name: &str) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(body),
        ec2::state::INVALID_PARAMETER,
        "case {case:?}: expected {code}, got {body}",
        code = ec2::state::INVALID_PARAMETER
    );
    assert_eq!(
        error_message(body),
        format!("Service {service_name} only supports the full-access endpoint policy."),
        "case {case:?}: the refusal must name the service the request asked for, got {body}"
    );
}

/// One policy document a row attaches, and whether it grants full access — every action, by
/// every principal, on every resource, and nothing else stated.
struct PolicyCase {
    case: &'static str,
    document: &'static str,
    grants_full_access: bool,
}

/// The documents, differing in FORM rather than only in value: three that narrow what the
/// endpoint may reach (by action, by principal — a member written as a structure rather than a
/// string — and by effect), one that grants everything but only for the requests a `Condition`
/// matches, and three spellings of the full-access grant (compact with a policy-grammar date,
/// each grant a single-entry list, and one statement written as a structure rather than a list,
/// carrying the `Sid` that only names it).
fn policy_cases() -> Vec<PolicyCase> {
    vec![
        PolicyCase {
            case: "a grant of one action",
            document: r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"iot:Connect","Resource":"*"}]}"#,
            grants_full_access: false,
        },
        PolicyCase {
            case: "a grant to one named principal",
            document: r#"{"Statement":[{"Effect":"Allow","Principal":{"AWS":"arn:aws:iam::123456789012:root"},"Action":"*","Resource":"*"}]}"#,
            grants_full_access: false,
        },
        PolicyCase {
            case: "a deny of everything",
            document: r#"{"Statement":[{"Effect":"Deny","Principal":"*","Action":"*","Resource":"*"}]}"#,
            grants_full_access: false,
        },
        PolicyCase {
            case: "a full grant narrowed only by a condition",
            document: r#"{"Statement":[{"Effect":"Allow","Principal":"*","Action":"*","Resource":"*","Condition":{"StringEquals":{"aws:PrincipalAccount":"123456789012"}}}]}"#,
            grants_full_access: false,
        },
        PolicyCase {
            case: "the full-access grant, written compactly",
            document: r#"{"Version":"2008-10-17","Statement":[{"Effect":"Allow","Principal":"*","Action":"*","Resource":"*"}]}"#,
            grants_full_access: true,
        },
        PolicyCase {
            case: "the full-access grant, each member a single-entry list",
            document: r#"{"Statement":[{"Effect":"Allow","Principal":["*"],"Action":["*"],"Resource":["*"]}]}"#,
            grants_full_access: true,
        },
        PolicyCase {
            case: "the full-access grant, one named statement rather than a list",
            document: r#"{"Statement":{"Sid":"FullAccess","Effect":"Allow","Principal":"*","Action":"*","Resource":"*"}}"#,
            grants_full_access: true,
        },
    ]
}

/// The endpoint services every row below attaches a document to: two the catalogue publishes
/// with no endpoint-policy support (the AWS IoT Core data plane and the Amazon AppStream 2.0
/// streaming service), one it publishes with it, and the one this account configured — so the
/// rows span both owners a name can carry.
async fn service_rows(case: &str, router: &MultiServiceRouter) -> Vec<(&'static str, String)> {
    vec![
        (
            "a published data-plane service",
            format!("com.amazonaws.{REGION}.iot.data"),
        ),
        (
            "a published streaming service",
            format!("com.amazonaws.{REGION}.appstream.streaming"),
        ),
        (
            "a published API service",
            format!("com.amazonaws.{REGION}.logs"),
        ),
        (
            "the account's own endpoint service",
            account_endpoint_service(case, router).await,
        ),
    ]
}

/// Assert the rows covered both answers the catalogue can give, so neither test passes because
/// every service happened to answer the same way.
fn assert_both_answers_covered(supported: usize, unsupported: usize) {
    assert!(
        supported > 0 && unsupported > 0,
        "the rows must cover both answers: {supported} service(s) supporting an endpoint \
         policy and {unsupported} supporting none"
    );
}

/// `CreateVpcEndpoint`: a document that grants less than full access is refused for a service
/// the catalogue reports no endpoint-policy support for, and nothing is stored; every other
/// row is served and the endpoint carries the document as supplied.
#[tokio::test]
async fn a_create_may_attach_only_what_the_service_supports() {
    let router = emulator();
    let built = network("the create table", &router).await;
    let services = service_rows("the create table", &router).await;
    let mut supported_services = 0usize;
    let mut unsupported_services = 0usize;

    for (service_case, service_name) in &services {
        let supported = policy_supported(service_case, &router, service_name).await;
        if supported {
            supported_services += 1;
        } else {
            unsupported_services += 1;
        }
        for row in policy_cases() {
            let case = format!("{service_case}, {}", row.case);
            let held = endpoint_ids(&case, &router).await;
            let mut params = vec![
                param("VpcId", &built.vpc_id),
                param("ServiceName", service_name),
                param("VpcEndpointType", "Interface"),
                param("PolicyDocument", row.document),
                // The private hosted zone is a question of its own; this suite is about the
                // policy, so no endpoint here asks for one.
                param("PrivateDnsEnabled", "false"),
            ];
            params.extend(indexed("SubnetId", &[built.subnet_id.as_str()]));
            let (status, body) = call(&router, "CreateVpcEndpoint", &params).await;

            if supported || row.grants_full_access {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the create to be served, got {status} with body \
                     {body}"
                );
                let endpoint_id = member(&case, &body, "vpcEndpointId");
                let answered = unescaped(&member(&case, &body, "policyDocument"));
                assert_eq!(
                    answered, row.document,
                    "case {case:?}: expected the document as supplied, got {answered:?}"
                );
                let read_back = stored_policy(&case, &router, &endpoint_id).await;
                assert_eq!(
                    read_back, row.document,
                    "case {case:?}: a read must return the document the endpoint carries, got \
                     {read_back:?}"
                );
                let now_held = endpoint_ids(&case, &router).await;
                assert_eq!(
                    now_held.len(),
                    held.len() + 1,
                    "case {case:?}: expected exactly one endpoint added, got {now_held:?}"
                );
            } else {
                assert_refused(&case, status, &body, service_name);
                let now_held = endpoint_ids(&case, &router).await;
                assert_eq!(
                    now_held, held,
                    "case {case:?}: a refused create must store nothing, got {now_held:?}"
                );
            }
        }
    }
    assert_both_answers_covered(supported_services, unsupported_services);
}

/// `ModifyVpcEndpoint`: the same rule on an endpoint that already exists — a narrowing document
/// is refused for a service with no endpoint-policy support and the endpoint keeps the document
/// it carried, a full-access document is served, and a reset back to the default is served too,
/// the rule being about what the document GRANTS rather than about touching the policy at all.
#[tokio::test]
async fn a_modify_may_attach_only_what_the_service_supports() {
    let router = emulator();
    let built = network("the modify table", &router).await;
    let services = service_rows("the modify table", &router).await;
    let mut supported_services = 0usize;
    let mut unsupported_services = 0usize;

    for (service_case, service_name) in &services {
        let supported = policy_supported(service_case, &router, service_name).await;
        if supported {
            supported_services += 1;
        } else {
            unsupported_services += 1;
        }
        // The endpoint each row modifies: created naming no document at all, so it starts out
        // carrying the one the service installs on an endpoint of its type.
        let mut params = vec![
            param("VpcId", &built.vpc_id),
            param("ServiceName", service_name),
            param("VpcEndpointType", "Interface"),
            param("PrivateDnsEnabled", "false"),
        ];
        params.extend(indexed("SubnetId", &[built.subnet_id.as_str()]));
        let created = ok(service_case, &router, "CreateVpcEndpoint", &params).await;
        let endpoint_id = member(service_case, &created, "vpcEndpointId");
        let default_policy = stored_policy(service_case, &router, &endpoint_id).await;

        for row in policy_cases() {
            let case = format!("{service_case}, {}", row.case);
            let carried = stored_policy(&case, &router, &endpoint_id).await;
            let (status, body) = call(
                &router,
                "ModifyVpcEndpoint",
                &[
                    param("VpcEndpointId", &endpoint_id),
                    param("PolicyDocument", row.document),
                ],
            )
            .await;
            if supported || row.grants_full_access {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected the modify to be served, got {status} with body \
                     {body}"
                );
                assert_eq!(
                    element(&body, "return").as_deref(),
                    Some("true"),
                    "case {case:?}: expected <return>true</return>, got {body}"
                );
                let read_back = stored_policy(&case, &router, &endpoint_id).await;
                assert_eq!(
                    read_back, row.document,
                    "case {case:?}: expected the endpoint to carry the document the modify \
                     named, got {read_back:?}"
                );
            } else {
                assert_refused(&case, status, &body, service_name);
                let read_back = stored_policy(&case, &router, &endpoint_id).await;
                assert_eq!(
                    read_back, carried,
                    "case {case:?}: a refused modify must leave the policy as it was \
                     ({carried:?}), got {read_back:?}"
                );
            }
        }

        // "(Gateway endpoint) Specify true to reset the policy document to the default policy.
        // The default policy allows full access to the service"
        // (`smithy_query(query="doc",shape="ModifyVpcEndpointRequest",member="ResetPolicy")`):
        // the default grants full access, which is what an endpoint to a service supporting no
        // endpoint policy already has, so a reset is served whatever that support — the refusal
        // above is about what a document GRANTS rather than about touching the policy at all.
        let reset_case = format!("{service_case}, a reset back to the default policy");
        let reset = ok(
            &reset_case,
            &router,
            "ModifyVpcEndpoint",
            &[
                param("VpcEndpointId", &endpoint_id),
                param("ResetPolicy", "true"),
            ],
        )
        .await;
        assert_eq!(
            element(&reset, "return").as_deref(),
            Some("true"),
            "case {reset_case:?}: expected <return>true</return>, got {reset}"
        );
        let read_back = stored_policy(&reset_case, &router, &endpoint_id).await;
        assert_eq!(
            read_back, default_policy,
            "case {reset_case:?}: expected the default document back, got {read_back:?}"
        );
    }
    assert_both_answers_covered(supported_services, unsupported_services);
}
