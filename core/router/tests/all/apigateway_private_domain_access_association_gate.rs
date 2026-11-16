// SPDX-License-Identifier: CC0-1.0

//! The readiness GATE a client waits on in front of API Gateway's PRIVATE custom-domain
//! surface: the `execute-api` interface VPC endpoint EC2 owns, and the domain name access
//! association that binds it to a private custom domain name.
//!
//! The rule under test: a resource a create answers in a NON-TERMINAL state reaches its
//! terminal state from the CLOCK alone — no further call by the caller — and the state the
//! clock materialised is the ONE value EVERY reader of that resource sees. Here those
//! readers are three, two of them in different services: `DescribeVpcEndpoints` naming the
//! endpoint by id, the `vpc-endpoint-state` filter that selects on that very state, and API
//! Gateway resolving the same identifier as an access association's source ("The identifier
//! of the domain name access association source. For a VPCE, the value is the VPC endpoint
//! ID", `CreateDomainNameAccessAssociation$accessAssociationSource`). So a client's
//! readiness poll converges, the surface behind the gate is reachable, and an identifier
//! naming no endpoint of the account is that operation's modelled `BadRequestException`
//! with nothing stored.
//!
//! On the wire EC2 spells the `State` enum's members with their first letter lowercased —
//! "`vpc-endpoint-state` - The state of the endpoint (`pendingAcceptance` | `pending` |
//! `available` | `deleting` | `deleted` | `rejected` | `failed`)"
//! (`DescribeVpcEndpointsRequest$Filters`) — which is the spelling a waiter compares
//! against, so the literals below are those values and not the enum member names.
//!
//! The rows differ along what the rule generalises over: how many subnets the endpoint was
//! created in, whether a security group was named, whether the private DNS member was sent
//! at all, and WHICH endpoint service of Amazon Web Services the endpoint reaches — none of
//! which decides the state, which is a fact of the endpoint's age. One further row is
//! created and DELETED while it is still non-terminal: the move the clock owed it is owed no
//! longer, so no later reader may report it, and API Gateway must refuse an association
//! naming it.
//!
//! This suite lives here rather than in either crate because the gate is only observable
//! across the two services on one engine: the endpoint and its state are EC2's, and the
//! private custom domain name and the access association that resolves that identifier are
//! API Gateway's. The EC2 half alone is tabled in that crate's
//! `tests/all/vpc_endpoint_readiness_poll.rs`, and the resolution of a source identifier in
//! the API Gateway crate's `tests/all/domain_name_access_association_endpoint.rs`.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on a sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::elbv2_network_resolution::{ec2_ok, Answer, BOOT_STACK_SIZE, REGION};
use crate::query_wire::{element, elements};

/// How long a readiness poll here is given before it is called a non-convergence.
///
/// The move under test is owed one of EC2's own transition spans — well under a second — and
/// nothing here sleeps FOR it: the condition is polled, so this deadline is only what ends a
/// state that never moves at all instead of hanging. It is therefore given a minute, which no
/// load this suite can meet will exhaust and no correct run will come near.
const POLL_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a poll waits between reads, as a client's own waiter does. This only bounds how
/// often the store is asked.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The non-terminal state an interface endpoint to an AWS-provided service is created in,
/// in the spelling EC2 puts on the wire.
const PENDING: &str = "pending";

/// The terminal state it reaches with nobody calling again, in the same spelling.
const AVAILABLE: &str = "available";

/// The `DescribeVpcEndpoints` filter that selects endpoints by that state, and the one that
/// selects them by id — both from that operation's own `Filters` documentation.
const STATE_FILTER: &str = "vpc-endpoint-state";
const ID_FILTER: &str = "vpc-endpoint-id";

/// The one value the modelled `AccessAssociationSourceType` enum declares.
const VPCE: &str = "VPCE";

/// The endpoint type a private custom domain name is reached through, as
/// `EndpointConfiguration$types` spells it.
const PRIVATE: &str = "PRIVATE";

/// The custom domain name the surface behind the gate is built on, and the name the
/// certificate below covers.
const DOMAIN_NAME: &str = "gate.private.example.com";

