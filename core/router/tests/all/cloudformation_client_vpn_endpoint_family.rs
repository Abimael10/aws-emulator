// SPDX-License-Identifier: CC0-1.0

//! The ASSEMBLED emulator serves a CloudFormation stack that declares the AWS Client VPN
//! family of Amazon EC2 — an `AWS::EC2::ClientVpnEndpoint` and the
//! `AWS::EC2::ClientVpnAuthorizationRule` that grants ingress on it: both resources are
//! created in Amazon EC2's own control plane against certificates Certificate Manager holds,
//! the stack publishes the identity EC2 minted, deleting the stack takes them away — and a
//! declaration EC2 REFUSES fails the resource with that service's own refusal and rolls the
//! stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error.** Three crates have to agree for that to hold
//! (CloudFormation submits the template and makes the calls, Amazon EC2 serves them, and
//! Certificate Manager holds the certificates the endpoint presents), so the suite lives here:
//! a crate-local test can only ever see one side of it.
//!
//! The deploy rows differ along what the rule generalises over: the size of the client address
//! range (the two published prefix lengths and one between them), the transport protocol and
//! port, whether the grant is to every client or to one identity-provider group, whether the
//! declaration carries a description and its own tags, and which published return value the
//! output reads. The refusal rows differ along the CLASS of declaration EC2 refuses: a client
//! range SMALLER than the documented minimum, one LARGER than the documented maximum, and a
//! certificate ARN the account holds no certificate for. Each is the owning service's own
//! refusal, reported on the resource that failed — never the `Template format error:
//! Unrecognized resource types` a type CloudFormation does not know is refused with, which no
//! row of either table may ever see.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The region every request here is signed for, and so the region the stack's resources
/// belong to.
const REGION: &str = "us-east-1";

/// How long the suite is willing to wait for a stack operation to settle, and how often it
/// looks again. Generous: the stack holds its authorization rule open until Amazon EC2
/// reports the rule in force, and the suite waits for the CONDITION (the stack's status, the
/// endpoint's existence) rather than for a fixed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(100);

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for two types AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The two resource types under test.
const ENDPOINT_TYPE: &str = "AWS::EC2::ClientVpnEndpoint";
const RULE_TYPE: &str = "AWS::EC2::ClientVpnAuthorizationRule";

/// The `ClientVpnAuthenticationType` of the mutual-authentication option, which is the one
/// that names a client root certificate chain
/// (`smithy_query(query="shape",shape="ClientVpnAuthenticationType")`).
const CERTIFICATE_AUTHENTICATION: &str = "certificate-authentication";

