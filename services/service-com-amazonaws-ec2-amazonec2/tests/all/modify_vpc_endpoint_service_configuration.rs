// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for `ModifyVpcEndpointServiceConfiguration` and the
//! `DescribeVpcEndpointServiceConfigurations` that reads back what it wrote, driven over
//! `ec2Query` (README.md / OPERATIONS.md "Wire protocol").
//!
//! The rule under test: a modify applies the `Add…`/`Remove…` pairs and the scalar members the
//! request carries to the STORED configuration the `ServiceId` resolves — the load balancers
//! behind it, whether a connection to it must be accepted, the address types and the Regions it
//! is offered from, and the private DNS name assigned to it — answers `Return=true`, and leaves
//! every member the request said nothing about exactly as it was; a request the service cannot
//! satisfy is refused with the EC2 code for its cause and changes nothing, and a `ServiceId` the
//! account offers no service under is refused as unresolvable, whether it never existed or has
//! been deleted.
//!
//! Each table row is a distinct input along what that rule generalises over: which member is
//! modified, whether it is added to or taken from, how many values the pair names, whether the
//! value is one the configuration already carries, and which member of the request is at fault.
//! "If you set or modify the private DNS name, you must prove that you own the private DNS
//! domain name", so the verification record a newly assigned name carries is part of the rule.
//! A modification is also ACCOUNTED: what the store charges a configuration's caller-supplied
//! data against its byte budget follows the record, so a modify charges what the record will
//! hold and refunds what it held, however often it is repeated.

use ec2::state::ComAmazonawsEc2Amazonec2State as Ec2State;
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{error_code, indexed};
use super::wire::{Param, child, child_text, document, param};

/// The Region every call here is signed for.
const REGION: &str = "us-west-2";

/// The HTTP status EC2 answers `DryRunOperation` with.
const DRY_RUN_STATUS: u16 = 412;

/// The emulator the tests drive. The store is reached only through the wire here, so the engine
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
    super::ec2_query::element(body, name)
        .unwrap_or_else(|| panic!("case {case:?}: no <{name}> in {body}"))
}

/// The ARN of the Network Load Balancer `name` — a resource of another service, so only ever a
/// well-formed ARN here: what this crate stores is the ARN the request named.
fn network_load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/net/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// The ARN of the Gateway Load Balancer `name`.
fn gateway_load_balancer_arn(name: &str) -> String {
    format!(
        "arn:aws:elasticloadbalancing:{REGION}:{account}:loadbalancer/gwy/{name}/0123456789abcdef",
        account = engine::emulator_account_id(),
    )
}

/// Configure one endpoint service behind the Network Load Balancers `names`, with `acceptance`
/// required, and answer the `ServiceId` the create minted.
async fn create_service(
    case: &str,
    router: &MultiServiceRouter,
    names: &[&str],
    acceptance: bool,
) -> String {
    let arns: Vec<String> = names.iter().map(|name| network_load_balancer_arn(name)).collect();
    let mut params = indexed(
        "NetworkLoadBalancerArn",
        &arns.iter().map(String::as_str).collect::<Vec<&str>>(),
    );
    params.push(param("AcceptanceRequired", &acceptance.to_string()));
    let body = ok(
        case,
        router,
        "CreateVpcEndpointServiceConfiguration",
        &params,
    )
    .await;
    member(case, &body, "serviceId")
}

/// The one stored configuration `DescribeVpcEndpointServiceConfigurations` reports for
/// `service_id`, as the members this suite asserts on.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Described {
    acceptance_required: String,
    network_load_balancer_arns: Vec<String>,
    gateway_load_balancer_arns: Vec<String>,
    supported_ip_address_types: Vec<String>,
    supported_regions: Vec<String>,
    private_dns_name: Option<String>,
    private_dns_name_state: Option<String>,
    remote_access_enabled: String,
}

