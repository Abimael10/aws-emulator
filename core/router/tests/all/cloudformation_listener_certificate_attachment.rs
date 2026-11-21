// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ElasticLoadBalancingV2::ListenerCertificate` a stack declares, across the three
//! services that span: CloudFormation deploys the resource, Elastic Load Balancing v2 owns the
//! listener's certificate list, and Certificate Manager owns the certificates themselves — all
//! served by ONE router over ONE engine, so what is asserted is what a client holding the three
//! SDKs sees.
//!
//! RULE: a declared resource that is no resource of its owning service but a BINDING — an entry
//! of a collection that service keeps — is deployed by the calls that add and remove one, so
//! after the stack reports the resource created that service's own read reports the binding;
//! a deployment that changes it adds what the declaration now names and removes what it no
//! longer names; a deployment whose binding that service REFUSES fails the resource with that
//! service's own refusal and leaves the binding in force untouched; and the stack's delete
//! removes exactly what the stack added. For this type the collection is a listener's
//! certificate list, the two calls are `AddListenerCertificates` and
//! `RemoveListenerCertificates`, and the read is `DescribeListenerCertificates` — which reports
//! "the default certificate and the certificate list for the specified HTTPS or TLS listener",
//! `IsDefault` telling the two apart.
//!
//! The rows differ along what the rule generalises over: how MANY certificates the resource
//! declares (one, and two — so the removal leg takes several away at once) and which
//! TLS-terminating protocol the listener that carries them speaks.
//!
//! The listener itself is created through Elastic Load Balancing's own API rather than declared
//! in the stack, so what the stack owns is the attachment alone: a certificate the listener
//! still presents after the stack is gone could then only be its own default one.
//!
//! Every certificate here is carried to `ISSUED` first, which is the only status a listener can
//! present a certificate in: a private-CA request needs no domain validation ("if you are
//! requesting a private certificate, domain validation is not required", `RequestCertificate`
//! doc), so it reaches that status and the suite waits for ACM to report it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, elements, form_encode, members_of, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The API versions an awsQuery request naming each model carries.
const CFN_VERSION: &str = "2010-05-15";
const ELBV2_VERSION: &str = "2015-12-01";
/// The API version an ec2Query request carries.
const EC2_VERSION: &str = "2016-11-15";

/// The host label and credential scope each service is addressed by.
const CFN_KEY: &str = "cloudformation";
const ELBV2_KEY: &str = "elasticloadbalancingv2";
const EC2_KEY: &str = "ec2";

/// The logical id the declared attachment carries in every row's template.
const LISTENER_CERT: &str = "ListenerCert";

/// The stack statuses the legs below wait for, and the resource status a refused update records
/// the provider's reason on.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";
const UPDATE_ROLLBACK_COMPLETE: &str = "UPDATE_ROLLBACK_COMPLETE";
const UPDATE_FAILED: &str = "UPDATE_FAILED";

/// A well-formed Certificate Manager certificate ARN that service never issued: the negative
/// leg's declaration, which only that service can resolve — and resolves to nothing.
const NEVER_ISSUED_ID: &str = "00000000-0000-0000-0000-000000000000";