/// The id prefix Amazon EC2 mints a Client VPN endpoint with, which is what this type's `Ref`
/// returns.
const ENDPOINT_ID_PREFIX: &str = "cvpn-endpoint-";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Call one Amazon EC2 action over its own ec2Query wire — the service that OWNS both types,
/// asked directly so what the stack claims is checked against that service's own answer.
async fn ec2(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2016-11-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One certificate created through Certificate Manager's OWN `RequestCertificate`, answering
/// the ARN that service minted for it — so the endpoint presents a certificate the owning
/// service really holds rather than one this suite made up.
async fn certificate(router: &MultiServiceRouter, case: &str, domain: &str) -> String {
    let body = json!({ "DomainName": domain, "ValidationMethod": "DNS" }).to_string();
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", "CertificateManager.RequestCertificate")
        .header("authorization", authorization("acm"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from RequestCertificate, got {status} with \
         {answered}"
    );
    answered["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {answered}"))
        .to_string()
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// The `DescribeStackEvents` body for a stack: where the per-resource failure REASON the
/// owning service answered is reported, which is what a rollback has to carry.
async fn stack_events(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let (status, body) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
         {body}"
    );
    body
}

/// The endpoints Amazon EC2 reports for `endpoint_id`, as the describe's own bodies — the
/// empty vector when it reports none, and a refusal when it holds no such endpoint.
async fn described_endpoint(
    router: &MultiServiceRouter,
    endpoint_id: &str,
) -> (u16, String) {
    ec2(
        router,
        "DescribeClientVpnEndpoints",
        &[("ClientVpnEndpointId.1", endpoint_id)],
    )
    .await
}

/// The networks Amazon EC2 reports authorization rules for on `endpoint_id`.
async fn authorized_networks(
    router: &MultiServiceRouter,
    case: &str,
    endpoint_id: &str,
) -> Vec<String> {
    let (status, body) = ec2(
        router,
        "DescribeClientVpnAuthorizationRules",
        &[("ClientVpnEndpointId", endpoint_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeClientVpnAuthorizationRules, got \
         {status} with body {body}"
    );
    let mut networks = elements(&body, "destinationCidr");
    networks.sort();
    networks
}

/// One row of the deploy table: the stack, the client range and wire options the endpoint is
/// declared with, and the grant its authorization rule carries.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// "Client CIDR range must have a size of at least /22 and must not be greater than /12"
    /// (`CreateClientVpnEndpointRequest$ClientCidrBlock`) — the rows span both published
    /// bounds and a size between them.
    client_cidr_block: &'static str,
    transport_protocol: Option<&'static str>,
    vpn_port: Option<&'static str>,
    description: Option<&'static str>,
    /// The network the rule grants, and the group it grants it to — `None` for the rule that
    /// grants every client access.
    network: &'static str,
    group: Option<&'static str>,
    /// Whether the endpoint's declaration carries its own `TagSpecifications`.
    declares_tags: bool,
}

/// The deploys the round trip is written against. They differ along every axis the rule
/// generalises over: the client range's size (the longest published prefix, one between the
/// bounds, and the shortest), the transport protocol and port, whether a description and the
/// type's own tags are declared, and whether the ingress is granted to one group or to every
/// client.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the smallest published client range, granted to every client",
            stack_name: "client-vpn-smallest-range",
            client_cidr_block: "10.100.0.0/22",
            transport_protocol: None,
            vpn_port: None,
            description: Some("the remote access endpoint"),
            network: "10.0.0.0/16",
            group: None,
            declares_tags: true,
        },
        Row {
            case: "a range between the bounds over TCP on the other published port",
            stack_name: "client-vpn-tcp-range",
            client_cidr_block: "172.16.0.0/16",
            transport_protocol: Some("tcp"),
            vpn_port: Some("1194"),
            description: None,
            network: "192.168.0.0/24",
            group: Some("engineering"),
            declares_tags: false,
        },
        Row {
            case: "the largest published client range, granted to one group",
            stack_name: "client-vpn-largest-range",
            client_cidr_block: "10.0.0.0/12",
            transport_protocol: Some("udp"),
            vpn_port: Some("443"),
            description: Some("the whole estate"),
            network: "203.0.113.0/24",
            group: Some("S-1-5-21-1004336348-1177238915-682003330-512"),
            declares_tags: true,
        },
    ]
}

/// The template one row deploys: the endpoint, the authorization rule that references it, and
/// the outputs that read each type's published `Ref`.
fn template(row: &Row, server_certificate: &str, client_certificate: &str) -> String {
    let mut endpoint = json!({
        "ClientCidrBlock": row.client_cidr_block,
        "ServerCertificateArn": server_certificate,
        "AuthenticationOptions": [{
            "Type": CERTIFICATE_AUTHENTICATION,
            "MutualAuthentication": {"ClientRootCertificateChainArn": client_certificate},
        }],
        "ConnectionLogOptions": {"Enabled": false},
    });
    if let Some(protocol) = row.transport_protocol {
        endpoint["TransportProtocol"] = json!(protocol);
    }
    if let Some(port) = row.vpn_port {
        endpoint["VpnPort"] = json!(port);
    }
    if let Some(description) = row.description {
        endpoint["Description"] = json!(description);
    }
    if row.declares_tags {
        endpoint["TagSpecifications"] = json!([{
            "ResourceType": "client-vpn-endpoint",
            "Tags": [{"Key": "team", "Value": "network"}],
        }]);
    }
    let mut rule = json!({
        "ClientVpnEndpointId": {"Ref": "Endpoint"},
        "TargetNetworkCidr": row.network,
        "Description": "the authorized network",
    });
    match row.group {
        Some(group) => rule["AccessGroupId"] = json!(group),
        None => rule["AuthorizeAllGroups"] = json!(true),
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Endpoint": {"Type": ENDPOINT_TYPE, "Properties": endpoint},
            "Rule": {"Type": RULE_TYPE, "Properties": rule},
        },
        "Outputs": {
            "EndpointRef": {"Value": {"Ref": "Endpoint"}},
            "RuleRef": {"Value": {"Ref": "Rule"}},
        }
    })
    .to_string()
}