/// The text of every `<item>` of the set `name` of `body`, read through the XML parser so the
/// items of ONE set are never confused with the identically shaped items of another.
fn set_texts(case: &str, body: &str, name: &str) -> Vec<String> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == name)
        .map(|set| {
            set.children()
                .filter(|item| item.is_element())
                .map(|item| item.text().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The `inner` member of every `<item>` of the set `name` — for a set whose items are structures.
fn set_members(case: &str, body: &str, name: &str, inner: &str) -> Vec<String> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == name)
        .map(|set| {
            set.children()
                .filter(|item| item.is_element())
                .filter_map(|item| child_text(item, inner))
                .collect()
        })
        .unwrap_or_default()
}

/// Read the stored configuration of `service_id` back through the describe.
async fn described(case: &str, router: &MultiServiceRouter, service_id: &str) -> Described {
    let body = ok(
        case,
        router,
        "DescribeVpcEndpointServiceConfigurations",
        &[param("ServiceId.1", service_id)],
    )
    .await;
    let parsed = document(case, &body);
    let private_dns_name_state = parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "privateDnsNameConfiguration")
        .and_then(|configuration| child_text(configuration, "state"));
    Described {
        acceptance_required: member(case, &body, "acceptanceRequired"),
        network_load_balancer_arns: set_texts(case, &body, "networkLoadBalancerArnSet"),
        gateway_load_balancer_arns: set_texts(case, &body, "gatewayLoadBalancerArnSet"),
        supported_ip_address_types: set_texts(case, &body, "supportedIpAddressTypeSet"),
        supported_regions: set_members(case, &body, "supportedRegionSet", "region"),
        private_dns_name: super::ec2_query::element(&body, "privateDnsName"),
        private_dns_name_state,
        remote_access_enabled: member(case, &body, "remoteAccessEnabled"),
    }
}

/// The `privateDnsNameConfiguration` of the one configuration a body carries, as the pair a
/// caller publishes to prove it owns the name.
fn verification_record(case: &str, body: &str) -> Option<(String, String)> {
    let parsed = document(case, body);
    parsed
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "privateDnsNameConfiguration")
        .and_then(|configuration| {
            let name = child(configuration, "name")?.text()?.to_string();
            let value = child(configuration, "value")?.text()?.to_string();
            Some((name, value))
        })
}

/// What one row of the modify table asks for, and what the configuration must report afterwards.
struct ModifyCase {
    case: &'static str,
    /// The form fields of the modify, beside the `ServiceId`.
    params: fn() -> Vec<Param>,
    /// The `AcceptanceRequired` the configuration must then report.
    acceptance_required: &'static str,
    /// The Network Load Balancers it must then stand behind, by name.
    network: &'static [&'static str],
    /// The address types it must then support.
    address_types: &'static [&'static str],
    /// The Regions it must then be offered from.
    regions: &'static [&'static str],
    /// The private DNS name it must then carry.
    private_dns_name: Option<&'static str>,
}