/// How long a wait gives a state the clock owes (a stack's status, a certificate's issuance, an
/// attachment the deployment has made) and how often it looks. Generous, and only here so a
/// failure is a failure rather than a hang: every wait below waits for its CONDITION, never for
/// a time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as the binary assembles it — which is what makes
/// the hand-off real: CloudFormation's provisioning call is served by Elastic Load Balancing,
/// whose certificate resolution is served by Certificate Manager.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// Call one awsQuery operation of the service `key` addresses, answered with its status and XML
/// body. The key is both the host label and the credential scope, which are the two signals the
/// router resolves a service by.
async fn query(
    router: &MultiServiceRouter,
    key: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{key}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{key}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation operation that must succeed.
async fn cfn(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, CFN_KEY, CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CloudFormation {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One Elastic Load Balancing v2 operation that must succeed.
async fn elbv2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, ELBV2_KEY, ELBV2_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ELBv2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 operation that must succeed.
async fn ec2(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, EC2_KEY, EC2_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EC2 {action} must answer 200, got {status} with body {body}"
    );
    body
}

/// The VPC a suite's load balancer is placed in, and the subnets of it the placement names.
struct Network {
    vpc: String,
    subnets: Vec<String>,
}

/// Create one VPC with a subnet in each of two Availability Zones through Amazon EC2's own
/// operations: Elastic Load Balancing resolves a placement against that service's own records,
/// so the network a load balancer is created in is a network EC2 really holds.
async fn seed_network(router: &MultiServiceRouter, case: &str, prefix: &str) -> Network {
    let cidr = format!("{prefix}.0.0/16");
    let created = ec2(router, case, "CreateVpc", &[("CidrBlock", &cidr)]).await;
    let vpc = opt_element(&created, "vpcId")
        .unwrap_or_else(|| panic!("case {case:?}: no vpcId in {created}"));
    let mut subnets = Vec::new();
    for (index, suffix) in ["a", "b"].into_iter().enumerate() {
        let zone = format!("{REGION}{suffix}");
        let subnet_cidr = format!("{prefix}.{}.0/24", index + 1);
        let created = ec2(
            router,
            case,
            "CreateSubnet",
            &[
                ("VpcId", &vpc),
                ("CidrBlock", &subnet_cidr),
                ("AvailabilityZone", &zone),
            ],
        )
        .await;
        subnets.push(
            opt_element(&created, "subnetId")
                .unwrap_or_else(|| panic!("case {case:?}: no subnetId in {created}")),
        );
    }
    Network { vpc, subnets }
}

/// Create one INTERNAL application load balancer in `network` with one HTTPS or TLS listener
/// carrying `default` as its own default certificate, and answer that listener's ARN.
///
/// Internal, so the placement needs no internet gateway: what this suite is about is the
/// certificate list of the listener, not how the load balancer is reached.
async fn seed_listener(
    router: &MultiServiceRouter,
    case: &str,
    label: &str,
    network: &Network,
    protocol: &str,
    port: &str,
    default: &str,
) -> String {
    let created = elbv2(
        router,
        case,
        "CreateLoadBalancer",
        &[
            ("Name", &format!("alb-{label}")),
            ("Scheme", "internal"),
            ("Subnets.member.1", &network.subnets[0]),
            ("Subnets.member.2", &network.subnets[1]),
        ],
    )
    .await;
    let load_balancer = opt_element(&created, "LoadBalancerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no LoadBalancerArn in {created}"));
    let created = elbv2(
        router,
        case,
        "CreateTargetGroup",
        &[
            ("Name", &format!("tg-{label}")),
            ("Protocol", "HTTP"),
            ("Port", "80"),
            ("TargetType", "ip"),
            ("VpcId", &network.vpc),
        ],
    )
    .await;
    let target_group = opt_element(&created, "TargetGroupArn")
        .unwrap_or_else(|| panic!("case {case:?}: no TargetGroupArn in {created}"));
    let created = elbv2(
        router,
        case,
        "CreateListener",
        &[
            ("LoadBalancerArn", &load_balancer),
            ("Protocol", protocol),
            ("Port", port),
            ("SslPolicy", "ELBSecurityPolicy-2016-08"),
            ("Certificates.member.1.CertificateArn", default),
            ("DefaultActions.member.1.Type", "forward"),
            ("DefaultActions.member.1.TargetGroupArn", &target_group),
        ],
    )
    .await;
    opt_element(&created, "ListenerArn")
        .unwrap_or_else(|| panic!("case {case:?}: no ListenerArn in {created}"))
}

/// Call one Certificate Manager operation over its awsJson1_1 wire.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/acm/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// Request one PRIVATE certificate from ACM, wait until ACM reports it `ISSUED`, and answer the
/// ARN that service minted — the only status a listener can present a certificate in.
async fn issued_certificate(router: &MultiServiceRouter, case: &str, domain: &str) -> String {
    let authority = format!(
        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority\
         /12345678-1234-1234-1234-123456789012"
    );
    let (status, body) = acm(
        router,
        "RequestCertificate",
        &json!({ "DomainName": domain, "CertificateAuthorityArn": authority }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestCertificate must answer 200, got {status} {body}"
    );
    let arn = body["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {body}"))
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, described) = acm(
            router,
            "DescribeCertificate",
            &json!({ "CertificateArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeCertificate must answer 200, got {status} {described}"
        );
        let reported = described["Certificate"]["Status"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if reported == "ISSUED" {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected ACM to report ISSUED for {arn}, got {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What `DescribeListenerCertificates` reports for `listener`, over EVERY page it answers: one
/// entry per certificate, carrying that certificate's own `IsDefault`.
async fn listener_certificates(
    router: &MultiServiceRouter,
    case: &str,
    listener: &str,
) -> BTreeMap<String, bool> {
    let mut reported = BTreeMap::new();
    let mut marker: Option<String> = None;
    loop {
        let mut members: Vec<(&str, &str)> = vec![("ListenerArn", listener)];
        if let Some(marker) = marker.as_deref() {
            members.push(("Marker", marker));
        }
        let body = elbv2(router, case, "DescribeListenerCertificates", &members).await;
        let page = block_of(&body, "Certificates").unwrap_or_default();
        for entry in members_of(page) {
            let Some(arn) = opt_element(entry, "CertificateArn") else {
                continue;
            };
            let is_default = opt_element(entry, "IsDefault").as_deref() == Some("true");
            reported.insert(arn, is_default);
        }
        marker = opt_element(&body, "NextMarker").filter(|next| !next.is_empty());
        if marker.is_none() {
            return reported;
        }
    }
}

/// Wait until the listener presents exactly `expected`, and answer the last reading either way —
/// so the caller's assertion names the case and the actual value.
async fn wait_for_certificates(
    router: &MultiServiceRouter,
    case: &str,
    listener: &str,
    expected: &BTreeMap<String, bool>,
) -> BTreeMap<String, bool> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = listener_certificates(router, case, listener).await;
        if &reported == expected || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `StackStatus` a stack reports right now.
async fn stack_status(router: &MultiServiceRouter, case: &str, stack: &str) -> String {
    let body = cfn(router, case, "DescribeStacks", &[("StackName", stack)]).await;
    opt_element(&body, "StackStatus").unwrap_or_default()
}

/// Wait until the stack reports `wanted`, and answer the last status it reported either way.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = stack_status(router, case, stack).await;
        if reported == wanted || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// The `ResourceStatusReason` the stack's events carry for `logical_id` at `status`, which is
/// where a provider's own account of a refusal is reported.
async fn event_reason(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    logical_id: &str,
    status: &str,
) -> Option<String> {
    let body = cfn(router, case, "DescribeStackEvents", &[("StackName", stack)]).await;
    let events = block_of(&body, "StackEvents").unwrap_or_default();
    members_of(events).into_iter().find_map(|event| {
        let about = opt_element(event, "LogicalResourceId").unwrap_or_default() == logical_id
            && opt_element(event, "ResourceStatus").unwrap_or_default() == status;
        about
            .then(|| elements(event, "ResourceStatusReason").into_iter().next())
            .flatten()
    })
}

/// The template a row deploys: the attachment alone, over the listener and the certificates the
/// leg names.
fn template(listener: &str, certificates: &[String]) -> String {
    let declared: Vec<Value> = certificates
        .iter()
        .map(|arn| json!({ "CertificateArn": arn }))
        .collect();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a listener's certificate list",
        "Resources": {
            LISTENER_CERT: {
                "Type": "AWS::ElasticLoadBalancingV2::ListenerCertificate",
                "Properties": {
                    "ListenerArn": listener,
                    "Certificates": declared,
                }
            }
        }
    })
    .to_string()
}

/// The map the rule says the listener presents: the default certificate, and each attached one
/// with `IsDefault` false.
fn expected_certificates(default: &str, attached: &[String]) -> BTreeMap<String, bool> {
    let mut expected = BTreeMap::new();
    expected.insert(default.to_string(), true);
    for arn in attached {
        expected.insert(arn.clone(), false);
    }
    expected
}

/// One row of the table: the listener's protocol, and how many certificates the declared
/// attachment carries.
struct Row {
    case: &'static str,
    /// A protocol the model brackets a listener's certificates to.
    protocol: &'static str,
    /// How many certificates the resource declares, which is how many the create attaches and
    /// how many the update that names one other than them takes away.
    declared: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "one certificate on an HTTPS listener",
        protocol: "HTTPS",
        declared: 1,
    },
    Row {
        case: "two certificates on an HTTPS listener",
        protocol: "HTTPS",
        declared: 2,
    },
    Row {
        case: "one certificate on a TLS listener",
        protocol: "TLS",
        declared: 1,
    },
];

/// RULE: the certificates an `AWS::ElasticLoadBalancingV2::ListenerCertificate` declares are
/// really added to the listener's certificate list, an update re-points the list at what it now
/// names, a declaration Elastic Load Balancing refuses fails the resource with that service's
/// own refusal and leaves the list as it was, and the stack's delete takes away exactly what it
/// added.
#[tokio::test]
async fn a_declared_listener_certificate_is_attached_to_the_listener() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One emulator per row, so each row's assertions are about its own store.
        let router = router();
        let stack = format!("listener-certificate-stack-{index}");

        // The listener's OWN default certificate, which the attachment never touches, the
        // certificates the row attaches, and the one a later leg re-points the resource at.
        let default = issued_certificate(&router, case, "default.example.com").await;
        let mut attached = Vec::new();
        for ordinal in 1..=row.declared {
            attached
                .push(issued_certificate(&router, case, &format!("sni-{ordinal}.example.com")).await);
        }
        let replacement = issued_certificate(&router, case, "replacement.example.com").await;

        let network = seed_network(&router, case, &format!("10.{index}")).await;
        let listener = seed_listener(
            &router,
            case,
            &format!("listener-cert-{index}"),
            &network,
            row.protocol,
            "443",
            &default,
        )
        .await;

        // Before the stack: the listener presents its default certificate and nothing else.
        let only_the_default = expected_certificates(&default, &[]);
        let reported = listener_certificates(&router, case, &listener).await;
        assert_eq!(
            reported, only_the_default,
            "case {case:?}: expected only the listener's default certificate before the stack, \
             got {reported:?}"
        );

        // 1. The create ATTACHES every declared certificate.
        cfn(
            &router,
            case,
            "CreateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template(&listener, &attached)),
            ],
        )
        .await;
        let settled = wait_for_stack_status(&router, case, &stack, CREATE_COMPLETE).await;
        assert_eq!(
            settled, CREATE_COMPLETE,
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {settled:?}"
        );
        let expected = expected_certificates(&default, &attached);
        let reported = wait_for_certificates(&router, case, &listener, &expected).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the default certificate and the declared ones attached, \
             got {reported:?}"
        );

        // 2. An update re-points the resource at ANOTHER certificate: the new one is added and
        //    every certificate the declaration no longer names is removed.
        let swapped = vec![replacement.clone()];
        cfn(
            &router,
            case,
            "UpdateStack",
            &[
                ("StackName", &stack),
                ("TemplateBody", &template(&listener, &swapped)),
            ],
        )
        .await;
        let settled = wait_for_stack_status(&router, case, &stack, UPDATE_COMPLETE).await;
        assert_eq!(
            settled, UPDATE_COMPLETE,
            "case {case:?}: expected the stack to reach {UPDATE_COMPLETE}, got {settled:?}"
        );
        let expected = expected_certificates(&default, &swapped);
        let reported = wait_for_certificates(&router, case, &listener, &expected).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected only the certificate the update names beside the default, \
             got {reported:?}"
        );

        // 3. An update naming a well-formed certificate ARN that service never issued FAILS the
        //    resource with that service's own refusal, and the certificate in force stays on.
        let never_issued = format!("arn:aws:acm:{REGION}:{ACCOUNT}:certificate/{NEVER_ISSUED_ID}");
        cfn(
            &router,
            case,
            "UpdateStack",
            &[
                ("StackName", &stack),
                (
                    "TemplateBody",
                    &template(&listener, std::slice::from_ref(&never_issued)),
                ),
            ],
        )
        .await;
        let settled = wait_for_stack_status(&router, case, &stack, UPDATE_ROLLBACK_COMPLETE).await;
        assert_eq!(
            settled, UPDATE_ROLLBACK_COMPLETE,
            "case {case:?}: expected the stack to reach {UPDATE_ROLLBACK_COMPLETE}, got \
             {settled:?}"
        );
        let reason = event_reason(&router, case, &stack, LISTENER_CERT, UPDATE_FAILED)
            .await
            .unwrap_or_default();
        let opening = format!(
            "Certificate '{never_issued}' not found (Service: AmazonElasticLoadBalancing; \
             Status Code: 400; Error Code: CertificateNotFound; Request ID: "
        );
        assert!(
            reason.starts_with(&opening) && reason.ends_with("; Proxy: null)"),
            "case {case:?}: expected the refused resource's reason to read {opening:?}<the \
             call's id>\"; Proxy: null)\", got {reason:?}"
        );
        // The ONE variable part past the ARN is the call's own id, which is not empty.
        let identified = reason
            .strip_prefix(&opening)
            .and_then(|rest| rest.strip_suffix("; Proxy: null)"))
            .unwrap_or_default();
        assert!(
            !identified.is_empty(),
            "case {case:?}: expected the reason to state the call's request id, got {reason:?}"
        );
        let expected = expected_certificates(&default, &swapped);
        let reported = listener_certificates(&router, case, &listener).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected a refused update to leave the certificate in force \
             attached, got {reported:?}"
        );

        // 4. The stack's delete takes away exactly what it added: the listener is left
        //    presenting its own default certificate.
        cfn(&router, case, "DeleteStack", &[("StackName", &stack)]).await;
        let reported =
            wait_for_certificates(&router, case, &listener, &only_the_default).await;
        assert_eq!(
            reported, only_the_default,
            "case {case:?}: expected the deleted stack to leave only the listener's default \
             certificate, got {reported:?}"
        );
    }
}