/// The deploy table: every row reaches `CREATE_COMPLETE`, publishes the endpoint id Amazon
/// EC2 minted, and is backed by that service's own reports of the endpoint and of the rule —
/// and deleting the stack takes both away.
#[tokio::test]
async fn a_stack_deploys_a_client_vpn_endpoint_and_its_authorization_rule() {
    for row in rows() {
        let case = row.case;
        let router = build_router();
        let server = certificate(&router, case, "vpn.example.com").await;
        let client = certificate(&router, case, "clients.example.com").await;
        let body = template(&row, &server, &client);

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &body)],
        )
        .await;
        assert!(
            !created.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected both types to be recognised, got {created}"
        );
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack — {ENDPOINT_TYPE} and \
             {RULE_TYPE} are types a template may declare — got {status} with body {created}"
        );

        let settled = settled_stack(&router, case, row.stack_name).await;
        assert_eq!(
            opt_element(&settled, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {settled} with the events \
             {events}",
            events = stack_events(&router, case, row.stack_name).await
        );
        let outputs = outputs_of(&settled);
        let endpoint_id = outputs
            .get("EndpointRef")
            .unwrap_or_else(|| panic!("case {case:?}: no EndpointRef output in {settled}"))
            .clone();
        assert!(
            endpoint_id.starts_with(ENDPOINT_ID_PREFIX),
            "case {case:?}: expected the id Amazon EC2 mints a Client VPN endpoint with, got \
             {endpoint_id:?}"
        );
        assert_eq!(
            outputs.get("RuleRef").map(String::as_str),
            Some(format!("{endpoint_id}|{network}", network = row.network).as_str()),
            "case {case:?}: an authorization rule's Ref is the endpoint and the network it \
             grants, got {outputs:?}"
        );

        // The resource the stack reports is the one that service holds, under the identity
        // the stack published.
        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        assert_eq!(
            physical_ids_of(&resources).get("Endpoint"),
            Some(&endpoint_id),
            "case {case:?}: expected the stack's endpoint resource to report the id EC2 \
             minted, got {resources}"
        );

        let (status, reported) = described_endpoint(&router, &endpoint_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected Amazon EC2 to report the endpoint, got {status} with \
             body {reported}"
        );
        assert_eq!(
            elements(&reported, "clientCidrBlock"),
            vec![row.client_cidr_block.to_string()],
            "case {case:?}: expected the client range the template declared, got {reported}"
        );
        assert_eq!(
            elements(&reported, "transportProtocol"),
            vec![row.transport_protocol.unwrap_or("udp").to_string()],
            "case {case:?}: expected the transport protocol in force, got {reported}"
        );
        assert_eq!(
            elements(&reported, "vpnPort"),
            vec![row.vpn_port.unwrap_or("443").to_string()],
            "case {case:?}: expected the port in force, got {reported}"
        );
        assert_eq!(
            elements(&reported, "serverCertificateArn"),
            vec![server.clone()],
            "case {case:?}: expected the certificate Certificate Manager holds, got {reported}"
        );
        // The three tags CloudFormation gives every resource it provisions name the stack and
        // the resource's place in it, and they reach Amazon EC2 on the create's own tag
        // specification: a service provisioning a resource for a caller is what SETS those
        // tags, so EC2 takes them on that in-process path.
        for key in [
            "aws:cloudformation:stack-name",
            "aws:cloudformation:stack-id",
            "aws:cloudformation:logical-id",
        ] {
            assert!(
                reported.contains(key),
                "case {case:?}: expected the endpoint to carry the propagated tag {key}, got \
                 {reported}"
            );
        }
        if row.declares_tags {
            assert!(
                reported.contains("network"),
                "case {case:?}: expected the endpoint to carry the declared tag, got {reported}"
            );
        }

        // The rule the stack claims to have granted is a rule that service holds, in force.
        let networks = authorized_networks(&router, case, &endpoint_id).await;
        assert_eq!(
            networks,
            vec![row.network.to_string()],
            "case {case:?}: expected the network the template authorized, got {networks:?}"
        );
        let (status, rules) = ec2(
            &router,
            "DescribeClientVpnAuthorizationRules",
            &[("ClientVpnEndpointId", &endpoint_id)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeClientVpnAuthorizationRules, got \
             {status} with body {rules}"
        );
        assert_eq!(
            element(&rules, "code"),
            "active",
            "case {case:?}: the stack is complete only once the rule is in force, got {rules}"
        );
        assert_eq!(
            elements(&rules, "accessAll"),
            vec![if row.group.is_none() { "true" } else { "false" }.to_string()],
            "case {case:?}: expected the grant the template declared, got {rules}"
        );
        assert_eq!(
            elements(&rules, "groupId"),
            row.group.map(str::to_string).into_iter().collect::<Vec<_>>(),
            "case {case:?}: expected the group the template granted the network to, got {rules}"
        );

        // ...and both are gone with the stack, which is what makes a stack's resources the
        // stack's own rather than leftovers.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", row.stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body \
             {deleted}"
        );
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, reported) = described_endpoint(&router, &endpoint_id).await;
            // An endpoint EC2 has taken down is reported `deleting` and then `deleted`, which
            // `ClientVpnEndpointStatusCode` declares — so what says the stack released it is
            // the endpoint leaving the states a live endpoint holds.
            let released = status != 200
                || elements(&reported, "code")
                    .iter()
                    .all(|code| code == "deleting" || code == "deleted");
            if released {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected the endpoint {endpoint_id:?} to be released with the \
                 stack, it is still held: {reported}"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
    }
}