/// The rows: one per member a modify can change, each asking for a different KIND of change —
/// a scalar set, one value added, several added, a value removed, a value added that is already
/// held, a name assigned, and a name removed.
fn modify_cases() -> Vec<ModifyCase> {
    vec![
        ModifyCase {
            case: "acceptance required turned off",
            params: || vec![param("AcceptanceRequired", "false")],
            acceptance_required: "false",
            network: &["alpha"],
            address_types: &["ipv4"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "one more Network Load Balancer behind the service",
            params: || {
                vec![param(
                    "AddNetworkLoadBalancerArn.1",
                    &network_load_balancer_arn("beta"),
                )]
            },
            acceptance_required: "true",
            network: &["alpha", "beta"],
            address_types: &["ipv4"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "two more Network Load Balancers, one of them already behind it",
            params: || {
                vec![
                    param(
                        "AddNetworkLoadBalancerArn.1",
                        &network_load_balancer_arn("alpha"),
                    ),
                    param(
                        "AddNetworkLoadBalancerArn.2",
                        &network_load_balancer_arn("gamma"),
                    ),
                ]
            },
            acceptance_required: "true",
            network: &["alpha", "gamma"],
            address_types: &["ipv4"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "one Network Load Balancer taken from behind it",
            params: || {
                vec![
                    param(
                        "AddNetworkLoadBalancerArn.1",
                        &network_load_balancer_arn("delta"),
                    ),
                    param(
                        "RemoveNetworkLoadBalancerArn.1",
                        &network_load_balancer_arn("alpha"),
                    ),
                ]
            },
            acceptance_required: "true",
            network: &["delta"],
            address_types: &["ipv4"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "the second address type added",
            params: || vec![param("AddSupportedIpAddressType.1", "ipv6")],
            acceptance_required: "true",
            network: &["alpha"],
            address_types: &["ipv4", "ipv6"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "one address type swapped for the other",
            params: || {
                vec![
                    param("AddSupportedIpAddressType.1", "ipv6"),
                    param("RemoveSupportedIpAddressType.1", "ipv4"),
                ]
            },
            acceptance_required: "true",
            network: &["alpha"],
            address_types: &["ipv6"],
            regions: &[],
            private_dns_name: None,
        },
        ModifyCase {
            case: "two Regions it can be reached from added",
            params: || {
                vec![
                    param("AddSupportedRegion.1", "us-east-1"),
                    param("AddSupportedRegion.2", "eu-west-1"),
                ]
            },
            acceptance_required: "true",
            network: &["alpha"],
            address_types: &["ipv4"],
            regions: &["us-east-1", "eu-west-1"],
            private_dns_name: None,
        },
        ModifyCase {
            case: "a private DNS name assigned",
            params: || vec![param("PrivateDnsName", "payments.example.com")],
            acceptance_required: "true",
            network: &["alpha"],
            address_types: &["ipv4"],
            regions: &[],
            private_dns_name: Some("payments.example.com"),
        },
        ModifyCase {
            case: "everything at once",
            params: || {
                vec![
                    param("AcceptanceRequired", "false"),
                    param(
                        "AddNetworkLoadBalancerArn.1",
                        &network_load_balancer_arn("epsilon"),
                    ),
                    param("AddSupportedIpAddressType.1", "ipv6"),
                    param("AddSupportedRegion.1", "us-east-2"),
                    param("PrivateDnsName", "shared.example.org"),
                ]
            },
            acceptance_required: "false",
            network: &["alpha", "epsilon"],
            address_types: &["ipv4", "ipv6"],
            regions: &["us-east-2"],
            private_dns_name: Some("shared.example.org"),
        },
    ]
}

/// Every row is applied to the stored configuration and read back through the describe, so the
/// answer is what was stored rather than values assembled for the reply.
#[tokio::test]
async fn each_modification_is_applied_to_the_stored_configuration_and_read_back() {
    for row in modify_cases() {
        let case = row.case;
        let router = emulator();
        let service_id = create_service(case, &router, &["alpha"], true).await;

        let mut params = vec![param("ServiceId", &service_id)];
        params.extend((row.params)());
        let body = ok(
            case,
            &router,
            "ModifyVpcEndpointServiceConfiguration",
            &params,
        )
        .await;
        assert_eq!(
            super::ec2_query::element(&body, "return").as_deref(),
            Some("true"),
            "case {case:?}: expected <return>true</return>, got {body}"
        );

        let reported = described(case, &router, &service_id).await;
        let expected = Described {
            acceptance_required: row.acceptance_required.to_string(),
            network_load_balancer_arns: row
                .network
                .iter()
                .map(|name| network_load_balancer_arn(name))
                .collect(),
            gateway_load_balancer_arns: Vec::new(),
            supported_ip_address_types: row
                .address_types
                .iter()
                .map(|address_type| (*address_type).to_string())
                .collect(),
            supported_regions: row
                .regions
                .iter()
                .map(|region| (*region).to_string())
                .collect(),
            private_dns_name: row.private_dns_name.map(str::to_string),
            // "If you set or modify the private DNS name, you must prove that you own the
            // private DNS domain name": an assigned name starts unverified.
            private_dns_name_state: row
                .private_dns_name
                .map(|_| ec2::state::DNS_NAME_STATE_PENDING_VERIFICATION.to_string()),
            // "Indicates whether consumers can access the service from a Region other than the
            // Region where the service is hosted": true exactly when such a Region is offered.
            remote_access_enabled: row
                .regions
                .iter()
                .any(|region| *region != REGION)
                .to_string(),
        };
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the stored configuration the modify asked for, got \
             {reported:?}"
        );
    }
}

/// A name a modify ASSIGNS carries the verification record derived from the service and the
/// name, a second modify naming the same name leaves that record standing, and
/// `RemovePrivateDnsName` takes both away.
#[tokio::test]
async fn a_private_dns_name_carries_its_verification_record_until_it_is_removed() {
    let case = "the private DNS name of one service";
    let router = emulator();
    let service_id = create_service(case, &router, &["alpha"], true).await;

    let assign = |name: &str| -> Vec<Param> {
        vec![
            param("ServiceId", &service_id),
            param("PrivateDnsName", name),
        ]
    };
    ok(
        case,
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &assign("payments.example.com"),
    )
    .await;
    let body = ok(
        case,
        &router,
        "DescribeVpcEndpointServiceConfigurations",
        &[param("ServiceId.1", &service_id)],
    )
    .await;
    let first = verification_record(case, &body)
        .unwrap_or_else(|| panic!("case {case:?}: an assigned name must carry a record: {body}"));

    // The same name again proves nothing new, so the record it is already under stands.
    ok(
        case,
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &assign("payments.example.com"),
    )
    .await;
    let body = ok(
        case,
        &router,
        "DescribeVpcEndpointServiceConfigurations",
        &[param("ServiceId.1", &service_id)],
    )
    .await;
    assert_eq!(
        verification_record(case, &body).as_ref(),
        Some(&first),
        "case {case:?}: naming the same private DNS name must leave its record standing, got \
         {body}"
    );

    // A DIFFERENT name is a different proof, so the record changes with it.
    ok(
        case,
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &assign("billing.example.com"),
    )
    .await;
    let body = ok(
        case,
        &router,
        "DescribeVpcEndpointServiceConfigurations",
        &[param("ServiceId.1", &service_id)],
    )
    .await;
    let second = verification_record(case, &body)
        .unwrap_or_else(|| panic!("case {case:?}: the new name must carry a record: {body}"));
    assert_ne!(
        second, first,
        "case {case:?}: a different private DNS name must be proven with a record of its own"
    );

    // And removing the name takes the record with it.
    ok(
        case,
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &[
            param("ServiceId", &service_id),
            param("RemovePrivateDnsName", "true"),
        ],
    )
    .await;
    let reported = described(case, &router, &service_id).await;
    assert_eq!(
        (
            reported.private_dns_name.clone(),
            reported.private_dns_name_state.clone()
        ),
        (None, None),
        "case {case:?}: a removed private DNS name leaves neither the name nor its record, got \
         {reported:?}"
    );
}

/// An authorized `DryRun` of a modify that would have worked is answered `DryRunOperation` and
/// changes nothing.
#[tokio::test]
async fn a_dry_run_reports_dry_run_operation_and_changes_nothing() {
    // Rows: the modification the dry run asks for.
    let rows: [(&str, &[(&str, &str)]); 3] = [
        ("acceptance waived", &[("AcceptanceRequired", "false")]),
        ("an address type added", &[("AddSupportedIpAddressType.1", "ipv6")]),
        (
            "a private DNS name assigned",
            &[("PrivateDnsName", "payments.example.com")],
        ),
    ];

    for (case, members) in rows {
        let router = emulator();
        let service_id = create_service(case, &router, &["alpha"], true).await;
        let before = described(case, &router, &service_id).await;

        let mut params = vec![param("ServiceId", &service_id)];
        params.extend(members.iter().map(|(name, value)| param(name, value)));
        params.push(param("DryRun", "true"));
        let (status, body) = call(&router, "ModifyVpcEndpointServiceConfiguration", &params).await;
        assert_eq!(
            status, DRY_RUN_STATUS,
            "case {case:?}: expected HTTP {DRY_RUN_STATUS} from an authorized dry run, got \
             {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            ec2::state::DRY_RUN_OPERATION,
            "case {case:?}: expected DryRunOperation, got {body}"
        );
        assert_eq!(
            described(case, &router, &service_id).await,
            before,
            "case {case:?}: a dry run must leave the configuration exactly as it was"
        );
    }
}

/// One row of the refusal table: the members at fault and the code that cause is refused with.
struct RefusalCase {
    case: &'static str,
    /// The form fields beside the `ServiceId`. A value of [`HELD_ARN`] or [`GATEWAY_ARN`] stands
    /// for a load balancer ARN this row builds, so no ARN is spelt twice.
    members: &'static [(&'static str, &'static str)],
    /// The EC2 error code the request must be refused with.
    code: &'static str,
}

/// The placeholder for the ARN of the load balancer the service was created behind.
const HELD_ARN: &str = "the-held-arn";

/// The placeholder for the ARN of a Gateway Load Balancer, which is of the other kind.
const GATEWAY_ARN: &str = "a-gateway-arn";

/// Every request the service cannot satisfy is refused with the code for its own cause, and the
/// stored configuration is left exactly as it was.
#[tokio::test]
async fn a_request_the_service_cannot_satisfy_is_refused_and_changes_nothing() {
    // Rows: the members at fault, and the code that cause is refused with.
    let rows: [RefusalCase; 8] = [
        RefusalCase {
            case: "the last load balancer taken from behind the service",
            members: &[("RemoveNetworkLoadBalancerArn.1", HELD_ARN)],
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a Gateway Load Balancer put behind an interface service",
            members: &[("AddGatewayLoadBalancerArn.1", GATEWAY_ARN)],
            code: "InvalidParameterCombination",
        },
        RefusalCase {
            case: "a value that is not a load balancer ARN",
            members: &[("AddNetworkLoadBalancerArn.1", "not-an-arn")],
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "an address type outside the published pair",
            members: &[("AddSupportedIpAddressType.1", "ipv5")],
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a value that names no Region",
            members: &[("AddSupportedRegion.1", "middle-earth-1")],
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a private DNS name that is not a domain name",
            members: &[("PrivateDnsName", "not a domain")],
            code: "InvalidParameterValue",
        },
        RefusalCase {
            case: "a private DNS name both assigned and removed",
            members: &[
                ("PrivateDnsName", "payments.example.com"),
                ("RemovePrivateDnsName", "true"),
            ],
            code: "InvalidParameterCombination",
        },
        RefusalCase {
            case: "a top-level domain nobody owns as the private DNS name",
            members: &[("PrivateDnsName", "example")],
            code: "InvalidParameterValue",
        },
    ];

    for RefusalCase {
        case,
        members,
        code: expected_code,
    } in rows
    {
        let router = emulator();
        let service_id = create_service(case, &router, &["alpha"], true).await;
        let before = described(case, &router, &service_id).await;

        let mut params = vec![param("ServiceId", &service_id)];
        for (name, value) in members {
            let value = match *value {
                HELD_ARN => network_load_balancer_arn("alpha"),
                GATEWAY_ARN => gateway_load_balancer_arn("omega"),
                other => other.to_string(),
            };
            params.push(param(name, &value));
        }
        let (status, body) = call(&router, "ModifyVpcEndpointServiceConfiguration", &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the request to be refused, got {status} with body {body}"
        );
        assert_eq!(
            error_code(&body),
            expected_code,
            "case {case:?}: expected {expected_code}, got {body}"
        );
        assert_eq!(
            described(case, &router, &service_id).await,
            before,
            "case {case:?}: a refused modify must change nothing"
        );
    }
}

/// A `ServiceId` the account offers no service under is refused as unresolvable, and a value that
/// is not a service id at all as malformed — including the id of a service that has been DELETED,
/// which is how a client reads a modification of a configuration that is gone.
#[tokio::test]
async fn a_service_id_that_resolves_to_nothing_is_refused() {
    let (router, engine) = super::ec2_query::emulator();
    let deleted = create_service("a deleted service", &router, &["alpha"], true).await;
    ok(
        "a deleted service",
        &router,
        "DeleteVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[deleted.as_str()]),
    )
    .await;

    // Rows: the id the modify names, and the code it is refused with.
    let absent = format!(
        "{prefix}-{suffix:017x}",
        prefix = ec2::state::VPC_ENDPOINT_SERVICE_ID_PREFIX,
        suffix = 0x0f00_0000_0000_0000_u64,
    );
    let rows: [(&str, &str, &str); 4] = [
        (
            "the id of a service that was deleted",
            deleted.as_str(),
            ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND,
        ),
        (
            "an id this account never offered a service under",
            absent.as_str(),
            ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_NOT_FOUND,
        ),
        (
            "a value that is not a service id at all",
            "vpce-svc-not-hex",
            ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED,
        ),
        (
            "an endpoint id where a service id belongs",
            "vpce-0a1b2c3d4e5f6a7b8",
            ec2::state::INVALID_VPC_ENDPOINT_SERVICE_ID_MALFORMED,
        ),
    ];

    for (case, service_id, expected_code) in rows {
        // Rows differing along whether the flag is set as well: the service the call is scoped
        // to is resolved BEFORE the flag is read, so a probe naming a service that does not
        // exist is told so rather than that the request would have succeeded.
        for flag in [None, Some("true")] {
            let mut params = vec![
                param("ServiceId", service_id),
                param("AcceptanceRequired", "false"),
            ];
            if let Some(value) = flag {
                params.push(param("DryRun", value));
            }
            let (status, body) =
                call(&router, "ModifyVpcEndpointServiceConfiguration", &params).await;
            assert_eq!(
                status, 400,
                "case {case:?} with DryRun={flag:?}: expected the request to be refused, got \
                 {status} with body {body}"
            );
            assert_eq!(
                error_code(&body),
                expected_code,
                "case {case:?} with DryRun={flag:?}: expected {expected_code}, got {body}"
            );
        }
    }

    // And the id that was deleted names no stored record either, so the refusal is about what
    // the account holds rather than about a record left behind for the modify to read.
    assert!(
        !engine
            .state::<Ec2State>()
            .lock()
            .vpc_endpoint_services
            .contains_key(&deleted),
        "the deleted configuration {deleted} must be out of the store"
    );
}

/// A modification is ACCOUNTED against the store's byte budget by what the record will hold: a
/// modify that names no `PrivateDnsName` leaves the name the configuration already carries, so
/// its bytes are still held and the budget is unchanged however often the call is repeated; one
/// that assigns, grows, shrinks or removes caller-supplied data moves the budget by exactly that
/// much, and deleting the configuration gives all of it back.
#[tokio::test]
async fn a_modification_charges_what_the_record_will_hold() {
    let (router, engine) = super::ec2_query::emulator();
    let stored_bytes = || engine.state::<Ec2State>().lock().stored_bytes();
    let empty = stored_bytes();

    let case = "a service with a private DNS name";
    let service_id = create_service(case, &router, &["alpha"], true).await;
    // The private DNS name is the member the rows below never mention, so it is what a charge
    // read off the REQUEST rather than off the record would lose.
    let name = "payments.example.com";
    ok(
        case,
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &[param("ServiceId", &service_id), param("PrivateDnsName", name)],
    )
    .await;
    let held = stored_bytes();

    // Rows: modifications that change nothing the budget counts, repeated — each asks for a
    // member the private DNS name is not, and the one before it leaves the store as it found it.
    let rows: [(&str, &[(&str, &str)]); 4] = [
        ("acceptance waived", &[("AcceptanceRequired", "false")]),
        ("acceptance required again", &[("AcceptanceRequired", "true")]),
        (
            "an address type added",
            &[("AddSupportedIpAddressType.1", "ipv6")],
        ),
        (
            "the same address type added again",
            &[("AddSupportedIpAddressType.1", "ipv6")],
        ),
    ];
    for attempt in 1..=3 {
        for (row, members) in rows {
            let case = format!("{row} (attempt {attempt})");
            let mut params = vec![param("ServiceId", &service_id)];
            params.extend(members.iter().map(|(name, value)| param(name, value)));
            ok(&case, &router, "ModifyVpcEndpointServiceConfiguration", &params).await;
            let after = stored_bytes();
            assert_eq!(
                after, held,
                "case {case:?}: a modification that changes no caller-supplied data must leave \
                 the budget at {held} bytes, got {after}"
            );
            assert_eq!(
                described(&case, &router, &service_id).await.private_dns_name,
                Some(name.to_string()),
                "case {case:?}: the private DNS name the request never mentioned must still be \
                 carried"
            );
        }
    }

    // A modification that DOES change what the record holds moves the budget by that much: one
    // more load balancer behind the service costs its ARN, and removing it gives the bytes back.
    let second = network_load_balancer_arn("beta");
    ok(
        "one more load balancer",
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &[
            param("ServiceId", &service_id),
            param("AddNetworkLoadBalancerArn.1", &second),
        ],
    )
    .await;
    let grown = stored_bytes();
    assert_eq!(
        grown,
        held + second.len(),
        "the added ARN must be charged: the budget held {held} bytes and now holds {grown}"
    );
    ok(
        "the load balancer taken away again",
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &[
            param("ServiceId", &service_id),
            param("RemoveNetworkLoadBalancerArn.1", &second),
        ],
    )
    .await;
    let shrunk = stored_bytes();
    assert_eq!(
        shrunk, held,
        "the removed ARN must be refunded: expected {held} bytes, got {shrunk}"
    );

    // Removing the private DNS name refunds its bytes, and the delete then gives back everything
    // the configuration ever took.
    ok(
        "the private DNS name removed",
        &router,
        "ModifyVpcEndpointServiceConfiguration",
        &[
            param("ServiceId", &service_id),
            param("RemovePrivateDnsName", "true"),
        ],
    )
    .await;
    let without_name = stored_bytes();
    assert_eq!(
        without_name,
        held - name.len(),
        "the removed private DNS name must be refunded: expected {} bytes, got {without_name}",
        held - name.len()
    );
    ok(
        "the configuration deleted",
        &router,
        "DeleteVpcEndpointServiceConfigurations",
        &indexed("ServiceId", &[service_id.as_str()]),
    )
    .await;
    let after = stored_bytes();
    assert_eq!(
        after, empty,
        "the delete must give every byte back: the store held {empty} bytes before the \
         configuration and holds {after} after deleting it"
    );
}