/// RULE: a certificate list belongs to the listener the resource names, so a stack that declares
/// attachments on two listeners leaves each one presenting its own — the binding is not a
/// property of the certificate but of the pair.
#[tokio::test]
async fn each_declared_attachment_reaches_its_own_listener() {
    let case = "two listeners, one attachment each";
    let router = router();
    let default = issued_certificate(&router, case, "default.example.com").await;
    let first = issued_certificate(&router, case, "first.example.com").await;
    let second = issued_certificate(&router, case, "second.example.com").await;

    let network = seed_network(&router, case, "10.9").await;
    let mut listeners = Vec::new();
    for (ordinal, port) in ["443", "8443"].into_iter().enumerate() {
        listeners.push(
            seed_listener(
                &router,
                case,
                &format!("two-listener-{ordinal}"),
                &network,
                "HTTPS",
                port,
                &default,
            )
            .await,
        );
    }

    let body = json!({
        "Resources": {
            "FirstCert": {
                "Type": "AWS::ElasticLoadBalancingV2::ListenerCertificate",
                "Properties": {
                    "ListenerArn": listeners[0],
                    "Certificates": [{ "CertificateArn": first }],
                }
            },
            "SecondCert": {
                "Type": "AWS::ElasticLoadBalancingV2::ListenerCertificate",
                "Properties": {
                    "ListenerArn": listeners[1],
                    "Certificates": [{ "CertificateArn": second }],
                }
            }
        }
    })
    .to_string();
    let stack = "two-listener-certificate-stack";
    cfn(
        &router,
        case,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &body)],
    )
    .await;
    let settled = wait_for_stack_status(&router, case, stack, CREATE_COMPLETE).await;
    assert_eq!(
        settled, CREATE_COMPLETE,
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {settled:?}"
    );

    for (listener, own) in listeners.iter().zip([&first, &second]) {
        let expected = expected_certificates(&default, std::slice::from_ref(own));
        let reported = wait_for_certificates(&router, case, listener, &expected).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected {listener} to present its own certificate beside the \
             default, got {reported:?}"
        );
    }
}