/// The account every call here is signed for, which is the one this emulator mints ARNs
/// under, for the one ARN this suite writes by hand: the private certificate authority
/// Certificate Manager records as the issuer of the certificate it is asked for.
const ACCOUNT: &str = "000000000000";

/// The one Certificate Manager status a certificate a custom domain name may present is in,
/// from that service's `CertificateStatus` enum, and the non-terminal status a requested
/// certificate waits in until its validation outcome falls due.
const CERTIFICATE_ISSUED: &str = "ISSUED";
const CERTIFICATE_PENDING_VALIDATION: &str = "PENDING_VALIDATION";

/// The error the API Gateway model declares for a request naming a resource it cannot
/// resolve, as a client reads it off `x-amzn-errortype`.
const BAD_REQUEST: &str = "BadRequestException";

/// What API Gateway refuses an `accessAssociationSource` naming no VPC endpoint of the
/// account with: the identifier the REQUEST sent, then the fixed text.
fn not_a_valid_endpoint_id(source: &str) -> String {
    format!("{source} is not a valid VPC endpoint id")
}

/// One API Gateway control-plane call, driven over the service's own restJson1 endpoint.
///
/// Written here rather than taken from the sibling `apigateway_private_vpce_invoke` suite
/// because that suite's request builder is private to it; what IS shared is the transport
/// and the EC2 plumbing (`crate::elbv2_network_resolution`), so both halves of this gate are
/// driven the way every other suite of this target drives them.
fn apigw(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (Answer, Option<String>) {
    let host = format!("apigateway.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", &host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/apigateway\
                 /aws4_request, SignedHeaders=host, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(body.unwrap_or_default().to_string()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    (
        Answer {
            status: response.status().as_u16(),
            text: String::from_utf8_lossy(response.body()).to_string(),
        },
        error,
    )
}

/// One Certificate Manager call over its own `awsJson1_1` endpoint, and the body it answered.
///
/// A custom domain name terminates TLS, so its create names the certificate the endpoint
/// presents and API Gateway resolves that against the service that holds it; the suite
/// therefore brings a real certificate in through Certificate Manager's own API rather than
/// naming one nothing issued.
fn acm_ok(
    case: &str,
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    operation: &str,
    body: &str,
) -> Value {
    let host = format!("acm.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/acm\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=deadbeef"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {operation}, got {status} with {text}"
    );
    parsed(case, &text)
}

/// One API Gateway call that has to answer `expected`, and the body it answered.
fn apigw_ok(
    case: &str,
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    method: &str,
    path: &str,
    body: Option<&str>,
    expected: u16,
) -> Value {
    let (answer, error) = apigw(router, runtime, method, path, body);
    assert_eq!(
        answer.status, expected,
        "case {case:?}: expected HTTP {expected} from {method} {path}, got {} {error:?} with \
         {:?}",
        answer.status, answer.text
    );
    parsed(case, &answer.text)
}

/// One JSON body, parsed.
fn parsed(case: &str, text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    serde_json::from_str(text)
        .unwrap_or_else(|error| panic!("case {case:?}: {error} reading {text}"))
}

/// One string member of a JSON body.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no string {name:?} in {body:?}"))
        .to_string()
}

/// Poll `read` until `settled` holds of what it answers, and answer that.
///
/// This is the client-side half of the rule under test: nothing is nudged between reads, so
/// a value that only ever changed because somebody called again would never satisfy the
/// predicate here either.
fn poll_until<R>(case: &str, mut read: impl FnMut() -> R, settled: impl Fn(&R) -> bool) -> R
where
    R: std::fmt::Debug,
{
    let deadline = Instant::now() + POLL_TIMEOUT;
    loop {
        let answer = read();
        if settled(&answer) {
            return answer;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the clock owed this move and it never came; last read {answer:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One row of the endpoint table: an interface endpoint created the way a client that is
/// about to use it creates one.
struct EndpointRow {
    case: &'static str,
    /// The endpoint service the endpoint is created to, without its Region label.
    service: &'static str,
    /// How many of the network's subnets the create names.
    subnets: usize,
    /// Whether the create names the VPC's own security group.
    with_group: bool,
    /// What the create sends in `PrivateDnsEnabled`, or `None` to leave the member out.
    private_dns: Option<bool>,
    /// Whether this endpoint is one a private custom domain name is reached through, and so
    /// one a domain name access association may name as its source. The row to another
    /// endpoint service is here for the EC2 half of the rule only.
    binds_a_domain: bool,
}

/// The rows: the `execute-api` endpoint a private custom domain name is defined in terms of,
/// created three ways, and one endpoint to another endpoint service of Amazon Web Services.
/// None of those differences decides the state.
fn endpoint_rows() -> Vec<EndpointRow> {
    vec![
        EndpointRow {
            case: "execute-api in one subnet, with a security group, private DNS off",
            service: "execute-api",
            subnets: 1,
            with_group: true,
            private_dns: Some(false),
            binds_a_domain: true,
        },
        EndpointRow {
            case: "execute-api in two subnets, with no security group and no private DNS member",
            service: "execute-api",
            subnets: 2,
            with_group: false,
            private_dns: None,
            binds_a_domain: true,
        },
        EndpointRow {
            case: "AWS Security Token Service in one subnet",
            service: "sts",
            subnets: 1,
            with_group: true,
            private_dns: Some(false),
            binds_a_domain: false,
        },
    ]
}

/// `CreateVpcEndpoint` for one row, answering `(the endpoint id, the state the create
/// answered)`.
fn create_endpoint(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    vpc: &str,
    subnets: &[String],
    group: &str,
    row: &EndpointRow,
) -> (String, String) {
    let service_name = format!("com.amazonaws.{REGION}.{}", row.service);
    let mut params: Vec<(String, String)> = vec![
        ("VpcId".to_string(), vpc.to_string()),
        ("ServiceName".to_string(), service_name),
        ("VpcEndpointType".to_string(), "Interface".to_string()),
    ];
    for (index, subnet) in subnets.iter().take(row.subnets).enumerate() {
        params.push((format!("SubnetId.{}", index + 1), subnet.clone()));
    }
    if row.with_group {
        params.push(("SecurityGroupId.1".to_string(), group.to_string()));
    }
    if let Some(private_dns) = row.private_dns {
        params.push(("PrivateDnsEnabled".to_string(), private_dns.to_string()));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let body = ec2_ok(router, runtime, "CreateVpcEndpoint", &borrowed);
    (element(&body, "vpcEndpointId"), element(&body, "state"))
}

/// The ids `DescribeVpcEndpoints` selects for one filter value.
fn filtered_ids(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    name: &str,
    value: &str,
) -> Vec<String> {
    let body = ec2_ok(
        router,
        runtime,
        "DescribeVpcEndpoints",
        &[("Filter.1.Name", name), ("Filter.1.Value.1", value)],
    );
    elements(&body, "vpcEndpointId")
}

/// The `accessAssociationSource` of every association the account holds, in page order.
fn listed_sources(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    case: &str,
) -> Vec<String> {
    let body = apigw_ok(
        case,
        router,
        runtime,
        "GET",
        "/domainnameaccessassociations?limit=500",
        None,
        200,
    );
    let items = match body.get("item") {
        None | Some(Value::Null) => Vec::new(),
        Some(item) => item
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected an item array, got {body:?}"))
            .clone(),
    };
    items
        .iter()
        .map(|item| member(case, item, "accessAssociationSource"))
        .collect()
}

#[test]
fn a_private_custom_domain_binds_the_endpoint_the_clock_made_available() {
    std::thread::Builder::new()
        .name("apigateway-private-domain-gate".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");

            // -----------------------------------------------------------------
            // The network every endpoint below is created in, built through EC2
            // itself, so every identifier asserted on is one EC2 minted.
            // -----------------------------------------------------------------
            let vpc = element(
                &ec2_ok(
                    &router,
                    &runtime,
                    "CreateVpc",
                    &[("CidrBlock", "10.221.0.0/16")],
                ),
                "vpcId",
            );
            // "To use private DNS, the VPC must have DNS hostnames and DNS resolution
            // enabled" (`CreateVpcEndpointRequest$PrivateDnsEnabled`), and an interface
            // endpoint created without that member asks for private DNS by default — so the
            // network turns both on, the way a client that provisions one does.
            for attribute in ["EnableDnsSupport", "EnableDnsHostnames"] {
                ec2_ok(
                    &router,
                    &runtime,
                    "ModifyVpcAttribute",
                    &[
                        ("VpcId", vpc.as_str()),
                        (&format!("{attribute}.Value"), "true"),
                    ],
                );
            }
            let mut subnets: Vec<String> = Vec::new();
            for (index, zone) in [format!("{REGION}a"), format!("{REGION}b")]
                .iter()
                .enumerate()
            {
                let created = ec2_ok(
                    &router,
                    &runtime,
                    "CreateSubnet",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("CidrBlock", &format!("10.221.{}.0/24", index + 1)),
                        ("AvailabilityZone", zone.as_str()),
                    ],
                );
                subnets.push(element(&created, "subnetId"));
            }
            let group = element(
                &ec2_ok(
                    &router,
                    &runtime,
                    "CreateSecurityGroup",
                    &[
                        ("VpcId", vpc.as_str()),
                        ("GroupName", "private-domain-gate"),
                        ("GroupDescription", "the group this suite created"),
                    ],
                ),
                "groupId",
            );

            // -----------------------------------------------------------------
            // Every row's endpoint, created the way a client creates one. The
            // create answers the NON-TERMINAL state, whatever the row.
            // -----------------------------------------------------------------
            let rows = endpoint_rows();
            let mut created: Vec<(&EndpointRow, String)> = Vec::new();
            for row in &rows {
                let (endpoint, answered) =
                    create_endpoint(&router, &runtime, &vpc, &subnets, &group, row);
                assert_eq!(
                    answered, PENDING,
                    "case {:?}: expected the create to answer {PENDING:?}, got {answered:?}",
                    row.case
                );
                created.push((row, endpoint));
            }

            // One more endpoint, created and DELETED while it is still non-terminal: the
            // move the clock owed it goes with it, so nothing may report it later.
            let doomed_row = EndpointRow {
                case: "an endpoint deleted while it was still pending",
                service: "execute-api",
                subnets: 1,
                with_group: true,
                private_dns: Some(false),
                binds_a_domain: false,
            };
            let (doomed, doomed_state) =
                create_endpoint(&router, &runtime, &vpc, &subnets, &group, &doomed_row);
            assert_eq!(
                doomed_state, PENDING,
                "case {:?}: expected the create to answer {PENDING:?}, got {doomed_state:?}",
                doomed_row.case
            );
            ec2_ok(
                &router,
                &runtime,
                "DeleteVpcEndpoints",
                &[("VpcEndpointId.1", doomed.as_str())],
            );

            // -----------------------------------------------------------------
            // The poll a waiter makes, per endpoint: the clock alone carries it
            // out of the non-terminal state, and what it holds then is the
            // terminal one.
            // -----------------------------------------------------------------
            for (row, endpoint) in &created {
                let read = || {
                    let body = ec2_ok(
                        &router,
                        &runtime,
                        "DescribeVpcEndpoints",
                        &[("VpcEndpointId.1", endpoint.as_str())],
                    );
                    (element(&body, "state"), body)
                };
                let (state, described) = poll_until(row.case, read, |(state, _)| state != PENDING);
                assert_eq!(
                    state, AVAILABLE,
                    "case {:?}: expected the poll on {endpoint:?} to end at {AVAILABLE:?}, \
                     got {state:?} in {described}",
                    row.case
                );
            }

            // The SAME materialized value is what the state filter selects on, so one
            // endpoint is never reported two ways: each is selected by the terminal state
            // and by none of them by the non-terminal one.
            let available_ids = filtered_ids(&router, &runtime, STATE_FILTER, AVAILABLE);
            let pending_ids = filtered_ids(&router, &runtime, STATE_FILTER, PENDING);
            for (row, endpoint) in &created {
                assert!(
                    available_ids.contains(endpoint),
                    "case {:?}: expected the {AVAILABLE:?} filter to select {endpoint:?}, \
                     got {available_ids:?}",
                    row.case
                );
                assert!(
                    !pending_ids.contains(endpoint),
                    "case {:?}: expected the {PENDING:?} filter not to select {endpoint:?}, \
                     got {pending_ids:?}",
                    row.case
                );
            }

            // The endpoint deleted while it was still owed a move is reported by NOBODY —
            // neither the id filter nor the state filters — and the waits above are what
            // carried the clock past the instant that move was due.
            for (case, name, value) in [
                ("the deleted endpoint, by id", ID_FILTER, doomed.as_str()),
                (
                    "the deleted endpoint, by the terminal state",
                    STATE_FILTER,
                    AVAILABLE,
                ),
                (
                    "the deleted endpoint, by the non-terminal state",
                    STATE_FILTER,
                    PENDING,
                ),
            ] {
                let selected = filtered_ids(&router, &runtime, name, value);
                assert!(
                    !selected.contains(&doomed),
                    "case {case:?}: expected {doomed:?} to be reported by nothing, got \
                     {selected:?}"
                );
            }

            // -----------------------------------------------------------------
            // The surface behind the gate: a PRIVATE custom domain name reached
            // through the endpoints that are now available.
            // -----------------------------------------------------------------
            // The certificate the private endpoint would PRESENT, brought in through
            // Certificate Manager's own API: a create that names none is refused, and API
            // Gateway resolves the one it names against the service that holds it, so the
            // certificate has to be a record that service reports as issued. "If you are
            // requesting a private certificate, domain validation is not required"
            // (`RequestCertificate`), which is the request that needs nothing published out
            // of band before the certificate can be used.
            let case = "the certificate the private endpoint presents";
            let requested = acm_ok(
                case,
                &router,
                &runtime,
                "RequestCertificate",
                &json!({
                    "DomainName": DOMAIN_NAME,
                    "CertificateAuthorityArn": format!(
                        "arn:aws:acm-pca:{REGION}:{ACCOUNT}:certificate-authority/\
                         11111111-2222-3333-4444-555555555555"
                    ),
                })
                .to_string(),
            );
            let certificate_arn = member(case, &requested, "CertificateArn");
            // Read back through the owner, which is also the wait the create below needs:
            // "domain validation is not required" for a private certificate, so the issuance
            // is owed from the request and falls due on the clock alone — the suite polls for
            // it rather than assuming it, and never sleeps for it.
            let read_status = || {
                let described = acm_ok(
                    case,
                    &router,
                    &runtime,
                    "DescribeCertificate",
                    &json!({ "CertificateArn": certificate_arn }).to_string(),
                );
                described
                    .get("Certificate")
                    .map(|certificate| member(case, certificate, "Status"))
                    .unwrap_or_else(|| panic!("case {case:?}: no Certificate in {described:?}"))
            };
            let status = poll_until(case, read_status, |status| {
                status != CERTIFICATE_PENDING_VALIDATION
            });
            assert_eq!(
                status, CERTIFICATE_ISSUED,
                "case {case:?}: expected {CERTIFICATE_ISSUED:?} for {certificate_arn:?}, got \
                 {status:?}"
            );

            let endpoint_ids: Vec<&str> = created
                .iter()
                .filter(|(row, _)| row.binds_a_domain)
                .map(|(_, endpoint)| endpoint.as_str())
                .collect();
            let domain_request = json!({
                "domainName": DOMAIN_NAME,
                "certificateArn": certificate_arn,
                "endpointConfiguration": {
                    "types": [PRIVATE],
                    "vpcEndpointIds": endpoint_ids,
                },
            })
            .to_string();
            let domain = apigw_ok(
                "the private custom domain name",
                &router,
                &runtime,
                "POST",
                "/domainnames",
                Some(&domain_request),
                201,
            );
            let domain_arn = member("the private custom domain name", &domain, "domainNameArn");

            // Each endpoint the account HAS is admitted as an access association source,
            // and each is its own source, so each stores its own association.
            for (row, endpoint) in created.iter().filter(|(row, _)| row.binds_a_domain) {
                let body = json!({
                    "domainNameArn": domain_arn,
                    "accessAssociationSourceType": VPCE,
                    "accessAssociationSource": endpoint,
                })
                .to_string();
                let association = apigw_ok(
                    row.case,
                    &router,
                    &runtime,
                    "POST",
                    "/domainnameaccessassociations",
                    Some(&body),
                    201,
                );
                let source = member(row.case, &association, "accessAssociationSource");
                assert_eq!(
                    source, *endpoint,
                    "case {:?}: expected the association to report the source it was \
                     created for, got {source:?}",
                    row.case
                );
            }

            // Read back through the implemented reader: what the creates reported is what
            // the collection holds.
            let listed = listed_sources(&router, &runtime, "after the admitted creates");
            for (row, endpoint) in created.iter().filter(|(row, _)| row.binds_a_domain) {
                assert!(
                    listed.contains(endpoint),
                    "case {:?}: expected {endpoint:?} among the listed sources, got {listed:?}",
                    row.case
                );
            }

            // -----------------------------------------------------------------
            // An identifier naming no endpoint of the account is the modelled
            // refusal, with nothing stored — the same resolution, answering the
            // other way. The rows differ along WHY it resolves to nothing: a
            // well-formed id of each width EC2 mints that nothing was created
            // under, one digit away from an id the account does hold, and the
            // endpoint that was deleted while it was still pending.
            // -----------------------------------------------------------------
            let held = endpoint_ids
                .first()
                .expect("a row that binds the domain name");
            // An id one HEXADECIMAL DIGIT away from one the account holds — the first digit of
            // the identifier rather than the last, since consecutively minted ids differ in
            // their last digits, and flipped to the other end of the digit range so the result
            // is an identifier no counter of this run has reached. Asserted below to be none
            // of the ids the account does hold, so this row can never become a positive case.
            let one_digit_off = {
                let hex = held
                    .strip_prefix("vpce-")
                    .expect("an endpoint id EC2 minted carries the prefix");
                let (first, rest) = hex.split_at(1);
                format!("vpce-{}{rest}", if first == "f" { "0" } else { "f" })
            };
            let held_ids: Vec<&str> = created
                .iter()
                .map(|(_, endpoint)| endpoint.as_str())
                .collect();
            assert!(
                !held_ids.contains(&one_digit_off.as_str()),
                "case \"an id one digit away from one the account does hold\": expected \
                 {one_digit_off:?} to name no endpoint of the account, got one of {held_ids:?}"
            );
            let refused: Vec<(&str, String)> = vec![
                (
                    "a 17-digit id the account has no endpoint under",
                    "vpce-0123456789abcdef0".to_string(),
                ),
                (
                    "an 8-digit id the account has no endpoint under",
                    "vpce-0a1b2c3d".to_string(),
                ),
                (
                    "an id one digit away from one the account does hold",
                    one_digit_off,
                ),
                (
                    "the endpoint deleted while it was still pending",
                    doomed.clone(),
                ),
            ];
            for (case, source) in &refused {
                let body = json!({
                    "domainNameArn": domain_arn,
                    "accessAssociationSourceType": VPCE,
                    "accessAssociationSource": source,
                })
                .to_string();
                let (answer, error) = apigw(
                    &router,
                    &runtime,
                    "POST",
                    "/domainnameaccessassociations",
                    Some(&body),
                );
                assert_eq!(
                    (answer.status, error.as_deref()),
                    (400, Some(BAD_REQUEST)),
                    "case {case:?}: expected 400 {BAD_REQUEST}, got {} {error:?} with {}",
                    answer.status,
                    answer.text
                );
                let parsed = parsed(case, &answer.text);
                let message = member(case, &parsed, "message");
                let expected = not_a_valid_endpoint_id(source);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
            }

            // NOTHING the refusals named was stored: the collection still holds exactly the
            // associations the admitted creates made.
            let listed = listed_sources(&router, &runtime, "after every refusal");
            assert_eq!(
                listed.len(),
                endpoint_ids.len(),
                "case \"after every refusal\": expected only the {} admitted associations, \
                 got {listed:?}",
                endpoint_ids.len()
            );
            for (case, source) in &refused {
                assert!(
                    !listed.contains(source),
                    "case {case:?}: expected no association stored for it, got {listed:?}"
                );
            }
        })
        .expect("the sized thread starts")
        .join()
        .expect("the suite runs to completion");
}