/// One row of the refusal table: the stack, the declaration Amazon EC2 refuses, and the text
/// its refusal carries.
struct RefusalRow {
    case: &'static str,
    stack_name: &'static str,
    /// The client range the endpoint is declared with.
    client_cidr_block: &'static str,
    /// Whether the server certificate the endpoint names is one Certificate Manager holds.
    certificate_is_held: bool,
    /// A fragment the failed resource's reason must carry — the owning service's own words.
    reason_carries: &'static str,
}

/// The refusals the rollback table is written against: a client range SMALLER than the
/// documented minimum, one LARGER than the documented maximum, and a certificate ARN the
/// account holds no certificate for. Each is Amazon EC2's own refusal of the declaration.
fn refusal_rows() -> Vec<RefusalRow> {
    vec![
        RefusalRow {
            case: "a client range below the documented minimum size",
            stack_name: "client-vpn-range-too-small",
            client_cidr_block: "10.0.0.0/28",
            certificate_is_held: true,
            reason_carries: "Client cidr block must be at least a /22 range",
        },
        RefusalRow {
            case: "a client range above the documented maximum size",
            stack_name: "client-vpn-range-too-large",
            client_cidr_block: "10.0.0.0/8",
            certificate_is_held: true,
            reason_carries: "Client cidr block must be at most a /12 range",
        },
        RefusalRow {
            case: "a server certificate the account does not hold",
            stack_name: "client-vpn-absent-certificate",
            client_cidr_block: "10.100.0.0/22",
            certificate_is_held: false,
            reason_carries: "Certificate not found",
        },
    ]
}

/// The refusal table: a declaration Amazon EC2 refuses fails the ENDPOINT resource with that
/// service's own refusal and rolls the stack back — it is never a template-format error, and
/// nothing is left behind in EC2.
#[tokio::test]
async fn a_declaration_amazon_ec2_refuses_rolls_the_stack_back_with_its_own_reason() {
    for row in refusal_rows() {
        let case = row.case;
        let router = build_router();
        let held = certificate(&router, case, "vpn.example.com").await;
        let client = certificate(&router, case, "clients.example.com").await;
        let server = if row.certificate_is_held {
            held
        } else {
            format!("arn:aws:acm:{REGION}:000000000000:certificate/{}", "0".repeat(36))
        };
        let body = json!({
            "Resources": {
                "Endpoint": {
                    "Type": ENDPOINT_TYPE,
                    "Properties": {
                        "ClientCidrBlock": row.client_cidr_block,
                        "ServerCertificateArn": server,
                        "AuthenticationOptions": [{
                            "Type": CERTIFICATE_AUTHENTICATION,
                            "MutualAuthentication": {
                                "ClientRootCertificateChainArn": client,
                            },
                        }],
                        "ConnectionLogOptions": {"Enabled": false},
                    }
                }
            }
        })
        .to_string();

        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &body)],
        )
        .await;
        assert!(
            !created.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: a declaration the owning service refuses is still a type the \
             template may declare, got {created}"
        );
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body \
             {created}"
        );

        let settled = settled_stack(&router, case, row.stack_name).await;
        assert_eq!(
            opt_element(&settled, "StackStatus").as_deref(),
            Some("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the refusal to roll the stack back, got {settled}"
        );

        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let events = stack_events(&router, case, row.stack_name).await;
        assert!(
            events.contains(row.reason_carries),
            "case {case:?}: expected the resource's failure to carry Amazon EC2's own reason \
             ({reason:?}), got {events}",
            reason = row.reason_carries
        );

        // Nothing was created in EC2: a resource the owning service refused is not one the
        // stack may report as made.
        let (status, reported) = ec2(&router, "DescribeClientVpnEndpoints", &[]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeClientVpnEndpoints, got {status} \
             with body {reported}"
        );
        assert!(
            elements(&reported, "clientVpnEndpointId").is_empty(),
            "case {case:?}: expected the account to hold no Client VPN endpoint, got {reported}"
        );
    }
}
